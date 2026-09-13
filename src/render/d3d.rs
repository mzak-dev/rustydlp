//! The D3D12 backend: Skia's Ganesh drawing straight into a DXGI swapchain.
//!
//! Adapted from skia-safe's own `examples/d3d-window`. Ganesh does no adapter
//! selection of its own, so the adapter walk is ours: the first hardware
//! adapter that gives a feature-level-11 device, stepping over WARP
//! (`DXGI_ADAPTER_FLAG_SOFTWARE`) -- a software D3D12 device would be slower
//! than the raster fallback it replaces.

use anyhow::{Context, Result};
use skia_safe::gpu::d3d::{BackendContext, TextureResourceInfo};
use skia_safe::gpu::{
    BackendRenderTarget, DirectContext, Protected, SurfaceOrigin, surfaces,
};
use skia_safe::{ColorType, Surface};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_STATE_COMMON, D3D12CreateDevice, ID3D12Device,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
    DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_NONE, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter1, IDXGIFactory4, IDXGISwapChain3,
};
use windows::core::Interface;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::Backend;
use crate::ui::Rgba;

const BUFFER_COUNT: u32 = 2;

pub struct D3dBackend {
    // Field order is drop order: surfaces reference the context's resources,
    // so they go before it.
    surfaces: Vec<Surface>,
    swap_chain: IDXGISwapChain3,
    context: DirectContext,
    current: usize,
    width: u32,
    height: u32,
}

impl D3dBackend {
    pub fn new(window: &Window) -> Result<Self> {
        let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }?;
        let (adapter, device) = hardware_adapter(&factory)?;
        let queue = unsafe { device.CreateCommandQueue(&Default::default()) }?;
        let backend = BackendContext {
            adapter,
            device,
            queue,
            memory_allocator: None,
            protected_context: Protected::No,
        };
        let context = unsafe { DirectContext::new_d3d(&backend, None) }
            .context("Skia could not build a D3D12 context")?;

        let swap_chain: IDXGISwapChain3 = unsafe {
            factory.CreateSwapChainForHwnd(
                &backend.queue,
                hwnd,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: width,
                    Height: height,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: BUFFER_COUNT,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    ..Default::default()
                },
                None,
                None,
            )
        }?
        .cast()?;

        let mut this = D3dBackend {
            surfaces: Vec::new(),
            swap_chain,
            context,
            current: 0,
            width,
            height,
        };
        this.wrap_buffers()?;
        Ok(this)
    }

    /// Wraps each swapchain buffer in a Skia surface.
    fn wrap_buffers(&mut self) -> Result<()> {
        for i in 0..BUFFER_COUNT {
            let resource = unsafe { self.swap_chain.GetBuffer(i) }?;
            let target = BackendRenderTarget::new_d3d(
                (self.width as i32, self.height as i32),
                &TextureResourceInfo {
                    resource,
                    alloc: None,
                    resource_state: D3D12_RESOURCE_STATE_COMMON,
                    format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    sample_count: 1,
                    level_count: 0,
                    sample_quality_pattern: DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
                    protected: Protected::No,
                },
            );
            let surface = surfaces::wrap_backend_render_target(
                &mut self.context,
                &target,
                SurfaceOrigin::TopLeft,
                ColorType::RGBA8888,
                None,
                None,
            )
            .context("Skia could not wrap a swapchain buffer")?;
            self.surfaces.push(surface);
        }
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        // ResizeBuffers fails while anything still holds a buffer, and Skia
        // may still have work queued against them.
        self.surfaces.clear();
        self.context.flush_submit_and_sync_cpu();
        unsafe {
            self.swap_chain.ResizeBuffers(
                BUFFER_COUNT,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }?;
        self.width = width;
        self.height = height;
        self.wrap_buffers()
    }
}

fn hardware_adapter(factory: &IDXGIFactory4) -> Result<(IDXGIAdapter1, ID3D12Device)> {
    // EnumAdapters1 errors with DXGI_ERROR_NOT_FOUND past the last adapter,
    // which `?` turns into "no usable adapter".
    for i in 0.. {
        let adapter = unsafe { factory.EnumAdapters1(i) }.context("no hardware D3D12 adapter")?;
        let desc = unsafe { adapter.GetDesc1() }?;
        if DXGI_ADAPTER_FLAG(desc.Flags as _) & DXGI_ADAPTER_FLAG_SOFTWARE != DXGI_ADAPTER_FLAG_NONE {
            continue;
        }
        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_ok()
            && let Some(device) = device
        {
            return Ok((adapter, device));
        }
    }
    unreachable!()
}

impl Backend for D3dBackend {
    fn begin_frame(&mut self, width: u32, height: u32, clear: Rgba) {
        if (width != self.width || height != self.height)
            && let Err(e) = self.resize(width, height)
        {
            eprintln!("rustydlp: swapchain resize failed: {e:#}");
        }
        self.current = unsafe { self.swap_chain.GetCurrentBackBufferIndex() } as usize;
        self.canvas()
            .clear(skia_safe::Color4f::new(clear.r, clear.g, clear.b, clear.a));
    }

    fn canvas(&mut self) -> &skia_safe::Canvas {
        self.surfaces[self.current].canvas()
    }

    fn present(&mut self) {
        self.context
            .flush_and_submit_surface(&mut self.surfaces[self.current], None);
        // Sync interval 1: blocks on vsync, which is also what paces redraws
        // during playback and hover-drag instead of spinning the CPU.
        let _ = unsafe { self.swap_chain.Present(1, DXGI_PRESENT::default()) }.ok();
    }
}
