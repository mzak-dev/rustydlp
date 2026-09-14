# Parity deviations

Scope for the interface rewrite is **visual parity with small fixes allowed**.
That weakens a screenshot diff as a gate, so every intended difference is listed
here: a diff against the gpui build that is on this list is deliberate, and a
diff that is not is a regression.

## Intended, already landed

| Difference | Why |
|---|---|
| Animation where `main` has none | `main` is static. The closed PR #9 added animation and was superseded by this rewrite; see the note in the plan about why its bouncy overshoot was impossible under gpui and is not under this renderer. Widgets settle with that bounce; whole screens do not — a page carrying every word on it past its resting point and back reads as a wobble, so a screen swap cross-fades and slides along the navbar's axis instead (`ui::anim::page_swap`). |
| Settings has a back button, top left of its title | It is the one screen reached from a toolbar button rather than from the tab bar, so the tab bar does not show where you are or offer a way out. `main` leaves that same toolbar toggle as the only way back. |
| Segmented tab bar has no animated active indicator | Same. The static look is what `main` shows. |
| `list_hover` and `radius` are guesses | Both are absent from gpui-component's `default-theme.json`; they come from its Rust defaults. **Sample from the running legacy build.** |
| Button `Default`/`Danger` colours are derived | `default-theme.json` has no `button*` keys. Derived from `secondary`/`danger`; **sample from the running build.** |
| Switch and tab metrics | Switch is gpui-component's documented 36x20/16/2. Tab padding and height are derived, not read. |
| A bidi selection draws as one span | Not two visual runs. The fields hold URLs, paths and yt-dlp arguments. |

## Must be resolved before parity is signed off

- **The font.** `.SystemUIFont` → Segoe UI is a prior, not a fact. Every text
  metric depends on it. See ADR-0002.
- **Nothing has been run.** The interface is verified by compilation and by
  rendering screens to a CPU surface. A human has to open the window.

## Behaviour changes from the dependency cleanup

| Change | Why |
|---|---|
| `save_job`/`delete_job` are transactional | rusqlite is real SQLite; the children rewrite is exactly a transaction's job. See ADR-0004. |
| Existing turso-written libraries open with rusqlite | **Unverified** — no such database was available to test. Check against a real one before release. |

## Deferred, deliberately out of scope for the port

- Scrollbars for the six scroll regions (the gpui build had none).
- Bounding the player's frame channel, currently unbounded with 8MB frames.
- `-pix_fmt yuv420p` + `Image::from_yuva_pixmaps`, or `-vf scale` to the stage
  size: today the player pushes 249 MB/s at 1080p30 to paint a ~900px stage.
- The Vulkan backend (ADR-0003).
