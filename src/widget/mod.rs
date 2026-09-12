//! The widgets the interface needs — the same seven types the gpui build used
//! from gpui-component, minus the ones it turned out not to use: there is no
//! Progress widget (the job-row bar is two nested divs) and no Select.

pub mod button;
pub mod icon;
pub mod input;
pub mod slider;
pub mod switch;
pub mod tabs;

pub use button::{Button, ButtonSize, ButtonVariant};
pub use icon::{Icon, IconName};
pub use input::{Input, InputState};
pub use slider::{Slider, SliderState};
pub use switch::Switch;
pub use tabs::{Tab, TabBar};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Backend;
    use crate::render::raster::RasterBackend;
    use crate::ui::element::{Element, IntoElement, h_flex};
    use crate::ui::event::dispatch_click;
    use crate::ui::layout::{FixedMetrics, ScrollState, layout};
    use crate::ui::paint::{Painter, paint};
    use crate::ui::style::Styled;
    use crate::ui::theme::theme;
    use crate::ui::{transparent, units::px};

    #[derive(Default)]
    struct State {
        clicked: Option<usize>,
        toggled: Option<bool>,
    }

    fn render(tree: &Element<State>, w: u32, h: u32) -> RasterBackend {
        let mut backend = RasterBackend::new(w, h);
        backend.begin_frame(w, h, transparent());
        let boxes = layout(tree, (w as f32, h as f32), &mut FixedMetrics::default(), &ScrollState::default());
        let mut painter = Painter::new();
        paint(backend.canvas(), &boxes, &mut painter, None);
        backend
    }

    /// A primary button fills with the primary colour at the size gpui-component
    /// uses for a small labelled button: 24px tall.
    #[test]
    fn a_small_primary_button_paints_at_its_documented_size() {
        let tree: Element<State> = Button::<State>::new("go")
            .small()
            .primary()
            .label("Download")
            .into_element();

        let boxes = layout(&tree, (200.0, 60.0), &mut FixedMetrics::default(), &ScrollState::default());
        assert_eq!(boxes[0].bounds.height, 24.0, "small + label is h_6");

        let mut r = render(&tree, 200, 60);
        // Inside the button body, away from the label glyphs and the border.
        let (rr, gg, bb, aa) = r.pixel(4, 12);
        assert_eq!(aa, 0xff, "the button body is opaque");
        let p = theme().primary;
        assert_eq!(
            (rr, gg, bb),
            (
                (p.r * 255.0) as u8,
                (p.g * 255.0) as u8,
                (p.b * 255.0) as u8
            ),
            "primary variant fills with the primary token",
        );
    }

    /// An icon-only small button is square, per gpui-component's `size_6`.
    #[test]
    fn an_icon_only_button_is_square() {
        let tree: Element<State> =
            Button::<State>::new("play").small().icon(IconName::Play).into_element();
        let boxes = layout(&tree, (200.0, 60.0), &mut FixedMetrics::default(), &ScrollState::default());
        assert_eq!((boxes[0].bounds.width, boxes[0].bounds.height), (24.0, 24.0));
    }

    /// Clicking a tab reports its index, which is what drives SidebarTab.
    #[test]
    fn clicking_a_tab_reports_its_index() {
        let tree: Element<State> = TabBar::<State>::new("navbar-tabs")
            .segmented()
            .selected_index(0)
            .children([
                Tab::new().label("Download"),
                Tab::new().label("Convert"),
                Tab::new().label("In Progress"),
            ])
            .on_click(|s: &mut State, ix| s.clicked = Some(ix))
            .into_element();

        let boxes = layout(&tree, (400.0, 40.0), &mut FixedMetrics::default(), &ScrollState::default());
        // Click inside the second tab.
        let second = boxes
            .iter()
            .filter(|b| b.node.and_then(|n| n.click_handler()).is_some())
            .nth(1)
            .expect("three clickable tabs");
        let (x, y) = (second.bounds.x + 2.0, second.bounds.y + 2.0);

        let mut state = State::default();
        assert!(dispatch_click(&boxes, x, y, &mut state));
        assert_eq!(state.clicked, Some(1));
    }

    /// A switch reports the value it is moving to, not the one it had — the shape
    /// the old `|this, checked, ..|` handlers expect.
    #[test]
    fn a_switch_reports_the_value_it_moves_to() {
        let on: Element<State> = Switch::<State>::new("sw")
            .checked(true)
            .label("Use as default")
            .on_click(|s: &mut State, checked| s.toggled = Some(checked))
            .into_element();

        let boxes = layout(&on, (200.0, 40.0), &mut FixedMetrics::default(), &ScrollState::default());
        let mut state = State::default();
        dispatch_click(&boxes, 10.0, 10.0, &mut state);
        assert_eq!(state.toggled, Some(false), "a checked switch moves to false");
    }

    /// The thumb sits at the far end when checked and at the near end when not,
    /// which is the only visual difference between the two states' geometry.
    #[test]
    fn the_switch_thumb_moves_with_the_state() {
        let thumb_x = |checked: bool| {
            let tree: Element<State> =
                Switch::<State>::new("sw").checked(checked).into_element();
            let boxes = layout(&tree, (200.0, 40.0), &mut FixedMetrics::default(), &ScrollState::default());
            // The deepest box is the thumb.
            boxes.last().expect("thumb").bounds.x
        };
        assert!(thumb_x(true) > thumb_x(false), "checked pushes the thumb right");
    }

    /// Caller style overrides win over the widget's own defaults, so `.w_full()`
    /// on a button still stretches it.
    #[test]
    fn caller_overrides_beat_the_widgets_defaults() {
        let tree: Element<State> = h_flex()
            .w(px(300.))
            .child(Button::<State>::new("b").small().label("Convert File").w_full())
            .into_element();
        let boxes = layout(&tree, (300.0, 40.0), &mut FixedMetrics::default(), &ScrollState::default());
        assert_eq!(boxes[1].bounds.width, 300.0, "w_full beat the intrinsic width");
    }
}
