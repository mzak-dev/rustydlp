//! Colour, with the one operation `app.rs` leans on: `.opacity(f)`.

/// Straight (non-premultiplied) RGBA, each channel `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// `0xRRGGBB`, fully opaque.
pub const fn rgb(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

pub const fn black() -> Rgba {
    rgb(0x000000)
}

pub const fn transparent() -> Rgba {
    Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
}

impl Rgba {
    /// Scales alpha, matching gpui's `Hsla::opacity` — which multiplies rather
    /// than replaces, so `.opacity(0.5)` on an already-translucent colour
    /// halves it again.
    pub fn opacity(mut self, factor: f32) -> Self {
        self.a *= factor.clamp(0.0, 1.0);
        self
    }

    pub fn is_transparent(&self) -> bool {
        self.a <= f32::EPSILON
    }
}
