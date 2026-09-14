# 0005 — A wasm32 render path, as an experiment

Date: 2026-09-14
Status: Accepted

## Context

[ADR-0001](0001-skia-safe-over-pure-wgpu.md) closed with *"No wasm. Linking
Skia into pure wasm needs a nightly flag, and the toolchain here is stable."*
This ADR was asked to reopen that, on stable, without contradicting the part
of ADR-0001's reasoning that is still true.

It still is: `skia-bindings` publishes no prebuilt binary for
`wasm32-unknown-unknown` under this crate's feature combination (confirmed by
actually asking it to try — the download is a real, current 404, not a
guess), and its fallback is a full Skia source build, which needs a
toolchain (depot_tools, a Skia checkout, and realistically Emscripten for a
browser target) this crate has never carried and is not taking on now. So
`skia-safe` — and with it `core/`, `app.rs`, `render/`, `shell.rs` — stays
exactly what ADR-0001 and [ADR-0003](0003-d3d12-behind-a-backend-trait.md)
already decided: native-only, D3D12 first, `render/soft.rs` under it.

What changed is the rest of the dependency graph. Checked the same way —
actually building each for `wasm32-unknown-unknown`, not read off a crate's
README — `taffy`, `cosmic-text`, `resvg` (already built on `tiny-skia`,
already in this tree for icon rendering), `tiny-skia` itself, `winit` and
`softbuffer` all compile clean on stable, no special flags. `winit` and
`softbuffer` both ship real, maintained web backends (a `<canvas>`-backed
window; a software surface presented through it). Nothing about *this* half
of ADR-0001's "no wasm" holds anymore, if it ever fully did.

That matters because [ADR-0003](0003-d3d12-behind-a-backend-trait.md) already
drew the one line that makes a second render backend cheap: *"Everything
above `DirectContext` is backend-agnostic, so swapping D3D12 in touches
nothing in `ui/`, `widget/` or `app_skia`."* The same is true one layer up —
`ui/element.rs`, `ui/layout.rs` (already generic over a `MeasureText` trait,
precisely so layout could be tested without a real shaper) and `ui/event.rs`
have no Skia in them at all. `ui/svg.rs` turned out to already be built on
`tiny_skia::Pixmap` under `resvg::render`, unconditionally — the "Skia" in
its file name is `ui/paint.rs`'s doing, not its own. Only `ui/text.rs` (hands
cosmic-text's shaped glyphs to a Skia `TextBlob`) and `ui/paint.rs` (draws
onto a `skia_safe::Canvas`) are actually Skia. That is a small, well-isolated
seam to duplicate for a second backend — smaller than D3D12 was.

## Decision

Add `src/web/`, a wasm32-only sibling to `render/` + `ui/text.rs` +
`ui/paint.rs`: `web::text::WebShaper` shapes with cosmic-text and rasterizes
glyphs with its own `SwashCache` instead of handing them to Skia; `web::paint`
walks the same `Box_` list `ui/paint.rs` does and draws it with `tiny-skia`;
`web::backend::WasmBackend` presents the finished frame through
`softbuffer`'s web (canvas) backend, the same "rasterize on the CPU, hand
softbuffer the buffer" shape as `render/soft.rs`. `web::mod` drives it with
winit's web event loop (`EventLoopExtWebSys::spawn_app`, since a browser tab
cannot block its one thread the way `EventLoop::run_app` wants to).

`core/`, `app.rs`, `render/` and `shell.rs` are gated
`#[cfg(not(target_arch = "wasm32"))]` in `src/lib.rs` and stay completely
unmodified. `ui/paint.rs` and `ui/text.rs` are gated the same way; everything
else in `ui/` and all of `widget/` is unconditional and shared by both
backends as-is.

**Deliberately not attempted** — not "for later," but out of scope for what
an experiment answering *"does this app's UI framework run on wasm32 at
all"* needs to answer:

- **Downloading, converting, or playing anything.** `core/` spawns yt-dlp and
  ffmpeg as subprocesses (`std::process::Command`), which do not exist as a
  concept in a browser sandbox — there is no sandboxed wasm target this
  becomes possible under, not just a missing crate feature. `src/web/`
  therefore has its own small, fabricated-data demo screen (`web::Demo`)
  standing in for `RustyDlp`, not a stub of the real one.
- **`RustyDlp`/`Updates` itself.** Its worker-offload model
  (`Updates::spawn`) is `std::thread::spawn`, which is a no-op-that-silently-
  fails on `wasm32-unknown-unknown` without cross-origin-isolation and a
  worker pool neither this build nor a plain static file server provides.
  Nothing in the demo needs a thread, so this was left rather than routed
  around.
- **Pixel parity with the native build.** `web::paint`'s clip is a plain
  rectangular `tiny_skia::Mask` (Skia's own `canvas.clip_rect` is
  anti-aliased; this is not), and its rounded rects are a cubic-BĂ©zier
  corner approximation (tiny-skia has no `RRect` primitive). Both are
  visually close at UI scale and not worth a shared abstraction for one
  consumer.
- **Resize, HiDPI, and keyboard/text input.** The canvas is created at a
  fixed logical size and never resized after; nothing in the demo screen is
  a text field. `ui/text.rs`'s `shape_truncated` (ellipsis truncation) was
  likewise left unported — nothing on the demo screen is long enough to need
  it.

## Consequences

- A contributor can run `cargo build --target wasm32-unknown-unknown --lib`
  (target installed automatically — see `rust-toolchain.toml`) and, after
  `wasm-bindgen --target web`, open the result in a browser and click around
  a real (if fabricated) screen. Build and run steps, and why `--lib` in
  particular: `web/README.md`.
- `Cargo.toml`'s `[dependencies]` is now split by `cfg(target_arch =
  "wasm32")`: `skia-safe`, `rusqlite`, `cpal` and `dirs` moved under
  `cfg(not(target_arch = "wasm32"))` (alongside the existing
  `cfg(windows)` stanza, unchanged); `taffy`, `cosmic-text`, `resvg`, `winit`
  and `softbuffer` stayed unconditional, since both backends need them.
  `[lib]` gained `crate-type = ["cdylib", "rlib"]` — `cdylib` is what
  `wasm-bindgen` processes; the native binary and `cargo test --lib` still
  link the `rlib` as before.
- Two shaping/rasterizing stacks now exist side by side (`ui/text.rs` +
  `ui/paint.rs` vs. `web/text.rs` + `web/paint.rs`), reachable only one at a
  time by construction — the `cfg`s make them mutually exclusive, not just
  conventionally separate. A change to the element tree, layout, or a widget
  in `ui/`/`widget/` is picked up by both automatically; a change to how a
  *box* gets drawn (a new paint primitive, a new `Content` variant) has to be
  taught to both, same as `render/d3d.rs` and `render/soft.rs` already
  double up on presenting a frame.
- This does not change what the app ships. `render/d3d.rs` is still the
  Windows `.exe`'s only real target; `web/` builds nothing CI runs today and
  is not part of the release pipeline in `docs/agents/releases.md`.
- If this experiment is ever promoted past "does it run" — real data via a
  fetch-based backend in place of `core/`, WebGPU instead of the CPU/
  softbuffer path, a shared clip/rounded-rect abstraction between `ui/paint.rs`
  and `web/paint.rs` — that is new scope for a follow-up ADR, not something
  this one commits to.
