//! KMS/DRM backend — runs directly on TTY hardware without a parent compositor.
//!
//! Uses libseat for session management, udev for GPU detection, DRM/GBM for
//! rendering, and libinput for keyboard/mouse input. Software cursor rendered
//! as part of each output frame.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use drm::control::ModeTypeFlags;

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                memory::MemoryRenderBuffer,
                surface::WaylandSurfaceRenderElement,
                AsRenderElements,
            },
            gles::GlesRenderer,
            ImportAll, ImportMem,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    desktop::{space::SpaceRenderElements, Window},
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{timer::{TimeoutAction, Timer}, EventLoop, RegistrationToken},
    reexports::input::Libinput,
    utils::{DeviceFd, Logical, Point, Scale, Transform},
};

use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use smithay::desktop::layer_map_for_output;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::backend::renderer::damage::OutputDamageTracker as ScreenshotDamageTracker;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Buffer as BufferCoord, Rectangle};

use crate::cursor::PointerRenderElement;
use crate::Marlow;

type CrtcHandle = drm::control::crtc::Handle;

// Output render elements: space windows + software cursor.
// Two-generic pattern (matches Anvil's approach) to satisfy SpaceRenderElements bounds.
smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Pointer=PointerRenderElement<R>,
    Solid=SolidColorRenderElement,
}

type ConcreteOutputElements = OutputRenderElements<
    GlesRenderer,
    WaylandSurfaceRenderElement<GlesRenderer>,
>;

type MarlowDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

type MarlowDrmOutput = smithay::backend::drm::output::DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

/// Per-output surface state.
pub struct OutputSurface {
    output: Output,
    drm_output: MarlowDrmOutput,
}

/// Opaque handle for storing GPU backend in state.
pub type GpuBackendHandle = GpuBackend;

/// Per-GPU backend state.
pub struct GpuBackend {
    drm_output_manager: MarlowDrmOutputManager,
    scanner: DrmScanner,
    surfaces: HashMap<CrtcHandle, OutputSurface>,
    _drm_token: RegistrationToken,
}

/// Run the compositor with KMS/DRM backend (TTY mode).
pub fn run_kms(event_loop: &mut EventLoop<Marlow>, state: &mut Marlow) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize session via libseat
    let (mut session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    tracing::info!("Session opened on seat: {seat_name}");

    // 2. Detect primary GPU
    let gpu_path = if let Ok(var) = std::env::var("MARLOW_DRM_DEVICE") {
        std::path::PathBuf::from(var)
    } else {
        primary_gpu(&seat_name)?
            .unwrap_or_else(|| {
                all_gpus(&seat_name)
                    .unwrap()
                    .into_iter()
                    .next()
                    .expect("No GPU found!")
            })
    };

    let drm_node = DrmNode::from_path(&gpu_path)?;
    let render_node = drm_node
        .node_with_type(NodeType::Render)
        .and_then(|r| r.ok())
        .unwrap_or(drm_node);
    tracing::info!("Primary GPU: {drm_node}, render node: {render_node}");

    // 3. Open DRM device via session
    let fd = session.open(
        &gpu_path,
        smithay::reexports::rustix::fs::OFlags::RDWR
            | smithay::reexports::rustix::fs::OFlags::CLOEXEC
            | smithay::reexports::rustix::fs::OFlags::NOCTTY
            | smithay::reexports::rustix::fs::OFlags::NONBLOCK,
    )?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    // 4. Create DRM device + GBM device
    // false = don't try to restore previous DRM state on drop (avoids EINVAL on exit)
    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), false)?;
    let gbm = GbmDevice::new(drm_fd)?;

    // 5. Create EGL display + GLES renderer
    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_context = EGLContext::new(&egl_display)?;
    let mut renderer = unsafe { GlesRenderer::new(egl_context)? };
    tracing::info!("GLES renderer initialized");

    // 6. Get renderer formats for DrmOutputManager
    let render_formats = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<Vec<_>>();

    // 7. Create allocator + framebuffer exporter
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node.into());

    let color_formats = [
        Fourcc::Argb8888,
        Fourcc::Abgr8888,
        Fourcc::Xrgb8888,
        Fourcc::Xbgr8888,
    ];

    // 8. Create DRM output manager
    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        color_formats.iter().copied(),
        render_formats,
    );

    // 9. Register DRM event source (VBlank handling)
    let node = drm_node;
    let drm_token = event_loop.handle().insert_source(
        drm_notifier,
        move |event, _metadata, state: &mut Marlow| match event {
            DrmEvent::VBlank(crtc) => {
                VBLANK_PENDING.store(false, Ordering::Relaxed);
                if let Some(gpu) = state.kms_backends.get_mut(&node) {
                    if let Some(surface) = gpu.surfaces.get_mut(&crtc) {
                        let _ = surface.drm_output.frame_submitted();
                    }
                }
                render_surface(state, node, crtc);
            }
            DrmEvent::Error(err) => {
                tracing::error!("DRM error: {err:?}");
            }
        },
    )?;

    // Store GPU backend
    let mut gpu_backend = GpuBackend {
        drm_output_manager,
        scanner: DrmScanner::new(),
        surfaces: HashMap::new(),
        _drm_token: drm_token,
    };

    // 10. Scan connectors and set up outputs
    scan_connectors(state, &mut gpu_backend, &mut renderer)?;

    state.kms_backends.insert(drm_node, gpu_backend);
    state.kms_renderer = Some(renderer);
    state.kms_primary_node = Some(drm_node);

    // 11. Initialize udev backend for hotplug
    let udev_backend = UdevBackend::new(&seat_name)?;
    event_loop.handle().insert_source(
        udev_backend,
        move |event, _, _state: &mut Marlow| match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!("UDev: device added {device_id} at {}", path.display());
            }
            UdevEvent::Changed { device_id } => {
                tracing::info!("UDev: device changed {device_id}");
            }
            UdevEvent::Removed { device_id } => {
                tracing::info!("UDev: device removed {device_id}");
            }
        },
    )?;

    // 12. Initialize libinput for keyboard/mouse
    let mut libinput_context =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput_context.udev_assign_seat(&seat_name).unwrap();

    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    event_loop.handle().insert_source(
        libinput_backend,
        move |event, _, state: &mut Marlow| {
            state.process_input_event(event);
        },
    )?;

    // 13. Handle session pause/resume (VT switch)
    event_loop.handle().insert_source(
        session_notifier,
        move |event, _, state: &mut Marlow| match event {
            SessionEvent::PauseSession => {
                tracing::info!("Session paused (VT switch away)");
                for gpu in state.kms_backends.values_mut() {
                    gpu.drm_output_manager.pause();
                }
                libinput_context.suspend();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("Session resumed (VT switch back)");
                if let Err(err) = libinput_context.resume() {
                    tracing::error!("Failed to resume libinput: {err:?}");
                }
                for gpu in state.kms_backends.values_mut() {
                    gpu.drm_output_manager
                        .lock()
                        .activate(false)
                        .ok();
                }
                // Re-render all outputs
                if let Some(node) = state.kms_primary_node {
                    let crtcs: Vec<_> = state
                        .kms_backends
                        .get(&node)
                        .map(|gpu| gpu.surfaces.keys().copied().collect())
                        .unwrap_or_default();
                    for crtc in crtcs {
                        render_surface(state, node, crtc);
                    }
                }
            }
        },
    )?;

    // 14. Trigger initial render for all outputs
    let initial_crtcs: Vec<_> = state
        .kms_backends
        .get(&drm_node)
        .map(|gpu| gpu.surfaces.keys().copied().collect())
        .unwrap_or_default();
    for crtc in initial_crtcs {
        render_surface(state, drm_node, crtc);
    }

    // 15. Fallback render timer — keeps the loop alive when VBlank isn't driving it.
    // Fires every 16ms (~60fps). Only actually renders when no VBlank is pending.
    let timer_node = drm_node;
    let timer = Timer::from_duration(Duration::from_millis(16));
    event_loop.handle().insert_source(
        timer,
        move |_instant, _metadata, state: &mut Marlow| {
            // Only render if no VBlank-driven frame is in flight
            if !VBLANK_PENDING.load(Ordering::Relaxed) {
                let crtcs: Vec<_> = state
                    .kms_backends
                    .get(&timer_node)
                    .map(|gpu| gpu.surfaces.keys().copied().collect())
                    .unwrap_or_default();
                for crtc in crtcs {
                    render_surface(state, timer_node, crtc);
                }
            }
            TimeoutAction::ToDuration(Duration::from_millis(16))
        },
    )?;

    tracing::info!("KMS backend initialized — compositor running on TTY");
    Ok(())
}

/// Explicit cleanup: drop surfaces before DRM device to avoid restore errors.
pub fn cleanup_kms(state: &mut Marlow) {
    for gpu in state.kms_backends.values_mut() {
        // Unmap all outputs from spaces
        for surface in gpu.surfaces.values() {
            state.user_space.unmap_output(&surface.output);
        }
        // Drop all DRM surfaces
        gpu.surfaces.clear();
    }
    // Drop GPU backends (DRM output managers)
    state.kms_backends.clear();
    // Drop renderer
    state.kms_renderer.take();
    tracing::info!("KMS cleanup complete");
}

/// Scan DRM connectors and set up outputs.
fn scan_connectors(
    state: &mut Marlow,
    gpu: &mut GpuBackend,
    renderer: &mut GlesRenderer,
) -> Result<(), Box<dyn std::error::Error>> {
    let scan_result = gpu.scanner.scan_connectors(gpu.drm_output_manager.device())?;

    for event in scan_result {
        match event {
            DrmScanEvent::Connected { connector, crtc: Some(crtc) } => {
                tracing::info!(
                    "Connector connected: {}-{}",
                    connector.interface().as_str(),
                    connector.interface_id()
                );

                let drm_mode = connector.modes().iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    .or_else(|| connector.modes().first())
                    .copied();

                let Some(drm_mode) = drm_mode else {
                    tracing::warn!("No modes available for connector");
                    continue;
                };

                let wl_mode = WlMode::from(drm_mode);
                let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));

                let output_name = format!(
                    "{}-{}",
                    connector.interface().as_str(),
                    connector.interface_id()
                );

                let output = Output::new(
                    output_name.clone(),
                    PhysicalProperties {
                        size: (phys_w as i32, phys_h as i32).into(),
                        subpixel: Subpixel::Unknown,
                        make: "Marlow".into(),
                        model: "KMS".into(),
                        serial_number: "0001".into(),
                    },
                );

                let _global = output.create_global::<Marlow>(&state.display_handle);
                output.set_preferred(wl_mode);
                output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));
                state.user_space.map_output(&output, (0, 0));
                state.output = Some(output.clone());

                // Initialize DRM output
                let drm_output = gpu.drm_output_manager
                    .lock()
                    .initialize_output::<GlesRenderer, ConcreteOutputElements>(
                        crtc,
                        drm_mode,
                        &[connector.handle()],
                        &output,
                        None,
                        renderer,
                        &DrmOutputRenderElements::default(),
                    )?;

                gpu.surfaces.insert(crtc, OutputSurface {
                    output,
                    drm_output,
                });

                tracing::info!("Output {output_name} configured: {}x{} @ {}Hz",
                    wl_mode.size.w, wl_mode.size.h, wl_mode.refresh / 1000);
            }
            DrmScanEvent::Disconnected { connector, crtc: Some(crtc) } => {
                tracing::info!(
                    "Connector disconnected: {}-{}",
                    connector.interface().as_str(),
                    connector.interface_id()
                );
                if let Some(surface) = gpu.surfaces.remove(&crtc) {
                    state.user_space.unmap_output(&surface.output);
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Frame counter for diagnostic logging.
static FRAME_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether a VBlank-driven render is currently pending (frame queued, waiting for VBlank).
/// When false, the fallback timer drives rendering instead.
static VBLANK_PENDING: AtomicBool = AtomicBool::new(false);

/// Render a single output surface with software cursor.
fn render_surface(state: &mut Marlow, node: DrmNode, crtc: CrtcHandle) {
    // Take renderer out of state temporarily to avoid borrow conflicts
    let mut renderer = match state.kms_renderer.take() {
        Some(r) => r,
        None => return,
    };

    let _result = (|| -> Option<()> {
        let gpu = state.kms_backends.get_mut(&node)?;
        let surface = gpu.surfaces.get_mut(&crtc)?;
        let output = &surface.output;

        // Get pointer location
        let pointer_location: Point<f64, Logical> = state
            .user_seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();

        // Get cursor image for current frame
        let cursor_frame = state.cursor.get_image(1, state.start_time.elapsed());
        let cursor_hotspot = (cursor_frame.xhot as i32, cursor_frame.yhot as i32);

        // Find or create MemoryRenderBuffer for this cursor frame
        let pointer_image = state
            .pointer_images
            .iter()
            .find_map(|(image, buffer)| {
                if image == &cursor_frame {
                    Some(buffer.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                let buffer = MemoryRenderBuffer::from_slice(
                    &cursor_frame.pixels_rgba,
                    Fourcc::Argb8888,
                    (cursor_frame.width as i32, cursor_frame.height as i32),
                    1,
                    Transform::Normal,
                    None,
                );
                state
                    .pointer_images
                    .push((cursor_frame, buffer.clone()));
                buffer
            });

        // Update pointer element
        state.pointer_element.set_buffer(pointer_image);
        state.pointer_element.set_status(state.cursor_status.clone());

        // Generate cursor render elements
        let output_geometry = state.user_space.output_geometry(output)?;
        let scale = Scale::from(output.current_scale().fractional_scale());
        let cursor_pos = pointer_location - output_geometry.loc.to_f64();
        let cursor_hotspot: Point<i32, Logical> = cursor_hotspot.into();

        let mut elements: Vec<ConcreteOutputElements> = state
            .pointer_element
            .render_elements(
                &mut renderer,
                (cursor_pos - cursor_hotspot.to_f64())
                    .to_physical(scale)
                    .to_i32_round(),
                scale,
                1.0,
            );

        // Arrange layer surfaces (waybar, swaybg, mako)
        {
            let mut layer_map = layer_map_for_output(output);
            layer_map.arrange();
        }

        // Collect layer elements split by z-order
        let (mut top_layer_elements, mut bottom_layer_elements) = {
            let layer_map = layer_map_for_output(output);
            let mut top_elems: Vec<ConcreteOutputElements> = Vec::new();
            let mut bottom_elems: Vec<ConcreteOutputElements> = Vec::new();
            let layer_count = layer_map.layers().count();
            let mut top_count = 0usize;
            let mut bottom_count = 0usize;

            for layer in layer_map.layers() {
                let layer_type = layer.layer();
                if let Some(geo) = layer_map.layer_geometry(layer) {
                    let loc = geo.loc.to_physical_precise_round(scale);
                    let layer_elems = layer.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                        &mut renderer,
                        loc,
                        scale,
                        1.0,
                    );
                    let converted: Vec<ConcreteOutputElements> = layer_elems
                        .into_iter()
                        .map(|e| OutputRenderElements::from(SpaceRenderElements::from(e)))
                        .collect();

                    {
                        let fn_ = FRAME_COUNT.load(std::sync::atomic::Ordering::Relaxed);
                        if fn_ < 20 || fn_ % 300 == 0 {
                            tracing::info!("layer_detail: type={:?} geo={}x{}+{}+{} render_elems={}",
                                layer_type, geo.size.w, geo.size.h, geo.loc.x, geo.loc.y, converted.len());
                        }
                    }

                    match layer_type {
                        WlrLayer::Overlay | WlrLayer::Top => {
                            top_count += converted.len();
                            top_elems.extend(converted);
                        }
                        WlrLayer::Bottom | WlrLayer::Background => {
                            bottom_count += converted.len();
                            bottom_elems.extend(converted);
                        }
                    }
                } else {
                    let fn_ = FRAME_COUNT.load(std::sync::atomic::Ordering::Relaxed);
                    if fn_ < 20 || fn_ % 300 == 0 {
                        tracing::info!("layer_no_geo: type={:?}", layer_type);
                    }
                }
            }

            // Debug: log layer info on first 20 frames + every 300th
            let frame_n = FRAME_COUNT.load(std::sync::atomic::Ordering::Relaxed);
            if frame_n < 20 || (frame_n % 300 == 0 && frame_n < 3000) {
                tracing::info!("layer_debug: total={} top={} bottom={}", layer_count, top_count, bottom_count);
            }

            (top_elems, bottom_elems)
        };

        // Overlay + Top layers (above windows)
        elements.append(&mut top_layer_elements);

        // Generate space render elements (windows)
        let space_elements = smithay::desktop::space::space_render_elements::<_, Window, _>(
            &mut renderer,
            [&state.user_space],
            output,
            1.0,
        )
        .ok()?;
        elements.extend(space_elements.into_iter().map(OutputRenderElements::from));

                // Bottom + Background layers (below windows)
        elements.append(&mut bottom_layer_elements);

        // Render frame
        let render_result = surface
            .drm_output
            .render_frame::<GlesRenderer, ConcreteOutputElements>(
                &mut renderer,
                &elements,
                [0.1, 0.1, 0.1, 1.0],
                FrameFlags::empty(),
            );

        // Diagnostic: log first 10 frames + every 300th frame
        let frame_n = FRAME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let window_count = state.user_space.elements().count();
        if frame_n < 10 || frame_n % 300 == 0 {
            tracing::info!(
                "Render frame {frame_n}: {window_count} windows, cursor at ({:.0},{:.0}), {} elements",
                pointer_location.x, pointer_location.y, elements.len()
            );
        }

        match render_result {
            Ok(result) => {
                if !result.is_empty {
                    // Frame has damage — queue it for VBlank presentation.
                    match surface.drm_output.queue_frame(()) {
                        Ok(()) => {
                            VBLANK_PENDING.store(true, Ordering::Relaxed);
                        }
                        Err(err) => {
                            tracing::warn!("Failed to queue frame: {err:?}");
                        }
                    }
                }
                // If is_empty (no damage), don't queue — the fallback timer
                // will retry in ~16ms, and when content changes (cursor moves,
                // window maps), there will be damage and queue_frame will succeed.
            }
            Err(err) => {
                tracing::warn!("Render error: {err:?}");
            }
        }

        // Send frame callbacks to user_space windows
        let elapsed = state.start_time.elapsed();
        let output = surface.output.clone();
        state.user_space.elements().for_each(|window| {
            window.send_frame(
                &output,
                elapsed,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        });

        // Send frame callbacks to layer surfaces
        {
            let layer_map = layer_map_for_output(&output);
            for layer in layer_map.layers() {
                layer.send_frame(
                    &output,
                    elapsed,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
        }

        // Shadow frame callbacks (15 FPS throttled)
        let now = std::time::Instant::now();
        if now.duration_since(state.last_shadow_frame) >= Duration::from_millis(66) {
            state.last_shadow_frame = now;
            state.shadow_space.elements().for_each(|window| {
                window.send_frame(
                    &output,
                    elapsed,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            });
            state.shadow_space.refresh();
        }

        state.user_space.refresh();
        state.popups.cleanup();
        {
            let mut layer_map = layer_map_for_output(&output);
            layer_map.cleanup();
        }
        state.cleanup_dead_windows();
        let _ = state.display_handle.flush_clients();

        Some(())
    })();

    // Screenshot capture (outside render closure to avoid borrow conflicts)
    if state.screenshot_pending {
        if let Some(output) = state.output.as_ref() {
            let output = output.clone();
            if let Some(b64) = capture_kms_screenshot(
                &mut renderer, &state.user_space, &output,
            ) {
                state.screenshot_data = Some(b64);
            }
        }
        state.screenshot_pending = false;
    }
    if state.shadow_screenshot_pending {
        if let Some(output) = state.output.as_ref() {
            let output = output.clone();
            if let Some(b64) = capture_kms_screenshot(
                &mut renderer, &state.shadow_space, &output,
            ) {
                state.shadow_screenshot_data = Some(b64);
            }
        }
        state.shadow_screenshot_pending = false;
    }

    // Put renderer back
    state.kms_renderer = Some(renderer);
}

/// Capture a space as a base64 PNG screenshot using an offscreen texture.
/// Returns Some(base64_string) on success, None on failure.
fn capture_kms_screenshot(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<Window>,
    output: &Output,
) -> Option<String> {
    use base64::Engine;

    let size = output.current_mode()?.size;
    let buf_size: smithay::utils::Size<i32, BufferCoord> = (size.w, size.h).into();

    // Create offscreen texture
    let mut texture = match <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
        renderer, Fourcc::Abgr8888, buf_size,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("KMS screenshot: create_buffer failed: {e:?}");
            return None;
        }
    };

    // Bind offscreen texture as render target
    let mut target = match renderer.bind(&mut texture) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("KMS screenshot: bind failed: {e:?}");
            return None;
        }
    };

    // Render space to offscreen target
    let mut damage_tracker = ScreenshotDamageTracker::from_output(output);
    let render_result = smithay::desktop::space::render_output::<
        _,
        WaylandSurfaceRenderElement<GlesRenderer>,
        _,
        _,
    >(
        output,
        renderer,
        &mut target,
        1.0,
        0,
        [space],
        &[] as &[WaylandSurfaceRenderElement<GlesRenderer>],
        &mut damage_tracker,
        [0.1, 0.1, 0.1, 1.0],
    );

    if let Err(e) = render_result {
        tracing::error!("KMS screenshot: render failed: {e:?}");
        return None;
    }

    // Read pixels from offscreen target
    let region = Rectangle::new(
        (0, 0).into(),
        (size.w, size.h).into(),
    );
    let mapping = match renderer.copy_framebuffer(&target, region, Fourcc::Abgr8888) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("KMS screenshot: copy_framebuffer failed: {e:?}");
            return None;
        }
    };
    let pixels = match renderer.map_texture(&mapping) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("KMS screenshot: map_texture failed: {e:?}");
            return None;
        }
    };

    let rgba = pixels.to_vec();
    let img = image::RgbaImage::from_raw(size.w as u32, size.h as u32, rgba)?;
    let mut png_buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_buf);
    if img.write_to(&mut cursor, image::ImageFormat::Png).is_err() {
        tracing::error!("KMS screenshot: PNG encoding failed");
        return None;
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
    tracing::info!("KMS screenshot captured: {}x{}, {} bytes PNG", size.w, size.h, png_buf.len());
    Some(b64)
}
