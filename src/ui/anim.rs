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
use super::units::Bounds;

/// How long every tween takes to settle. One knob, so every animated surface
/// in the app moves at the same pace.
pub const DURATION: Duration = Duration::from_millis(300);

/// How long a tween that is *following live data* takes to catch up.
///
/// Much shorter than `DURATION`, and paired with a curve that does not
/// overshoot (see [`Animator::follow_f32`]): a download's progress bar is not
/// a surface settling into place after a gesture, it is a readout of a number
/// that keeps changing. Given the full 300ms of a bounce it never arrives
/// before the next value retargets it, so it sits permanently behind the
/// truth, wobbling — which is what a stuttering progress bar actually is.
pub const FOLLOW: Duration = Duration::from_millis(120);

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

/// Decelerating, no overshoot. The bounce above is the app's language for a
/// *widget* settling into place, but a whole screen carrying every word on it
/// past its resting point and back reads as a wobble rather than as motion, so
/// page swaps (and the overlays that behave like one) ease with this instead.
fn ease_out_cubic(t: f32) -> f32 {
    let t = 1.0 - t;
    1.0 - t * t * t
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

fn elapsed_fraction_over(started: Instant, now: Instant, duration: Duration) -> f32 {
    (now.duration_since(started).as_secs_f32() / duration.as_secs_f32()).min(1.0)
}

fn elapsed_fraction(started: Instant, now: Instant) -> f32 {
    elapsed_fraction_over(started, now, DURATION)
}

/// A `started` timestamp that already reads as settled (`elapsed_fraction`
/// returns `1.0` for it immediately), for recording a transition that has
/// nothing to actually move from.
fn settled_start(now: Instant) -> Instant {
    now.checked_sub(DURATION).unwrap_or(now)
}

/// How far, along the navigation axis, the pair of screens in a page swap
/// travels. Deliberately short: a full-width slide at this duration reads as a
/// lurch, while nothing at all is what made the old swap feel like two
/// unrelated screens rather than one movement between them.
pub const SWAP_DISTANCE: f32 = 36.0;

/// The fades are deliberately much shorter than the slide: the outgoing screen
/// is gone by `SWAP_FADE_OUT` and the incoming one is solid by
/// `SWAP_FADE_IN_END`, leaving the last third of the transition as a
/// fully-opaque screen still gliding into place. Stretching either fade across
/// the whole duration instead leaves a long stretch where both screens are
/// nearly transparent -- the window looking empty mid-navigation, which is
/// worse than the brief crossover these numbers trade it for.
const SWAP_FADE_OUT: f32 = 0.3;
const SWAP_FADE_IN_START: f32 = 0.25;
const SWAP_FADE_IN_END: f32 = 0.65;

/// The offsets and opacities of both screens in a page swap, at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSwap {
    pub outgoing_offset: f32,
    pub outgoing_opacity: f32,
    pub incoming_offset: f32,
    pub incoming_opacity: f32,
}

/// A shared-axis swap: both screens slide the same way at the same time, while
/// the outgoing one fades out early and the incoming one fades in late.
///
/// Sliding *together* is what makes it read as one surface moving under a
/// window rather than as one screen leaving and another arriving; staggering
/// the fades is what keeps the two sets of text from ghosting through each
/// other in the middle.
///
/// `progress` is `0.0..=1.0` from [`Animator::transition`]. `direction` is
/// `1.0` when navigating forward (the new screen comes in from the right) and
/// `-1.0` when going back.
pub fn page_swap(progress: f32, direction: f32, distance: f32) -> PageSwap {
    let t = progress.clamp(0.0, 1.0);
    let eased = ease_out_cubic(t);
    PageSwap {
        outgoing_offset: -direction * distance * eased,
        outgoing_opacity: 1.0 - (t / SWAP_FADE_OUT).min(1.0),
        incoming_offset: direction * distance * (1.0 - eased),
        incoming_opacity: ((t - SWAP_FADE_IN_START) / (SWAP_FADE_IN_END - SWAP_FADE_IN_START))
            .clamp(0.0, 1.0),
    }
}

/// Eases a rectangle from `from` to `to`, for a view that grows out of the
/// thing that opened it rather than appearing over it.
///
/// This renderer has no transform, let alone a scale — so this is not a
/// scaled-up snapshot of the origin, the way a compositor would do it. It is
/// the real box, laid out at every intermediate size: the card is genuinely
/// `rect` wide on each frame, and whatever is inside it is laid out (and
/// clipped) to that. Which is why the content inside it fades in late, once
/// the frame is most of the way to full size.
pub fn morph_rect(from: Bounds, to: Bounds, progress: f32) -> Bounds {
    let t = ease_out_cubic(progress.clamp(0.0, 1.0));
    Bounds {
        x: lerp(from.x, to.x, t),
        y: lerp(from.y, to.y, t),
        width: lerp(from.width, to.width, t),
        height: lerp(from.height, to.height, t),
    }
}

/// Which curve a tween eases with. Widgets settling into place bounce; a
/// readout following live data must not, or it reads as noise in the data.
#[derive(Clone, Copy, PartialEq)]
enum Curve {
    Back,
    Out,
}

impl Curve {
    fn apply(self, t: f32) -> f32 {
        match self {
            Curve::Back => ease_out_back(t),
            Curve::Out => ease_out_cubic(t),
        }
    }
}

struct Tween<T> {
    from: T,
    to: T,
    started: Instant,
    duration: Duration,
    curve: Curve,
}

/// A looping indicator's clock. Unlike every other entry here it never
/// settles, so it is kept alive by being *asked for*: `last_seen` is bumped on
/// every poll, and `is_animating` only counts cycles polled within
/// `CYCLE_IDLE`. Stop drawing the indicator and the redraw loop goes back to
/// sleep on its own, with no teardown call to forget.
struct Cycle {
    started: Instant,
    last_seen: Instant,
}

/// How long after its last poll a cycle stops holding the loop awake — two
/// frames at 60Hz, rounded up.
const CYCLE_IDLE: Duration = Duration::from_millis(50);

struct Entrance {
    identity: u64,
    /// What was showing before `identity` took over, for as long as the swap
    /// is still playing. `None` once it has settled -- which is what tells
    /// `transition`'s caller it can stop building the outgoing screen.
    previous: Option<u64>,
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
    cycles: RefCell<HashMap<String, Cycle>>,
}

impl Animator {
    /// Eases toward `target`, keyed by `key`. Returns `target` itself the
    /// first time this key is seen or once the transition has settled;
    /// otherwise a possibly-overshooting in-between value.
    pub fn tween_f32(&self, key: impl Into<String>, target: f32) -> f32 {
        self.tween_f32_over(key, target, DURATION, Curve::Back)
    }

    /// Eases toward `target` fast and without overshoot, for a value that is
    /// *following something outside the interface* rather than settling after
    /// an interaction — a download's progress, a conversion's. See [`FOLLOW`].
    pub fn follow_f32(&self, key: impl Into<String>, target: f32) -> f32 {
        self.tween_f32_over(key, target, FOLLOW, Curve::Out)
    }

    fn tween_f32_over(
        &self,
        key: impl Into<String>,
        target: f32,
        duration: Duration,
        curve: Curve,
    ) -> f32 {
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
                    let t = elapsed_fraction_over(tw.started, now, tw.duration);
                    tw.from = lerp(tw.from, tw.to, tw.curve.apply(t));
                    tw.to = target;
                    tw.started = now;
                    tw.duration = duration;
                    tw.curve = curve;
                }
                let t = elapsed_fraction_over(tw.started, now, tw.duration);
                lerp(tw.from, tw.to, tw.curve.apply(t))
            }
            None => {
                // Backdated rather than `now`: the very first time a key is
                // seen there is no prior value to move from, so this starts
                // already settled instead of reading as "still animating"
                // for one spurious `DURATION`.
                table.insert(
                    key,
                    Tween {
                        from: target,
                        to: target,
                        started: settled_start(now),
                        duration,
                        curve,
                    },
                );
                target
            }
        }
    }

    /// Same as [`Self::tween_f32`], for a background or text colour.
    pub fn tween_color(&self, key: impl Into<String>, target: Rgba) -> Rgba {
        self.tween_color_over(key, target, DURATION, Curve::Back)
    }

    /// A colour that has to keep up with the pointer rather than settle after
    /// a decision: hover highlights, which at the widget's 300ms trail far
    /// enough behind the cursor to feel broken.
    pub fn follow_color(&self, key: impl Into<String>, target: Rgba) -> Rgba {
        self.tween_color_over(key, target, FOLLOW, Curve::Out)
    }

    fn tween_color_over(
        &self,
        key: impl Into<String>,
        target: Rgba,
        duration: Duration,
        curve: Curve,
    ) -> Rgba {
        let key = key.into();
        let mut table = self.colors.borrow_mut();
        let now = Instant::now();
        match table.get_mut(&key) {
            Some(tw) => {
                if tw.to != target {
                    let t = elapsed_fraction_over(tw.started, now, tw.duration);
                    tw.from = lerp_rgba(tw.from, tw.to, tw.curve.apply(t));
                    tw.to = target;
                    tw.started = now;
                    tw.duration = duration;
                    tw.curve = curve;
                }
                let t = elapsed_fraction_over(tw.started, now, tw.duration);
                lerp_rgba(tw.from, tw.to, tw.curve.apply(t))
            }
            None => {
                table.insert(
                    key,
                    Tween { from: target, to: target, started: settled_start(now), duration, curve },
                );
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
        self.transition(key, identity).1
    }

    /// [`Self::entrance_progress`] plus *what it is transitioning from*: the
    /// identity that was showing before this one, for as long as the swap is
    /// still playing, and `None` once it has settled.
    ///
    /// That second half is what makes a swap read as one movement rather than
    /// two unrelated ones. With only a progress number the caller can fade the
    /// arriving screen in, but the one it replaced is already gone by the first
    /// frame, so the transition starts from an empty window; knowing which
    /// screen to keep building lets both be on screen at once and cross over.
    pub fn transition(&self, key: impl Into<String>, identity: u64) -> (Option<u64>, f32) {
        let key = key.into();
        let mut table = self.entrances.borrow_mut();
        let now = Instant::now();
        match table.get_mut(&key) {
            Some(e) => {
                if e.identity != identity {
                    // Whatever was showing becomes the outgoing screen --
                    // including mid-swap, so a second navigation before the
                    // first settles crosses over from what is actually on
                    // screen rather than from the screen before that.
                    e.previous = Some(e.identity);
                    e.identity = identity;
                    e.started = now;
                }
                let t = elapsed_fraction(e.started, now);
                if t >= 1.0 {
                    e.previous = None;
                }
                (e.previous, t)
            }
            None => {
                table.insert(
                    key,
                    Entrance { identity, previous: None, started: settled_start(now) },
                );
                // The very first render of a screen shouldn't play an
                // entrance from nothing -- there is no "before" to animate
                // from -- so this starts already settled.
                (None, 1.0)
            }
        }
    }

    /// A phase that runs `0.0 -> 1.0` over `period` and starts again, for an
    /// indeterminate indicator: something is happening, but nothing knows how
    /// far along it is. Every other animation here has a destination; this one
    /// is the admission that this one doesn't.
    ///
    /// Polling is what keeps it running (see [`Cycle`]) — call it while the
    /// indicator is on screen and stop when it isn't.
    pub fn cycle(&self, key: impl Into<String>, period: Duration) -> f32 {
        let key = key.into();
        let now = Instant::now();
        let mut table = self.cycles.borrow_mut();
        let cycle = table.entry(key).or_insert(Cycle { started: now, last_seen: now });
        cycle.last_seen = now;
        let elapsed = now.duration_since(cycle.started).as_secs_f32();
        let period = period.as_secs_f32().max(f32::EPSILON);
        (elapsed % period) / period
    }

    /// The back-out curve `tween_f32`/`tween_color` ease with, exposed so
    /// `entrance_progress`'s raw fraction can drive a position with the same
    /// motion language.
    pub fn ease(t: f32) -> f32 {
        ease_out_back(t)
    }

    /// The page-swap curve: decelerating, no overshoot. See
    /// [`ease_out_cubic`].
    pub fn ease_out(t: f32) -> f32 {
        ease_out_cubic(t)
    }

    /// Whether anything is still short of `DURATION` since it last changed —
    /// the event loop uses this to decide whether to keep waking up and
    /// redrawing on a timer, the same way it does while a video plays.
    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        let running = |tw: &Tween<f32>| elapsed_fraction_over(tw.started, now, tw.duration) < 1.0;
        self.floats.borrow().values().any(running)
            || self
                .colors
                .borrow()
                .values()
                .any(|tw| elapsed_fraction_over(tw.started, now, tw.duration) < 1.0)
            || self.entrances.borrow().values().any(|e| elapsed_fraction(e.started, now) < 1.0)
            // An indeterminate indicator never settles, so it keeps the loop
            // awake for exactly as long as something is still drawing it.
            || self.cycles.borrow().values().any(|c| now.duration_since(c.last_seen) < CYCLE_IDLE)
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

    /// A progress bar follows a number that keeps changing. On the widget
    /// bounce it never reached one value before the next arrived, so it sat
    /// permanently behind the truth and jittered where the overshoot met the
    /// next retarget. Following has to be quick and monotonic instead.
    #[test]
    fn a_follow_tween_catches_up_quickly_and_never_overshoots() {
        let anim = Animator::default();
        anim.follow_f32("pct", 0.0);
        std::thread::sleep(FOLLOW + Duration::from_millis(10));

        // Retargeted repeatedly, the way live progress arrives.
        let mut last = 0.0;
        for step in 1..=4 {
            let target = step as f32 * 0.2;
            let v = anim.follow_f32("pct", target);
            assert!(v <= target + f32::EPSILON, "overshot {target}: {v}");
            assert!(v >= last, "went backwards: {last} then {v}");
            last = v;
            std::thread::sleep(Duration::from_millis(40));
        }

        // And it actually arrives, well inside the 300ms a widget takes.
        std::thread::sleep(FOLLOW + Duration::from_millis(10));
        assert_eq!(anim.follow_f32("pct", 0.8), 0.8);
        assert!(!anim.is_animating(), "settled, so the redraw loop can sleep");
    }

    /// The indeterminate bar's clock: it loops forever, but only holds the
    /// redraw loop awake while something is still asking it for a phase.
    #[test]
    fn a_cycle_wraps_and_only_runs_while_it_is_polled() {
        let anim = Animator::default();
        let period = Duration::from_millis(120);

        let first = anim.cycle("bar", period);
        assert!(first < 0.2, "starts near the beginning of its period: {first}");
        assert!(anim.is_animating(), "a polled cycle keeps the loop redrawing");

        std::thread::sleep(Duration::from_millis(60));
        let mid = anim.cycle("bar", period);
        assert!(mid > first, "advances: {first} then {mid}");

        // Past the end of a period it wraps rather than clamping.
        std::thread::sleep(period);
        let wrapped = anim.cycle("bar", period);
        assert!((0.0..=1.0).contains(&wrapped), "stays in 0..=1: {wrapped}");

        // Stop drawing the indicator -- stop polling -- and the loop is free
        // to go back to sleep without anything having to be torn down.
        std::thread::sleep(CYCLE_IDLE + Duration::from_millis(10));
        assert!(!anim.is_animating(), "an unpolled cycle holds nothing awake");
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
    fn a_swap_keeps_the_outgoing_identity_until_it_settles() {
        let anim = Animator::default();
        assert_eq!(anim.transition("pane", 1), (None, 1.0), "nothing to come from yet");

        let (from, t) = anim.transition("pane", 2);
        assert_eq!(from, Some(1), "the screen being replaced stays available to render");
        assert!(t < 0.1, "and the swap starts at the beginning: {t}");

        // A second navigation before the first settles crosses over from
        // what is actually on screen, not from the screen before that.
        let (from, _) = anim.transition("pane", 3);
        assert_eq!(from, Some(2));

        std::thread::sleep(DURATION + Duration::from_millis(10));
        assert_eq!(anim.transition("pane", 3), (None, 1.0), "settled: nothing left to cross from");
    }

    /// The two screens have to move together and overlap, or the swap reads as
    /// one screen leaving and an unrelated one arriving -- which is the whole
    /// complaint this motion exists to answer.
    #[test]
    fn a_page_swap_slides_both_screens_the_same_way_and_overlaps_their_fades() {
        let start = page_swap(0.0, 1.0, SWAP_DISTANCE);
        assert_eq!(start.outgoing_offset, 0.0, "the outgoing screen starts where it sat");
        assert_eq!(start.outgoing_opacity, 1.0);
        assert_eq!(start.incoming_offset, SWAP_DISTANCE, "the incoming one starts off to the right");
        assert_eq!(start.incoming_opacity, 0.0);

        let end = page_swap(1.0, 1.0, SWAP_DISTANCE);
        assert_eq!(end.incoming_offset, 0.0, "and lands exactly in place");
        assert_eq!(end.incoming_opacity, 1.0);
        assert_eq!(end.outgoing_opacity, 0.0, "with the old one gone");

        let mid = page_swap(0.5, 1.0, SWAP_DISTANCE);
        assert!(mid.outgoing_offset < 0.0 && mid.incoming_offset > 0.0, "same direction of travel");
        assert!(
            mid.outgoing_opacity < mid.incoming_opacity,
            "the fades are staggered, so the arriving screen is the one being read"
        );

        // Going back mirrors it, so the motion says which way you moved.
        let back = page_swap(0.25, -1.0, SWAP_DISTANCE);
        let forward = page_swap(0.25, 1.0, SWAP_DISTANCE);
        assert_eq!(back.incoming_offset, -forward.incoming_offset);
        assert_eq!(back.outgoing_offset, -forward.outgoing_offset);
    }

    /// A morph has to start exactly on the rectangle it was handed and land
    /// exactly on its target: a view that grows out of a tile but starts a few
    /// pixels off it, or settles a few pixels short, is a view that jumps
    /// twice instead of moving once.
    #[test]
    fn a_morph_starts_on_the_origin_rect_and_lands_on_the_target() {
        let tile = Bounds { x: 20.0, y: 100.0, width: 220.0, height: 180.0 };
        let card = Bounds { x: 165.0, y: 76.0, width: 850.0, height: 608.0 };

        assert_eq!(morph_rect(tile, card, 0.0), tile);
        assert_eq!(morph_rect(tile, card, 1.0), card);

        let mid = morph_rect(tile, card, 0.5);
        assert!(mid.width > tile.width && mid.width < card.width, "grows: {mid:?}");
        assert!(mid.x > tile.x && mid.x < card.x, "and travels: {mid:?}");

        // Clamped, so a progress that has run past the end cannot carry the
        // card past where it is supposed to stop.
        assert_eq!(morph_rect(tile, card, 1.4), card);
    }

    /// Whatever the curve, a screen may not overshoot its resting place: the
    /// bounce the widgets use would carry a whole page of text past the edge
    /// it is sliding toward and back.
    #[test]
    fn the_page_curve_never_overshoots() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let eased = Animator::ease_out(t);
            assert!((0.0..=1.0).contains(&eased), "escaped 0..=1 at {t}: {eased}");
        }
        assert_eq!(Animator::ease_out(0.0), 0.0);
        assert_eq!(Animator::ease_out(1.0), 1.0);
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
