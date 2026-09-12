//! The two player sliders: seek and volume.
//!
//! Both work in normalised units, which is how `app.rs` created them — the seek
//! bar as a fraction of the duration, the volume as a gain factor.

use std::rc::Rc;

use crate::ui::element::{Element, IntoElement, SharedString, div, h_flex};
use crate::ui::style::Styled;
use crate::ui::theme::theme;
use crate::ui::units::{Bounds, px, relative};

/// The retained part of a slider: its range and where the thumb is.
#[derive(Clone, Debug)]
pub struct SliderState {
    min: f32,
    max: f32,
    step: f32,
    value: f32,
}

impl Default for SliderState {
    fn default() -> Self {
        SliderState { min: 0.0, max: 1.0, step: 0.01, value: 0.0 }
    }
}

impl SliderState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn min(mut self, min: f32) -> Self {
        self.min = min;
        self
    }

    pub fn max(mut self, max: f32) -> Self {
        self.max = max;
        self
    }

    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn default_value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn set_value(&mut self, value: f32) {
        self.value = value.clamp(self.min, self.max);
    }

    /// Where the thumb sits, `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// The value a click or drag at `x` within `track` selects, snapped to `step`.
    pub fn value_at(&self, track: &Bounds, x: f32) -> f32 {
        if track.width <= 0.0 {
            return self.value;
        }
        let fraction = ((x - track.x) / track.width).clamp(0.0, 1.0);
        let raw = self.min + fraction * (self.max - self.min);
        if self.step > 0.0 {
            let snapped = (raw / self.step).round() * self.step;
            snapped.clamp(self.min, self.max)
        } else {
            raw.clamp(self.min, self.max)
        }
    }
}

const TRACK_H: f32 = 4.0;
const THUMB: f32 = 12.0;

type Handler<S> = Rc<dyn Fn(&mut S, f32)>;

pub struct Slider<S> {
    id: SharedString,
    state: SliderState,
    disabled: bool,
    on_change: Option<Handler<S>>,
}

impl<S> Slider<S> {
    pub fn new(id: impl Into<SharedString>, state: &SliderState) -> Self {
        Slider { id: id.into(), state: state.clone(), disabled: false, on_change: None }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Fires with the value the pointer picked.
    ///
    /// The player deliberately acts on release rather than on every move: each
    /// seek re-spawns two ffmpeg processes, so doing it per mouse-move would
    /// thrash. The caller keeps that split; this only reports the value.
    pub fn on_change(mut self, handler: impl Fn(&mut S, f32) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl<S: 'static> IntoElement<S> for Slider<S> {
    fn into_element(self) -> Element<S> {
        let t = theme();
        let fraction = self.state.fraction();
        let filled = if self.disabled { t.muted_foreground } else { t.primary };

        let track = div()
            .id(SharedString::from(format!("{}-track", self.state.value)))
            .w_full()
            .h(px(TRACK_H))
            .rounded_full()
            .bg(t.muted)
            .child(div().h_full().rounded_full().bg(filled).w(relative(fraction)));

        let thumb = div()
            .w(px(THUMB))
            .h(px(THUMB))
            .flex_shrink_0()
            .rounded_full()
            .bg(if self.disabled { t.muted_foreground } else { t.primary });

        let mut el = h_flex()
            .id(self.id)
            .w_full()
            .items_center()
            .gap_2()
            .child(track)
            .child(thumb);
        if !self.disabled {
            el = el.cursor_pointer();
        }
        el.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seek bar is a fraction of the duration and the volume a gain factor,
    /// so both live in 0..1 and the thumb fraction is the value itself.
    #[test]
    fn a_normalised_slider_maps_value_straight_to_fraction() {
        let mut s = SliderState::new().min(0.0).max(1.0).step(0.001);
        s.set_value(0.25);
        assert_eq!(s.fraction(), 0.25);
    }

    /// Values are clamped to the range, so a seek past the end cannot ask the
    /// player for a position beyond the file.
    #[test]
    fn values_clamp_to_the_range() {
        let mut s = SliderState::new().min(0.0).max(1.0);
        s.set_value(5.0);
        assert_eq!(s.value(), 1.0);
        s.set_value(-1.0);
        assert_eq!(s.value(), 0.0);
    }

    /// A click maps the x offset across the track to a value, snapped to the
    /// step the state was built with.
    #[test]
    fn a_click_maps_across_the_track_and_snaps_to_the_step() {
        let s = SliderState::new().min(0.0).max(1.0).step(0.25);
        let track = Bounds { x: 100.0, y: 0.0, width: 200.0, height: 4.0 };
        assert_eq!(s.value_at(&track, 100.0), 0.0, "left edge");
        assert_eq!(s.value_at(&track, 300.0), 1.0, "right edge");
        assert_eq!(s.value_at(&track, 200.0), 0.5, "middle");
        // 0.3 of the way snaps to the nearest quarter.
        assert_eq!(s.value_at(&track, 160.0), 0.25);
        // Outside the track clamps rather than extrapolating.
        assert_eq!(s.value_at(&track, 0.0), 0.0);
        assert_eq!(s.value_at(&track, 9999.0), 1.0);
    }
}
