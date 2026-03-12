pub mod grabs;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
};

use crate::Marlow;
use smithay::desktop::{layer_map_for_output, WindowSurfaceType};
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use crate::input::grabs::{MoveSurfaceGrab, ResizeSurfaceGrab};

/// Compositor keybind actions.
enum KeyAction {
    Quit,
    LaunchMarlow,
    LaunchTerminal,
    VoicePTTPress,
    VoicePTTRelease,
    ProactivityToggle,
    Logout,
    PowerMenu,
    LaunchApps,
}

impl Marlow {
    /// Process hardware input events — routed to user_seat only.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let code = event.key_code();

                let key_state = event.state();
                let action = self.user_seat.get_keyboard().unwrap().input::<KeyAction, _>(
                    self,
                    code,
                    key_state,
                    serial,
                    time,
                    |_, modifiers, keysym| {
                        let sym = keysym.modified_sym();

                        // Always log Super key presses
                        if modifiers.logo {
                            tracing::info!(
                                "KEY: logo={} keysym={:#x} code={:?} ctrl={} alt={} shift={}",
                                modifiers.logo, Into::<u32>::into(sym), code,
                                modifiers.ctrl, modifiers.alt, modifiers.shift
                            );
                        }

                        use smithay::backend::input::KeyState;
                        if key_state == KeyState::Released {
                            // Super+V release triggers voice PTT stop
                            if modifiers.logo && sym == keysyms::KEY_v.into() {
                                FilterResult::Intercept(KeyAction::VoicePTTRelease)
                            } else {
                                FilterResult::Forward
                            }
                        } else if modifiers.ctrl && sym == keysyms::KEY_q.into() {
                            FilterResult::Intercept(KeyAction::Quit)
                        } else if modifiers.logo && sym == keysyms::KEY_m.into() {
                            FilterResult::Intercept(KeyAction::LaunchMarlow)
                        } else if modifiers.logo && sym == keysyms::KEY_Return.into() {
                            FilterResult::Intercept(KeyAction::LaunchTerminal)
                        } else if modifiers.logo && sym == keysyms::KEY_Escape.into() {
                            FilterResult::Intercept(KeyAction::ProactivityToggle)
                        } else if modifiers.logo && modifiers.shift && sym == keysyms::KEY_e.into() {
                            FilterResult::Intercept(KeyAction::Logout)
                        } else if modifiers.logo && sym == keysyms::KEY_a.into() {
                            FilterResult::Intercept(KeyAction::LaunchApps)
                        } else if modifiers.logo && sym == keysyms::KEY_p.into() {
                            FilterResult::Intercept(KeyAction::PowerMenu)
                        } else if modifiers.logo && sym == keysyms::KEY_v.into() {
                            FilterResult::Intercept(KeyAction::VoicePTTPress)
                        } else {
                            FilterResult::Forward
                        }
                    },
                );

                match action {
                    Some(KeyAction::Quit) => {
                        tracing::info!("Ctrl+Q detected — stopping compositor");
                        self.loop_signal.stop();
                    }
                    Some(KeyAction::LaunchMarlow) => {
                        tracing::info!("Super+M — launching Marlow launcher");
                        match std::process::Command::new("python3")
                            .arg("/home/josemarlow/marlow/marlow/launcher.py")
                            .env("WAYLAND_DISPLAY", &self.socket_name)
                            .env("XDG_RUNTIME_DIR",
                                std::env::var("XDG_RUNTIME_DIR").unwrap_or_default())
                            .spawn()
                        {
                            Ok(_) => tracing::info!("Launcher spawned OK"),
                            Err(e) => tracing::error!("Failed to spawn launcher: {e}"),
                        }
                    }
                    Some(KeyAction::LaunchTerminal) => {
                        tracing::info!("Super+Return — launching terminal");
                        std::process::Command::new("foot")
                            .args(["--override", "colors.background=282c34",
                                   "--override", "pad=8x8"])
                            .spawn().ok();
                    }
                    Some(KeyAction::VoicePTTPress) => {
                        tracing::info!("Super+V — voice push-to-talk START");
                        let _ = std::fs::write("/tmp/marlow-voice-trigger", "press");
                    }
                    Some(KeyAction::ProactivityToggle) => {
                        tracing::info!("Super+Escape — toggling proactivity");
                        self.event_queue.push(marlow_ipc::Event::ProactivityToggle);
                    }
                    Some(KeyAction::VoicePTTRelease) => {
                        tracing::info!("Super+V — voice push-to-talk STOP");
                        let _ = std::fs::write("/tmp/marlow-voice-trigger", "release");
                    }
                    Some(KeyAction::Logout) => {
                        tracing::info!("Super+Shift+E — logout");
                        self.loop_signal.stop();
                    }
                    Some(KeyAction::LaunchApps) => {
                        tracing::info!("Super+A — app launcher");
                        std::process::Command::new("wofi")
                            .args(["--show", "drun", "--allow-images", "--image-size", "24"])
                            .env("WAYLAND_DISPLAY", &self.socket_name)
                            .env("XDG_RUNTIME_DIR",
                                std::env::var("XDG_RUNTIME_DIR").unwrap_or_default())
                            .spawn()
                            .ok();
                    }
                    Some(KeyAction::PowerMenu) => {
                        tracing::info!("Super+P — power menu");
                        std::process::Command::new("marlow-power-menu")
                            .env("WAYLAND_DISPLAY", &self.socket_name)
                            .env("XDG_RUNTIME_DIR",
                                std::env::var("XDG_RUNTIME_DIR").unwrap_or_default())
                            .spawn()
                            .ok();
                    }
                    _ => {}
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.user_seat.get_pointer().unwrap();
                let mut pos = pointer.current_location();
                let delta = event.delta();
                pos += delta;

                // Clamp to output bounds
                if let Some(output) = self.user_space.outputs().next() {
                    if let Some(output_geo) = self.user_space.output_geometry(output) {
                        pos.x = pos.x.clamp(
                            output_geo.loc.x as f64,
                            (output_geo.loc.x + output_geo.size.w) as f64,
                        );
                        pos.y = pos.y.clamp(
                            output_geo.loc.y as f64,
                            (output_geo.loc.y + output_geo.size.h) as f64,
                        );
                    }
                }

                let serial = SERIAL_COUNTER.next_serial();
                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.user_space.outputs().next().unwrap();
                let output_geo = self.user_space.output_geometry(output).unwrap();
                let pos =
                    event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.user_seat.get_pointer().unwrap();
                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.user_seat.get_pointer().unwrap();
                let keyboard = self.user_seat.get_keyboard().unwrap();
                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let button_state = event.state();

                // Check if Alt is held for compositor-initiated move/resize
                let alt_held = keyboard.modifier_state().alt;

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    if let Some((window, _loc)) = self
                        .user_space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
                    {
                        let surface = window.toplevel().unwrap().wl_surface().clone();
                        self.user_space.raise_element(&window, true);
                        keyboard.set_focus(
                            self,
                            Some(surface.clone()),
                            serial,
                        );

                        // Alt+Left click = move, Alt+Right click = resize
                        if alt_held {
                            let initial_window_location = self.user_space.element_location(&window).unwrap();
                            // BTN_LEFT = 0x110 (272), BTN_RIGHT = 0x111 (273)
                            if button == 0x110 {
                                let start_data = pointer.grab_start_data().unwrap();
                                let grab = MoveSurfaceGrab {
                                    start_data,
                                    window: window.clone(),
                                    initial_window_location,
                                };
                                pointer.set_grab(self, grab, serial, smithay::input::pointer::Focus::Clear);
                            } else if button == 0x111 {
                                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                                let initial_window_size = window.geometry().size;
                                let start_data = pointer.grab_start_data().unwrap();
                                let grab = ResizeSurfaceGrab::start(
                                    start_data,
                                    window.clone(),
                                    crate::input::grabs::resize_grab::ResizeEdge::BOTTOM_RIGHT,
                                    smithay::utils::Rectangle::new(initial_window_location, initial_window_size),
                                );
                                window.toplevel().unwrap().with_pending_state(|state| {
                                    state.states.set(xdg_toplevel::State::Resizing);
                                });
                                window.toplevel().unwrap().send_pending_configure();
                                pointer.set_grab(self, grab, serial, smithay::input::pointer::Focus::Clear);
                            }
                        }

                        // Conflict check: if agent is focused on this window, agent loses focus
                        let agent_kb = self.agent_seat.get_keyboard().unwrap();
                        if let Some(agent_focus) = agent_kb.current_focus() {
                            if self.seat_arbiter.check_conflict(&surface, Some(&agent_focus)) {
                                let window_id = self
                                    .surface_to_window_id(&surface)
                                    .unwrap_or(u64::MAX);
                                agent_kb.set_focus(
                                    self,
                                    Option::<WlSurface>::None,
                                    serial,
                                );
                                self.event_queue.push(
                                    marlow_ipc::Event::ConflictDetected {
                                        window_id,
                                        reason: "user_override".to_string(),
                                    },
                                );
                                tracing::info!(
                                    "Seat conflict: user overrides agent on window {window_id}"
                                );
                            }
                        }

                        self.user_space.elements().for_each(|window| {
                            window.toplevel().unwrap().send_pending_configure();
                        });
                    } else {
                        // Check if click landed on a layer surface (sidebar, etc.)
                        // First pass: find the surface (immutable borrow of self)
                        let layer_wl_surface = {
                            let output = self.user_space.outputs().next().cloned();
                            output.and_then(|o| {
                                let layer_map = layer_map_for_output(&o);
                                for layer_type in [WlrLayer::Overlay, WlrLayer::Top] {
                                    for layer in layer_map.layers_on(layer_type) {
                                        if layer.can_receive_keyboard_focus() {
                                            if let Some(geo) = layer_map.layer_geometry(layer) {
                                                let relative = pointer.current_location() - geo.loc.to_f64();
                                                if layer.surface_under(relative, WindowSurfaceType::ALL).is_some() {
                                                    return Some(layer.layer_surface().wl_surface().clone());
                                                }
                                            }
                                        }
                                    }
                                }
                                None
                            })
                        };

                        // Second pass: grant focus (mutable borrow of self)
                        if let Some(wl_surface) = layer_wl_surface {
                            keyboard.set_focus(self, Some(wl_surface), serial);
                            tracing::info!("Keyboard focus granted to layer surface");
                        } else {
                            self.user_space.elements().for_each(|window| {
                                window.set_activated(false);
                                window.toplevel().unwrap().send_pending_configure();
                            });
                            keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                        }
                    }
                };

                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();

                let horizontal_amount = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| {
                        event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
                    });
                let vertical_amount = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| {
                        event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.
                    });
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.user_seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}
