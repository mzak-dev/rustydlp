# 0002 — cosmic-text shapes, Skia rasterizes

Date: 2026-09-12
Status: Accepted

## Context

[ADR-0001](0001-skia-safe-over-pure-wgpu.md) settled on skia-safe, which offers
its own text stack: the `textlayout` module wraps Harfbuzz and ICU and would
have given shaping, line breaking and fallback in one object.

## Decision

Shape with **cosmic-text** (rustybuzz, pure Rust) and rasterize with **Skia**.
cosmic-text returns glyph ids and positions; those go straight into a
`TextBlob` built over a `Typeface` made from the same font bytes.

## Consequences

- **No `SwashCache` bitmap bridge.** The obvious wiring — cosmic-text →
  SwashCache → glyph bitmaps → upload and cache them ourselves — rebuilds an
  atlas Skia already has. Going to `TextBlob` instead lets Skia's GPU glyph
  atlas do the caching, hinting and subpixel antialiasing.
- **Shaping is pure Rust**, so no Harfbuzz or ICU is used at runtime and
  `textlayout` stays off. It does *not* remove the ICU payload: `embed-icudtl`
  is in skia-safe's defaults and dropping it loses the prebuilt binaries.
- **Text inputs share the font database.** `InputState` keeps its own
  cosmic-text buffer, shaped against the very `FontSystem` the painting uses
  (`Shaper::with_shared_fonts`). Anything less and a caret would be measured
  with different metrics than the glyphs beside it.
- **The caret comes from cosmic-text's own cursor positioning**, which walks the
  glyph run and accounts for RTL, rather than from a prefix width. That was the
  reason to build the input on `cosmic_text::Editor` rather than hand-roll it:
  backspace deletes a grapheme rather than a byte, which a CJK filename needs.
- Known gap, documented at its definition: a selection spanning a bidi boundary
  is drawn as one span rather than the two visual runs it really occupies.

## Still open

What font gpui actually resolved on Windows. gpui-component sets
`font_family = ".SystemUIFont"`, gpui's per-platform alias; on Windows that goes
through DirectWrite to the system UI font, and **Segoe UI is a strong prior, not
a confirmed fact**. Every text metric in the parity pass depends on it, so read
it off the running legacy build before pinning the family.
