//! rustydlp, as a library.
//!
//! The binary is a few lines that open the window; everything else lives here so
//! that the interface can be compiled and tested without one:
//!
//! ```text
//! cargo test --lib
//! ```
//!
//! renders whole screens to a CPU Skia surface, with no GPU and no window.

/// Everything that is not the user interface. Depends on no GUI crate.
///
/// Native-only: spawns yt-dlp/ffmpeg subprocesses, opens a bundled SQLite file
/// and talks to a real audio device, none of which exist in a browser sandbox.
#[cfg(not(target_arch = "wasm32"))]
pub mod core;

/// Embedded icons.
pub mod assets;

/// The interactive app: `core/` plus OS threads to run it off the UI thread.
/// Native-only for the same reason `core` is.
#[cfg(not(target_arch = "wasm32"))]
pub mod app;
/// Skia render backends (D3D12, and the CPU raster/softbuffer fallback).
/// Native-only: no published skia-safe prebuilt covers wasm32-unknown-unknown
/// for this feature combo, and building Skia from source needs a toolchain
/// this crate does not carry. See docs/adr/0001 and docs/adr/0005.
#[cfg(not(target_arch = "wasm32"))]
pub mod render;
/// The native window and its winit desktop event loop.
#[cfg(not(target_arch = "wasm32"))]
pub mod shell;
pub mod ui;
pub mod widget;

/// A rendering-only experiment: the same `ui`/`widget` element tree, laid out
/// with the same taffy engine, painted onto an HTML canvas with a tiny-skia +
/// cosmic-text pipeline instead of Skia. No `core/`, no downloads, no
/// playback -- see docs/adr/0005-wasm-render-experiment.md.
#[cfg(target_arch = "wasm32")]
pub mod web;
