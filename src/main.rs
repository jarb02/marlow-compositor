#![allow(irrefutable_let_patterns)]

mod backend;
mod input;
mod shell;
mod wayland;

mod state;
pub use state::Marlow;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let mut event_loop: EventLoop<Marlow> = EventLoop::try_new()?;
    let display: Display<Marlow> = Display::new()?;
    let mut state = Marlow::new(&mut event_loop, display);

    backend::winit::init_winit(&mut event_loop, &mut state)?;

    tracing::info!("Marlow Compositor running on {}", state.socket_name.to_string_lossy());

    // Set WAYLAND_DISPLAY so child processes connect to us
    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);

    // Optionally spawn a client: marlow-compositor -c foot
    spawn_client();

    event_loop.run(None, &mut state, move |_| {})?;

    Ok(())
}

fn init_logging() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }
}

fn spawn_client() {
    let mut args = std::env::args().skip(1);
    let flag = args.next();
    let arg = args.next();

    match (flag.as_deref(), arg) {
        (Some("-c") | Some("--command"), Some(command)) => {
            std::process::Command::new(command).spawn().ok();
        }
        _ => {}
    }
}
