//! The element tree.
//!
//! Generic over the application state `S` so that click handlers can be plain
//! `Fn(&mut S)` closures. That is what lets a ported handler keep the shape it
//! had under gpui's `cx.listener(|this, ..| ..)` — the closure still receives
//! `&mut RustyDlp` — without the element tree knowing anything about the app.
//! `app.rs` writes one alias (`type AnyElement = ui::AnyElement<RustyDlp>;`)
//! and its ~30 element-returning signatures are unchanged.

use std::sync::Arc;

use super::style::{StyleRefinement, Styled};

/// Cheap-to-clone immutable string, standing in for `gpui::SharedString`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedString(Arc<str>);

impl From<&str> for SharedString {
    fn from(s: &str) -> Self {
        SharedString(Arc::from(s))
    }
}

impl From<String> for SharedString {
    fn from(s: String) -> Self {
        SharedString(Arc::from(s.as_str()))
    }
}

impl std::ops::Deref for SharedString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SharedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A click handler. Takes the app state directly, so a ported handler keeps the
/// shape `cx.listener(|this, ..| ..)` gave it.
pub type ClickHandler<S> = Box<dyn Fn(&mut S)>;

/// A hover-enter/leave handler: `true` when the pointer enters the box,
/// `false` when it leaves. Requires `.id()` (see `Div::on_hover`), since the
/// shell tracks *which* box is hovered by that id across frames — the tree is
/// rebuilt every frame, so there is no stable box index to compare against.
pub type HoverHandler<S> = Box<dyn Fn(&mut S, bool)>;

/// What a box paints inside itself, beyond its own background and border.
pub enum Content<S> {
    Children(Vec<Element<S>>),
    /// A decoded RGBA frame or a file on disk, drawn to fit the box.
    Image(ImageSource),
    /// An SVG drawn as an alpha mask and filled with the box's text colour —
    /// which is how gpui's `svg()` behaves, and how both call sites in `app.rs`
    /// use it (`.path(..).text_color(..)`).
    Svg(SharedString),
    /// An SVG drawn with its own colours, unlike `Svg` above. See `color_svg`.
    ColorSvg(SharedString),
}

pub enum ImageSource {
    /// Tightly packed RGBA8, `width * height * 4`.
    Rgba {
        width: u32,
        height: u32,
        data: Arc<Vec<u8>>,
        /// A caller-chosen tag identifying this exact buffer, distinct from
        /// any other one the caller will ever produce -- the video player
        /// uses its frame's presentation timestamp bits, since two frames
        /// never share one. Not `Arc::as_ptr(&data)`: the paint-side cache
        /// this exists for (`Painter`, in `ui/paint.rs`) is checked *every*
        /// redraw, including the many that repaint an unchanged frame while
        /// waiting for the next one, and an allocator that reuses a
        /// same-sized just-freed block (which every video frame is) would
        /// otherwise make a genuinely new frame look identical to the one
        /// before it.
        id: u64,
    },
    Path(std::path::PathBuf),
}

/// A styled box: gpui's `div()`.
pub struct Div<S> {
    pub(crate) style: StyleRefinement,
    /// Applied over `style` only while the pointer is inside the box.
    pub(crate) hover_style: Option<StyleRefinement>,
    /// Retained-state identity. Required before `overflow_y_scroll`, because a
    /// scroll offset has to survive across frames.
    pub(crate) id: Option<SharedString>,
    pub(crate) content: Content<S>,
    pub(crate) on_click: Option<ClickHandler<S>>,
    pub(crate) on_hover: Option<HoverHandler<S>>,
}

pub fn div<S>() -> Div<S> {
    Div {
        style: StyleRefinement::default(),
        hover_style: None,
        id: None,
        content: Content::Children(Vec::new()),
        on_click: None,
        on_hover: None,
    }
}

/// `div().flex().flex_row()`, as gpui-component spells it.
pub fn h_flex<S>() -> Div<S> {
    div().flex_row()
}

/// `div().flex().flex_col()`.
pub fn v_flex<S>() -> Div<S> {
    div().flex_col()
}

/// An image box. Sizing and `object_fit` come from the style, as in gpui.
pub fn img<S>(source: impl Into<ImageSource>) -> Div<S> {
    let mut d = div();
    d.content = Content::Image(source.into());
    d
}

/// An SVG box, painted as a mask tinted with the box's text colour.
pub fn svg<S>() -> Div<S> {
    let mut d = div();
    d.content = Content::Svg(SharedString::from(""));
    d
}

/// An SVG box painted with its own colours, not tinted -- for a logo mark
/// like the navbar's, where the whole point is the colour. `svg()` would
/// flatten it to a silhouette in the box's text colour, same as every
/// Lucide icon it draws; this is the one exception, so it is its own
/// element rather than a flag on that one.
pub fn color_svg<S>(path: impl Into<SharedString>) -> Div<S> {
    let mut d = div();
    d.content = Content::ColorSvg(path.into());
    d
}

impl From<std::path::PathBuf> for ImageSource {
    fn from(p: std::path::PathBuf) -> Self {
        ImageSource::Path(p)
    }
}

impl<S> Div<S> {
    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The SVG to draw. Mirrors `gpui::svg().path(..)`.
    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.content = Content::Svg(path.into());
        self
    }

    pub fn child(mut self, child: impl IntoElement<S>) -> Self {
        if let Content::Children(kids) = &mut self.content {
            kids.push(child.into_element());
        }
        self
    }

    pub fn children<I>(mut self, children: I) -> Self
    where
        I: IntoIterator,
        I::Item: IntoElement<S>,
    {
        if let Content::Children(kids) = &mut self.content {
            kids.extend(children.into_iter().map(IntoElement::into_element));
        }
        self
    }

    /// Style overrides that apply only while hovered.
    pub fn hover(mut self, f: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self {
        self.hover_style = Some(f(StyleRefinement::default()));
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut S) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// Runs `handler(state, true)` the frame the pointer enters this box (or
    /// the nearest ancestor of the hit box that has one, same ancestor-walk
    /// `on_click` gets), and `handler(state, false)` the frame it leaves.
    /// Needs `.id()` — see `HoverHandler`.
    pub fn on_hover(mut self, handler: impl Fn(&mut S, bool) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    pub fn content(&self) -> &Content<S> {
        &self.content
    }

    pub fn hover_style(&self) -> Option<&StyleRefinement> {
        self.hover_style.as_ref()
    }

    pub fn click_handler(&self) -> Option<&ClickHandler<S>> {
        self.on_click.as_ref()
    }

    pub fn hover_handler(&self) -> Option<&HoverHandler<S>> {
        self.on_hover.as_ref()
    }

    pub fn element_id(&self) -> Option<&SharedString> {
        self.id.as_ref()
    }

    pub fn into_any_element(self) -> Element<S> {
        Element::Node(Box::new(self))
    }
}

impl<S> Styled for Div<S> {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// A run of text. Inherits colour, size and weight from its ancestors, so a
/// bare `.child("Settings")` picks up whatever the enclosing box set.
pub struct TextNode {
    pub(crate) text: SharedString,
    pub(crate) style: StyleRefinement,
}

impl Styled for TextNode {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Both variants are boxed: each carries a `StyleRefinement`, which is wide, and
/// elements get moved around a lot while a tree is built.
pub enum Element<S> {
    Node(Box<Div<S>>),
    Text(Box<TextNode>),
}

/// The type-erased element `app.rs` passes around. Already an enum rather than
/// a trait object, so erasing costs nothing.
pub type AnyElement<S> = Element<S>;

pub trait IntoElement<S> {
    fn into_element(self) -> Element<S>;
}

/// `.into_any_element()` under gpui's name, so ported call sites keep it.
pub trait IntoAnyElement<S>: IntoElement<S> {
    fn into_any_element(self) -> Element<S>
    where
        Self: Sized,
    {
        self.into_element()
    }
}

impl<S, T: IntoElement<S>> IntoAnyElement<S> for T {}

impl<S> IntoElement<S> for Element<S> {
    fn into_element(self) -> Element<S> {
        self
    }
}

impl<S> IntoElement<S> for Div<S> {
    fn into_element(self) -> Element<S> {
        Element::Node(Box::new(self))
    }
}

fn text_element<S>(text: SharedString) -> Element<S> {
    Element::Text(Box::new(TextNode { text, style: StyleRefinement::default() }))
}

impl<S> IntoElement<S> for &str {
    fn into_element(self) -> Element<S> {
        text_element(SharedString::from(self))
    }
}

impl<S> IntoElement<S> for String {
    fn into_element(self) -> Element<S> {
        text_element(SharedString::from(self))
    }
}

impl<S> IntoElement<S> for SharedString {
    fn into_element(self) -> Element<S> {
        text_element(self)
    }
}

impl<S> Element<S> {
    pub(crate) fn style(&self) -> &StyleRefinement {
        match self {
            Element::Node(d) => &d.style,
            Element::Text(t) => &t.style,
        }
    }
}
