// Hides the console window that would otherwise sit behind the GUI on Windows.
// Kept on for debug builds so panics and logs stay visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Must run before anything else: Velopack may terminate/restart the
    // process itself to handle install/update/uninstall lifecycle events.
    #[cfg(windows)]
    velopack::VelopackApp::build().run();

    #[cfg(windows)]
    rustydlp::core::runner::seed_ytdlp_if_missing();

    #[cfg(windows)]
    check_for_updates_in_background();

    if let Err(e) = rustydlp::shell::run() {
        eprintln!("rustydlp: {e}");
        std::process::exit(1);
    }
}

/// Checks GitHub Releases for a newer version and, if found, downloads it and
/// schedules it to apply the next time the app exits normally — never an
/// immediate forced restart, which could cut off an in-progress download.
/// Best-effort: any failure here (offline, rate-limited, whatever) just means
/// no update this launch, logged and otherwise ignored.
#[cfg(windows)]
fn check_for_updates_in_background() {
    std::thread::spawn(|| {
        let source = velopack::sources::GithubSource::new(
            "https://github.com/mzak-dev/rustydlp",
            None,
            false,
        );
        let um = match velopack::UpdateManager::new(source, None, None) {
            Ok(um) => um,
            Err(e) => return eprintln!("rustydlp: update check unavailable: {e}"),
        };
        let updates = match um.check_for_updates() {
            Ok(velopack::UpdateCheck::UpdateAvailable(u)) => u,
            Ok(_) => return,
            Err(e) => return eprintln!("rustydlp: update check failed: {e}"),
        };
        if let Err(e) = um.download_updates(&updates, None) {
            return eprintln!("rustydlp: update download failed: {e}");
        }
        if let Err(e) =
            um.wait_exit_then_apply_updates(&*updates, true, false, Vec::<&str>::new())
        {
            eprintln!("rustydlp: scheduling update apply failed: {e}");
        }
    });
}
