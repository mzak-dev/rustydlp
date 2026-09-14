# Transition contact sheets

One sheet per animated transition, each sampled at 0 / 60 / 120 / 180 / 240 /
300 ms — the whole of `ui::anim::DURATION` — left to right, top row first.
Rendered through the app's own pipeline onto a CPU Skia surface, so they are
what the window actually paints, not a mock-up.

Generated, not hand-captured. They go stale the moment the motion changes:

```
cargo test --lib -- --ignored transition_contact_sheets
```

writes them to `target/transitions`, and `fixture-thumb.png` alongside them —
a generated stand-in for a real thumbnail, because the morph's whole claim is
that the same picture is on screen at both ends of it and a fixture with no
art on disk cannot show that. Copy the sheets here when the motion changes.
