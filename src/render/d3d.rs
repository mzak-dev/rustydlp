//! The D3D12 backend.
//!
//! **Not implemented.** This module exists so the shape is settled and the CI
//! workflow's existing reference to `render/d3d.rs` has something to point at;
//! the code is not written, and nothing here pretends otherwise.
//!
//! It is the only part of the interface that cannot be built or checked off
//! Windows — D3D12 and the `windows` crate are Windows-only — so it is the one
//! piece that has to be written on a Windows machine or in CI. Everything above
//! `DirectContext` is backend-agnostic and already works on
//! [`super::raster::RasterBackend`], which is what lets the interface be
//! golden-tested without a GPU at all.
//!
//! # What it has to do
//!
//! 1. `CreateDXGIFactory2`, then walk `EnumAdapters1` and **step over WARP**
//!    (`DXGI_ADAPTER_FLAG_SOFTWARE`) — the behaviour
//!    `.github/workflows/rust.yml` already documents when it skips
//!    `builds_a_skia_context_on_a_real_d3d12_device` in CI.
//! 2. `D3D12CreateDevice` on the chosen adapter, then `CreateCommandQueue`
//!    (`D3D12_COMMAND_LIST_TYPE_DIRECT`).
//! 3. Hand the adapter, device and queue to `skia_safe::gpu::d3d::BackendContext`
//!    and build a `DirectContext` with `direct_contexts::make_direct3d`.
//! 4. `CreateSwapChainForHwnd` against the winit window's HWND (via
//!    `RawWindowHandle::Win32`), then per frame wrap the current back buffer as a
//!    `BackendRenderTarget` and `surfaces::wrap_backend_render_target` it into a
//!    `Surface`. Resize tears down and rebuilds the buffers.
//!
//! Note that Skia's Ganesh does **no** adapter selection of its own:
//! `PowerPreference`, `adapter.get_info()` and `DeviceType::DiscreteGpu` are
//! wgpu APIs and do not apply here. Step 1 is entirely ours.
//!
//! # Before starting
//!
//! Check that a published skia-safe prebuilt covers `x86_64-pc-windows-msvc`
//! **with the `d3d` feature**. Prebuilt archives are keyed on the feature combo,
//! and a combo with no published archive falls back to building Skia from source
//! — which the published crate cannot do unaided. That was measured on the
//! `binary-cache`-only combo, which 404s. If `default + d3d` has no archive
//! either, CI pays a full Skia source build on every run, and that is worth
//! knowing before the backend is written rather than after.

use anyhow::{Result, bail};

/// Builds the D3D12 backend for a window.
///
/// Always returns an error today; see the module docs for what it has to do.
pub fn new() -> Result<super::raster::RasterBackend> {
    bail!(
        "the D3D12 backend is not implemented yet; this build presents the \
         CPU-rasterized frame instead (see src/render/d3d.rs)"
    )
}
