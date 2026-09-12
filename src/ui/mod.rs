//! The interface layer: element tree, layout, paint.

pub mod color;
pub mod element;
pub mod layout;
pub mod event;
pub mod paint;
pub mod style;
pub mod svg;
pub mod text;
pub mod theme;
pub mod units;

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
