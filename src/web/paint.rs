//! Draws laid-out boxes onto a tiny-skia pixmap.
//!
//! The wasm32 counterpart to `ui/paint.rs`. Same box list, same paint order,
//! same background/border/text/content sequence -- deliberately kept
//! line-for-line comparable to the native version so the two are easy to
//! diff, the same reason `app.rs` stayed parallel to the gpui build it
//! replaced. The two real differences:
//!
//! - No Skia canvas to save/clip/restore, so clipping is a `tiny_skia::Mask`
//!   built fresh per clipped box (see `clip_mask`) rather than a stack.
//! - No `Content::Image`: the demo screen never shows a decoded video frame
//!   or a file thumbnail, and `core/` -- the only thing that ever produces
//!   one -- does not exist in this build. See ADR-0005.

use resvg::tiny_skia;
use tiny_skia::{Color, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapMut, Rect, Transform};

use crate::ui::color::Rgba;
use crate::ui::element::Content;
use crate::ui::layout::{Box_, inherited_clip};
use crate::ui::style::StyleRefinement;
use crate::ui::svg::SvgRenderer;
use crate::ui::units::Bounds;

use super::text::WebShaper;

fn to_rect(b: &Bounds) -> Option<Rect> {
    Rect::from_xywh(b.x, b.y, b.width, b.height)
}

fn to_color(c: Rgba) -> Color {
    // `Color::from_rgba` clamps rather than failing, which is all a style
    // value derived from arithmetic (`.opacity(a)`) needs.
    Color::from_rgba(c.r, c.g, c.b, c.a).unwrap_or(Color::TRANSPARENT)
}

fn solid_paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(to_color(color));
    paint.anti_alias = true;
    paint
}

/// `rounded_full()` means "pill": half the shorter side, only knowable once
/// layout has produced a size. Mirrors `ui::paint::radius_of`.
fn radius_of(style: &StyleRefinement, b: &Bounds) -> f32 {
    if style.corner_pill == Some(true) {
        b.width.min(b.height) / 2.0
    } else {
        style.corner_radius.unwrap_or(0.0)
    }
}

/// A rounded rectangle as a path, corners approximated with cubic BĂ©ziers
/// (the standard ~0.552 "kappa" constant) -- tiny-skia has no rounded-rect
/// primitive of its own, unlike Skia's `RRect`. Indistinguishable from a true
/// arc at UI corner radii; `None` only when `b` itself is degenerate.
fn rounded_rect_path(b: &Bounds, radius: f32) -> Option<tiny_skia::Path> {
    let r = radius.max(0.0).min(b.width / 2.0).min(b.height / 2.0);
    let (x, y, w, h) = (b.x, b.y, b.width, b.height);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    if r <= 0.01 {
        return Some(PathBuilder::from_rect(to_rect(b)?));
    }
    const KAPPA: f32 = 0.552_284_8;
    let k = r * KAPPA;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// A mask covering exactly `clip`, at the pixmap's own size -- one allocation
/// per clipped box per frame. Cheap enough for a demo-sized tree and canvas;
/// a scroll region or two per screen, not thousands. `ui/paint.rs`'s
/// `canvas.clip_rect` is anti-aliased and this is not: a byte is either in
/// the rect or out of it, which is the one visible difference at a clipped
/// edge.
fn clip_mask(width: u32, height: u32, clip: &Bounds) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let x0 = clip.x.max(0.0).round() as u32;
    let y0 = clip.y.max(0.0).round() as u32;
    let x1 = (clip.x + clip.width).max(0.0).round().min(width as f32) as u32;
    let y1 = (clip.y + clip.height).max(0.0).round().min(height as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return Some(mask);
    }
    let data = mask.data_mut();
    for row in y0..y1 {
        let start = (row * width + x0) as usize;
        let end = (row * width + x1) as usize;
        data[start..end].fill(255);
    }
    Some(mask)
}

/// Blends a straight-alpha `(r, g, b, a)` source pixel over `pixmap`'s
/// premultiplied one at `(x, y)`, standard "over" compositing. The manual
/// per-pixel route -- rather than tiny-skia's own path/rect fills -- is what
/// both glyph rasterization (`web::text::WebShaper::draw`, fed one pixel at a
/// time by cosmic-text's `SwashCache`) and icon/illustration blitting below
/// need: neither has a vector shape to fill, only a coverage or colour buffer
/// already rasterized by someone else (swash, resvg).
pub(super) fn blend_over(pixmap: &mut PixmapMut, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if a == 0 {
        return;
    }
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let idx = (y as u32 * pixmap.width() + x as u32) as usize;
    let dst = pixmap.pixels_mut()[idx];
    let sa = a as u32;
    let inv = 255 - sa;
    let over = |s: u8, d: u8| -> u8 {
        let s_premul = (s as u32 * sa) / 255;
        (s_premul + (d as u32 * inv) / 255).min(255) as u8
    };
    let out_a = (sa + (dst.alpha() as u32 * inv) / 255).min(255) as u8;
    let out_r = over(r, dst.red()).min(out_a);
    let out_g = over(g, dst.green()).min(out_a);
    let out_b = over(b, dst.blue()).min(out_a);
    if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a) {
        pixmap.pixels_mut()[idx] = px;
    }
}

/// Blits straight RGBA (an icon mask/tint, or a colour icon) at `b`'s origin,
/// already at `b`'s pixel size. Mirrors `ui/paint.rs::draw_rgba`, minus the
/// Skia image upload -- there is no GPU atlas here to upload into, so this
/// just walks the bytes and composites them directly.
fn draw_rgba(pixmap: &mut PixmapMut, b: &Bounds, width: u32, rgba: &[u8], clip: Option<Bounds>) {
    let (ox, oy) = (b.x.round() as i32, b.y.round() as i32);
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let (dx, dy) = (i as u32 % width, i as u32 / width);
        let (x, y) = (ox + dx as i32, oy + dy as i32);
        if let Some(clip) = clip
            && !clip.contains(x as f32, y as f32)
        {
            continue;
        }
        blend_over(pixmap, x, y, px[0], px[1], px[2], px[3]);
    }
}

fn draw_borders(
    pixmap: &mut PixmapMut,
    b: &Bounds,
    widths: crate::ui::style::Edges<f32>,
    color: Rgba,
    radius: f32,
    mask: Option<&Mask>,
) {
    if color.is_transparent() {
        return;
    }
    let paint = solid_paint(color);
    let uniform =
        widths.top == widths.right && widths.right == widths.bottom && widths.bottom == widths.left;
    if uniform && widths.top > 0.0 {
        let w = widths.top;
        let inset = Bounds {
            x: b.x + w / 2.0,
            y: b.y + w / 2.0,
            width: (b.width - w).max(0.0),
            height: (b.height - w).max(0.0),
        };
        let r = (radius - w / 2.0).max(0.0);
        if let Some(path) = rounded_rect_path(&inset, r) {
            let stroke = tiny_skia::Stroke {
                width: w,
                ..Default::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), mask);
        }
        return;
    }
    let mut fill = |r: Option<Rect>| {
        if let Some(r) = r {
            pixmap.fill_rect(r, &paint, Transform::identity(), mask);
        }
    };
    if widths.top > 0.0 {
        fill(Rect::from_xywh(b.x, b.y, b.width, widths.top));
    }
    if widths.bottom > 0.0 {
        fill(Rect::from_xywh(
            b.x,
            b.y + b.height - widths.bottom,
            b.width,
            widths.bottom,
        ));
    }
    if widths.left > 0.0 {
        fill(Rect::from_xywh(b.x, b.y, widths.left, b.height));
    }
    if widths.right > 0.0 {
        fill(Rect::from_xywh(
            b.x + b.width - widths.right,
            b.y,
            widths.right,
            b.height,
        ));
    }
}

/// Everything painting needs across a frame: the shaper (owns the font
/// database and glyph cache) and the SVG icon rasterizer. Mirrors
/// `ui::paint::Painter`, minus the video-frame cache `Content::Image` would
/// need -- nothing in this build ever produces one.
#[derive(Default)]
pub struct Painter {
    pub shaper: WebShaper,
    pub svg: SvgRenderer,
}

/// Paints every box in order, onto `pixmap`. `boxes` must be the list
/// `layout` returned, parent-before-child.
pub fn paint<S>(
    pixmap: &mut Pixmap,
    boxes: &[Box_<'_, S>],
    painter: &mut Painter,
    pointer: Option<(f32, f32)>,
) {
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut pixmap = pixmap.as_mut();
    for (i, b) in boxes.iter().enumerate() {
        if b.bounds.width <= 0.0 || b.bounds.height <= 0.0 {
            continue;
        }
        let clip = inherited_clip(boxes, i);
        if let Some(c) = clip
            && c.width <= 0.0
        {
            continue;
        }
        let mask = clip.and_then(|c| clip_mask(width, height, &c));

        let mut style = b.style.clone();
        if let Some(hover) = b.node.and_then(|n| n.hover_style())
            && let Some((px_, py_)) = pointer
            && b.bounds.contains(px_, py_)
            && clip.is_none_or(|c| c.contains(px_, py_))
        {
            style.layer(hover);
        }
        let alpha = style.opacity.unwrap_or(1.0);
        let radius = radius_of(&style, &b.bounds);

        if let Some(bg) = style.background
            && !bg.is_transparent()
            && let Some(path) = rounded_rect_path(&b.bounds, radius)
        {
            let paint = solid_paint(bg.opacity(alpha));
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask.as_ref(),
            );
        }

        let widths = style.border_widths.resolved();
        if let Some(bc) = style.border_color {
            draw_borders(
                &mut pixmap,
                &b.bounds,
                widths,
                bc.opacity(alpha),
                radius,
                mask.as_ref(),
            );
        }

        if let Some(text) = b.text {
            let line = painter.shaper.shape(
                text,
                b.inherited.font_size,
                b.inherited.line_height,
                b.inherited.bold,
                None,
            );
            let color = b.inherited.color.opacity(alpha);
            let rgba8 = [
                (color.r * 255.0).round() as u8,
                (color.g * 255.0).round() as u8,
                (color.b * 255.0).round() as u8,
                (color.a * 255.0).round() as u8,
            ];
            painter.shaper.draw(
                &mut pixmap,
                (b.bounds.x, b.bounds.y + line.baseline),
                &line,
                rgba8,
                clip,
            );
        }

        match b.node.map(|n| n.content()) {
            Some(Content::Svg(path)) if !path.is_empty() => {
                let w = b.bounds.width.round().max(1.0) as u32;
                let h = b.bounds.height.round().max(1.0) as u32;
                let tint = b.inherited.color.opacity(alpha);
                if let Some(rgba) = painter.svg.tinted_rgba(path, w, h, tint) {
                    draw_rgba(&mut pixmap, &b.bounds, w, &rgba, clip);
                }
            }
            Some(Content::ColorSvg(path)) if !path.is_empty() => {
                let w = b.bounds.width.round().max(1.0) as u32;
                let h = b.bounds.height.round().max(1.0) as u32;
                if let Some(rgba) = painter.svg.color_rgba(path, w, h) {
                    draw_rgba(&mut pixmap, &b.bounds, w, rgba, clip);
                }
            }
            // `Content::Image` (a decoded video frame or a file thumbnail) is
            // deliberately unhandled -- see the module doc.
            _ => {}
        }
    }
}
