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
        // Find a free X display (skip displays with existing lock files)
        let x_display = (0..10)
            .find(|n| !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists())
            .map(|n| format!(":{n}"))
            .unwrap_or_else(|| ":1".to_string());

        match std::process::Command::new("xwayland-satellite")
            .arg(&x_display)
            .spawn()
        {
            Ok(child) => {
                state.xwayland_process = Some(child);
                std::env::set_var("DISPLAY", &x_display);
                tracing::info!("xwayland-satellite started on {x_display}");
            }
            Err(e) => {
                tracing::warn!("xwayland-satellite not available: {e} (X11 apps won't work)");
            }
        }
    }

    // Auto-spawn essential apps in KMS mode (full desktop session)
    if !use_winit {
        cleanup_stale_processes();
        spawn_session_apps();
    }

    // Optionally spawn additional clients: marlow-compositor -c foot -c foot
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

/// Kill leftover processes from previous compositor sessions.
/// Firefox in particular leaves zombie/locked processes that prevent
/// shadow mode from launching a new instance.
fn cleanup_stale_processes() {
    use std::process::Command;

    // Kill leftover Firefox (zombies from previous shadow mode tests)
    let _ = Command::new("pkill").args(["-9", "firefox"]).status();

    // Remove Firefox profile locks so new instances can start
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/josemarlow".into());
    let mozilla_dir = format!("{}/.mozilla/firefox", home);
    if let Ok(entries) = std::fs::read_dir(&mozilla_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                for lock_name in &["lock", ".parentlock"] {
                    let lock = path.join(lock_name);
                    if lock.exists() {
                        let _ = std::fs::remove_file(&lock);
                        tracing::info!("Removed stale lock: {}", lock.display());
                    }
                }
            }
        }
    }

    // Kill leftover Marlow daemon
    let _ = Command::new("pkill").args(["-f", "daemon_linux"]).status();

    // Brief pause so sockets/locks are fully released
    std::thread::sleep(std::time::Duration::from_millis(500));

    tracing::info!("Stale process cleanup complete");
}

/// Auto-spawn essential session apps (KMS mode only).
fn spawn_session_apps() {
    // Mako notification daemon (must start before notify-send)
    match std::process::Command::new("mako").spawn() {
        Ok(_) => tracing::info!("Spawned mako notification daemon"),
        Err(_) => tracing::info!("mako not found, notifications disabled"),
    }

    // Swaybg wallpaper (dark blue fallback color)
    match std::process::Command::new("swaybg")
        .args(["-m", "solid_color", "-c", "#1a1a2e"])
        .spawn()
    {
        Ok(_) => tracing::info!("Spawned swaybg wallpaper"),
        Err(_) => tracing::info!("swaybg not found, no wallpaper"),
    }

    // Waybar top panel with Marlow branding
    let waybar_config = "/home/josemarlow/marlow-compositor/config/waybar/config.jsonc";
    let waybar_style = "/home/josemarlow/marlow-compositor/config/waybar/style.css";
    match std::process::Command::new("waybar")
        .args(["-c", waybar_config, "-s", waybar_style])
        .spawn()
    {
        Ok(_) => tracing::info!("Spawned waybar with Marlow config"),
        Err(_) => tracing::info!("waybar not found, skipping panel"),
    }

    // Foot terminal with dark background for visibility
    match std::process::Command::new("foot")
        .args(["--override", "colors.background=282c34",
               "--override", "pad=8x8"])
        .spawn()
    {
        Ok(_) => tracing::info!("Spawned foot terminal"),
        Err(e) => tracing::warn!("Failed to spawn foot: {e}"),
    }

    // Welcome notification (delayed so mako has time to start)
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        std::process::Command::new("notify-send")
            .args(["-t", "5000", "Marlow OS", "Ready. Press Super+M to talk to Marlow."])
            .spawn()
            .ok();
    });
}

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
