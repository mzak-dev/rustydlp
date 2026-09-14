// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// The native window entry point. Not built for wasm32: `cargo build --target
/// wasm32-unknown-unknown` still compiles this `[[bin]]` alongside the `[lib]`
/// wasm-bindgen actually processes (see `docs/adr/0005-wasm-render-experiment.md`
/// and `web/README.md`), even though nothing loads or runs it there -- a
/// browser calls the `#[wasm_bindgen(start)]` function in `web::start`
/// directly once the module is instantiated, never this `main`.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    if let Err(e) = rustydlp::shell::run() {
        eprintln!("rustydlp: {e}");
        std::process::exit(1);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
