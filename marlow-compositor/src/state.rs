use std::collections::{HashMap, HashSet};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::{ffi::OsString, sync::Arc};

use smithay::{
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{pointer::CursorImageStatus, Seat, SeatState},
    output::Output,
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, LoopSignal, Mode, PostAction},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Display, DisplayHandle,
        },
    },
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::XdgShellState,
        },
        shm::ShmState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
        fractional_scale::FractionalScaleManagerState,
        selection::primary_selection::PrimarySelectionState,
    },
};

use smithay::utils::IsAlive;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;

use crate::seat::arbiter::SeatArbiter;

pub struct Marlow {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    // Dual spaces: user (visible) + shadow (invisible)
    pub user_space: Space<Window>,
    pub shadow_space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay state
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Marlow>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,

    // Dual-seat: user (hardware) + agent (IPC)
    pub user_seat: Seat<Self>,
    pub agent_seat: Seat<Self>,
    pub seat_arbiter: SeatArbiter,

    // IPC
    pub ipc_listener: Option<UnixListener>,
    pub ipc_clients: Vec<UnixStream>,
    pub ipc_socket_path: Option<PathBuf>,

    // Screenshot (flag + buffer for async capture in render loop)
    pub screenshot_pending: bool,
    pub screenshot_data: Option<String>, // base64 PNG

    // Shadow screenshot (separate flag + buffer)
    pub shadow_screenshot_pending: bool,
    pub shadow_screenshot_window_id: Option<u64>,
    pub shadow_screenshot_data: Option<String>,

    // Event streaming
    pub event_queue: Vec<marlow_ipc::Event>,
    pub ipc_subscribed: HashSet<usize>, // indices of subscribed clients

    // Window registry: stable IDs that survive space moves
    pub window_registry: Vec<(u64, Window)>,
    pub shadow_window_ids: HashSet<u64>,
    pub next_window_id: u64,

    // Shadow mode: pending launches
    pub shadow_pending_count: u32,

    // Output reference for shadow frame callbacks
    pub output: Option<Output>,

    // Shadow frame timing (15 FPS = 66ms)
    pub last_shadow_frame: std::time::Instant,

    // Cursor state (software cursor for KMS)
    pub cursor: crate::cursor::Cursor,
    pub cursor_status: CursorImageStatus,
    pub pointer_element: crate::cursor::PointerElement,
    pub pointer_images: Vec<(xcursor::parser::Image, MemoryRenderBuffer)>,

    // Layer shell
    pub layer_shell_state: WlrLayerShellState,
    pub viewporter_state: ViewporterState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub xdg_decoration_state: XdgDecorationState,
    pub primary_selection_state: PrimarySelectionState,

    // KMS backend state (only used when running in TTY mode)
    pub kms_backends: HashMap<smithay::backend::drm::DrmNode, crate::backend::kms::GpuBackendHandle>,
    pub kms_renderer: Option<smithay::backend::renderer::gles::GlesRenderer>,
    pub kms_primary_node: Option<smithay::backend::drm::DrmNode>,

    // xwayland-satellite process (KMS mode)
    pub xwayland_process: Option<std::process::Child>,
}

impl Marlow {
    pub fn new(event_loop: &mut EventLoop<Self>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let popups = PopupManager::default();
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();

        // Keyboard layout: reads XKB_DEFAULT_LAYOUT env var (default: "us")
        // Set XKB_DEFAULT_LAYOUT=latam for Latin American keyboards
        let kb_layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "us".into());

        // User seat: receives hardware input (libinput/winit events)
        let mut user_seat: Seat<Self> = seat_state.new_wl_seat(&dh, "user");
        user_seat.add_keyboard(smithay::input::keyboard::XkbConfig {
            layout: &kb_layout,
            ..Default::default()
        }, 200, 25).unwrap();
        user_seat.add_pointer();

        // Agent seat: receives IPC commands from the Python agent.
        let mut agent_seat: Seat<Self> = seat_state.new_wl_seat(&dh, "marlow-agent");
        agent_seat.add_keyboard(smithay::input::keyboard::XkbConfig {
            layout: &kb_layout,
            ..Default::default()
        }, 200, 25).unwrap();
        agent_seat.add_pointer();

        let user_space = Space::default();
        let shadow_space = Space::default();
        let socket_name = Self::init_wayland_listener(display, event_loop);
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            display_handle: dh,
            user_space,
            shadow_space,
            loop_signal,
            socket_name,
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups,
            user_seat,
            agent_seat,
            seat_arbiter: SeatArbiter::new(),
            ipc_listener: None,
            ipc_clients: Vec::new(),
            ipc_socket_path: None,
            screenshot_pending: false,
            screenshot_data: None,
            shadow_screenshot_pending: false,
            shadow_screenshot_window_id: None,
            shadow_screenshot_data: None,
            event_queue: Vec::new(),
            ipc_subscribed: HashSet::new(),
            window_registry: Vec::new(),
            shadow_window_ids: HashSet::new(),
            next_window_id: 0,
            shadow_pending_count: 0,
            output: None,
            last_shadow_frame: start_time,
            cursor: crate::cursor::Cursor::load(),
            cursor_status: CursorImageStatus::default_named(),
            pointer_element: crate::cursor::PointerElement::default(),
            pointer_images: Vec::new(),
            layer_shell_state,
            viewporter_state,
            fractional_scale_state,
            xdg_decoration_state,
            primary_selection_state,
            kms_backends: HashMap::new(),
            kms_renderer: None,
            kms_primary_node: None,
            xwayland_process: None,
        }
    }

    fn init_wayland_listener(display: Display<Marlow>, event_loop: &mut EventLoop<Self>) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().unwrap();
        let socket_name = listening_socket.socket_name().to_os_string();
        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    /// Find surface under pointer — searches user_space only (for hardware input).
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.user_space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + location).to_f64()))
        })
    }

    /// Register a new window and return its stable ID.
    pub fn register_window(&mut self, window: Window) -> u64 {
        let id = self.next_window_id;
        self.next_window_id += 1;
        self.window_registry.push((id, window));
        id
    }

    /// Find a window by its stable ID.
    pub fn find_window_by_id(&self, id: u64) -> Option<&Window> {
        self.window_registry.iter().find(|(wid, _)| *wid == id).map(|(_, w)| w)
    }

    /// Check if a window is in shadow space.
    pub fn is_shadow(&self, id: u64) -> bool {
        self.shadow_window_ids.contains(&id)
    }

    /// Get the appropriate space for a window (by ID).
    pub fn window_space(&self, id: u64) -> &Space<Window> {
        if self.shadow_window_ids.contains(&id) {
            &self.shadow_space
        } else {
            &self.user_space
        }
    }

    /// Get the appropriate space mutably for a window (by ID).
    pub fn window_space_mut(&mut self, id: u64) -> &mut Space<Window> {
        if self.shadow_window_ids.contains(&id) {
            &mut self.shadow_space
        } else {
            &mut self.user_space
        }
    }

    /// Map a WlSurface to a stable window ID via the registry.
    pub fn surface_to_window_id(&self, surface: &WlSurface) -> Option<u64> {
        self.window_registry.iter().find_map(|(id, w)| {
            if w.toplevel().unwrap().wl_surface() == surface {
                Some(*id)
            } else {
                None
            }
        })
    }

    /// Clean up dead windows from the registry.
    pub fn cleanup_dead_windows(&mut self) {
        let dead: Vec<u64> = self
            .window_registry
            .iter()
            .filter(|(_, w)| !w.alive())
            .map(|(id, _)| *id)
            .collect();

        for id in &dead {
            self.shadow_window_ids.remove(id);
            self.event_queue.push(marlow_ipc::Event::WindowDestroyed {
                window_id: *id,
            });
        }

        self.window_registry.retain(|(_, w)| w.alive());
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
