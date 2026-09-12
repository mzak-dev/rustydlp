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
pub mod core;

/// Embedded icons.
pub mod assets;

pub mod app;
pub mod render;
pub mod shell;
pub mod ui;
pub mod widget;
