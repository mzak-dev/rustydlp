//! Lengths, mirroring the subset of gpui's unit types `app.rs` actually uses.

use std::ops::{Add, Mul, Sub};

/// A device-independent pixel. Multiplied by the window's scale factor only at
/// paint time, so every layout number in the app stays in logical units.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Pixels(pub f32);

pub const fn px(v: f32) -> Pixels {
    Pixels(v)
}

impl Pixels {
    pub fn max(self, other: Self) -> Self {
        Pixels(self.0.max(other.0))
    }
}

impl Mul<f32> for Pixels {
    type Output = Pixels;
    fn mul(self, rhs: f32) -> Pixels {
        Pixels(self.0 * rhs)
    }
}

impl Add for Pixels {
    type Output = Pixels;
    fn add(self, rhs: Pixels) -> Pixels {
        Pixels(self.0 + rhs.0)
    }
}

impl Sub for Pixels {
    type Output = Pixels;
    fn sub(self, rhs: Pixels) -> Pixels {
        Pixels(self.0 - rhs.0)
    }
}

/// A length that may be absolute, a fraction of the parent, or left to the
/// layout engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f32),
    /// `0.0..=1.0` of the parent's corresponding axis.
    Fraction(f32),
    Auto,
}

/// A fraction of the parent, as gpui spells it.
pub const fn relative(f: f32) -> Length {
    Length::Fraction(f)
}

impl From<Pixels> for Length {
    fn from(p: Pixels) -> Length {
        Length::Px(p.0)
    }
}

impl From<f32> for Length {
    fn from(v: f32) -> Length {
        Length::Px(v)
    }
}

impl Length {
    pub(crate) fn to_taffy(self) -> taffy::style::Dimension {
        match self {
            Length::Px(v) => taffy::style::Dimension::length(v),
            Length::Fraction(f) => taffy::style::Dimension::percent(f),
            Length::Auto => taffy::style::Dimension::auto(),
        }
    }
}

/// An axis-aligned rectangle in logical pixels, as produced by layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Bounds {
    /// The overlapping region, or `None` when they do not overlap at all.
    pub fn intersect(&self, other: &Bounds) -> Option<Bounds> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Bounds {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}
