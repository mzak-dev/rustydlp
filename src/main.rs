// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = rustydlp::shell::run() {
        eprintln!("rustydlp: {e}");
        std::process::exit(1);
    }
}
