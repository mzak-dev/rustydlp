//! Tiny value-tweening helper.
//!
//! There is no per-widget retained state here the way gpui's
//! `window.use_keyed_state` gave it: `RustyDlp` builds a fresh element tree
//! every render, so instead this holds its own small table, keyed by a
//! caller-chosen string, and is meant to live as one field on the app and be
//! consulted from `&self` methods (hence `RefCell` rather than `&mut self`
//! throughout — `sidebar()` and `job_row()` are ported from gpui signatures
//! that took `&self`, and keeping that is what lets the two stay diffable).

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::color::Rgba;

/// How long every tween takes to settle. One knob, so every animated surface
/// in the app moves at the same pace.
pub const DURATION: Duration = Duration::from_millis(300);

/// Overshoots past the target then settles back -- the "bounce" motion
/// language. Unlike gpui's `AnimationElement`, which debug_asserts the eased
/// delta stays in `[0, 1]` and panics the instant a true overshoot curve is
/// played (the reason an earlier attempt at this on the gpui build could only
/// fake a bounce behind a `.clamp(0.0, 1.0)`, and even that clamped version
/// crashed until it was added), this renderer's own paint code puts no such
/// constraint on intermediate values: it is just numbers fed to `lerp`. So
/// this is the textbook analytic back-out formula, not an approximation.
fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let t = t - 1.0;
    1.0 + C3 * t * t * t + C1 * t * t
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    // Clamped per channel: the bounce curve overshoots past 1.0 and back
    // below 0.0 by design for widths and positions, but a colour channel
    // outside 0..=1 doesn't mean anything to the painter, so this is where
    // the overshoot gets reined back in rather than at every call site.
    Rgba {
        r: lerp(a.r, b.r, t).clamp(0.0, 1.0),
        g: lerp(a.g, b.g, t).clamp(0.0, 1.0),
        b: lerp(a.b, b.b, t).clamp(0.0, 1.0),
        a: lerp(a.a, b.a, t).clamp(0.0, 1.0),
    }
}

fn elapsed_fraction(started: Instant, now: Instant) -> f32 {
    (now.duration_since(started).as_secs_f32() / DURATION.as_secs_f32()).min(1.0)
}

/// A `started` timestamp that already reads as settled (`elapsed_fraction`
/// returns `1.0` for it immediately), for recording a transition that has
/// nothing to actually move from.
fn settled_start(now: Instant) -> Instant {
    now.checked_sub(DURATION).unwrap_or(now)
}

struct Tween<T> {
    from: T,
    to: T,
    started: Instant,
}

struct Entrance {
    identity: u64,
    started: Instant,
}

/// A small table of in-flight transitions, keyed by whatever the caller uses
/// to identify "this same animated thing across renders" — an element id, a
/// job id plus a field name, a screen name.
#[derive(Default)]
pub struct Animator {
    floats: RefCell<HashMap<String, Tween<f32>>>,
    colors: RefCell<HashMap<String, Tween<Rgba>>>,
    entrances: RefCell<HashMap<String, Entrance>>,
}

impl Animator {
    /// Eases toward `target`, keyed by `key`. Returns `target` itself the
    /// first time this key is seen or once the transition has settled;
    /// otherwise a possibly-overshooting in-between value.
    pub fn tween_f32(&self, key: impl Into<String>, target: f32) -> f32 {
        let key = key.into();
        let mut table = self.floats.borrow_mut();
        let now = Instant::now();
        match table.get_mut(&key) {
            Some(tw) => {
                if tw.to != target {
                    // Retarget from wherever the animation currently sits,
                    // not from the old target, so reversing direction
                    // mid-flight (collapsing the sidebar back open before the
                    // first tween finished) doesn't jump.
                    let t = elapsed_fraction(tw.started, now);
                    tw.from = lerp(tw.from, tw.to, ease_out_back(t));
                    tw.to = target;
                    tw.started = now;
                }
                let t = elapsed_fraction(tw.started, now);
                lerp(tw.from, tw.to, ease_out_back(t))
            }
            None => {
                // Backdated rather than `now`: the very first time a key is
                // seen there is no prior value to move from, so this starts
                // already settled instead of reading as "still animating"
                // for one spurious `DURATION`.
                table.insert(key, Tween { from: target, to: target, started: settled_start(now) });
                target
            }
        }
    }

    /// Same as [`Self::tween_f32`], for a background or text colour.
    pub fn tween_color(&self, key: impl Into<String>, target: Rgba) -> Rgba {
        let key = key.into();
        let mut table = self.colors.borrow_mut();
        let now = Instant::now();
        match table.get_mut(&key) {
            Some(tw) => {
                if tw.to != target {
                    let t = elapsed_fraction(tw.started, now);
                    tw.from = lerp_rgba(tw.from, tw.to, ease_out_back(t));
                    tw.to = target;
                    tw.started = now;
                }
                let t = elapsed_fraction(tw.started, now);
                lerp_rgba(tw.from, tw.to, ease_out_back(t))
            }
            None => {
                table.insert(key, Tween { from: target, to: target, started: settled_start(now) });
                target
            }
        }
    }

    /// Progress toward 1.0 since `identity` (whatever value tells apart "the
    /// same screen" from "a different one") last changed at this `key` — a
    /// discrete replay-from-the-start transition (a pane entrance), not a
    /// smoothly retargetable one. `0.0` right after a change, `1.0` once
    /// `DURATION` has elapsed. Callers combine this with `ease_out_back` (for
    /// a position) or use it directly (for an opacity fade-in).
    pub fn entrance_progress(&self, key: impl Into<String>, identity: u64) -> f32 {
        let key = key.into();
        let mut table = self.entrances.borrow_mut();
        let now = Instant::now();
        match table.get_mut(&key) {
            Some(e) => {
                if e.identity != identity {
                    e.identity = identity;
                    e.started = now;
                }
                elapsed_fraction(e.started, now)
            }
            None => {
                table.insert(key, Entrance { identity, started: settled_start(now) });
                // The very first render of a screen shouldn't play an
                // entrance from nothing -- there is no "before" to animate
                // from -- so this starts already settled.
                1.0
            }
        }
    }

    /// The back-out curve `tween_f32`/`tween_color` ease with, exposed so
    /// `entrance_progress`'s raw fraction can drive a position with the same
    /// motion language.
    pub fn ease(t: f32) -> f32 {
        ease_out_back(t)
    }

    /// Whether anything is still short of `DURATION` since it last changed —
    /// the event loop uses this to decide whether to keep waking up and
    /// redrawing on a timer, the same way it does while a video plays.
    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        self.floats.borrow().values().any(|tw| elapsed_fraction(tw.started, now) < 1.0)
            || self.colors.borrow().values().any(|tw| elapsed_fraction(tw.started, now) < 1.0)
            || self.entrances.borrow().values().any(|e| elapsed_fraction(e.started, now) < 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_key_returns_the_target_unanimated() {
        let anim = Animator::default();
        assert_eq!(anim.tween_f32("w", 260.0), 260.0);
    }

    #[test]
    fn a_settled_key_keeps_returning_its_target() {
        let anim = Animator::default();
        anim.tween_f32("w", 56.0);
        std::thread::sleep(DURATION + Duration::from_millis(10));
        assert_eq!(anim.tween_f32("w", 56.0), 56.0);
    }

    #[test]
    fn retargeting_mid_flight_starts_from_the_current_value_not_the_old_target() {
        let anim = Animator::default();
        anim.tween_f32("w", 260.0);
        std::thread::sleep(DURATION + Duration::from_millis(10));
        // Settled at 260. Now collapse, and let the tween run partway...
        anim.tween_f32("w", 56.0);
        std::thread::sleep(Duration::from_millis(100));
        let mid = anim.tween_f32("w", 56.0);
        assert!(mid < 260.0 && mid > 0.0, "moved partway toward the new target: {mid}");
        // ...and immediately reopen before it finishes. The animation must
        // continue from wherever it actually is, not restart at 260.
        let reopened = anim.tween_f32("w", 260.0);
        assert!(
            (reopened - mid).abs() < 40.0,
            "reopening should pick up near {mid}, not jump: got {reopened}"
        );
    }

    #[test]
    fn is_animating_reports_false_once_everything_has_settled() {
        let anim = Animator::default();
        anim.tween_f32("w", 260.0);
        assert!(!anim.is_animating(), "no prior value to move from, so nothing to animate");
        anim.tween_f32("w", 56.0);
        assert!(anim.is_animating());
        std::thread::sleep(DURATION + Duration::from_millis(10));
        anim.tween_f32("w", 56.0);
        assert!(!anim.is_animating());
    }

    #[test]
    fn a_colour_tween_never_leaves_the_valid_channel_range() {
        let anim = Animator::default();
        let dim = Rgba { r: 0.1, g: 0.1, b: 0.1, a: 1.0 };
        let bright = Rgba { r: 0.9, g: 0.9, b: 0.9, a: 1.0 };
        anim.tween_color("c", dim);
        for _ in 0..5 {
            let c = anim.tween_color("c", bright);
            for ch in [c.r, c.g, c.b, c.a] {
                assert!((0.0..=1.0).contains(&ch), "channel escaped 0..=1: {ch}");
            }
        }
    }

    #[test]
    fn entrance_progress_replays_from_zero_when_identity_changes() {
        let anim = Animator::default();
        assert_eq!(anim.entrance_progress("pane", 1), 1.0, "first render has nothing to animate from");
        let just_changed = anim.entrance_progress("pane", 2);
        assert!(just_changed < 0.1, "identity changed, so progress restarts near 0: {just_changed}");
        // Re-rendering the *same* screen (same identity) must not restart it.
        std::thread::sleep(Duration::from_millis(50));
        let still_same = anim.entrance_progress("pane", 2);
        assert!(still_same > just_changed, "progress keeps advancing for an unchanged identity");
    }
}
