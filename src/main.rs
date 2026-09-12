// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// `legacy` wins when both interfaces are compiled in: during the parity period
// the gpui build is the reference the new one is diffed against, so it is the
// one a plain `cargo run` should give you. The replacement runs with
// `--no-default-features --features skia`.
#[cfg(feature = "legacy")]
fn main() {
    rustydlp::legacy_main();
}

#[cfg(all(feature = "skia", not(feature = "legacy")))]
fn main() {
    if let Err(e) = rustydlp::shell::run() {
        eprintln!("rustydlp: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(any(feature = "legacy", feature = "skia")))]
fn main() {
    eprintln!("rustydlp: built with no interface; enable either `legacy` or `skia`.");
    std::process::exit(1);
}
