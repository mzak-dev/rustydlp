# 0001 — Adopt `gpui_component::dialog` for the New-download and Convert-format overlays

Date: 2026-08-07
Status: Accepted

## Context

Both the New-download modal (`RustyDlp::modal`) and the Convert-format picker
(`RustyDlp::convert_format_picker`) were hand-rolled: a single
`div().absolute().inset_0()` dimmed backdrop plus a centered card, gated by a
plain `bool` / `Option<String>` field and rendered inline from the top-level
`render()`. Both carried the same comment, explaining the choice at the time:

> hand-rolled overlay rather than gpui_component::dialog — one
> absolutely-positioned div, no modal-manager lifecycle to learn

That trade-off held as long as neither overlay needed to animate. Adding
motion changes it: `gpui_component::dialog::Dialog` already ships a
fade + slide-down entrance, a focus trap, Esc-to-close, and
click-outside-to-close, all through `Root`'s dialog stack — which `main.rs`
was already set up to host:

> The first level inside the window must be a Root — it hosts modals,
> drawers and notifications for everything below it

...but nothing in the app used it yet. Reimplementing any of that by hand to
match would mean rebuilding a chunk of what the library already does.

## Decision

Both overlays move to `window.open_dialog(cx, ...)`. `RustyDlp`'s `modal` and
`convert_picker` fields stay as the source of truth for *what* to show; they
now gate *opening* the dialog stack rather than *rendering* the overlay
directly.

## Consequences

- Gains: focus trap, Esc-to-close, click-outside-to-close, consistent dialog
  chrome (shadow, radius, close button) — all free, all previously absent.
- The dialog's content closures run with `&mut Window, &mut App`, not
  `&mut Context<RustyDlp>` — the same constraint `navbar`'s `TabBar::on_click`
  already works around via `cx.entity()` + `.update()`. Both overlays' click
  handlers need the same treatment.
- `Dialog`'s entrance animation is hardcoded (`ANIMATION_DURATION` = 250ms,
  fixed `cubic_bezier(0.32, 0.72, 0., 1.)` in the vendored
  `crates/ui/src/dialog/dialog.rs`) with no builder method to override
  duration or easing. The app's motion language elsewhere is a bouncy
  overshoot (see [`CONTEXT.md`](../../CONTEXT.md#motion-system)); dialogs
  don't get it, and won't unless the vendored fork is patched. Accepted as a
  known, intentional inconsistency rather than forking `gpui-component`.
- Supersedes the "no modal-manager lifecycle to learn" reasoning in the
  original inline comments — motion was the deciding factor that reasoning
  didn't anticipate.
