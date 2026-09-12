//! Icons: an embedded SVG drawn as a mask tinted by the surrounding text colour.

use crate::ui::element::{Div, Element, IntoElement, SharedString, svg};
use crate::ui::style::Styled;
use crate::ui::units::{Pixels, px};

/// The Lucide icons the interface uses, vendored under `assets/icons`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
    ChevronLeft,
    ChevronRight,
    Folder,
    Pause,
    Play,
    Plus,
    Replace,
    Settings,
}

impl IconName {
    pub fn path(self) -> &'static str {
        match self {
            IconName::ChevronLeft => "icons/chevron-left.svg",
            IconName::ChevronRight => "icons/chevron-right.svg",
            IconName::Folder => "icons/folder.svg",
            IconName::Pause => "icons/pause.svg",
            IconName::Play => "icons/play.svg",
            IconName::Plus => "icons/plus.svg",
            IconName::Replace => "icons/replace.svg",
            IconName::Settings => "icons/settings.svg",
        }
    }
}

/// gpui-component's default icon box.
pub const DEFAULT_ICON_SIZE: Pixels = px(16.);

#[derive(Clone)]
pub struct Icon {
    path: SharedString,
    size: Pixels,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Icon { path: SharedString::from(name.path()), size: DEFAULT_ICON_SIZE }
    }

    /// An icon with no art yet, for the call sites that pick a path at runtime —
    /// the player's volume toggle swaps between two of them.
    pub fn empty() -> Self {
        Icon { path: SharedString::from(""), size: DEFAULT_ICON_SIZE }
    }

    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.path = path.into();
        self
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub(crate) fn element<S>(self) -> Div<S> {
        svg().path(self.path).w(self.size).h(self.size).flex_shrink_0()
    }
}

impl<S> IntoElement<S> for Icon {
    fn into_element(self) -> Element<S> {
        self.element().into_element()
    }
}
