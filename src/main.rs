// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// `legacy` wins when both interfaces are compiled in: during the parity period
// the gpui build is the reference the new one is diffed against, so it is the
// one a plain `cargo run` should give you.
#[cfg(feature = "legacy")]
fn main() {
    rustydlp::legacy_main();
}

#[cfg(not(feature = "legacy"))]
fn main() {
    eprintln!(
        "rustydlp: this build has no interface. The skia interface is still being \
         built bottom-up and has no window yet — it is exercised through the \
         library's golden tests. Build with `--features legacy` to run the app."
    );
    std::process::exit(1);
}
