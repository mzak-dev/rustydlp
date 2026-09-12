//! CPU backend.
//!
//! Needs no GPU, adapter or window, so the interface can be rendered and checked
//! in an ordinary test process. The existing CI skips its only GPU test because
//! *"it needs a real D3D12 adapter"*; this is the backend that makes the
//! interface testable anyway.

use skia_safe::{AlphaType, ColorType, ImageInfo, Surface, surfaces};

use super::Backend;
use crate::ui::Rgba;

pub struct RasterBackend {
    surface: Surface,
    width: u32,
    height: u32,
}

impl RasterBackend {
    pub fn new(width: u32, height: u32) -> Self {
        RasterBackend {
            surface: surfaces::raster_n32_premul((width as i32, height as i32))
                .expect("raster surface"),
            width,
            height,
        }
    }

    /// RGBA8 unpremultiplied, row-major — byte order stated explicitly rather
    /// than relying on what N32 happens to be on this platform.
    pub fn read_rgba(&mut self) -> Vec<u8> {
        let info = ImageInfo::new(
            (self.width as i32, self.height as i32),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let row_bytes = self.width as usize * 4;
        let mut out = vec![0u8; row_bytes * self.height as usize];
        let ok = self.surface.read_pixels(&info, &mut out, row_bytes, (0, 0));
        assert!(ok, "read_pixels failed on a raster surface");
        out
    }

    /// The pixel at `(x, y)` as `(r, g, b, a)`.
    pub fn pixel(&mut self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let w = self.width;
        let px = self.read_rgba();
        let i = ((y * w + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3])
    }

    pub fn encode_png(&mut self) -> Vec<u8> {
        let image = self.surface.image_snapshot();
        image
            .encode(None, skia_safe::EncodedImageFormat::PNG, None)
            .expect("png encode")
            .as_bytes()
            .to_vec()
    }
}

impl Backend for RasterBackend {
    fn begin_frame(&mut self, width: u32, height: u32, clear: Rgba) {
        if width != self.width || height != self.height {
            *self = RasterBackend::new(width, height);
        }
        self.surface
            .canvas()
            .clear(skia_safe::Color4f::new(clear.r, clear.g, clear.b, clear.a));
    }

    fn canvas(&mut self) -> &skia_safe::Canvas {
        self.surface.canvas()
    }

    fn present(&mut self) {}
}
