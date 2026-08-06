// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod model;
mod runner;
mod store;
mod ytdlp;

use gpui::*;
use gpui_component::{Root, Theme, ThemeMode};

fn main() {
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
