//! The segmented tab bar in the navbar.
//!
//! UNCONFIRMED metrics: gpui-component's segmented variant sizes its tabs
//! through a variant/size matrix and animates the active indicator. The static
//! look is reproduced here (the animated version was on the closed PR #9);
//! sample the running legacy build during the parity pass.

use std::rc::Rc;

use crate::ui::element::{Element, IntoElement, SharedString, div, h_flex};
use crate::ui::style::Styled;
use crate::ui::theme::theme;
use crate::ui::units::px;

pub struct Tab {
    label: SharedString,
}

impl Tab {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Tab { label: SharedString::from("") }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }
}

type Handler<S> = Rc<dyn Fn(&mut S, usize)>;

pub struct TabBar<S> {
    id: SharedString,
    tabs: Vec<Tab>,
    selected: usize,
    segmented: bool,
    on_click: Option<Handler<S>>,
}

impl<S> TabBar<S> {
    pub fn new(id: impl Into<SharedString>) -> Self {
        TabBar {
            id: id.into(),
            tabs: Vec::new(),
            selected: 0,
            segmented: false,
            on_click: None,
        }
    }

    pub fn segmented(mut self) -> Self {
        self.segmented = true;
        self
    }

    pub fn selected_index(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    pub fn children(mut self, tabs: impl IntoIterator<Item = Tab>) -> Self {
        self.tabs.extend(tabs);
        self
    }

    /// Receives the index of the tab that was clicked.
    pub fn on_click(mut self, handler: impl Fn(&mut S, usize) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl<S: 'static> IntoElement<S> for TabBar<S> {
    fn into_element(self) -> Element<S> {
        let t = theme();
        let selected = self.selected;
        let handler = self.on_click;

        let tabs = self.tabs.into_iter().enumerate().map(|(i, tab)| {
            let active = i == selected;
            let mut el = div()
                .id(SharedString::from(format!("tab-{i}")))
                .h(px(28.))
                .px(12.0)
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius)
                .text_sm()
                .cursor_pointer()
                .text_color(if active { t.tab_active_foreground } else { t.tab_foreground })
                .child(tab.label);
            if active {
                el = el.bg(t.tab_active);
            } else {
                el = el.hover(move |s| s.bg(t.accent.opacity(0.6)));
            }
            if let Some(h) = handler.clone() {
                el = el.on_click(move |state: &mut S| h(state, i));
            }
            el
        });

        let mut bar = h_flex().id(self.id).items_center().gap_1();
        if self.segmented {
            bar = bar.p(2.0).rounded(t.radius).bg(t.muted);
        }
        bar.children(tabs).into_element()
    }
}
