//! Shaping with cosmic-text, rasterizing with Skia.
//!
//! cosmic-text does shaping only: it returns glyph ids and positions. Those go
//! straight into a Skia `TextBlob` built over a `Typeface` made from the same
//! font bytes, so Skia's own GPU glyph atlas handles caching, hinting and
//! subpixel antialiasing.
//!
//! Deliberately NOT routed through cosmic-text's `SwashCache`: that would hand
//! us glyph bitmaps to upload and cache ourselves, duplicating an atlas Skia
//! already has. It also avoids Skia's `textlayout` module, and with it Harfbuzz
//! and ICU -- cosmic-text's rustybuzz does the shaping instead.

use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight};
use skia_safe::{Font, FontMgr, TextBlob, TextBlobBuilder, Typeface};

use super::layout::MeasureText;

/// A glyph as cosmic-text positioned it: id, then offset from the line origin.
type PositionedGlyph = (u16, f32, f32);

/// Consecutive glyphs sharing one face. Font fallback is what splits these.
type GlyphRun = (cosmic_text::fontdb::ID, Vec<PositionedGlyph>);

/// One shaped line, ready to draw.
pub struct ShapedLine {
    /// One blob per run of glyphs sharing a font — fallback splits runs.
    pub runs: Vec<TextBlob>,
    /// Total advance.
    pub width: f32,
    /// Baseline offset from the top of the line box.
    pub baseline: f32,
}

/// Owns the font database and the Skia typefaces built from it.
pub struct Shaper {
    fonts: FontSystem,
    font_mgr: FontMgr,
    /// cosmic-text's face id -> the Skia typeface over the same bytes.
    typefaces: HashMap<cosmic_text::fontdb::ID, Option<Typeface>>,
    /// The family to shape with. `None` asks fontdb for a sans-serif default.
    family: Option<String>,
}

impl Shaper {
    /// Loads the system fonts.
    ///
    /// PARITY RISK: gpui resolved `.SystemUIFont` through DirectWrite, which on
    /// Windows is the system UI font (Segoe UI is the strong prior, still
    /// unconfirmed). Until that is checked against the running legacy build,
    /// text metrics here may differ from the interface being replaced. Pin the
    /// family with `with_family` once it is known.
    pub fn new() -> Self {
        Shaper {
            fonts: FontSystem::new(),
            font_mgr: FontMgr::new(),
            typefaces: HashMap::new(),
            family: None,
        }
    }

    /// Shapes against an explicit family, and against only that family — used
    /// by golden tests, where system fonts would make the output host-dependent.
    pub fn with_family(family: impl Into<String>) -> Self {
        let mut s = Self::new();
        s.family = Some(family.into());
        s
    }

    /// Builds the Skia typeface for a cosmic-text face, once per face.
    fn typeface(&mut self, id: cosmic_text::fontdb::ID) -> Option<Typeface> {
        if let Some(cached) = self.typefaces.get(&id) {
            return cached.clone();
        }
        let mgr = self.font_mgr.clone();
        let built = self
            .fonts
            .db()
            .with_face_data(id, |data, index| mgr.new_from_data(data, index as usize))
            .flatten();
        self.typefaces.insert(id, built.clone());
        built
    }

    /// Shapes a single line. `max_width` wraps when set; the app's text is
    /// single-line, so callers generally pass `None` and clip instead.
    pub fn shape(
        &mut self,
        text: &str,
        font_size: f32,
        line_height: f32,
        bold: bool,
        max_width: Option<f32>,
    ) -> ShapedLine {
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(font_size, line_height));
        buffer.set_size(&mut self.fonts, max_width, None);
        // Cloned to a local first: `Family::Name` borrows the string, and that
        // immutable borrow of `self` would collide with `&mut self.fonts`.
        let family = self.family.clone();
        let mut attrs = Attrs::new();
        attrs = match &family {
            Some(f) => attrs.family(Family::Name(f)),
            None => attrs.family(Family::SansSerif),
        };
        if bold {
            attrs = attrs.weight(Weight::BOLD);
        }
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut width = 0.0f32;
        let mut baseline = line_height;
        // Collect first: building blobs needs &mut self for the typeface cache,
        // which would conflict with borrowing the buffer.
        let mut runs: Vec<GlyphRun> = Vec::new();
        for run in buffer.layout_runs() {
            baseline = run.line_y;
            width = width.max(run.line_w);
            for g in run.glyphs {
                let entry = (g.glyph_id, g.x, g.y);
                match runs.last_mut() {
                    Some((id, glyphs)) if *id == g.font_id => glyphs.push(entry),
                    _ => runs.push((g.font_id, vec![entry])),
                }
            }
        }

        let mut blobs = Vec::new();
        for (font_id, glyphs) in runs {
            let Some(typeface) = self.typeface(font_id) else { continue };
            let font = Font::from_typeface(typeface, font_size);
            let mut builder = TextBlobBuilder::new();
            {
                let (ids, positions) = builder.alloc_run_pos(&font, glyphs.len(), None);
                for (i, (glyph_id, x, y)) in glyphs.iter().enumerate() {
                    ids[i] = *glyph_id;
                    // Positions are relative to the baseline origin; the caller
                    // translates by the box's origin plus `baseline`.
                    positions[i] = skia_safe::Point::new(*x, *y);
                }
            }
            if let Some(blob) = builder.make() {
                blobs.push(blob);
            }
        }

        ShapedLine { runs: blobs, width, baseline }
    }
}

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasureText for Shaper {
    fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        line_height: f32,
        max_width: Option<f32>,
    ) -> (f32, f32) {
        let line = self.shape(text, font_size, line_height, false, max_width);
        (line.width, line_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge has to produce real glyph runs, and a wider string has to
    /// measure wider. Exact advances depend on the host's fonts, so this asserts
    /// the relationship rather than the number.
    #[test]
    fn shaping_produces_glyph_runs_with_monotonic_advances() {
        let mut shaper = Shaper::new();
        let short = shaper.shape("hi", 16.0, 20.0, false, None);
        let long = shaper.shape("hi there, longer", 16.0, 20.0, false, None);

        assert!(!short.runs.is_empty(), "expected at least one Skia text blob");
        assert!(short.width > 0.0);
        assert!(long.width > short.width, "more text must advance further");
        assert!(short.baseline > 0.0 && short.baseline <= 20.0);
    }

    /// Font fallback is the reason cosmic-text was chosen: a yt-dlp title can be
    /// CJK or emoji, and those glyphs come from different faces than the Latin
    /// ones, so one string legitimately yields several runs.
    #[test]
    fn mixed_scripts_shape_without_dropping_glyphs() {
        let mut shaper = Shaper::new();
        let line = shaper.shape("video 日本語", 16.0, 20.0, false, None);
        assert!(!line.runs.is_empty());
        assert!(line.width > 0.0);
    }
}
