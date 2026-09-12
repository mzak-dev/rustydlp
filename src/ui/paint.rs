//! Draws laid-out boxes onto a Skia canvas.

use skia_safe::{Canvas, Color4f, Paint, RRect, Rect};

use super::color::Rgba;
use super::element::Content;
use super::layout::Box_;
use super::style::{Edges, StyleRefinement};
use super::text::Shaper;
use super::units::Bounds;

fn to_color(c: Rgba) -> Color4f {
    Color4f::new(c.r, c.g, c.b, c.a)
}

fn rect_of(b: &Bounds) -> Rect {
    Rect::from_xywh(b.x, b.y, b.width, b.height)
}

/// `rounded_full()` means "pill": half the shorter side, which is only knowable
/// once layout has produced a size.
fn radius_of(style: &StyleRefinement, b: &Bounds) -> f32 {
    if style.corner_pill == Some(true) {
        b.width.min(b.height) / 2.0
    } else {
        style.corner_radius.unwrap_or(0.0)
    }
}

fn rrect_of(b: &Bounds, radius: f32) -> RRect {
    RRect::new_rect_xy(rect_of(b), radius, radius)
}

/// The clip a box inherits: the intersection of every clipping ancestor. Walked
/// per box rather than tracked with a canvas save-stack, because layout hands
/// back a flat list and a stack would have to be rebuilt from it anyway.
fn inherited_clip<S>(boxes: &[Box_<'_, S>], mut index: usize) -> Option<Rect> {
    let mut clip: Option<Rect> = None;
    while let Some(parent) = boxes[index].parent {
        let p = &boxes[parent];
        if p.clips {
            let r = rect_of(&p.bounds);
            clip = match clip {
                None => Some(r),
                // Intersection: an ancestor can only shrink the visible area.
                // An empty result means the box is entirely scrolled out of
                // view, and the caller skips it.
                Some(mut c) => {
                    if c.intersect(r) {
                        Some(c)
                    } else {
                        return Some(Rect::new_empty());
                    }
                }
            };
        }
        index = parent;
    }
    clip
}

fn draw_borders(canvas: &Canvas, b: &Bounds, widths: Edges<f32>, color: Rgba, radius: f32) {
    if color.is_transparent() {
        return;
    }
    let mut paint = Paint::new(to_color(color), None);
    paint.set_anti_alias(true);

    let uniform = widths.top == widths.right && widths.right == widths.bottom
        && widths.bottom == widths.left;
    if uniform && widths.top > 0.0 {
        // Borders sit inside the box, as in CSS, so the stroke is centred half a
        // width in from the edge.
        let w = widths.top;
        let inset = Rect::from_xywh(
            b.x + w / 2.0,
            b.y + w / 2.0,
            (b.width - w).max(0.0),
            (b.height - w).max(0.0),
        );
        paint.set_style(skia_safe::paint::Style::Stroke);
        paint.set_stroke_width(w);
        let r = (radius - w / 2.0).max(0.0);
        canvas.draw_rrect(RRect::new_rect_xy(inset, r, r), &paint);
        return;
    }
    // Per-side widths: drawn as filled strips. Every such call site in the app
    // (the sidebar's right edge, the navbar's bottom edge) is square.
    paint.set_style(skia_safe::paint::Style::Fill);
    if widths.top > 0.0 {
        canvas.draw_rect(Rect::from_xywh(b.x, b.y, b.width, widths.top), &paint);
    }
    if widths.bottom > 0.0 {
        canvas.draw_rect(
            Rect::from_xywh(b.x, b.y + b.height - widths.bottom, b.width, widths.bottom),
            &paint,
        );
    }
    if widths.left > 0.0 {
        canvas.draw_rect(Rect::from_xywh(b.x, b.y, widths.left, b.height), &paint);
    }
    if widths.right > 0.0 {
        canvas.draw_rect(
            Rect::from_xywh(b.x + b.width - widths.right, b.y, widths.right, b.height),
            &paint,
        );
    }
}

/// Paints every box in order. `boxes` must be the list `layout` returned, in
/// that order: it is already parent-before-child, which is the paint order.
pub fn paint<S>(canvas: &Canvas, boxes: &[Box_<'_, S>], shaper: &mut Shaper) {
    for (i, b) in boxes.iter().enumerate() {
        if b.bounds.width <= 0.0 || b.bounds.height <= 0.0 {
            continue;
        }
        let clip = inherited_clip(boxes, i);
        if let Some(c) = clip
            && c.is_empty()
        {
            continue;
        }

        let restore_to = canvas.save();
        if let Some(c) = clip {
            canvas.clip_rect(c, None, Some(true));
        }
        // Subtree opacity would need its own layer; every `.opacity()` in the
        // app is on a colour rather than an element, so this applies to the box
        // itself and is flagged if an element-level one ever appears.
        let alpha = b.style.opacity.unwrap_or(1.0);

        let radius = radius_of(&b.style, &b.bounds);
        if let Some(bg) = b.style.background
            && !bg.is_transparent()
        {
            let mut paint = Paint::new(to_color(bg.opacity(alpha)), None);
            paint.set_anti_alias(true);
            canvas.draw_rrect(rrect_of(&b.bounds, radius), &paint);
        }

        let widths = b.style.border_widths.resolved();
        if let Some(bc) = b.style.border_color {
            draw_borders(canvas, &b.bounds, widths, bc.opacity(alpha), radius);
        }

        if let Some(text) = b.text {
            let line = shaper.shape(
                text,
                b.inherited.font_size,
                b.inherited.line_height,
                b.inherited.bold,
                None,
            );
            let mut paint = Paint::new(to_color(b.inherited.color.opacity(alpha)), None);
            paint.set_anti_alias(true);
            for blob in &line.runs {
                canvas.draw_text_blob(blob, (b.bounds.x, b.bounds.y + line.baseline), &paint);
            }
        }

        if let Some(Content::Image(src)) = b.content {
            draw_image(canvas, &b.bounds, src, b.style.object_fit, alpha);
        }
        // Content::Svg is not drawn yet: gpui painted it as an alpha mask tinted
        // with the box's text colour, and reproducing that needs skia-safe's
        // `svg` feature, which is not in the feature combo that has prebuilts.
        // Tracked as an open item rather than silently approximated.

        canvas.restore_to_count(restore_to);
    }
}

fn draw_image(
    canvas: &Canvas,
    b: &Bounds,
    src: &super::element::ImageSource,
    fit: Option<super::style::ObjectFit>,
    alpha: f32,
) {
    use super::element::ImageSource;
    use super::style::ObjectFit;

    let image = match src {
        ImageSource::Rgba { width, height, data } => {
            let info = skia_safe::ImageInfo::new(
                (*width as i32, *height as i32),
                skia_safe::ColorType::RGBA8888,
                skia_safe::AlphaType::Unpremul,
                None,
            );
            skia_safe::images::raster_from_data(
                &info,
                skia_safe::Data::new_copy(data),
                *width as usize * 4,
            )
        }
        ImageSource::Path(p) => std::fs::read(p)
            .ok()
            .and_then(|bytes| skia_safe::Image::from_encoded(skia_safe::Data::new_copy(&bytes))),
    };
    let Some(image) = image else { return };

    let (iw, ih) = (image.width() as f32, image.height() as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    // Contain letterboxes, Cover crops; both preserve aspect ratio, which is
    // what keeps a 1920x1080 frame from stretching into the player stage.
    let scale = match fit.unwrap_or(ObjectFit::Contain) {
        ObjectFit::Contain => (b.width / iw).min(b.height / ih),
        ObjectFit::Cover => (b.width / iw).max(b.height / ih),
    };
    let (w, h) = (iw * scale, ih * scale);
    let dst = Rect::from_xywh(
        b.x + (b.width - w) / 2.0,
        b.y + (b.height - h) / 2.0,
        w,
        h,
    );

    let restore = canvas.save();
    canvas.clip_rect(rect_of(b), None, Some(true));
    let mut paint = Paint::default();
    paint.set_alpha_f(alpha);
    canvas.draw_image_rect(
        &image,
        None,
        dst,
        &paint,
    );
    canvas.restore_to_count(restore);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::raster::RasterBackend;
    use crate::render::Backend;
    use crate::ui::color::rgb;
    use crate::ui::element::{Element, div, h_flex};
    use crate::ui::layout::{FixedMetrics, layout};
    use crate::ui::style::Styled;
    use crate::ui::theme::theme;
    use crate::ui::units::px;

    type E = Element<()>;

    fn render(tree: &E, w: u32, h: u32) -> RasterBackend {
        render_on(tree, w, h, theme().background)
    }

    /// Clearing to transparent makes "nothing was drawn here" directly
    /// assertable, which an opaque background would hide behind alpha 255.
    fn render_on(tree: &E, w: u32, h: u32, clear: crate::ui::Rgba) -> RasterBackend {
        let mut backend = RasterBackend::new(w, h);
        backend.begin_frame(w, h, clear);
        let boxes = layout(tree, (w as f32, h as f32), &mut FixedMetrics::default());
        // Shaping is irrelevant here: the tree is boxes only, so no system font
        // can make this assertion host-dependent.
        let mut shaper = Shaper::new();
        paint(backend.canvas(), &boxes, &mut shaper);
        backend
    }

    /// The window shell, end to end: layout through paint to pixels. A fixed
    /// sidebar with its right-hand border, and the pane beside it.
    #[test]
    fn the_window_shell_paints_sidebar_border_and_pane() {
        let t = theme();
        let tree: E = h_flex()
            .w_full()
            .h_full()
            .child(
                div()
                    .w(px(260.))
                    .h_full()
                    .bg(t.sidebar)
                    .border_r_1()
                    .border_color(t.sidebar_border),
            )
            .child(div().flex_1().h_full().bg(t.background))
            .into_any_element();

        let mut r = render(&tree, 1180, 760);

        assert_eq!(r.pixel(10, 10), (0x0a, 0x0a, 0x0a, 0xff), "sidebar fill");
        // The border is the sidebar's last column, drawn inside the box.
        assert_eq!(r.pixel(259, 400), (0x26, 0x26, 0x26, 0xff), "sidebar right border");
        assert_eq!(r.pixel(600, 400), (0x0a, 0x0a, 0x0a, 0xff), "main pane fill");
        assert!(!r.encode_png().is_empty(), "frame encodes to a PNG");
    }

    /// `rounded_full()` has to resolve against the painted size, so the corners
    /// of a pill are outside it and the middle is inside.
    #[test]
    fn rounded_full_clips_the_corners_of_a_pill() {
        let tree: E = div()
            .w(px(40.))
            .h(px(20.))
            .rounded_full()
            .bg(rgb(0xffffff))
            .into_any_element();

        let mut r = render_on(&tree, 40, 20, crate::ui::transparent());
        assert_eq!(r.pixel(20, 10), (0xff, 0xff, 0xff, 0xff), "centre is filled");
        assert_eq!(r.pixel(0, 0).3, 0x00, "the corner is outside the pill, so untouched");
    }

    /// An overflow-hidden ancestor must actually clip: this is what keeps a
    /// scrolled job list inside its column.
    #[test]
    fn an_overflow_hidden_ancestor_clips_its_children() {
        let tree: E = div()
            .w(px(100.))
            .h(px(20.))
            .overflow_hidden()
            .child(div().w(px(100.)).h(px(200.)).bg(rgb(0xffffff)))
            .into_any_element();

        // Canvas taller than the clipping box, so anything unclipped shows.
        let mut r = render_on(&tree, 100, 100, crate::ui::transparent());
        assert_eq!(r.pixel(50, 10), (0xff, 0xff, 0xff, 0xff), "inside the clip");
        assert_eq!(r.pixel(50, 50).3, 0x00, "below the clip, nothing drawn");
    }
}
