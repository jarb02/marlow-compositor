use crate::state::ClientState;
use crate::Marlow;

// ─── Compositor + SHM ───

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_surface::WlSurface},
        Client,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shm::{ShmHandler, ShmState},
    },
};

impl CompositorHandler for Marlow {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        if !smithay::wayland::compositor::is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
                root = parent;
            }
            // Check both spaces for the window
            if let Some(window) = self
                .user_space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            } else if let Some(window) = self
                .shadow_space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        };

        crate::shell::handle_commit(&mut self.popups, &self.user_space, &self.shadow_space, surface);
        crate::input::grabs::resize_grab::handle_commit(&mut self.user_space, surface);
    }
}

impl BufferHandler for Marlow {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Marlow {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(Marlow);
delegate_shm!(Marlow);

// ─── Seat ───

use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::{delegate_data_device, delegate_output, delegate_seat};

impl SeatHandler for Marlow {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Marlow> {
        &mut self.seat_state
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Data device focus only for the user seat (clipboard/DnD)
        if seat.name() == "user" {
            let dh = &self.display_handle;
            let client = focused.and_then(|s| dh.get_client(s.id()).ok());
            set_data_device_focus(dh, seat, client);
        }

        // Emit WindowFocused event for IPC subscribers
        if let Some(surface) = focused {
            if let Some(window_id) = self.surface_to_window_id(surface) {
                let title = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|d| d.lock().ok())
                        .and_then(|d| d.title.clone())
                        .unwrap_or_default()
                });
                self.event_queue.push(marlow_ipc::Event::WindowFocused {
                    window_id,
                    title,
                });
            }
        }
    }
}

delegate_seat!(Marlow);

// ─── Data Device (clipboard + drag-and-drop) ───

impl SelectionHandler for Marlow {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Marlow {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for Marlow {}
impl WaylandDndGrabHandler for Marlow {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let ptr = seat.get_pointer().unwrap();
                let start_data = ptr.grab_start_data().unwrap();
                let grab =
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                ptr.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => {
                source.cancel();
            }
        }
    }
}

delegate_data_device!(Marlow);

// ─── Output ───

impl OutputHandler for Marlow {}
delegate_output!(Marlow);

// ─── Layer Shell (wlr-layer-shell: waybar, etc.) ───

use smithay::delegate_layer_shell;
use smithay::desktop::{layer_map_for_output, LayerSurface};
use smithay::output::Output;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};

impl WlrLayerShellHandler for Marlow {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.user_space.outputs().next().unwrap().clone());
        let mut map = layer_map_for_output(&output);
        map.map_layer(&LayerSurface::new(surface, namespace)).unwrap();
        tracing::info!("Layer surface mapped");
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        if let Some((mut map, layer)) = self.user_space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&l| l.layer_surface() == &surface)
                .cloned();
            layer.map(|l| (map, l))
        }) {
            map.unmap_layer(&layer);
            tracing::info!("Layer surface destroyed");
        }
    }
}

delegate_layer_shell!(Marlow);
