# rustyDLP — Context

## Motion system

Introduced 2026-08-07 (see [ADR-0001](docs/adr/0001-adopt-dialog-for-overlays.md)).

rustyDLP is a native `gpui` app — animation goes through `gpui-component`'s
`Transition` builder (`gpui_component::animation`: easing presets, slide/fade/
width/height effects, wraps `gpui::with_animation`/`Animation`). No animation
crate is a dependency of its own; the primitive already ships with
`gpui-component`, which the app already depends on.

**Bounce** is the app's default motion language: a back-out curve (eases past
the target, then settles) rather than a plain ease-out. It applies to every
animated surface in scope, including high-frequency ones — job-row progress
fill and hover feedback — chosen deliberately over damping those, even though
frequent overshoot risks reading as busy during an active download. Revisit
if it does.

**Exception — dialogs.** `gpui_component::dialog::Dialog`'s entrance
animation is hardcoded in the vendored source (fixed duration, fixed easing,
no builder method to override either). Dialogs use that built-in animation
as-is rather than forking the dependency to make it bounce. Dialog entrances
read flatter/snappier than the rest of the app — an intentional, known
inconsistency, not an oversight.

**Toggle-driven animation** (sidebar collapse, hover, selection): keyed off
`window.use_keyed_state` remembering the last *settled* value, compared each
render against the current prop. On a change, play a fresh clip (a distinct
`ElementId::NamedInteger` per target value, so toggling back before it
finishes still restarts cleanly) and update the settled value once the clip's
duration elapses. `gpui_component::switch::Switch` is the reference
implementation of this pattern.

**Out of scope for now**: reduced-motion / OS motion-preference support.
Revisit if requested — unconfirmed whether `gpui` exposes the platform
setting on Windows at all.

## Terms

- **Overlay** — a full-window dimmed backdrop with centered content (the
  New-download form, the Convert-format picker). Both are `Dialog`s, not
  hand-rolled `div`s, as of ADR-0001.
