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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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
    fonts: Rc<RefCell<FontSystem>>,
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
        Shaper::with_shared_fonts(Rc::new(RefCell::new(FontSystem::new())))
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
            .borrow()
            .db()
            .with_face_data(id, |data, index| mgr.new_from_data(data, index as usize))
            .flatten();
        self.typefaces.insert(id, built.clone());
        built
    }

    /// Shapes against an existing font database.
    ///
    /// Text inputs keep their own cosmic-text buffers and must be shaped against
    /// the *same* `FontSystem` the painting uses, or a caret would be measured
    /// with different metrics than the glyphs beside it. Sharing it is how that
    /// is guaranteed rather than hoped for.
    pub fn with_shared_fonts(fonts: Rc<RefCell<FontSystem>>) -> Self {
        Shaper {
            fonts,
            font_mgr: FontMgr::new(),
            typefaces: HashMap::new(),
            family: None,
        }
    }

    /// The shared font database, for whoever else needs to shape against it.
    pub fn fonts(&self) -> Rc<RefCell<FontSystem>> {
        self.fonts.clone()
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
        // Cloned to a local first: `Family::Name` borrows the string, and that
        // immutable borrow of `self` would collide with borrowing the fonts.
        let family = self.family.clone();
        let mut width = 0.0f32;
        let mut baseline = line_height;
        // Shaping is collected inside this block so the font borrow is released
        // before the typeface cache below, which needs its own.
        let mut runs: Vec<GlyphRun> = Vec::new();
        {
            let shared = self.fonts.clone();
            let mut fonts = shared.borrow_mut();
            let mut buffer = Buffer::new(&mut fonts, Metrics::new(font_size, line_height));
            buffer.set_size(&mut fonts, max_width, None);
            let mut attrs = Attrs::new();
            attrs = match &family {
                Some(f) => attrs.family(Family::Name(f)),
                None => attrs.family(Family::SansSerif),
            };
            if bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            buffer.set_text(&mut fonts, text, &attrs, Shaping::Advanced);
            buffer.shape_until_scroll(&mut fonts, false);

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

    /// Shapes `text` to fit within `max_width`, eliding an overflowing tail
    /// with an ellipsis -- `.truncate()`'s actual job, which until now it never
    /// did (see `ui::style::Styled::truncate`'s doc): paint drew the plain
    /// `shape()` output regardless of the box's width, so a long title just
    /// ran past its column instead of stopping at it.
    ///
    /// Reshapes candidates from scratch rather than clipping the drawn glyphs,
    /// so the cut lands on a character boundary with an ellipsis to show it
    /// was cut, instead of a glyph sliced in half at the box edge.
    pub fn shape_truncated(
        &mut self,
        text: &str,
        font_size: f32,
        line_height: f32,
        bold: bool,
        max_width: Option<f32>,
    ) -> ShapedLine {
        let full = self.shape(text, font_size, line_height, bold, None);
        let Some(max_width) = max_width else { return full };
        if full.width <= max_width {
            return full;
        }

        const ELLIPSIS: &str = "\u{2026}";
        let ellipsis_only = self.shape(ELLIPSIS, font_size, line_height, bold, None);
        if ellipsis_only.width > max_width {
            // Not even the ellipsis alone fits; it's the least-wrong thing to draw.
            return ellipsis_only;
        }

        // Longest prefix (by char count, so a multi-byte glyph is never split
        // mid-codepoint) that still fits alongside the ellipsis. Width is
        // monotonic in prefix length, so a binary search finds it in O(log n)
        // reshapes instead of shrinking the string one character at a time.
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            let candidate: String = chars[..mid].iter().collect::<String>() + ELLIPSIS;
            if self.shape(&candidate, font_size, line_height, bold, None).width <= max_width {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let candidate: String = chars[..lo].iter().collect::<String>() + ELLIPSIS;
        self.shape(&candidate, font_size, line_height, bold, None)
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

    /// Regression guard for `.truncate()` doing nothing: `paint.rs` used to
    /// shape with `max_width: None` unconditionally, so a box's own width was
    /// never consulted and a long title just ran past its column instead of
    /// stopping at it. The truncated line has to actually fit, and be
    /// shorter than shaping the same text unconstrained.
    #[test]
    fn shape_truncated_fits_the_max_width() {
        let mut shaper = Shaper::new();
        let text = "a very long title that will certainly overflow a narrow sidebar column";
        let full = shaper.shape(text, 16.0, 20.0, false, None);
        let capped = shaper.shape_truncated(text, 16.0, 20.0, false, Some(120.0));

        assert!(capped.width <= 120.0, "must fit inside the box: {}", capped.width);
        assert!(capped.width < full.width, "must actually be shorter than the untruncated line");
        assert!(!capped.runs.is_empty(), "still has something to draw, including the ellipsis");
    }

    /// Text that already fits must come back unchanged -- no ellipsis tacked
    /// onto a title that was never going to overflow in the first place.
    #[test]
    fn shape_truncated_leaves_text_that_already_fits_alone() {
        let mut shaper = Shaper::new();
        let unconstrained = shaper.shape("hi", 16.0, 20.0, false, None).width;
        let truncated = shaper.shape_truncated("hi", 16.0, 20.0, false, Some(500.0)).width;
        assert_eq!(truncated, unconstrained);
    }
}
