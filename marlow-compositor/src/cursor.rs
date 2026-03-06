//! Cursor theme loading and pointer rendering.
//!
//! Loads xcursor themes (respects XCURSOR_THEME/XCURSOR_SIZE env vars),
//! with a fallback arrow cursor if theme loading fails.

use std::io::Read;
use std::time::Duration;

use smithay::{
    backend::renderer::{
        element::{
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Kind,
        },
        ImportAll, ImportMem, Renderer, Texture,
    },
    input::pointer::CursorImageStatus,
    utils::{Physical, Point, Scale},
};

use xcursor::{
    parser::{parse_xcursor, Image},
    CursorTheme,
};

smithay::backend::renderer::element::render_elements! {
    pub PointerRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
}

/// Loaded cursor theme with animation support.
pub struct Cursor {
    icons: Vec<Image>,
    pub size: u32,
}

impl Cursor {
    pub fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME")
            .ok()
            .unwrap_or_else(|| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);

        let theme = CursorTheme::load(&name);
        let icons = load_icon(&theme).unwrap_or_else(|err| {
            tracing::warn!("Failed to load xcursor theme: {err}, using fallback");
            fallback_cursor()
        });

        tracing::info!("Cursor loaded: theme={name}, size={size}, frames={}", icons.len());
        Cursor { icons, size }
    }

    pub fn get_image(&self, scale: u32, time: Duration) -> Image {
        let size = self.size * scale;
        frame(time.as_millis() as u32, size, &self.icons)
    }
}

/// Software cursor element that renders as part of the output frame.
pub struct PointerElement {
    buffer: Option<MemoryRenderBuffer>,
    status: CursorImageStatus,
}

impl Default for PointerElement {
    fn default() -> Self {
        Self {
            buffer: None,
            status: CursorImageStatus::default_named(),
        }
    }
}

impl PointerElement {
    pub fn set_status(&mut self, status: CursorImageStatus) {
        self.status = status;
    }

    pub fn set_buffer(&mut self, buffer: MemoryRenderBuffer) {
        self.buffer = Some(buffer);
    }
}

impl<T: Texture + Clone + Send + 'static, R> AsRenderElements<R> for PointerElement
where
    R: Renderer<TextureId = T> + ImportAll + ImportMem,
{
    type RenderElement = PointerRenderElement<R>;
    fn render_elements<E>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        _scale: Scale<f64>,
        _alpha: f32,
    ) -> Vec<E>
    where
        E: From<PointerRenderElement<R>>,
    {
        if matches!(self.status, CursorImageStatus::Hidden) {
            return vec![];
        }
        if let Some(buffer) = self.buffer.as_ref() {
            vec![PointerRenderElement::<R>::from(
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_f64(),
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .expect("Lost cursor buffer"),
            )
            .into()]
        } else {
            vec![]
        }
    }
}

// --- xcursor loading helpers ---

fn nearest_images(size: u32, images: &[Image]) -> impl Iterator<Item = &Image> {
    let nearest = images
        .iter()
        .min_by_key(|image| (size as i32 - image.size as i32).abs())
        .unwrap();
    images
        .iter()
        .filter(move |image| image.width == nearest.width && image.height == nearest.height)
}

fn frame(mut millis: u32, size: u32, images: &[Image]) -> Image {
    let total = nearest_images(size, images).fold(0, |acc, image| acc + image.delay);
    if total == 0 {
        return nearest_images(size, images).next().unwrap().clone();
    }
    millis %= total;
    for img in nearest_images(size, images) {
        if millis < img.delay {
            return img.clone();
        }
        millis -= img.delay;
    }
    unreachable!()
}

fn load_icon(theme: &CursorTheme) -> Result<Vec<Image>, Box<dyn std::error::Error>> {
    let icon_path = theme.load_icon("default").ok_or("No default cursor in theme")?;
    let mut file = std::fs::File::open(icon_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    parse_xcursor(&data).ok_or_else(|| "Failed to parse xcursor file".into())
}

/// Generate a simple 16x16 white arrow cursor as fallback.
fn fallback_cursor() -> Vec<Image> {
    let w = 16u32;
    let h = 16u32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];

    // Arrow shape: each row defines which columns are filled
    let rows: &[(u32, &[u32])] = &[
        (0, &[0]),
        (1, &[0, 1]),
        (2, &[0, 1, 2]),
        (3, &[0, 1, 2, 3]),
        (4, &[0, 1, 2, 3, 4]),
        (5, &[0, 1, 2, 3, 4, 5]),
        (6, &[0, 1, 2, 3, 4, 5, 6]),
        (7, &[0, 1, 2, 3, 4, 5, 6, 7]),
        (8, &[0, 1, 2, 3, 4, 5, 6, 7, 8]),
        (9, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        (10, &[0, 1, 2, 3, 4, 5]),
        (11, &[0, 1, 2, 5, 6]),
        (12, &[0, 1, 6, 7]),
        (13, &[0, 7, 8]),
        (14, &[8, 9]),
        (15, &[9]),
    ];

    for &(row, cols) in rows {
        for &col in cols {
            let idx = ((row * w + col) * 4) as usize;
            // ARGB8888: white opaque
            pixels[idx] = 255; // A
            pixels[idx + 1] = 255; // R
            pixels[idx + 2] = 255; // G
            pixels[idx + 3] = 255; // B
        }
    }

    vec![Image {
        size: 16,
        width: w,
        height: h,
        xhot: 0,
        yhot: 0,
        delay: 1,
        pixels_rgba: pixels,
        pixels_argb: vec![],
    }]
}
