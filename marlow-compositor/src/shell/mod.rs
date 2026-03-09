use smithay::{
    delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, PopupKind, PopupManager, Space, Window,
    },
    input::{
        pointer::{Focus, GrabStartData as PointerGrabStartData},
        Seat,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            protocol::{wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Rectangle, Serial, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
    },
};

use smithay::desktop::layer_map_for_output;

use crate::{
    input::grabs::{MoveSurfaceGrab, ResizeSurfaceGrab},
    Marlow,
};

impl XdgShellHandler for Marlow {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());

        // Extract metadata
        let wl_surface = surface.wl_surface();
        let (title, app_id) = with_states(wl_surface, |states| {
            let data = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap());
            match data {
                Some(d) => (
                    d.title.clone().unwrap_or_default(),
                    d.app_id.clone().unwrap_or_default(),
                ),
                None => (String::new(), String::new()),
            }
        });

        // Register with stable ID
        let window_id = self.register_window(window.clone());

        // Route to shadow_space if there are pending shadow launches
        // Timeout: if shadow_pending_count has been > 0 for more than 10s, reset it
        if self.shadow_pending_count > 0 {
            if let Some(ts) = self.shadow_pending_timestamp {
                if ts.elapsed().as_secs() > 10 {
                    tracing::warn!(
                        "shadow_pending_count={} timed out after 10s, resetting to 0",
                        self.shadow_pending_count
                    );
                    self.shadow_pending_count = 0;
                    self.shadow_pending_timestamp = None;
                }
            }
        }

        if self.shadow_pending_count > 0 {
            self.shadow_pending_count -= 1;
            if self.shadow_pending_count == 0 {
                self.shadow_pending_timestamp = None;
            }
            self.shadow_space.map_element(window, (0, 0), false);
            self.shadow_window_ids.insert(window_id);
            tracing::info!("Window {window_id} mapped to shadow_space (title={title:?})");
        } else {
            self.user_space.map_element(window.clone(), (0, 0), false);
            tracing::info!(
                "Window {window_id} mapped to user_space (title={title:?}, app_id={app_id:?})"
            );
            self.tile_windows();
        }

        // Emit WindowCreated event
        self.event_queue.push(marlow_ipc::Event::WindowCreated {
            window_id,
            title,
            app_id,
        });
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();
        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();
            let window = self
                .user_space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.user_space.element_location(&window).unwrap();

            let grab = MoveSurfaceGrab {
                start_data,
                window,
                initial_window_location,
            };

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();
        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();
            let window = self
                .user_space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.user_space.element_location(&window).unwrap();
            let initial_window_size = window.geometry().size;

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });
            surface.send_pending_configure();

            let grab = ResizeSurfaceGrab::start(
                start_data,
                window,
                edges.into(),
                Rectangle::new(initial_window_location, initial_window_size),
            );

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
}

delegate_xdg_shell!(Marlow);

fn check_grab(
    seat: &Seat<Marlow>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<PointerGrabStartData<Marlow>> {
    let pointer = seat.get_pointer()?;

    if !pointer.has_grab(serial) {
        return None;
    }

    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;

    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }

    Some(start_data)
}

/// Should be called on `WlSurface::commit`
pub fn handle_commit(
    popups: &mut PopupManager,
    user_space: &Space<Window>,
    shadow_space: &Space<Window>,
    surface: &WlSurface,
) {
    // Handle toplevel commits — check both spaces
    let window = user_space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
        .cloned()
        .or_else(|| {
            shadow_space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == surface)
                .cloned()
        });

    if let Some(window) = window {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        if !initial_configure_sent {
            window.toplevel().unwrap().send_configure();
        }
    }

    // Handle popup commits
    popups.commit(surface);
    if let Some(popup) = popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    xdg.send_configure().expect("initial configure failed");
                }
            }
            PopupKind::InputMethod(ref _input_method) => {}
        }
    }
}

impl Marlow {
    /// Tile all user_space windows in a master-stack layout.
    ///
    /// - 1 window: full available area
    /// - 2 windows: split left/right (50/50)
    /// - 3+ windows: master left half, stack on right
    pub fn tile_windows(&mut self) {
        let Some(output) = self.user_space.outputs().next().cloned() else {
            return;
        };

        let map = layer_map_for_output(&output);
        let zone = map.non_exclusive_zone();

        // Available tiling area (non-exclusive zone excludes waybar + layer surfaces)
        let output_geo = self
            .user_space
            .output_geometry(&output)
            .unwrap_or_else(|| smithay::utils::Rectangle::from_loc_and_size((0, 0), (1366, 768)));

        let area_x = zone.loc.x;
        let area_y = if zone.loc.y > 0 {
            zone.loc.y
        } else {
            // Fallback: scan Top layer surfaces for height
            use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
            let mut top_h = 0;
            for layer in map.layers_on(WlrLayer::Top) {
                if let Some(geo) = map.layer_geometry(layer) {
                    if geo.size.h > 0 {
                        top_h = geo.size.h;
                        break;
                    }
                }
            }
            if top_h == 0 {
                top_h = 32;
            }
            top_h
        };
        let area_w = if zone.size.w > 0 {
            zone.size.w
        } else {
            output_geo.size.w
        };
        let area_h = if zone.size.h > 0 {
            zone.size.h
        } else {
            output_geo.size.h - area_y
        };

        let windows: Vec<_> = self.user_space.elements().cloned().collect();
        let count = windows.len();
        if count == 0 {
            return;
        }

        let gap = 4i32;

        for (i, window) in windows.iter().enumerate() {
            let (x, y, w, h) = match count {
                1 => (area_x, area_y, area_w, area_h),
                2 => {
                    let half_w = (area_w - gap) / 2;
                    if i == 0 {
                        (area_x, area_y, half_w, area_h)
                    } else {
                        (area_x + half_w + gap, area_y, area_w - half_w - gap, area_h)
                    }
                }
                _ => {
                    // Master-stack layout
                    let master_w = (area_w - gap) / 2;
                    if i == 0 {
                        (area_x, area_y, master_w, area_h)
                    } else {
                        let stack_n = (count - 1) as i32;
                        let stack_h = (area_h - gap * (stack_n - 1)) / stack_n;
                        let si = (i - 1) as i32;
                        (
                            area_x + master_w + gap,
                            area_y + si * (stack_h + gap),
                            area_w - master_w - gap,
                            stack_h,
                        )
                    }
                }
            };

            // Reposition window in space
            self.user_space.map_element(window.clone(), (x, y), false);

            // Send configure with tiled size
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(Size::from((w, h)));
                });
                toplevel.send_pending_configure();
            }
        }

        tracing::info!(
            "Tiled {} windows in {}x{} area at ({},{})",
            count,
            area_w,
            area_h,
            area_x,
            area_y
        );
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .user_space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        else {
            return;
        };

        let output = self.user_space.outputs().next().unwrap();
        let output_geo = self.user_space.output_geometry(output).unwrap();
        let window_geo = self.user_space.element_geometry(window).unwrap();

        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
