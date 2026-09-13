# 0005 — Velopack for the installer, a GPL ffmpeg build, and a seed/ vs bin/ split for yt-dlp

Date: 2026-09-13
Status: Accepted

## Context

The app shipped as a bare `rustydlp.exe` release asset. `runner.rs` already
resolved `ffmpeg`/`ffprobe`/`yt-dlp` through explicit override → `<exe_dir>/bin`
→ `%LOCALAPPDATA%\rustyDLP\bin` → `PATH`, and the `.gitignore` comment on `/bin`
("fetched, not tracked") shows bundling was always the intended endpoint — it
just never got wired up. Users had to source and place all three binaries
themselves.

## Decision

Package with **Velopack**: `vpk pack` wraps a plain directory of files (no
Tauri/Electron needed for a non-webview app) into a Windows installer, and
`velopack::UpdateManager` gets auto-updates against a GitHub Releases feed
essentially for free, on top of a release pipeline that already publishes to
GitHub Releases via release-please.

**ffmpeg build: GPL, not LGPL.** `runner.rs`'s convert feature uses `libx264`,
which LGPL ffmpeg builds exclude. Bundling GPL ffmpeg as an unlinked
subprocess binary is "mere aggregation" — no copyleft effect on rustyDLP's own
source — but its license text and a source pointer ship alongside it
(`THIRD_PARTY_NOTICES.txt`).

**yt-dlp seeds into app-data; it does not bundle into `bin/`.** Velopack
replaces the app's whole installed directory on every auto-update.
`resolve()` checks `<exe_dir>/bin` before the app-data bin dir, so a yt-dlp
bundled there would out-rank, and — on every silent app update — silently
revert, a self-updated copy. The installer instead drops the initial copy at
`<exe_dir>/seed/yt-dlp.exe`; `seed_ytdlp_if_missing()` copies it into
`%LOCALAPPDATA%\rustyDLP\bin` once, only if nothing is there yet, and never
touches an existing copy. After that, `resolve()` never finds anything in
`<exe_dir>/bin` for yt-dlp, so the app-data copy — and `yt-dlp -U`'s
self-updates — stays authoritative across every future app update. ffmpeg and
ffprobe have no self-update mechanism, so they bundle straight into
`<exe_dir>/bin` and are simply refreshed with each app release.

## Consequences

- `resolve()`, `ffmpeg_path()`, `ffprobe_path()`, and `ytdlp_path()` needed no
  changes — the lookup order already matched this layout.
- `VelopackApp::build().run()` must be the first thing `main()` does, ahead of
  window/event-loop setup — Velopack may terminate/restart the process to
  handle install/update/uninstall lifecycle events.
- The `build` CI job now fetches a pinned-by-tag ffmpeg build and the latest
  yt-dlp release at build time, and drives `vpk pack`; `attach-binary` uploads
  the resulting `Releases/*` (installer, portable bundle, update feed) instead
  of a bare exe.
- Update checking (`UpdateManager` + `sources::GithubSource`) starts as a
  silent background check-and-apply-on-restart. No UI was added for it yet;
  the existing "not found" banner pattern in `app.rs` is a ready template if
  a visible "update available" banner is wanted later.
