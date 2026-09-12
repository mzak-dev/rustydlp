# 0001 — skia-safe rather than pure wgpu for the interface rewrite

Date: 2026-09-12
Status: Accepted

## Context

The interface was gpui plus gpui-component. That cost 924 crates, three version
pins that exist only to match what gpui compiles against (`image`, `smallvec`,
and the toolchain channel), and a git dependency that cannot be pinned with a
`rev` without compiling gpui twice. What it returned was six widget types —
both modals were already hand-rolled, and the reactive entity graph held nothing
but widget state.

The guidance this work started from (`rust-wgpu-gui`) is written around wgpu:
its whole section on adapters, `PowerPreference` and backend fallback is wgpu's
API. The obvious reading was therefore wgpu-first.

## Decision

skia-safe, with a purpose-built element tree, taffy layout and our own widgets
on top.

## Consequences

The deciding factor was text. `app.rs` contained **zero** font code, because
gpui shaped, fell back and truncated for us via DirectWrite. This is a yt-dlp
client: titles and filenames are CJK, Cyrillic, Arabic and emoji, not English
labels, so a stack without font fallback renders tofu on ordinary downloads and
a truncation that is not grapheme-aware splits an emoji at the ellipsis. Owning
that outright was the largest hidden cost in the rewrite and the thing most
likely to make "1:1" untrue.

Knowingly accepted against that:

- **Multi-backend fallback is now ours to build.** Ganesh does no adapter
  selection at all, so the wgpu guidance does not transfer: we create the device
  and the swapchain. wgpu would have given adapter selection, `PowerPreference`
  and Vulkan→DX12 fallback as features. See [ADR-0003](0003-d3d12-behind-a-backend-trait.md).
- **A C++ build and a ~15MB payload**, and prebuilt archives keyed on the cargo
  feature combo: a combo with no published archive silently becomes a full Skia
  source build. Measured — `binary-cache` alone 404s.
- **`embed-icudtl` ships whether we shape with ICU or not**, because it is in
  skia-safe's default features and leaving the defaults loses the prebuilts.
  So [ADR-0002](0002-cosmic-text-shapes-skia-rasterizes.md) does not shed that
  payload; it only avoids *using* ICU.
- **No wasm.** Linking Skia into pure wasm needs a nightly flag, and the
  toolchain here is stable.

One unplanned gain: Skia's raster surface needs no GPU, so the whole interface
is golden-tested on an ordinary CI runner — something the gpui build could never
do, and which the existing workflow admits when it skips its one GPU test.
