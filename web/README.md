# wasm render experiment

What this is and why: `docs/adr/0005-wasm-render-experiment.md`. This file is
just the build steps.

## Build

```sh
rustup target add wasm32-unknown-unknown   # already in rust-toolchain.toml
cargo install wasm-bindgen-cli --version 0.2.126 --locked

cargo build --release --target wasm32-unknown-unknown --lib
wasm-bindgen --target web --out-dir web/dist \
  target/wasm32-unknown-unknown/release/rustydlp.wasm
```

`--lib` matters: the `[[bin]]` (the native window entry point) and `[lib]`
(what `wasm-bindgen` needs) both produce a `rustydlp.wasm` for this target —
building both trips a filename-collision warning and one silently
overwrites the other. Building only the lib avoids it.

The `wasm-bindgen-cli` version **must match** the `wasm-bindgen` version
`Cargo.lock` resolves to (check with `grep -A1 '^name = "wasm-bindgen"'
Cargo.lock`) — a mismatch fails at the `wasm-bindgen` step, not silently.

## Run

```sh
python3 -m http.server -d web 8080
```

Then open `http://localhost:8080/` — a bare page that just loads and runs the
build, nothing more. `http://localhost:8080/demo.html` is the same build
behind a longer, standalone page (what "actually runs" vs. what's fabricated
demo data, at a glance) — what to share if the point is showing someone the
experiment rather than developing against it.

`web/dist/` is build output (gitignored); everything else in `web/` is
tracked.
