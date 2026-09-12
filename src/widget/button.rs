//! Buttons.
//!
//! Metrics read out of gpui-component's own `button.rs` at the pinned rev rather
//! than guessed: a small button with a label is `h_6` + `px_2`, an icon-only
//! small button is `size_6`, medium is `h_8` + `px_2p5` / `size_8`, and icon and
//! label are separated by `gap_1`.

use std::rc::Rc;

use super::icon::Icon;
use crate::ui::color::{Rgba, transparent};
use crate::ui::element::{Element, IntoElement, SharedString, h_flex};
use crate::ui::style::{StyleRefinement, Styled};
use crate::ui::theme::theme;
use crate::ui::units::px;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    #[default]
    Default,
    Primary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonSize {
    Small,
    #[default]
    Medium,
}

/// Background, foreground and border for a variant.
///
/// UNCONFIRMED: gpui-component's `default-theme.json` has no `button*` keys, so
/// these are derived from the tokens it does define — Default from `secondary`,
/// Primary from `primary`, Ghost transparent over `secondary_foreground`. Sample
/// the running legacy build during the parity pass before trusting them.
fn colors(variant: ButtonVariant, selected: bool) -> (Rgba, Rgba, Rgba) {
    let t = theme();
    let (bg, fg, border) = match variant {
        ButtonVariant::Default => (t.secondary, t.secondary_foreground, t.input_border),
        ButtonVariant::Primary => (t.primary, t.primary_foreground, t.primary),
        ButtonVariant::Ghost => (transparent(), t.secondary_foreground, transparent()),
        ButtonVariant::Danger => (t.danger, t.primary_foreground, t.danger),
    };
    if selected {
        (t.accent, t.accent_foreground, border)
    } else {
        (bg, fg, border)
    }
}

fn hover_bg(variant: ButtonVariant) -> Rgba {
    let t = theme();
    match variant {
        ButtonVariant::Default => t.secondary_hover,
        ButtonVariant::Primary => t.primary_hover,
        ButtonVariant::Ghost => t.accent,
        ButtonVariant::Danger => t.danger.opacity(0.85),
    }
}

type Handler<S> = Rc<dyn Fn(&mut S)>;

pub struct Button<S> {
    id: SharedString,
    label: Option<SharedString>,
    icon: Option<Icon>,
    variant: ButtonVariant,
    size: ButtonSize,
    selected: bool,
    on_click: Option<Handler<S>>,
    style: StyleRefinement,
}

impl<S> Button<S> {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Button {
            id: id.into(),
            label: None,
            icon: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            selected: false,
            on_click: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn small(mut self) -> Self {
        self.size = ButtonSize::Small;
        self
    }

    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    pub fn ghost(mut self) -> Self {
        self.variant = ButtonVariant::Ghost;
        self
    }

    pub fn danger(mut self) -> Self {
        self.variant = ButtonVariant::Danger;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut S) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl<S> Styled for Button<S> {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Lets `.icon(IconName::Plus)` and `.icon(Icon::empty().path(..))` both work.
impl From<super::icon::IconName> for Icon {
    fn from(name: super::icon::IconName) -> Icon {
        Icon::new(name)
    }
}

impl<S: 'static> IntoElement<S> for Button<S> {
    fn into_element(self) -> Element<S> {
        let t = theme();
        let (bg, fg, border) = colors(self.variant, self.selected);
        let icon_only = self.label.is_none();

        let mut el = h_flex()
            .id(self.id)
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .rounded(t.radius)
            .bg(bg)
            .text_color(fg)
            .cursor_pointer();

        if !border.is_transparent() {
            el = el.border_1().border_color(border);
        }

        // Sizes transcribed from gpui-component: icon-only buttons are square.
        el = match (self.size, icon_only) {
            (ButtonSize::Small, true) => el.w(px(24.)).h(px(24.)),
            (ButtonSize::Small, false) => el.h(px(24.)).px(8.0).gap_1(),
            (ButtonSize::Medium, true) => el.w(px(32.)).h(px(32.)),
            (ButtonSize::Medium, false) => el.h(px(32.)).px(10.0).gap_1(),
        };

        if self.size == ButtonSize::Small {
            el = el.text_sm();
        }

        let hover = hover_bg(self.variant);
        el = el.hover(move |s| s.bg(hover));

        if let Some(icon) = self.icon {
            el = el.child(icon.element::<S>());
        }
        if let Some(label) = self.label {
            el = el.child(label);
        }
        if let Some(handler) = self.on_click {
            el = el.on_click(move |state: &mut S| handler(state));
        }

        // Caller overrides (`.w_full()`, `.flex_shrink_0()`) layer on last so
        // they beat the size defaults above.
        let mut element = el;
        element.style().layer(&self.style);
        element.into_element()
    }
}
