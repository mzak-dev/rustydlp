//! Presents a tiny-skia pixmap through softbuffer's web backend.
//!
//! The wasm32 counterpart to `render/soft.rs`: same idea (rasterize on the
//! CPU, hand the finished frame to softbuffer, which owns getting pixels on
//! screen), same 0RGB packing, different rasterizer underneath. softbuffer's
//! web backend draws through the canvas's `CanvasRenderingContext2D` rather
//! than a native swapchain, but the `Surface`/`Context` API winit's window
//! satisfies is identical either way -- nothing here is wasm-specific except
//! the module it lives in.

use std::num::NonZeroU32;
use std::sync::Arc;

use resvg::tiny_skia;
use winit::window::Window;

use crate::ui::Rgba;

pub struct WasmBackend {
    pixmap: tiny_skia::Pixmap,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

impl WasmBackend {
    pub fn new(window: Arc<Window>) -> anyhow::Result<Self> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow::anyhow!("softbuffer context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow::anyhow!("softbuffer surface: {e}"))?;
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        Ok(WasmBackend {
            pixmap: tiny_skia::Pixmap::new(w, h)
                .ok_or_else(|| anyhow::anyhow!("zero-sized pixmap"))?,
            surface,
        })
    }

    pub fn begin_frame(&mut self, width: u32, height: u32, clear: Rgba) {
        let resized = width != self.pixmap.width() || height != self.pixmap.height();
        if resized && let Some(p) = tiny_skia::Pixmap::new(width, height) {
            self.pixmap = p;
        }
        let color = tiny_skia::Color::from_rgba(clear.r, clear.g, clear.b, clear.a)
            .unwrap_or(tiny_skia::Color::BLACK);
        self.pixmap.fill(color);
    }

    pub fn pixmap_mut(&mut self) -> &mut tiny_skia::Pixmap {
        &mut self.pixmap
    }

    pub fn present(&mut self) {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        if let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h))
            && self.surface.resize(nw, nh).is_ok()
            && let Ok(mut buffer) = self.surface.buffer_mut()
        {
            // softbuffer wants 0RGB in a u32 per pixel; tiny-skia's pixels are
            // premultiplied, so unpremultiply before packing or a translucent
            // pixel would come out too dark. Nothing in this app's own
            // drawing is translucent all the way to the canvas edge, but a
            // hover/opacity tween can leave one mid-blend for a frame.
            for (dst, px) in buffer.iter_mut().zip(self.pixmap.pixels()) {
                let c = px.demultiply();
                *dst = (c.red() as u32) << 16 | (c.green() as u32) << 8 | c.blue() as u32;
            }
            let _ = buffer.present();
        }
    }
}
