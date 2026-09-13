//! Fallback: rasterize on the CPU, present with softbuffer.
//!
//! Used when no hardware D3D12 adapter is available (WARP-only VMs, remote
//! sessions) and off Windows. Slow at video resolutions -- every frame is read
//! back and converted pixel by pixel -- but always works.

use std::num::NonZeroU32;
use std::sync::Arc;

use winit::window::Window;

use super::Backend;
use super::raster::RasterBackend;
use crate::ui::Rgba;

pub struct SoftBackend {
    raster: RasterBackend,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

impl SoftBackend {
    pub fn new(window: Arc<Window>) -> anyhow::Result<Self> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow::anyhow!("softbuffer context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow::anyhow!("softbuffer surface: {e}"))?;
        let size = window.inner_size();
        Ok(SoftBackend {
            raster: RasterBackend::new(size.width.max(1), size.height.max(1)),
            surface,
        })
    }
}

impl Backend for SoftBackend {
    fn begin_frame(&mut self, width: u32, height: u32, clear: Rgba) {
        self.raster.begin_frame(width, height, clear);
    }

    fn canvas(&mut self) -> &skia_safe::Canvas {
        self.raster.canvas()
    }

    fn present(&mut self) {
        let (w, h) = self.raster.size();
        let rgba = self.raster.read_rgba();
        if let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h))
            && self.surface.resize(nw, nh).is_ok()
            && let Ok(mut buffer) = self.surface.buffer_mut()
        {
            // softbuffer wants 0RGB in a u32 per pixel.
            for (dst, px) in buffer.iter_mut().zip(rgba.chunks_exact(4)) {
                *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
            }
            let _ = buffer.present();
        }
    }
}
