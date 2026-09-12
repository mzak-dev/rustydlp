//! Style state and the builder trait that sets it.
//!
//! Shaped after gpui's own split: a `StyleRefinement` of all-`Option` fields,
//! and a `Styled` trait whose methods set them. Keeping that shape is what lets
//! `app.rs` port by changing imports rather than call sites, and it is what
//! makes `.hover(|s| s.bg(..))` express a *partial* override the way it does in
//! gpui — the closure refines a blank refinement, which is then layered over the
//! base style only while hovered.

use super::color::Rgba;
use super::units::{Length, Pixels, px};

/// gpui's spacing unit: one step is 0.25rem, and the app's base font is 16px,
/// so a step is 4px. `gap_2()` is two steps. Transcribed from the Tailwind scale
/// gpui mirrors.
pub const SPACING_STEP: f32 = 4.0;

const fn step(n: f32) -> f32 {
    SPACING_STEP * n
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
}

/// How an image fills the box it is drawn into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFit {
    /// Whole image visible, letterboxed.
    Contain,
    /// Box fully covered, image cropped.
    Cover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl Edges<Option<f32>> {
    fn all(&mut self, v: f32) {
        *self = Edges { top: Some(v), right: Some(v), bottom: Some(v), left: Some(v) };
    }
    pub(crate) fn resolved(&self) -> Edges<f32> {
        Edges {
            top: self.top.unwrap_or(0.0),
            right: self.right.unwrap_or(0.0),
            bottom: self.bottom.unwrap_or(0.0),
            left: self.left.unwrap_or(0.0),
        }
    }
}

/// Every style property, each `None` until set. Layering is a field-wise
/// "later wins if set", which is how hover and selected states compose.
#[derive(Clone, Debug, Default)]
pub struct StyleRefinement {
    // -- layout ------------------------------------------------------------
    pub flex: Option<bool>,
    pub flex_direction: Option<FlexDirection>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub flex_wrap: Option<bool>,
    pub align_items: Option<AlignItems>,
    pub justify_content: Option<JustifyContent>,
    pub gap: Option<f32>,
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub min_width: Option<Length>,
    pub min_height: Option<Length>,
    pub max_width: Option<Length>,
    pub padding: Edges<Option<f32>>,
    pub margin: Edges<Option<f32>>,
    pub absolute: Option<bool>,
    pub inset: Edges<Option<f32>>,
    pub overflow_y_scroll: Option<bool>,
    pub overflow_hidden: Option<bool>,

    // -- paint -------------------------------------------------------------
    pub background: Option<Rgba>,
    pub text_color: Option<Rgba>,
    pub font_size: Option<Pixels>,
    pub line_height: Option<Pixels>,
    pub font_bold: Option<bool>,
    pub corner_radius: Option<f32>,
    /// `true` means "pill": radius is half the box's shorter side, resolved at
    /// paint time because it depends on the computed size.
    pub corner_pill: Option<bool>,
    pub border_widths: Edges<Option<f32>>,
    pub border_color: Option<Rgba>,
    pub opacity: Option<f32>,
    pub truncate: Option<bool>,
    pub cursor_pointer: Option<bool>,
    pub object_fit: Option<ObjectFit>,
}

impl StyleRefinement {
    /// Field-wise overlay: anything set in `other` wins.
    pub fn layer(&mut self, other: &StyleRefinement) {
        macro_rules! take {
            ($($f:ident),* $(,)?) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            flex, flex_direction, flex_grow, flex_shrink, flex_wrap, align_items,
            justify_content, gap, width, height, min_width, min_height, max_width,
            absolute, overflow_y_scroll, overflow_hidden, background, text_color,
            font_size, line_height, font_bold, corner_radius, corner_pill, border_color,
            opacity, truncate, cursor_pointer, object_fit,
        );
        macro_rules! take_edges {
            ($($f:ident),* $(,)?) => { $(
                if other.$f.top.is_some() { self.$f.top = other.$f.top; }
                if other.$f.right.is_some() { self.$f.right = other.$f.right; }
                if other.$f.bottom.is_some() { self.$f.bottom = other.$f.bottom; }
                if other.$f.left.is_some() { self.$f.left = other.$f.left; }
            )* };
        }
        take_edges!(padding, margin, inset, border_widths);
    }
}

/// Sets style properties. Named and spelled as in gpui so that ported call
/// sites read identically.
pub trait Styled: Sized {
    fn style(&mut self) -> &mut StyleRefinement;

    // -- flex --------------------------------------------------------------
    fn flex(mut self) -> Self {
        self.style().flex = Some(true);
        self
    }
    fn flex_col(mut self) -> Self {
        let s = self.style();
        s.flex = Some(true);
        s.flex_direction = Some(FlexDirection::Column);
        self
    }
    fn flex_row(mut self) -> Self {
        let s = self.style();
        s.flex = Some(true);
        s.flex_direction = Some(FlexDirection::Row);
        self
    }
    fn flex_1(mut self) -> Self {
        let s = self.style();
        s.flex_grow = Some(1.0);
        s.flex_shrink = Some(1.0);
        self
    }
    fn flex_shrink_0(mut self) -> Self {
        self.style().flex_shrink = Some(0.0);
        self
    }
    fn flex_wrap(mut self) -> Self {
        self.style().flex_wrap = Some(true);
        self
    }
    fn items_center(mut self) -> Self {
        self.style().align_items = Some(AlignItems::Center);
        self
    }
    fn justify_center(mut self) -> Self {
        self.style().justify_content = Some(JustifyContent::Center);
        self
    }
    fn justify_end(mut self) -> Self {
        self.style().justify_content = Some(JustifyContent::End);
        self
    }
    fn justify_between(mut self) -> Self {
        self.style().justify_content = Some(JustifyContent::SpaceBetween);
        self
    }

    // -- size --------------------------------------------------------------
    fn w(mut self, l: impl Into<Length>) -> Self {
        self.style().width = Some(l.into());
        self
    }
    fn h(mut self, l: impl Into<Length>) -> Self {
        self.style().height = Some(l.into());
        self
    }
    fn w_full(self) -> Self {
        self.w(Length::Fraction(1.0))
    }
    fn h_full(self) -> Self {
        self.h(Length::Fraction(1.0))
    }
    fn size_full(self) -> Self {
        self.w_full().h_full()
    }
    fn min_w(mut self, l: impl Into<Length>) -> Self {
        self.style().min_width = Some(l.into());
        self
    }
    fn max_w(mut self, l: impl Into<Length>) -> Self {
        self.style().max_width = Some(l.into());
        self
    }
    /// Undoes flex's automatic minimum size, which is what lets a flex child
    /// shrink below its content and so makes `truncate` and scrolling work.
    fn min_w_0(self) -> Self {
        self.min_w(px(0.))
    }
    fn min_h_0(mut self) -> Self {
        self.style().min_height = Some(Length::Px(0.0));
        self
    }

    // -- spacing -----------------------------------------------------------
    fn gap(mut self, v: f32) -> Self {
        self.style().gap = Some(v);
        self
    }
    fn gap_0p5(self) -> Self {
        self.gap(step(0.5))
    }
    fn gap_1(self) -> Self {
        self.gap(step(1.0))
    }
    fn gap_2(self) -> Self {
        self.gap(step(2.0))
    }
    fn gap_3(self) -> Self {
        self.gap(step(3.0))
    }
    fn gap_4(self) -> Self {
        self.gap(step(4.0))
    }
    fn gap_5(self) -> Self {
        self.gap(step(5.0))
    }

    fn p(mut self, v: f32) -> Self {
        self.style().padding.all(v);
        self
    }
    fn p_2(self) -> Self {
        self.p(step(2.0))
    }
    fn p_3(self) -> Self {
        self.p(step(3.0))
    }
    fn p_4(self) -> Self {
        self.p(step(4.0))
    }
    fn p_5(self) -> Self {
        self.p(step(5.0))
    }
    fn px(mut self, v: f32) -> Self {
        let s = self.style();
        s.padding.left = Some(v);
        s.padding.right = Some(v);
        self
    }
    fn px_2(self) -> Self {
        self.px(step(2.0))
    }
    fn px_4(self) -> Self {
        self.px(step(4.0))
    }
    fn py(mut self, v: f32) -> Self {
        let s = self.style();
        s.padding.top = Some(v);
        s.padding.bottom = Some(v);
        self
    }
    fn py_1p5(self) -> Self {
        self.py(step(1.5))
    }
    fn py_2(self) -> Self {
        self.py(step(2.0))
    }
    fn pl(mut self, v: f32) -> Self {
        self.style().padding.left = Some(v);
        self
    }
    fn pr(mut self, v: f32) -> Self {
        self.style().padding.right = Some(v);
        self
    }
    fn pt(mut self, v: f32) -> Self {
        self.style().padding.top = Some(v);
        self
    }
    fn pb(mut self, v: f32) -> Self {
        self.style().padding.bottom = Some(v);
        self
    }
    fn ml(mut self, v: f32) -> Self {
        self.style().margin.left = Some(v);
        self
    }
    fn mr(mut self, v: f32) -> Self {
        self.style().margin.right = Some(v);
        self
    }
    fn mb(mut self, v: f32) -> Self {
        self.style().margin.bottom = Some(v);
        self
    }
    fn pt_3(mut self) -> Self {
        self.style().padding.top = Some(step(3.0));
        self
    }
    fn mt_1(mut self) -> Self {
        self.style().margin.top = Some(step(1.0));
        self
    }

    // -- paint -------------------------------------------------------------
    fn bg(mut self, c: Rgba) -> Self {
        self.style().background = Some(c);
        self
    }
    fn text_color(mut self, c: Rgba) -> Self {
        self.style().text_color = Some(c);
        self
    }
    fn border_color(mut self, c: Rgba) -> Self {
        self.style().border_color = Some(c);
        self
    }
    fn rounded(mut self, r: Pixels) -> Self {
        self.style().corner_radius = Some(r.0);
        self
    }
    fn rounded_full(mut self) -> Self {
        self.style().corner_pill = Some(true);
        self
    }
    fn border_1(mut self) -> Self {
        self.style().border_widths.all(1.0);
        self
    }
    fn border_t_1(mut self) -> Self {
        self.style().border_widths.top = Some(1.0);
        self
    }
    fn border_r_1(mut self) -> Self {
        self.style().border_widths.right = Some(1.0);
        self
    }
    fn border_b_1(mut self) -> Self {
        self.style().border_widths.bottom = Some(1.0);
        self
    }
    fn border_l_1(mut self) -> Self {
        self.style().border_widths.left = Some(1.0);
        self
    }
    fn opacity(mut self, v: f32) -> Self {
        self.style().opacity = Some(v);
        self
    }

    // -- type --------------------------------------------------------------
    fn text_size(mut self, size: Pixels, line_height: Pixels) -> Self {
        let s = self.style();
        s.font_size = Some(size);
        s.line_height = Some(line_height);
        self
    }
    /// Tailwind `text-xs`: 12px on a 16px line box.
    fn text_xs(self) -> Self {
        self.text_size(px(12.), px(16.))
    }
    /// Tailwind `text-sm`: 14px on a 20px line box.
    fn text_sm(self) -> Self {
        self.text_size(px(14.), px(20.))
    }
    fn font_bold(mut self) -> Self {
        self.style().font_bold = Some(true);
        self
    }
    /// Clip to one line and end it with an ellipsis.
    fn truncate(mut self) -> Self {
        self.style().truncate = Some(true);
        self
    }

    // -- position ----------------------------------------------------------
    fn absolute(mut self) -> Self {
        self.style().absolute = Some(true);
        self
    }
    /// A no-op for layout here: every box already establishes a containing
    /// block for its absolute children. Kept so call sites port unchanged, and
    /// because it documents intent at the call site.
    fn relative(self) -> Self {
        self
    }
    fn inset_0(mut self) -> Self {
        self.style().inset.all(0.0);
        self
    }
    fn top(mut self, v: Pixels) -> Self {
        self.style().inset.top = Some(v.0);
        self
    }

    // -- overflow ----------------------------------------------------------
    fn overflow_y_scroll(mut self) -> Self {
        self.style().overflow_y_scroll = Some(true);
        self
    }
    fn overflow_hidden(mut self) -> Self {
        self.style().overflow_hidden = Some(true);
        self
    }
    fn cursor_pointer(mut self) -> Self {
        self.style().cursor_pointer = Some(true);
        self
    }
    fn object_fit(mut self, fit: ObjectFit) -> Self {
        self.style().object_fit = Some(fit);
        self
    }
}

impl Styled for StyleRefinement {
    fn style(&mut self) -> &mut StyleRefinement {
        self
    }
}

/// `.when(..)` / `.map(..)` / `.when_some(..)`, which `app.rs` uses 50 times
/// between them. gpui supplies these through `gpui::prelude::FluentBuilder`.
pub trait FluentBuilder: Sized {
    fn map<U>(self, f: impl FnOnce(Self) -> U) -> U {
        f(self)
    }
    fn when(self, condition: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if condition { f(self) } else { self }
    }
    fn when_some<T>(self, option: Option<T>, f: impl FnOnce(Self, T) -> Self) -> Self {
        match option {
            Some(value) => f(self, value),
            None => self,
        }
    }
}

impl<T: Sized> FluentBuilder for T {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale has to match gpui's or every ported layout shifts. One step is
    /// 0.25rem against a 16px base.
    #[test]
    fn spacing_scale_matches_the_tailwind_steps_gpui_uses() {
        let s = StyleRefinement::default().gap_2();
        assert_eq!(s.gap, Some(8.0));
        let s = StyleRefinement::default().py_1p5();
        assert_eq!(s.padding.top, Some(6.0));
        assert_eq!(s.padding.left, None, "py must not touch the horizontal edges");
        let s = StyleRefinement::default().p_5();
        assert_eq!(s.padding.resolved().left, 20.0);
    }

    /// Layering is how hover works: only the fields the hover closure set may
    /// win, everything else must survive from the base style.
    #[test]
    fn layering_only_overrides_fields_that_were_set() {
        use super::super::color::rgb;
        let mut base = StyleRefinement::default().bg(rgb(0x111111)).text_color(rgb(0x222222));
        let hover = StyleRefinement::default().bg(rgb(0x333333));
        base.layer(&hover);
        assert_eq!(base.background, Some(rgb(0x333333)), "hover bg wins");
        assert_eq!(base.text_color, Some(rgb(0x222222)), "text colour survives");
    }

    /// Edges layer per-side, so `.border_1()` then a hover `.border_t_1()` does
    /// not wipe the other three sides.
    #[test]
    fn edge_layering_is_per_side() {
        let mut base = StyleRefinement::default().border_1();
        let mut over = StyleRefinement::default();
        over.border_widths.top = Some(3.0);
        base.layer(&over);
        let e = base.border_widths.resolved();
        assert_eq!((e.top, e.right, e.bottom, e.left), (3.0, 1.0, 1.0, 1.0));
    }
}
