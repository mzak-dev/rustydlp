//! Layout, delegated to taffy.
//!
//! taffy is the same engine gpui uses, which is the point: flex behaviour
//! matches the interface being replaced by construction rather than by
//! eyeballing screenshots. This module's only job is translating our
//! `StyleRefinement` into `taffy::Style`, measuring text leaves, and flattening
//! the result into absolute paint-order boxes.

use std::collections::HashMap;

use taffy::geometry::Point;
use taffy::prelude::*;
use taffy::style::{
    AlignItems as TAlign, Dimension, FlexDirection as TDir, JustifyContent as TJustify,
    LengthPercentage, LengthPercentageAuto, Overflow, Position,
};

use super::color::Rgba;
use super::element::{Content, Div, Element};
use super::style::{AlignItems, FlexDirection, JustifyContent, StyleRefinement};
use super::theme::{BASE_FONT_SIZE, BASE_LINE_HEIGHT, theme};
use super::units::{Bounds, Length};

/// Measures a run of text. Behind a trait so layout can be tested with
/// deterministic metrics instead of whatever fonts the host happens to have —
/// the real implementation shapes with cosmic-text.
pub trait MeasureText {
    /// Width and height the text needs. `max_width` is `Some` when the layout
    /// engine has already decided the available width.
    fn measure(&mut self, text: &str, font_size: f32, line_height: f32, max_width: Option<f32>)
    -> (f32, f32);
}

/// Fixed advance per character. Test-only: makes layout assertions exact.
pub struct FixedMetrics {
    /// Advance as a fraction of the font size.
    pub advance_ratio: f32,
}

impl Default for FixedMetrics {
    fn default() -> Self {
        FixedMetrics { advance_ratio: 0.5 }
    }
}

impl MeasureText for FixedMetrics {
    fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        line_height: f32,
        _max_width: Option<f32>,
    ) -> (f32, f32) {
        (text.chars().count() as f32 * font_size * self.advance_ratio, line_height)
    }
}

/// Text properties a child inherits from its ancestors.
#[derive(Clone, Copy, Debug)]
pub struct Inherited {
    pub font_size: f32,
    pub line_height: f32,
    pub color: Rgba,
    pub bold: bool,
    /// Whether the nearest styled ancestor asked for `.truncate()`. A text
    /// leaf carries no `StyleRefinement` of its own (see `Box_::style`'s
    /// doc), so this is the only way the wrapping div's request reaches the
    /// text it actually applies to — the same route `font_size`/`color`/
    /// `bold` already take to get from a styled div down to its bare string.
    pub truncate: bool,
    /// The product of every `.opacity()` from the root down to this box. The
    /// painter has no layer stack to fade a subtree with, so the fade is
    /// carried down here instead and applied per box — which is what lets one
    /// `.opacity()` on a screen's wrapper fade the whole screen, rather than
    /// only the (usually empty) wrapper box itself.
    pub opacity: f32,
    /// False inside a `.pointer_events_none()` subtree: the box is painted but
    /// takes no part in hit-testing, hover or scrolling.
    pub interactive: bool,
}

impl Default for Inherited {
    fn default() -> Self {
        Inherited {
            font_size: BASE_FONT_SIZE.0,
            line_height: BASE_FONT_SIZE.0 * BASE_LINE_HEIGHT,
            color: theme().foreground,
            bold: false,
            truncate: false,
            opacity: 1.0,
            interactive: true,
        }
    }
}

impl Inherited {
    fn refine(mut self, style: &StyleRefinement) -> Self {
        if let Some(s) = style.font_size {
            self.font_size = s.0;
            // A size without an explicit line height gets the base ratio.
            self.line_height = s.0 * BASE_LINE_HEIGHT;
        }
        if let Some(lh) = style.line_height {
            self.line_height = lh.0;
        }
        if let Some(c) = style.text_color {
            self.color = c;
        }
        if let Some(b) = style.font_bold {
            self.bold = b;
        }
        if let Some(t) = style.truncate {
            self.truncate = t;
        }
        // Multiplied, not overwritten: a half-faded screen containing an
        // already-dimmed row has to end up dimmer than either alone.
        if let Some(o) = style.opacity {
            self.opacity *= o;
        }
        // One-way: an inert subtree cannot opt a child back in, the same way
        // CSS's `pointer-events: none` needs an explicit `auto` to undo and
        // nothing here has a use for one.
        if style.pointer_events_none == Some(true) {
            self.interactive = false;
        }
        self
    }
}

/// Per-scroll-region offsets, keyed by the element id. `overflow_y_scroll`
/// needs an `.id()` for exactly this reason: the offset is retained state and
/// has to survive across frames.
#[derive(Debug, Default)]
pub struct ScrollState {
    offsets: HashMap<String, f32>,
}

impl ScrollState {
    pub fn offset(&self, id: &str) -> f32 {
        self.offsets.get(id).copied().unwrap_or(0.0)
    }

    /// Scrolls by `dy`, clamped to `0..=max`. Returns whether anything moved,
    /// so a caller can decide whether a redraw is needed.
    pub fn scroll_by(&mut self, id: &str, dy: f32, max: f32) -> bool {
        let slot = self.offsets.entry(id.to_string()).or_insert(0.0);
        let next = (*slot + dy).clamp(0.0, max.max(0.0));
        let moved = next != *slot;
        *slot = next;
        moved
    }
}

/// The clip a box inherits: the intersection of every clipping ancestor.
///
/// Walked per box rather than tracked with a save-stack, because layout hands
/// back a flat list. `None` means unclipped; an empty `Bounds` means the box is
/// entirely outside its clip — scrolled out of view — and can be skipped.
pub fn inherited_clip<S>(boxes: &[Box_<'_, S>], mut index: usize) -> Option<Bounds> {
    let mut clip: Option<Bounds> = None;
    while let Some(parent) = boxes[index].parent {
        let p = &boxes[parent];
        if p.clips {
            clip = match clip {
                None => Some(p.bounds),
                Some(c) => match c.intersect(&p.bounds) {
                    Some(next) => Some(next),
                    None => return Some(Bounds::default()),
                },
            };
        }
        index = parent;
    }
    clip
}

/// One box, positioned absolutely in the window, in paint order.
pub struct Box_<'a, S> {
    pub bounds: Bounds,
    pub style: StyleRefinement,
    pub inherited: Inherited,
    /// The box's source element, when it is a `div` rather than a text leaf.
    /// Carries the click handler, the hover overrides and the scroll identity.
    pub node: Option<&'a Div<S>>,
    /// Set for text leaves.
    pub text: Option<&'a str>,
    /// Index of this box's parent in the flattened list, for hit-test
    /// ancestry and for clipping.
    pub parent: Option<usize>,
    pub clips: bool,
    /// For a scroll region: how far it can scroll before its content ends.
    pub scroll_max: f32,
}

fn dim(l: Option<Length>) -> Dimension {
    l.map(Length::to_taffy).unwrap_or_else(Dimension::auto)
}

fn lp(v: Option<f32>) -> LengthPercentage {
    LengthPercentage::length(v.unwrap_or(0.0))
}

/// For `inset`: unset means "auto", i.e. let the other edges decide.
fn lpa(v: Option<f32>) -> LengthPercentageAuto {
    match v {
        Some(v) => LengthPercentageAuto::length(v),
        None => LengthPercentageAuto::auto(),
    }
}

/// For `margin`: unset means zero. An `auto` margin would centre the box,
/// which is emphatically not what "no margin set" means.
fn lpa0(v: Option<f32>) -> LengthPercentageAuto {
    LengthPercentageAuto::length(v.unwrap_or(0.0))
}

fn to_taffy_style(s: &StyleRefinement) -> Style {
    let mut out = Style {
        display: if s.flex == Some(true) { Display::Flex } else { Display::Block },
        flex_direction: match s.flex_direction {
            Some(FlexDirection::Column) => TDir::Column,
            _ => TDir::Row,
        },
        flex_grow: s.flex_grow.unwrap_or(0.0),
        // CSS's default is 1; gpui's `flex_shrink_0()` is what turns it off.
        flex_shrink: s.flex_shrink.unwrap_or(1.0),
        flex_wrap: if s.flex_wrap == Some(true) { FlexWrap::Wrap } else { FlexWrap::NoWrap },
        size: Size { width: dim(s.width), height: dim(s.height) },
        min_size: Size { width: dim(s.min_width), height: dim(s.min_height) },
        max_size: Size { width: dim(s.max_width), height: Dimension::auto() },
        padding: Rect {
            top: lp(s.padding.top),
            right: lp(s.padding.right),
            bottom: lp(s.padding.bottom),
            left: lp(s.padding.left),
        },
        margin: Rect {
            top: lpa0(s.margin.top),
            right: lpa0(s.margin.right),
            bottom: lpa0(s.margin.bottom),
            left: lpa0(s.margin.left),
        },
        border: Rect {
            top: lp(s.border_widths.top),
            right: lp(s.border_widths.right),
            bottom: lp(s.border_widths.bottom),
            left: lp(s.border_widths.left),
        },
        gap: Size { width: lp(s.gap), height: lp(s.gap) },
        ..Default::default()
    };

    if let Some(a) = s.align_items {
        out.align_items = Some(match a {
            AlignItems::Start => TAlign::FlexStart,
            AlignItems::Center => TAlign::Center,
            AlignItems::End => TAlign::FlexEnd,
        });
    }
    if let Some(j) = s.justify_content {
        out.justify_content = Some(match j {
            JustifyContent::Start => TJustify::FlexStart,
            JustifyContent::Center => TJustify::Center,
            JustifyContent::End => TJustify::FlexEnd,
            JustifyContent::SpaceBetween => TJustify::SpaceBetween,
        });
    }
    let inset = Rect {
        top: lpa(s.inset.top),
        right: lpa(s.inset.right),
        bottom: lpa(s.inset.bottom),
        left: lpa(s.inset.left),
    };
    if s.absolute == Some(true) {
        out.position = Position::Absolute;
        out.inset = inset;
    } else if s.relative == Some(true) {
        // As in CSS: the box is laid out in flow, then drawn shifted by its
        // inset, leaving everything around it where it was. An `auto` edge
        // (the default) shifts by nothing.
        out.position = Position::Relative;
        out.inset = inset;
    }
    // Scroll and hidden both clip; gpui draws no scrollbar, and taffy's
    // default scrollbar_width of 0 matches that, so no gutter is reserved.
    if s.overflow_y_scroll == Some(true) {
        out.overflow = Point { x: Overflow::Hidden, y: Overflow::Scroll };
    } else if s.overflow_hidden == Some(true) {
        out.overflow = Point { x: Overflow::Hidden, y: Overflow::Hidden };
    }
    out
}

/// Text leaves carry what the measure callback needs.
struct TextCtx {
    text: String,
    font_size: f32,
    line_height: f32,
}

/// Lays `root` out into `viewport` and flattens it to absolute paint-order boxes.
pub fn layout<'a, S>(
    root: &'a Element<S>,
    viewport: (f32, f32),
    measurer: &mut dyn MeasureText,
    scroll: &ScrollState,
) -> Vec<Box_<'a, S>> {
    let mut tree: TaffyTree<TextCtx> = TaffyTree::new();
    // What each taffy node came from. Keyed by NodeId rather than indexed by
    // it: taffy packs a slotmap generation into NodeId, so the raw value is
    // astronomically large and using it as a Vec index tries to allocate
    // hundreds of gigabytes.
    let mut sources: HashMap<NodeId, (&'a Element<S>, Inherited)> = HashMap::new();

    fn build<'a, S>(
        tree: &mut TaffyTree<TextCtx>,
        sources: &mut HashMap<NodeId, (&'a Element<S>, Inherited)>,
        el: &'a Element<S>,
        inherited: Inherited,
    ) -> NodeId {
        let style = el.style().clone();
        let inherited = inherited.refine(&style);
        let taffy_style = to_taffy_style(&style);

        let node = match el {
            Element::Text(t) => tree
                .new_leaf_with_context(
                    taffy_style,
                    TextCtx {
                        text: t.text.to_string(),
                        font_size: inherited.font_size,
                        line_height: inherited.line_height,
                    },
                )
                .expect("taffy leaf"),
            Element::Node(d) => match &d.content {
                Content::Children(kids) => {
                    let children: Vec<NodeId> = kids
                        .iter()
                        .map(|k| build(tree, sources, k, inherited))
                        .collect();
                    tree.new_with_children(taffy_style, &children).expect("taffy node")
                }
                // Images and SVGs are leaves: their box is sized by style, not
                // by intrinsic size. gpui substitutes the natural size when a
                // dimension is Auto; every call site in app.rs sets both, so
                // nothing here depends on that.
                _ => tree.new_leaf(taffy_style).expect("taffy leaf"),
            },
        };
        sources.insert(node, (el, inherited));
        node
    }

    let root_node = build(&mut tree, &mut sources, root, Inherited::default());

    tree.compute_layout_with_measure(
        root_node,
        Size {
            width: AvailableSpace::Definite(viewport.0),
            height: AvailableSpace::Definite(viewport.1),
        },
        |known, available, _node, ctx, _style| {
            let Some(ctx) = ctx else {
                return Size::ZERO;
            };
            if let (Some(w), Some(h)) = (known.width, known.height) {
                return Size { width: w, height: h };
            }
            let max = match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            };
            let (w, h) = measurer.measure(&ctx.text, ctx.font_size, ctx.line_height, max);
            Size { width: known.width.unwrap_or(w), height: known.height.unwrap_or(h) }
        },
    )
    .expect("taffy layout");

    // Flatten depth-first, accumulating absolute offsets.
    let mut out: Vec<Box_<'a, S>> = Vec::new();
    fn flatten<'a, S>(
        tree: &TaffyTree<TextCtx>,
        sources: &HashMap<NodeId, (&'a Element<S>, Inherited)>,
        node: NodeId,
        origin: (f32, f32),
        parent: Option<usize>,
        out: &mut Vec<Box_<'a, S>>,
        scroll: &ScrollState,
    ) {
        let l = tree.layout(node).expect("laid out");
        let bounds = Bounds {
            x: origin.0 + l.location.x,
            y: origin.1 + l.location.y,
            width: l.size.width,
            height: l.size.height,
        };
        let (el, inherited) = sources[&node];
        let style = el.style().clone();
        let clips = style.overflow_hidden == Some(true) || style.overflow_y_scroll == Some(true);
        // Named `src` rather than `node`: `node` here is the taffy NodeId.
        let (src, text) = match el {
            Element::Node(d) => (Some(&**d), None),
            Element::Text(t) => (None, Some(&*t.text)),
        };
        // A scroll region shifts its children up by its offset; how far it may
        // shift is the overflow of its content past its own box.
        let scroll_max = (l.content_size.height - l.size.height).max(0.0);
        let offset = match (style.overflow_y_scroll, src.and_then(|d| d.element_id())) {
            (Some(true), Some(id)) => scroll.offset(id).min(scroll_max),
            _ => 0.0,
        };

        let me = out.len();
        out.push(Box_ {
            bounds,
            style,
            inherited,
            node: src,
            text,
            parent,
            clips,
            scroll_max,
        });

        for child in tree.children(node).expect("children") {
            flatten(
                tree,
                sources,
                child,
                (bounds.x, bounds.y - offset),
                Some(me),
                out,
                scroll,
            );
        }
    }
    flatten(&tree, &sources, root_node, (0.0, 0.0), None, &mut out, scroll);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::element::{Element, div, h_flex, v_flex};
    use crate::ui::style::Styled;
    use crate::ui::units::px;

    /// No app state in these trees; the parameter just has to be something.
    type E = Element<()>;

    fn lay(root: &E, viewport: (f32, f32)) -> Vec<Box_<'_, ()>> {
        layout(root, viewport, &mut FixedMetrics::default(), &ScrollState::default())
    }

    /// The shape the whole window uses: a fixed-width sidebar beside a pane
    /// that takes the rest. If this is wrong, every ported screen is wrong.
    #[test]
    fn a_fixed_sidebar_leaves_the_rest_to_the_flexible_pane() {
        let tree: E = h_flex()
            .w_full()
            .h_full()
            .child(div().w(px(260.)).h_full())
            .child(div().flex_1())
            .into_any_element();

        let boxes = lay(&tree, (1180.0, 760.0));
        assert_eq!(boxes[1].bounds.width, 260.0, "sidebar keeps its fixed width");
        assert_eq!(boxes[1].bounds.height, 760.0, "and fills the height");
        assert_eq!(boxes[2].bounds.width, 920.0, "pane takes the remainder");
        assert_eq!(boxes[2].bounds.x, 260.0, "and starts after the sidebar");
    }

    /// Padding and gap both have to come out of the content box, in the right
    /// axis, or rows drift. `p_3` is 12px, `gap_2` is 8px.
    #[test]
    fn padding_and_gap_consume_space_on_the_right_axis() {
        let tree: E = v_flex()
            .w(px(100.))
            .p_3()
            .gap_2()
            .child(div().h(px(10.)))
            .child(div().h(px(10.)))
            .into_any_element();

        let boxes = lay(&tree, (200.0, 200.0));
        assert_eq!(boxes[1].bounds.x, 12.0, "left padding offsets the first child");
        assert_eq!(boxes[1].bounds.y, 12.0, "top padding too");
        assert_eq!(boxes[1].bounds.width, 76.0, "100 - 12 left - 12 right");
        assert_eq!(boxes[2].bounds.y, 30.0, "12 padding + 10 height + 8 gap");
    }

    /// The modal overlay: absolutely positioned, inset 0, covering its parent.
    #[test]
    fn an_absolute_inset_0_child_covers_its_parent() {
        let tree: E = div()
            .w(px(400.))
            .h(px(300.))
            .child(div().absolute().inset_0())
            .into_any_element();

        let boxes = lay(&tree, (400.0, 300.0));
        assert_eq!(boxes[1].bounds.width, 400.0);
        assert_eq!(boxes[1].bounds.height, 300.0);
        assert_eq!((boxes[1].bounds.x, boxes[1].bounds.y), (0.0, 0.0));
    }

    /// `min_h_0` is what lets a scroll region shrink below its content — the
    /// reason `app.rs` pairs it with `flex_1` on every scrolling column.
    #[test]
    fn min_h_0_lets_a_flex_child_shrink_below_its_content() {
        let tall = || div().h(px(500.));
        let without: E = v_flex()
            .h(px(100.))
            .child(div().flex_1().child(tall()))
            .into_any_element();
        let with: E = v_flex()
            .h(px(100.))
            .child(div().flex_1().min_h_0().child(tall()))
            .into_any_element();

        let a = lay(&without, (200.0, 100.0));
        let b = lay(&with, (200.0, 100.0));
        assert_eq!(a[1].bounds.height, 500.0, "automatic minimum is the content height");
        assert_eq!(b[1].bounds.height, 100.0, "min_h_0 releases it");
    }

    /// Text inherits size and colour from whichever ancestor set them, which is
    /// how `.text_xs().text_color(..)` on a wrapper styles a bare string child.
    ///
    /// The width is the *parent's*, not the glyphs'. That matches gpui, which
    /// builds its text leaf with `request_measured_layout(Default::default(), ..)`
    /// — and `gpui::Style::default()` is `display: Block`, so a text box fills
    /// its block parent and the glyphs are painted left-aligned inside it. It is
    /// also what makes `.truncate()` meaningful: the box is already constrained,
    /// so the text has something to be clipped to.
    #[test]
    fn text_inherits_size_and_colour_from_its_ancestors() {
        use crate::ui::color::rgb;
        let tree: E = div()
            .text_xs()
            .text_color(rgb(0xa3a3a3))
            .child(div().child("hi"))
            .into_any_element();

        let boxes = lay(&tree, (200.0, 200.0));
        let text = boxes.iter().find(|b| b.text == Some("hi")).expect("text box");
        assert_eq!(text.inherited.font_size, 12.0);
        assert_eq!(text.inherited.color, rgb(0xa3a3a3));
        assert_eq!(text.bounds.height, 16.0, "measured to the text_xs line box");
        assert_eq!(text.bounds.width, 200.0, "block text box fills its parent");
    }

    /// One `.opacity()` on a wrapper has to fade everything inside it, because
    /// that is what a screen crossfade is: the painter has no layer to fade, so
    /// the value is carried down here and applied per box.
    #[test]
    fn opacity_multiplies_all_the_way_down_the_tree() {
        let tree: E = div()
            .opacity(0.5)
            .child(div().child(div().opacity(0.5).child("x")))
            .into_any_element();

        let boxes = lay(&tree, (200.0, 200.0));
        assert_eq!(boxes[0].inherited.opacity, 0.5, "the wrapper itself");
        assert_eq!(boxes[1].inherited.opacity, 0.5, "a plain child inherits it");
        assert_eq!(boxes[2].inherited.opacity, 0.25, "and a dimmed one compounds with it");
        let text = boxes.iter().find(|b| b.text == Some("x")).expect("text box");
        assert_eq!(text.inherited.opacity, 0.25, "text leaves are faded too");
    }

    /// The outgoing screen of a page swap is painted but inert, and so is
    /// everything inside it.
    #[test]
    fn pointer_events_none_makes_the_whole_subtree_inert() {
        let tree: E = div()
            .child(div().pointer_events_none().child(div().child("ghost")))
            .child(div().child("live"))
            .into_any_element();

        let boxes = lay(&tree, (200.0, 200.0));
        let by_text = |t: &str| {
            boxes.iter().find(|b| b.text == Some(t)).unwrap_or_else(|| panic!("no {t}"))
        };
        assert!(!by_text("ghost").inherited.interactive, "inert all the way down");
        assert!(by_text("live").inherited.interactive, "and only inside that subtree");
    }

    /// `position: relative` + an inset shifts where a box is drawn without
    /// moving anything around it — how a card lifts into place during its
    /// entrance while the row it sits in stays put.
    #[test]
    fn a_relative_inset_offsets_the_box_without_reflowing_its_siblings() {
        let tree: E = v_flex()
            .w(px(100.))
            .child(div().h(px(20.)).relative().top(px(-6.)))
            .child(div().h(px(20.)))
            .into_any_element();

        let boxes = lay(&tree, (100.0, 100.0));
        assert_eq!(boxes[1].bounds.y, -6.0, "drawn six pixels up");
        assert_eq!(boxes[2].bounds.y, 20.0, "its sibling stays where the flow put it");
    }

    /// In a flex row -- which is what `h_flex()` gives, and what most of the
    /// app's text sits in -- the text box is sized by its measurement instead,
    /// because a flex item with `flex_grow: 0` does not stretch along the main
    /// axis. This is the case `truncate` + `min_w_0` is fighting in the sidebar.
    #[test]
    fn text_in_a_flex_row_is_sized_by_its_measurement() {
        let tree: E = h_flex().w(px(200.)).child("hi").into_any_element();
        let boxes = lay(&tree, (200.0, 200.0));
        let text = boxes.iter().find(|b| b.text == Some("hi")).expect("text box");
        // 2 chars at the 16px base size, 0.5 advance ratio under FixedMetrics.
        assert_eq!(text.bounds.width, 16.0);
    }
}
