# 0003 — D3D12 behind a backend trait, Vulkan deferred

Date: 2026-09-12
Status: Accepted

## Context

[ADR-0001](0001-skia-safe-over-pure-wgpu.md) means we create the GPU device and
swapchain ourselves: Ganesh starts at `DirectContext` and does no adapter
selection. The `rust-wgpu-gui` guidance is emphatic about Vulkan on consumer
Windows hardware — hybrid iGPU/dGPU cross-adapter present can *panic*,
swapchain reconfiguration can fail with "already acquired image", RenderDoc can
freeze on the first frame, and AMD Adrenalin overlays cause unexplained API
crashes — and its stated remedy is to fall back to DX12. This app ships a
Windows `.exe`; its users are on laptops.

Vulkan primary with a DX12 fallback was considered and rejected, because with
Skia each backend is built by hand (`ash` for Vulkan, the `windows` crate for
D3D12: two device stacks, two swapchains, two resource-recreation paths), and
because the worst Vulkan failure is a panic rather than a recoverable
`SurfaceError` — so a true in-flight fallback cannot cover the case it is
wanted for.

## Decision

A `render::Backend` trait. **D3D12** is the intended backend; **Vulkan** is
deferred and additive. A **raster** implementation exists for tests, and
softbuffer presents the raster frame until D3D12 lands.

## Consequences

- Everything above `DirectContext` is backend-agnostic, so swapping D3D12 in
  touches nothing in `ui/`, `widget/` or `app_skia`.
- `render/raster.rs` needs no GPU, adapter or window, which is what makes the
  interface golden-testable on a plain CI runner. The existing workflow skips
  its only GPU test because *"it needs a real D3D12 adapter"*.
- **`render/d3d.rs` is not implemented.** It is the one module that cannot be
  compiled off Windows, so it has to be written there; its module docs carry the
  full sequence (adapter walk stepping over WARP, device and queue, Skia's
  `BackendContext`, swapchain) rather than leaving it to be rediscovered.
- Until then the frame is rasterized on the CPU and presented with softbuffer.
  That is correct but will not keep up with the player, which pushes 249 MB/s at
  1080p30 — so D3D12 is a prerequisite for shipping, not a nice-to-have.
- **Check before starting:** that a published skia-safe prebuilt covers
  `x86_64-pc-windows-msvc` *with* the `d3d` feature. Archives are keyed on the
  feature combo and a combo without one forces a full Skia source build.
- The workflow's comment that *"the crate is `cfg(windows)` top to bottom"* is
  false today — the only Windows-specific code is `CREATE_NO_WINDOW` — and the
  D3D12 backend is what will make it true.
