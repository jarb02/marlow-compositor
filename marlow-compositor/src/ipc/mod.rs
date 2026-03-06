use std::io::{self, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use marlow_ipc::{write_message, Request, Response, WindowInfo};
use serde_json::json;

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

/// Poll for new connections and process requests.
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

    // We need to process clients one at a time because handle_request
    // may need &mut state for FocusWindow. Use index-based iteration.
    let n = state.ipc_clients.len();
    for i in 0..n {
        match try_read_request(&mut state.ipc_clients[i]) {
            Ok(Some(request)) => {
                let response = handle_request(request, state);
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

    for i in to_remove.into_iter().rev() {
        state.ipc_clients.remove(i);
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

/// Dispatch a request.
fn handle_request(request: Request, state: &mut Marlow) -> Response {
    match request {
        Request::Ping => Response::Ok {
            data: json!("pong"),
        },

        Request::ListWindows => {
            let windows: Vec<WindowInfo> = state
                .space
                .elements()
                .enumerate()
                .map(|(i, window)| {
                    let geo = state.space.element_geometry(window).unwrap_or_default();
                    let toplevel = window.toplevel().unwrap();
                    let wl_surface = toplevel.wl_surface();

                    let (title, app_id) = smithay::wayland::compositor::with_states(
                        wl_surface,
                        |states| {
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
                        },
                    );

                    let focused = state
                        .seat
                        .get_keyboard()
                        .and_then(|kb| kb.current_focus().map(|focus| focus == *wl_surface))
                        .unwrap_or(false);

                    WindowInfo {
                        window_id: i as u64,
                        title,
                        app_id,
                        x: geo.loc.x,
                        y: geo.loc.y,
                        width: geo.size.w,
                        height: geo.size.h,
                        focused,
                    }
                })
                .collect();

            Response::Ok {
                data: serde_json::to_value(&windows).unwrap_or(json!([])),
            }
        }

        Request::FocusWindow { window_id } => {
            let window = state.space.elements().nth(window_id as usize).cloned();

            match window {
                Some(w) => {
                    state.space.raise_element(&w, true);
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = state.seat.get_keyboard() {
                        keyboard.set_focus(
                            state,
                            Some(w.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                    }
                    state.space.elements().for_each(|win| {
                        win.toplevel().unwrap().send_pending_configure();
                    });
                    Response::Ok {
                        data: json!({"focused": window_id}),
                    }
                }
                None => Response::Error {
                    message: format!("Window {window_id} not found"),
                },
            }
        }

        _ => Response::Error {
            message: "Not implemented yet".to_string(),
        },
    }
}
