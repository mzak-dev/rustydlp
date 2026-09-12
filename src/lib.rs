//! rustydlp, as a library.
//!
//! The binary is a few lines that pick an interface and start it; everything
//! else lives here so that the interface under construction can be compiled
//! and tested without the one it replaces:
//!
//! ```text
//! cargo test --no-default-features --features skia --lib
//! ```
//!
//! builds `core/`, `ui/` and `render/` with no gpui in the dependency graph.

/// Everything that is not the user interface. Depends on no GUI crate.
pub mod core;

// -- the gpui interface (feature `legacy`) -----------------------------------
#[cfg(feature = "legacy")]
pub mod app;
#[cfg(feature = "legacy")]
pub mod assets;

// -- the replacement (feature `skia`) ---------------------------------------
#[cfg(feature = "skia")]
pub mod render;
#[cfg(feature = "skia")]
pub mod ui;

/// Opens the gpui window and runs until it closes.
#[cfg(feature = "legacy")]
pub fn legacy_main() {
    use gpui::*;
    use gpui_component::{Root, Theme, ThemeMode};

    gpui_platform::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_component::init(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1180.), px(760.)), cx)),
                ..Default::default()
            };

            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    Theme::change(ThemeMode::Dark, Some(window), cx);
                    let view = cx.new(|cx| app::RustyDlp::new(window, cx));
                    // The first level inside the window must be a Root — it hosts
                    // modals, drawers and notifications for everything below it.
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}
