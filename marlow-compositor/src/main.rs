#![allow(irrefutable_let_patterns)]

mod backend;
mod cursor;
mod input;
mod ipc;
mod seat;
mod shell;
mod wayland;

mod state;
pub use state::Marlow;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    let use_winit = args.contains(&"--winit".to_string());

    let mut event_loop: EventLoop<Marlow> = EventLoop::try_new()?;
    let display: Display<Marlow> = Display::new()?;
    let mut state = Marlow::new(&mut event_loop, display);

    // Initialize backend
    if use_winit {
        tracing::info!("Starting with Winit backend (nested)");
        backend::winit::init_winit(&mut event_loop, &mut state)?;
    } else {
        tracing::info!("Starting with KMS backend (TTY direct)");
        backend::kms::run_kms(&mut event_loop, &mut state)?;
    }

    // Start IPC server
    match ipc::init_ipc(&mut state) {
        Ok(path) => tracing::info!("IPC socket: {}", path.display()),
        Err(e) => tracing::error!("Failed to start IPC: {e}"),
    }

    tracing::info!("Marlow Compositor running on {}", state.socket_name.to_string_lossy());

    // Set WAYLAND_DISPLAY so child processes connect to us
    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);

    // Launch xwayland-satellite for X11 app compatibility (KMS mode only)
    if !use_winit {
        match std::process::Command::new("xwayland-satellite")
            .arg(":0")
            .spawn()
        {
            Ok(child) => {
                state.xwayland_process = Some(child);
                std::env::set_var("DISPLAY", ":0");
                tracing::info!("xwayland-satellite started on :0");
            }
            Err(e) => {
                tracing::warn!("xwayland-satellite not available: {e} (X11 apps won't work)");
            }
        }
    }

    // Optionally spawn clients: marlow-compositor -c foot -c foot
    spawn_clients();

    event_loop.run(None, &mut state, |state| {
        ipc::poll_ipc(state);
    })?;

    // Cleanup: KMS surfaces before DRM device drops (avoids restore errors)
    backend::kms::cleanup_kms(&mut state);

    // Cleanup: kill xwayland-satellite
    if let Some(mut child) = state.xwayland_process.take() {
        child.kill().ok();
        child.wait().ok();
    }

    // Cleanup IPC socket
    if let Some(path) = &state.ipc_socket_path {
        std::fs::remove_file(path).ok();
    }

    Ok(())
}

fn init_logging() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }
}

/// Spawn client processes. Supports multiple -c flags:
///   marlow-compositor -c foot -c foot
fn spawn_clients() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if (args[i] == "-c" || args[i] == "--command") && i + 1 < args.len() {
            let command = &args[i + 1];
            tracing::info!("Spawning client: {command}");
            std::process::Command::new(command).spawn().ok();
            i += 2;
        } else {
            i += 1;
        }
    }
}
