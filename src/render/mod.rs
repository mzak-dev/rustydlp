//! Skia render backends.
//!
//! Everything above `DirectContext` is backend-agnostic, so the interface is a
//! narrow trait: hand out a canvas, then present. `raster` is the CPU backend —
//! no GPU, which is what lets the whole interface be golden-tested on a plain CI
//! runner. `d3d` (the D3D12 backend the CI workflow already names) is the
//! shipping one and can only be compiled on Windows.

#[cfg(windows)]
pub mod d3d;
pub mod raster;
pub mod soft;

/// A surface to draw a frame into.
pub trait Backend {
    /// Resizes if needed and returns a canvas cleared to `clear`.
    fn begin_frame(&mut self, width: u32, height: u32, clear: super::ui::Rgba);
    fn canvas(&mut self) -> &skia_safe::Canvas;
    /// Flushes the frame to wherever it goes.
    fn present(&mut self);
}
