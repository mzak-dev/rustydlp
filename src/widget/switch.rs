//! A labelled toggle.
//!
//! Track and thumb sizes are gpui-component's medium switch, read from its
//! source: a 36x20 track with a 16px thumb inset by 2, and a pill radius because
//! the theme radius is at least 4.

use std::rc::Rc;

use crate::ui::element::{Element, IntoElement, SharedString, div, h_flex};
use crate::ui::style::Styled;
use crate::ui::theme::theme;
use crate::ui::units::px;

const TRACK_W: f32 = 36.0;
const TRACK_H: f32 = 20.0;
const THUMB: f32 = 16.0;
const INSET: f32 = 2.0;

type Handler<S> = Rc<dyn Fn(&mut S, bool)>;

pub struct Switch<S> {
    id: SharedString,
    checked: bool,
    label: Option<SharedString>,
    on_click: Option<Handler<S>>,
}

impl<S> Switch<S> {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Switch { id: id.into(), checked: false, label: None, on_click: None }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The handler receives the value the switch is moving *to*, which is how the
    /// old call sites read (`|this, checked, ..|`).
    pub fn on_click(mut self, handler: impl Fn(&mut S, bool) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl<S: 'static> IntoElement<S> for Switch<S> {
    fn into_element(self) -> Element<S> {
        let t = theme();
        let checked = self.checked;

        // The thumb is positioned by padding rather than an offset, so the track
        // needs no absolute child: padding-left flips between the two ends.
        let lead = if checked { TRACK_W - THUMB - INSET } else { INSET };
        let track = div()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .flex_shrink_0()
            .rounded_full()
            .bg(if checked { t.primary } else { t.switch_track })
            .flex()
            .items_center()
            .pl(lead)
            .child(
                div()
                    .w(px(THUMB))
                    .h(px(THUMB))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(if checked { t.primary_foreground } else { t.foreground }),
            );

        let mut el = h_flex().id(self.id).items_center().gap_2().cursor_pointer().child(track);
        if let Some(label) = self.label {
            el = el.child(label);
        }
        if let Some(handler) = self.on_click {
            el = el.on_click(move |state: &mut S| handler(state, !checked));
        }
        el.into_element()
    }
}
