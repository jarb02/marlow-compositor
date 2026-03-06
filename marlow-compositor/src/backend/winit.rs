use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            element::surface::WaylandSurfaceRenderElement,
            gles::{GlesRenderer, GlesTarget},
            ExportMem,
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::Marlow;

pub fn init_winit(
    event_loop: &mut EventLoop<Marlow>,
    state: &mut Marlow,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init()?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Marlow".into(),
            model: "Compositor".into(),
            serial_number: "0001".into(),
        },
    );

    let _global = output.create_global::<Marlow>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.user_space.map_output(&output, (0, 0));

    // Store output reference for shadow frame callbacks
    state.output = Some(output.clone());

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut shadow_damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                // Render user_space to screen
                {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    smithay::desktop::space::render_output::<
                        _,
                        WaylandSurfaceRenderElement<GlesRenderer>,
                        _,
                        _,
                    >(
                        &output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        0,
                        [&state.user_space],
                        &[],
                        &mut damage_tracker,
                        [0.1, 0.1, 0.1, 1.0],
                    )
                    .unwrap();
                }

                backend.submit(Some(&[damage])).unwrap();

                // Screenshot capture after submit — re-bind to read the back buffer
                if state.screenshot_pending {
                    let (renderer, framebuffer) = backend.bind().unwrap();
                    capture_screenshot(renderer, &framebuffer, size.w, size.h, state);
                }

                // Shadow screenshot: render shadow_space to framebuffer, capture, don't submit.
                // The next Redraw will overwrite this with user_space (full damage).
                if state.shadow_screenshot_pending {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    smithay::desktop::space::render_output::<
                        _,
                        WaylandSurfaceRenderElement<GlesRenderer>,
                        _,
                        _,
                    >(
                        &output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        0,
                        [&state.shadow_space],
                        &[],
                        &mut shadow_damage_tracker,
                        [0.1, 0.1, 0.1, 1.0],
                    )
                    .unwrap();
                    capture_shadow_screenshot(renderer, &framebuffer, size.w, size.h, state);
                }

                // Send frame callbacks to user_space windows
                state.user_space.elements().for_each(|window| {
                    window.send_frame(
                        &output,
                        state.start_time.elapsed(),
                        Some(Duration::ZERO),
                        |_, _| Some(output.clone()),
                    )
                });

                // Send frame callbacks to shadow_space windows (15 FPS throttled)
                let now = std::time::Instant::now();
                if now.duration_since(state.last_shadow_frame) >= Duration::from_millis(66) {
                    state.last_shadow_frame = now;
                    state.shadow_space.elements().for_each(|window| {
                        window.send_frame(
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        )
                    });
                    state.shadow_space.refresh();
                }

                state.user_space.refresh();
                state.popups.cleanup();
                state.cleanup_dead_windows();
                let _ = state.display_handle.flush_clients();

                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        })?;

    Ok(())
}

/// Capture the current framebuffer as base64 PNG (user_space screenshot).
fn capture_screenshot(
    renderer: &mut GlesRenderer,
    framebuffer: &GlesTarget<'_>,
    width: i32,
    height: i32,
    state: &mut Marlow,
) {
    use base64::Engine;
    use smithay::backend::allocator::Fourcc;

    let region = Rectangle::new((0, 0).into(), (width, height).into());
    match renderer.copy_framebuffer(framebuffer, region, Fourcc::Abgr8888) {
        Ok(mapping) => match renderer.map_texture(&mapping) {
            Ok(pixels) => {
                let rgba = pixels.to_vec();
                if let Some(img) =
                    image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                {
                    let mut png_buf = Vec::new();
                    let mut cursor = std::io::Cursor::new(&mut png_buf);
                    if img
                        .write_to(&mut cursor, image::ImageFormat::Png)
                        .is_ok()
                    {
                        let b64 = base64::engine::general_purpose::STANDARD
                            .encode(&png_buf);
                        state.screenshot_data = Some(b64);
                        tracing::info!(
                            "Screenshot captured: {}x{}, {} bytes PNG",
                            width,
                            height,
                            png_buf.len()
                        );
                    } else {
                        tracing::error!("Screenshot PNG encoding failed");
                    }
                } else {
                    tracing::error!("Screenshot: invalid pixel data dimensions");
                }
            }
            Err(e) => tracing::error!("Screenshot map_texture failed: {e:?}"),
        },
        Err(e) => tracing::error!("Screenshot copy_framebuffer failed: {e:?}"),
    }
    state.screenshot_pending = false;
}

/// Capture shadow_space framebuffer as base64 PNG.
fn capture_shadow_screenshot(
    renderer: &mut GlesRenderer,
    framebuffer: &GlesTarget<'_>,
    width: i32,
    height: i32,
    state: &mut Marlow,
) {
    use base64::Engine;
    use smithay::backend::allocator::Fourcc;

    let region = Rectangle::new((0, 0).into(), (width, height).into());
    match renderer.copy_framebuffer(framebuffer, region, Fourcc::Abgr8888) {
        Ok(mapping) => match renderer.map_texture(&mapping) {
            Ok(pixels) => {
                let rgba = pixels.to_vec();
                if let Some(img) =
                    image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                {
                    let mut png_buf = Vec::new();
                    let mut cursor = std::io::Cursor::new(&mut png_buf);
                    if img
                        .write_to(&mut cursor, image::ImageFormat::Png)
                        .is_ok()
                    {
                        let b64 = base64::engine::general_purpose::STANDARD
                            .encode(&png_buf);
                        state.shadow_screenshot_data = Some(b64);
                        tracing::info!(
                            "Shadow screenshot captured: {}x{}, {} bytes PNG",
                            width,
                            height,
                            png_buf.len()
                        );
                    }
                }
            }
            Err(e) => tracing::error!("Shadow screenshot map_texture failed: {e:?}"),
        },
        Err(e) => tracing::error!("Shadow screenshot copy_framebuffer failed: {e:?}"),
    }
    state.shadow_screenshot_pending = false;
}
