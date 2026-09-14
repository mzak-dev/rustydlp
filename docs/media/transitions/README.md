# Transition contact sheets

One sheet per animation, sampled six times across its own span, left to right,
top row first — the whole of `ui::anim::DURATION` for a transition, one full
sweep of `LOADING_SWEEP` for `player-loading`, which loops rather than
finishing. Rendered through the app's own pipeline onto a CPU Skia surface, so
they are what the window actually paints, not a mock-up.

Generated, not hand-captured. They go stale the moment the motion changes:

```
cargo test --lib -- --ignored transition_contact_sheets
```

writes them to `target/transitions`, along with two stand-ins it needs and
leaves behind: `fixture-thumb.png`, because the morph's whole claim is that the
same picture is on screen at both ends of it and a fixture with no art on disk
cannot show that, and `fixture-clip.mp4`, an empty file that exists only so the
player will try to open it. Copy the sheets here when the motion changes.
