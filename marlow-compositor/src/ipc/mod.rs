use std::io::{self, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use marlow_ipc::{write_message, Event, Request, Response, WindowInfo};
use smithay::utils::IsAlive;
use serde_json::json;
use smithay::backend::input::{ButtonState, KeyState};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::utils::SERIAL_COUNTER;

use crate::Marlow;

/// Default socket path: $XDG_RUNTIME_DIR/marlow-compositor.sock
fn socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("marlow-compositor.sock")
}

/// Initialize the IPC Unix socket server.
pub fn init_ipc(state: &mut Marlow) -> io::Result<PathBuf> {
    let path = socket_path();

    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    state.ipc_listener = Some(listener);
    state.ipc_socket_path = Some(path.clone());

    tracing::info!("IPC server listening on {}", path.display());
    Ok(path)
}

/// Poll for new connections, process requests, and push events.
/// Call this from the calloop idle callback.
pub fn poll_ipc(state: &mut Marlow) {
    // Accept new connections
    if let Some(ref listener) = state.ipc_listener {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true).ok();
                    tracing::info!("IPC client connected");
                    state.ipc_clients.push(stream);
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::error!("IPC accept error: {e}");
                    break;
                }
            }
        }
    }

    // Process requests
    let mut to_remove = Vec::new();
    let n = state.ipc_clients.len();
    for i in 0..n {
        match try_read_request(&mut state.ipc_clients[i]) {
            Ok(Some(request)) => {
                tracing::info!("IPC request from client {i}: {request:?}");
                let response = handle_request(request, state, i);
                if write_message(&mut state.ipc_clients[i], &response).is_err() {
                    to_remove.push(i);
                }
            }
            Ok(None) => {} // No data (WouldBlock)
            Err(_) => {
                tracing::info!("IPC client disconnected");
                to_remove.push(i);
            }
        }
    }

    // Push queued events to subscribed clients
    if !state.event_queue.is_empty() && !state.ipc_subscribed.is_empty() {
        let events: Vec<Event> = state.event_queue.drain(..).collect();
        let mut sub_remove = Vec::new();

        for &client_idx in &state.ipc_subscribed {
            if client_idx < state.ipc_clients.len() && !to_remove.contains(&client_idx) {
                for event in &events {
                    if write_message(&mut state.ipc_clients[client_idx], event).is_err() {
                        sub_remove.push(client_idx);
                        break;
                    }
                }
            }
        }

        for idx in sub_remove {
            state.ipc_subscribed.remove(&idx);
        }
    }

    // Remove disconnected clients (in reverse to preserve indices)
    for i in to_remove.into_iter().rev() {
        state.ipc_clients.remove(i);
        state.ipc_subscribed.remove(&i);
        // Shift down subscribed indices above the removed one
        let shifted: Vec<usize> = state
            .ipc_subscribed
            .iter()
            .map(|&idx| if idx > i { idx - 1 } else { idx })
            .collect();
        state.ipc_subscribed = shifted.into_iter().collect();
    }
}

/// Try to read a framed request. Non-blocking.
fn try_read_request(stream: &mut UnixStream) -> io::Result<Option<Request>> {
    let mut len_buf = [0u8; 4];
    match stream.read(&mut len_buf) {
        Ok(0) => return Err(io::Error::new(io::ErrorKind::ConnectionReset, "EOF")),
        Ok(n) if n < 4 => {
            stream.set_nonblocking(false)?;
            stream.read_exact(&mut len_buf[n..])?;
            stream.set_nonblocking(true)?;
        }
        Ok(_) => {}
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e),
    }

    let msg_len = u32::from_le_bytes(len_buf) as usize;
    if msg_len > 16 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }

    stream.set_nonblocking(false)?;
    let mut payload = vec![0u8; msg_len];
    stream.read_exact(&mut payload)?;
    stream.set_nonblocking(true)?;

    rmp_serde::from_slice(&payload)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Helper: build WindowInfo from a registry entry.
fn build_window_info(state: &Marlow, id: u64) -> Option<WindowInfo> {
    let window = state.find_window_by_id(id)?;
    let space = state.window_space(id);
    let geo = space.element_geometry(window).unwrap_or_default();
    let toplevel = window.toplevel().unwrap();
    let wl_surface = toplevel.wl_surface();

    let (title, app_id) = smithay::wayland::compositor::with_states(wl_surface, |states| {
        let data = states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .map(|d| d.lock().unwrap());
        match data {
            Some(d) => (
                d.title.clone().unwrap_or_default(),
                d.app_id.clone().unwrap_or_default(),
            ),
            None => (String::new(), String::new()),
        }
    });

    let focused = state
        .user_seat
        .get_keyboard()
        .and_then(|kb| kb.current_focus().map(|focus| focus == *wl_surface))
        .unwrap_or(false);

    Some(WindowInfo {
        window_id: id,
        title,
        app_id,
        x: geo.loc.x,
        y: geo.loc.y,
        width: geo.size.w,
        height: geo.size.h,
        focused,
    })
}

/// Dispatch a request.
fn handle_request(request: Request, state: &mut Marlow, client_idx: usize) -> Response {
    match request {
        Request::Ping => Response::Ok {
            data: json!("pong"),
        },

        Request::ListWindows => {
            let total = state.window_registry.len();
            let shadow_count = state.shadow_window_ids.len();
            let alive_count = state.window_registry.iter().filter(|(_, w)| w.alive()).count();
            tracing::info!(
                "ListWindows: registry={total}, shadow_ids={shadow_count}, alive={alive_count}"
            );

            let windows: Vec<WindowInfo> = state
                .window_registry
                .iter()
                .filter(|(id, w)| {
                    let is_shadow = state.shadow_window_ids.contains(id);
                    let is_alive = w.alive();
                    if !is_alive || is_shadow {
                        tracing::info!(
                            "ListWindows: filtered window {id} (shadow={is_shadow}, alive={is_alive})"
                        );
                    }
                    !is_shadow && is_alive
                })
                .filter_map(|(id, _)| build_window_info(state, *id))
                .collect();

            tracing::info!("ListWindows: returning {} windows", windows.len());
            Response::Ok {
                data: serde_json::to_value(&windows).unwrap_or(json!([])),
            }
        }

        Request::GetShadowWindows => {
            let windows: Vec<WindowInfo> = state
                .window_registry
                .iter()
                .filter(|(id, w)| state.shadow_window_ids.contains(id) && w.alive())
                .filter_map(|(id, _)| build_window_info(state, *id))
                .collect();

            Response::Ok {
                data: serde_json::to_value(&windows).unwrap_or(json!([])),
            }
        }

        Request::FocusWindow { window_id } => {
            let window = state.find_window_by_id(window_id).cloned();

            match window {
                Some(w) => {
                    let space = state.window_space_mut(window_id);
                    space.raise_element(&w, true);
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = state.agent_seat.get_keyboard() {
                        keyboard.set_focus(
                            state,
                            Some(w.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                    }
                    // Send pending configure to all windows in the relevant space
                    let is_shadow = state.is_shadow(window_id);
                    if is_shadow {
                        state.shadow_space.elements().for_each(|win| {
                            win.toplevel().unwrap().send_pending_configure();
                        });
                    } else {
                        state.user_space.elements().for_each(|win| {
                            win.toplevel().unwrap().send_pending_configure();
                        });
                    }
                    Response::Ok {
                        data: json!({"focused": window_id}),
                    }
                }
                None => Response::Error {
                    message: format!("Window {window_id} not found"),
                },
            }
        }

        Request::GetWindowInfo { window_id } => {
            match build_window_info(state, window_id) {
                Some(info) => Response::Ok {
                    data: serde_json::to_value(&info).unwrap_or(json!(null)),
                },
                None => Response::Error {
                    message: format!("Window {window_id} not found"),
                },
            }
        }

        // ─── Input commands ───

        Request::SendKey {
            window_id: _,
            key,
            pressed,
        } => handle_send_key(state, key, pressed),

        Request::SendText {
            window_id: _,
            text,
        } => handle_send_text(state, &text),

        Request::SendClick {
            window_id,
            x,
            y,
            button,
        } => handle_send_click(state, window_id, x, y, button),

        Request::SendHotkey {
            window_id: _,
            modifiers,
            key,
        } => handle_send_hotkey(state, &modifiers, &key),

        // ─── Screenshot ───

        Request::RequestScreenshot { window_id } => {
            // Check if this is a shadow window screenshot request
            if let Some(wid) = window_id {
                if state.is_shadow(wid) {
                    // Shadow screenshot
                    if let Some(data) = state.shadow_screenshot_data.take() {
                        return Response::Ok {
                            data: json!({"image": data, "format": "png", "encoding": "base64"}),
                        };
                    } else {
                        state.shadow_screenshot_pending = true;
                        state.shadow_screenshot_window_id = Some(wid);
                        return Response::Ok {
                            data: json!({"pending": true}),
                        };
                    }
                }
            }

            // Regular (user_space) screenshot
            if let Some(data) = state.screenshot_data.take() {
                Response::Ok {
                    data: json!({"image": data, "format": "png", "encoding": "base64"}),
                }
            } else {
                state.screenshot_pending = true;
                Response::Ok {
                    data: json!({"pending": true}),
                }
            }
        }

        // ─── Shadow Mode ───

        Request::LaunchInShadow { command } => {
            state.shadow_pending_count += 1;
            if state.shadow_pending_timestamp.is_none() {
                state.shadow_pending_timestamp = Some(std::time::Instant::now());
            }
            tracing::info!("LaunchInShadow: '{command}', pending_count={}", state.shadow_pending_count);

            // Split command into program + args (supports "firefox https://...")
            let parts: Vec<&str> = command.split_whitespace().collect();
            let (program, args) = if parts.is_empty() {
                (command.as_str(), vec![])
            } else {
                (parts[0], parts[1..].to_vec())
            };

            match std::process::Command::new(program).args(&args).spawn() {
                Ok(child) => {
                    tracing::info!("LaunchInShadow: spawned PID {}", child.id());
                    Response::Ok {
                        data: json!({"launched": command, "pid": child.id()}),
                    }
                }
                Err(e) => {
                    state.shadow_pending_count -= 1;
                    Response::Error {
                        message: format!("Failed to launch '{command}': {e}"),
                    }
                }
            }
        }

        Request::MoveToShadow { window_id } => {
            let window = state.find_window_by_id(window_id).cloned();
            match window {
                Some(w) if !state.is_shadow(window_id) => {
                    state.user_space.unmap_elem(&w);
                    state.shadow_space.map_element(w, (0, 0), false);
                    state.shadow_window_ids.insert(window_id);
                    tracing::info!("Window {window_id} moved to shadow_space");
                    state.event_queue.push(marlow_ipc::Event::WindowMovedToShadow {
                        window_id,
                    });
                    Response::Ok {
                        data: json!({"moved_to_shadow": window_id}),
                    }
                }
                Some(_) => Response::Error {
                    message: format!("Window {window_id} is already in shadow"),
                },
                None => Response::Error {
                    message: format!("Window {window_id} not found"),
                },
            }
        }

        Request::MoveToUser { window_id } => {
            let window = state.find_window_by_id(window_id).cloned();
            match window {
                Some(w) if state.is_shadow(window_id) => {
                    state.shadow_space.unmap_elem(&w);

                    // Cascade positioning: find a free spot below waybar
                    let pos = state.user_space.outputs().next().map(|o| {
                        let map = smithay::desktop::layer_map_for_output(o);
                        let zone = map.non_exclusive_zone();
                        if zone.loc.y > 0 {
                            (zone.loc.x, zone.loc.y)
                        } else {
                            (0, 32)
                        }
                    }).unwrap_or((0, 32));

                    let mut final_pos = pos;
                    for existing in state.user_space.elements() {
                        if let Some(eloc) = state.user_space.element_location(existing) {
                            if eloc.x == final_pos.0 && eloc.y == final_pos.1 {
                                final_pos = (final_pos.0 + 30, final_pos.1 + 30);
                            }
                        }
                    }

                    state.user_space.map_element(w.clone(), final_pos, false);
                    state.shadow_window_ids.remove(&window_id);

                    // Focus the promoted window via user_seat
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = state.user_seat.get_keyboard() {
                        keyboard.set_focus(
                            state,
                            Some(w.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                    }
                    state.user_space.raise_element(&w, true);

                    tracing::info!(
                        "Window {window_id} promoted to user_space at ({},{}) with focus",
                        final_pos.0, final_pos.1
                    );
                    state.event_queue.push(marlow_ipc::Event::WindowMovedToUser {
                        window_id,
                    });
                    Response::Ok {
                        data: json!({"moved_to_user": window_id, "x": final_pos.0, "y": final_pos.1}),
                    }
                }
                Some(_) => Response::Error {
                    message: format!("Window {window_id} is already in user_space"),
                },
                None => Response::Error {
                    message: format!("Window {window_id} not found"),
                },
            }
        }

        // ─── Event subscription ───

        Request::Subscribe { events: _ } => {
            state.ipc_subscribed.insert(client_idx);
            tracing::info!("Client {client_idx} subscribed to events");
            Response::Ok {
                data: json!({"subscribed": true}),
            }
        }

        // ─── Seat status ───

        Request::GetSeatStatus => {
            let user_focus = state
                .user_seat
                .get_keyboard()
                .and_then(|kb| kb.current_focus())
                .and_then(|surface| state.surface_to_window_id(&surface));

            let agent_focus = state
                .agent_seat
                .get_keyboard()
                .and_then(|kb| kb.current_focus())
                .and_then(|surface| state.surface_to_window_id(&surface));

            let conflict = user_focus.is_some()
                && agent_focus.is_some()
                && user_focus == agent_focus;

            let shadow_count = state.shadow_window_ids.len();

            Response::Ok {
                data: json!({
                    "user_focus": user_focus,
                    "agent_focus": agent_focus,
                    "conflict": conflict,
                    "wayland_display": state.socket_name.to_string_lossy(),
                    "shadow_count": shadow_count,
                }),
            }
        }
    }
}

// ─── Input handlers ───

/// Send a single key press/release to the focused client.
fn handle_send_key(state: &mut Marlow, key: u32, pressed: bool) -> Response {
    let keyboard = match state.agent_seat.get_keyboard() {
        Some(kb) => kb,
        None => {
            return Response::Error {
                message: "No keyboard".to_string(),
            }
        }
    };

    let serial = SERIAL_COUNTER.next_serial();
    let key_state = if pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    };
    let time = state.start_time.elapsed().as_millis() as u32;

    keyboard.input::<(), _>(state, key.into(), key_state, serial, time, |_, _, _| {
        FilterResult::Forward
    });

    Response::Ok {
        data: json!({"key": key, "pressed": pressed}),
    }
}

/// Type a string by synthesizing key press/release for each character.
fn handle_send_text(state: &mut Marlow, text: &str) -> Response {
    let keyboard = match state.agent_seat.get_keyboard() {
        Some(kb) => kb,
        None => {
            return Response::Error {
                message: "No keyboard".to_string(),
            }
        }
    };

    let mut typed = 0u32;
    let base_time = state.start_time.elapsed().as_millis() as u32;

    for (i, ch) in text.chars().enumerate() {
        let Some((keycode, shift)) = char_to_key(ch) else {
            tracing::warn!("SendText: unmappable char '{ch}'");
            continue;
        };

        let time = base_time + (i as u32 * 2);
        let serial = SERIAL_COUNTER.next_serial();

        // Press shift if needed
        if shift {
            let s = SERIAL_COUNTER.next_serial();
            keyboard.input::<(), _>(state, KEY_LEFTSHIFT.into(), KeyState::Pressed, s, time, |_, _, _| {
                FilterResult::Forward
            });
        }

        // Key press
        keyboard.input::<(), _>(state, keycode.into(), KeyState::Pressed, serial, time, |_, _, _| {
            FilterResult::Forward
        });

        // Key release
        let serial2 = SERIAL_COUNTER.next_serial();
        keyboard.input::<(), _>(
            state,
            keycode.into(),
            KeyState::Released,
            serial2,
            time + 1,
            |_, _, _| FilterResult::Forward,
        );

        // Release shift if needed
        if shift {
            let s = SERIAL_COUNTER.next_serial();
            keyboard.input::<(), _>(
                state,
                KEY_LEFTSHIFT.into(),
                KeyState::Released,
                s,
                time + 1,
                |_, _, _| FilterResult::Forward,
            );
        }

        typed += 1;
    }

    Response::Ok {
        data: json!({"typed": typed, "total": text.len()}),
    }
}

/// Send a mouse click at (x, y) relative to the window.
fn handle_send_click(state: &mut Marlow, window_id: u64, x: f64, y: f64, button: u32) -> Response {
    let window = state.find_window_by_id(window_id).cloned();

    let window = match window {
        Some(w) => w,
        None => {
            return Response::Error {
                message: format!("Window {window_id} not found"),
            }
        }
    };

    let space = state.window_space(window_id);
    let window_loc = space
        .element_location(&window)
        .unwrap_or_default()
        .to_f64();
    let abs_pos = (window_loc.x + x, window_loc.y + y).into();

    let pointer = state.agent_seat.get_pointer().unwrap();
    // Shadow-aware: search the correct space based on window_id
    let under = if state.is_shadow(window_id) {
        state.shadow_surface_under(abs_pos)
    } else {
        state.surface_under(abs_pos)
    };
    let time = state.start_time.elapsed().as_millis() as u32;

    // Linux button codes: BTN_LEFT=0x110, BTN_RIGHT=0x111, BTN_MIDDLE=0x112
    let btn_code = match button {
        0 | 1 => 0x110, // left
        2 => 0x111,     // right
        3 => 0x112,     // middle
        _ => 0x110,
    };

    // Move pointer
    let serial = SERIAL_COUNTER.next_serial();
    pointer.motion(
        state,
        under.clone(),
        &MotionEvent {
            location: abs_pos,
            serial,
            time,
        },
    );
    pointer.frame(state);

    // Focus the window on click
    let serial = SERIAL_COUNTER.next_serial();
    let keyboard = state.agent_seat.get_keyboard().unwrap();
    let space = state.window_space_mut(window_id);
    space.raise_element(&window, true);
    keyboard.set_focus(
        state,
        Some(window.toplevel().unwrap().wl_surface().clone()),
        serial,
    );

    // Press
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        state,
        &ButtonEvent {
            button: btn_code,
            state: ButtonState::Pressed,
            serial,
            time,
        },
    );
    pointer.frame(state);

    // Release
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        state,
        &ButtonEvent {
            button: btn_code,
            state: ButtonState::Released,
            serial,
            time: time + 10,
        },
    );
    pointer.frame(state);

    Response::Ok {
        data: json!({"clicked": true, "x": x, "y": y, "button": button}),
    }
}

/// Send a hotkey combination (modifiers + key).
fn handle_send_hotkey(state: &mut Marlow, modifiers: &[String], key: &str) -> Response {
    let keyboard = match state.agent_seat.get_keyboard() {
        Some(kb) => kb,
        None => {
            return Response::Error {
                message: "No keyboard".to_string(),
            }
        }
    };

    // Resolve modifier keycodes
    let mut mod_codes: Vec<u32> = Vec::new();
    for m in modifiers {
        match modifier_to_keycode(m) {
            Some(code) => mod_codes.push(code),
            None => {
                return Response::Error {
                    message: format!("Unknown modifier: {m}"),
                }
            }
        }
    }

    // Resolve main key
    let main_key = match key_name_to_keycode(key) {
        Some(code) => code,
        None => {
            return Response::Error {
                message: format!("Unknown key: {key}"),
            }
        }
    };

    let time = state.start_time.elapsed().as_millis() as u32;

    // Press modifiers
    for &code in &mod_codes {
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.input::<(), _>(state, code.into(), KeyState::Pressed, serial, time, |_, _, _| {
            FilterResult::Forward
        });
    }

    // Press main key
    let serial = SERIAL_COUNTER.next_serial();
    keyboard.input::<(), _>(state, main_key.into(), KeyState::Pressed, serial, time, |_, _, _| {
        FilterResult::Forward
    });

    // Release main key
    let serial = SERIAL_COUNTER.next_serial();
    keyboard.input::<(), _>(
        state,
        main_key.into(),
        KeyState::Released,
        serial,
        time + 1,
        |_, _, _| FilterResult::Forward,
    );

    // Release modifiers (reverse order)
    for &code in mod_codes.iter().rev() {
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.input::<(), _>(state, code.into(), KeyState::Released, serial, time + 1, |_, _, _| {
            FilterResult::Forward
        });
    }

    Response::Ok {
        data: json!({"hotkey": format!("{}+{}", modifiers.join("+"), key)}),
    }
}

// ─── Keycode tables (evdev / US QWERTY) ───

const KEY_LEFTSHIFT: u32 = 42;

/// Map a character to (evdev_keycode, needs_shift).
fn char_to_key(c: char) -> Option<(u32, bool)> {
    Some(match c {
        // Letters (lowercase)
        'a' => (30, false),  'b' => (48, false),  'c' => (46, false),
        'd' => (32, false),  'e' => (18, false),  'f' => (33, false),
        'g' => (34, false),  'h' => (35, false),  'i' => (23, false),
        'j' => (36, false),  'k' => (37, false),  'l' => (38, false),
        'm' => (50, false),  'n' => (49, false),  'o' => (24, false),
        'p' => (25, false),  'q' => (16, false),  'r' => (19, false),
        's' => (31, false),  't' => (20, false),  'u' => (22, false),
        'v' => (47, false),  'w' => (17, false),  'x' => (45, false),
        'y' => (21, false),  'z' => (44, false),
        // Letters (uppercase = shift)
        'A' => (30, true),   'B' => (48, true),   'C' => (46, true),
        'D' => (32, true),   'E' => (18, true),   'F' => (33, true),
        'G' => (34, true),   'H' => (35, true),   'I' => (23, true),
        'J' => (36, true),   'K' => (37, true),   'L' => (38, true),
        'M' => (50, true),   'N' => (49, true),   'O' => (24, true),
        'P' => (25, true),   'Q' => (16, true),   'R' => (19, true),
        'S' => (31, true),   'T' => (20, true),   'U' => (22, true),
        'V' => (47, true),   'W' => (17, true),   'X' => (45, true),
        'Y' => (21, true),   'Z' => (44, true),
        // Digits
        '1' => (2, false),   '2' => (3, false),   '3' => (4, false),
        '4' => (5, false),   '5' => (6, false),   '6' => (7, false),
        '7' => (8, false),   '8' => (9, false),   '9' => (10, false),
        '0' => (11, false),
        // Shifted digits
        '!' => (2, true),    '@' => (3, true),    '#' => (4, true),
        '$' => (5, true),    '%' => (6, true),    '^' => (7, true),
        '&' => (8, true),    '*' => (9, true),    '(' => (10, true),
        ')' => (11, true),
        // Whitespace
        ' ' => (57, false),  '\n' => (28, false), '\t' => (15, false),
        // Symbols
        '-' => (12, false),  '_' => (12, true),
        '=' => (13, false),  '+' => (13, true),
        '[' => (26, false),  '{' => (26, true),
        ']' => (27, false),  '}' => (27, true),
        ';' => (39, false),  ':' => (39, true),
        '\'' => (40, false), '"' => (40, true),
        '`' => (41, false),  '~' => (41, true),
        '\\' => (43, false), '|' => (43, true),
        ',' => (51, false),  '<' => (51, true),
        '.' => (52, false),  '>' => (52, true),
        '/' => (53, false),  '?' => (53, true),
        _ => return None,
    })
}

/// Map modifier names to evdev keycodes.
fn modifier_to_keycode(name: &str) -> Option<u32> {
    match name.to_lowercase().as_str() {
        "ctrl" | "control" | "lctrl" => Some(29),
        "shift" | "lshift" => Some(42),
        "alt" | "lalt" => Some(56),
        "super" | "meta" | "logo" => Some(125),
        "rctrl" => Some(97),
        "rshift" => Some(54),
        "ralt" => Some(100),
        _ => None,
    }
}

/// Map key names to evdev keycodes.
fn key_name_to_keycode(name: &str) -> Option<u32> {
    // First try single-char mapping
    if name.len() == 1 {
        if let Some((code, _)) = char_to_key(name.chars().next().unwrap()) {
            return Some(code);
        }
    }

    match name.to_lowercase().as_str() {
        "escape" | "esc" => Some(1),
        "f1" => Some(59),    "f2" => Some(60),    "f3" => Some(61),
        "f4" => Some(62),    "f5" => Some(63),    "f6" => Some(64),
        "f7" => Some(65),    "f8" => Some(66),    "f9" => Some(67),
        "f10" => Some(68),   "f11" => Some(87),   "f12" => Some(88),
        "enter" | "return" => Some(28),
        "tab" => Some(15),
        "backspace" => Some(14),
        "delete" | "del" => Some(111),
        "insert" | "ins" => Some(110),
        "space" => Some(57),
        "up" => Some(103),    "down" => Some(108),
        "left" => Some(105),  "right" => Some(106),
        "home" => Some(102),  "end" => Some(107),
        "pageup" | "pgup" => Some(104),
        "pagedown" | "pgdn" => Some(109),
        "capslock" => Some(58),
        "printscreen" | "prtsc" => Some(99),
        "pause" => Some(119),
        _ => None,
    }
}
