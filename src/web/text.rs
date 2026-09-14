//! Shaping *and* rasterizing with cosmic-text.
//!
//! `ui/text.rs` shapes with cosmic-text but hands the glyph ids to Skia's own
//! atlas to rasterize. There is no Skia here (see ADR-0005), so this module
//! does the other half too, with cosmic-text's own `SwashCache` -- the
//! pure-Rust glyph atlas the native path deliberately avoids duplicating.
//! Coverage bitmaps come back exactly like `ui/svg.rs`'s icon masks: one byte
//! of coverage per pixel, tinted with whatever colour the caller wants.

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
};
use resvg::tiny_skia;

use crate::ui::layout::MeasureText;
use crate::ui::units::Bounds;

/// One glyph, positioned in whole pixels -- cosmic-text's own hinting already
/// snapped it there (see `LayoutGlyph::physical`).
struct PositionedGlyph {
    cache_key: CacheKey,
    x: i32,
    y: i32,
}

/// A shaped line, ready to rasterize. Unlike `ui::text::ShapedLine`, there is
/// no `TextBlob` to hold -- the cache key is enough to ask `SwashCache` for
/// pixels when `WebShaper::draw` actually paints it.
pub struct ShapedLine {
    glyphs: Vec<PositionedGlyph>,
    pub width: f32,
    pub baseline: f32,
}

/// Work Sans (SIL OFL — `assets/fonts/WorkSans-OFL.txt`), the one font this
/// build ships. Not a design choice, a load-bearing one: `FontSystem::new()`
/// finds `fontdb` a system font directory to scan on every other target this
/// crate builds for, but a browser sandbox has no such directory, so on
/// wasm32 it comes back with an empty database — and shaping *anything*
/// against an empty database panics deep in cosmic-text/rustybuzz rather
/// than, say, drawing tofu. Confirmed by running the demo before this font
/// was wired in: `docs/adr/0005-wasm-render-experiment.md`.
const REGULAR: &[u8] = include_bytes!("../../assets/fonts/WorkSans-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../assets/fonts/WorkSans-Bold.ttf");

/// Owns the font database and the glyph-bitmap cache.
pub struct WebShaper {
    fonts: FontSystem,
    cache: SwashCache,
}

impl WebShaper {
    pub fn new() -> Self {
        let mut fonts = FontSystem::new();
        let db = fonts.db_mut();
        db.load_font_data(REGULAR.to_vec());
        db.load_font_data(BOLD.to_vec());
        // `FontSystem::new()` points the generic `sans-serif` family (what
        // `Family::SansSerif` below resolves through) at "Open Sans" — a
        // sensible default when a system font store is there to have it,
        // moot here since nothing by that name is loaded. Repoint it at the
        // one family this database actually has.
        db.set_sans_serif_family("Work Sans");
        WebShaper {
            fonts,
            cache: SwashCache::new(),
        }
    }

    /// Shapes a single line. Mirrors `ui::text::Shaper::shape` minus the
    /// truncation/ellipsis path (`shape_truncated`) -- the demo screen has no
    /// text long enough to need it, so it was left for a follow-up rather
    /// than ported speculatively.
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
        let mut attrs = Attrs::new().family(Family::SansSerif);
        if bold {
            attrs = attrs.weight(Weight::BOLD);
        }
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut glyphs = Vec::new();
        let mut width = 0.0f32;
        let mut baseline = line_height;
        for run in buffer.layout_runs() {
            baseline = run.line_y;
            width = width.max(run.line_w);
            for g in run.glyphs {
                let physical = g.physical((0.0, 0.0), 1.0);
                glyphs.push(PositionedGlyph {
                    cache_key: physical.cache_key,
                    x: physical.x,
                    y: physical.y,
                });
            }
        }
        ShapedLine {
            glyphs,
            width,
            baseline,
        }
    }

    /// Draws a shaped line's glyphs into `pixmap`, baseline at `origin`,
    /// every covered pixel set to `color`. `clip`, when set, is the same
    /// inherited-clip rectangle `crate::web::paint` computes for boxes --
    /// glyphs outside it are skipped pixel by pixel rather than drawn and
    /// covered, since there is no clip-mask machinery in this module (see
    /// `crate::web::paint::clip_mask` for why boxes get one and text does
    /// not: a mask big enough for the canvas is one allocation per box, and
    /// text is drawn glyph by glyph already).
    pub fn draw(
        &mut self,
        pixmap: &mut tiny_skia::PixmapMut,
        origin: (f32, f32),
        line: &ShapedLine,
        color: [u8; 4],
        clip: Option<Bounds>,
    ) {
        let (ox, oy) = (origin.0.round() as i32, origin.1.round() as i32);
        let base = cosmic_text::Color::rgba(color[0], color[1], color[2], color[3]);
        for g in &line.glyphs {
            let (gx, gy) = (ox + g.x, oy + g.y);
            self.cache
                .with_pixels(&mut self.fonts, g.cache_key, base, |px, py, c| {
                    let (x, y) = (gx + px, gy + py);
                    if let Some(clip) = clip
                        && !clip.contains(x as f32, y as f32)
                    {
                        return;
                    }
                    super::paint::blend_over(pixmap, x, y, c.r(), c.g(), c.b(), c.a());
                });
        }
    }
}

impl Default for WebShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasureText for WebShaper {
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
