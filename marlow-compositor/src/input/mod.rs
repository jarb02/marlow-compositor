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

/// Compositor keybind actions.
enum KeyAction {
    Quit,
    LaunchMarlow,
    LaunchTerminal,
}

/// Counter for diagnostic logging (first N events only).
static INPUT_LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const INPUT_LOG_MAX: u32 = 20;

impl Marlow {
    /// Process hardware input events — routed to user_seat only.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let code = event.key_code();

                // Diagnostic: log first N key events
                let n = INPUT_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < INPUT_LOG_MAX {
                    tracing::info!("Input: key code={code:?} state={:?}", event.state());
                }

                let action = self.user_seat.get_keyboard().unwrap().input::<KeyAction, _>(
                    self,
                    code,
                    event.state(),
                    serial,
                    time,
                    |_, modifiers, keysym| {
                        let sym = keysym.modified_sym();
                        if modifiers.ctrl && sym == keysyms::KEY_q.into() {
                            FilterResult::Intercept(KeyAction::Quit)
                        } else if modifiers.logo && sym == keysyms::KEY_m.into() {
                            FilterResult::Intercept(KeyAction::LaunchMarlow)
                        } else if modifiers.logo && sym == keysyms::KEY_Return.into() {
                            FilterResult::Intercept(KeyAction::LaunchTerminal)
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
                        std::process::Command::new("python3")
                            .arg("/home/josemarlow/marlow/launcher.py")
                            .spawn()
                            .ok();
                    }
                    Some(KeyAction::LaunchTerminal) => {
                        tracing::info!("Super+Return — launching terminal");
                        std::process::Command::new("foot").spawn().ok();
                    }
                    _ => {}
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                // Relative pointer motion (touchpad/mouse in KMS mode)
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

                // Diagnostic: log first N motion events
                let n = INPUT_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < INPUT_LOG_MAX {
                    tracing::info!("Input: pointer motion dx={:.1} dy={:.1} -> ({:.0},{:.0})",
                        delta.x, delta.y, pos.x, pos.y);
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

                // Diagnostic: log button events
                let n = INPUT_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < INPUT_LOG_MAX {
                    tracing::info!("Input: button={button} state={button_state:?} at ({:.0},{:.0})",
                        pointer.current_location().x, pointer.current_location().y);
                }

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

                        // Conflict check: if agent is focused on this window, agent loses focus
                        let agent_kb = self.agent_seat.get_keyboard().unwrap();
                        if let Some(agent_focus) = agent_kb.current_focus() {
                            if self.seat_arbiter.check_conflict(&surface, Some(&agent_focus)) {
                                let window_id = self
                                    .surface_to_window_id(&surface)
                                    .unwrap_or(u64::MAX);
                                // Clear agent focus — user takes over
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
                        self.user_space.elements().for_each(|window| {
                            window.set_activated(false);
                            window.toplevel().unwrap().send_pending_configure();
                        });
                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
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
