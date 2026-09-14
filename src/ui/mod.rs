//! The interface layer: element tree, layout, paint.

pub mod anim;
pub mod color;
pub mod element;
pub mod event;
pub mod layout;
/// Draws laid-out boxes onto a Skia canvas. Native-only: see `crate::web::paint`
/// for the wasm32 equivalent, which draws onto a tiny-skia pixmap instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod paint;
pub mod style;
pub mod svg;
/// Shapes with cosmic-text, rasterizes with Skia's own glyph atlas.
/// Native-only: see `crate::web::text` for the wasm32 equivalent, which
/// rasterizes with cosmic-text's own `SwashCache` instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod text;
pub mod theme;
pub mod units;

pub use anim::Animator;
pub use color::{Rgba, black, rgb, transparent};
pub use element::{
    AnyElement, Content, Div, Element, ImageSource, IntoElement, SharedString, TextNode, div,
    h_flex, img, svg, v_flex,
};
pub use style::{
    AlignItems, Edges, FlexDirection, FluentBuilder, JustifyContent, ObjectFit, StyleRefinement,
    Styled,
};
pub use theme::{BASE_FONT_SIZE, BASE_LINE_HEIGHT, Theme, theme};
pub use units::{Bounds, Length, Pixels, px, relative};
