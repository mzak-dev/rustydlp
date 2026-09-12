//! Pointer routing: what is under the pointer, what a click runs, what scrolls.
//!
//! All of it reads the flat box list layout produced. Nothing here keeps state
//! except `ScrollState`, which lives in `layout` because layout is what consumes
//! it.

use super::layout::{Box_, inherited_clip};

/// Whether the box is under the pointer and not clipped away from it.
fn visible_at<S>(boxes: &[Box_<'_, S>], i: usize, x: f32, y: f32) -> bool {
    let b = &boxes[i];
    if !b.bounds.contains(x, y) {
        return false;
    }
    inherited_clip(boxes, i).is_none_or(|c| c.contains(x, y))
}

/// The topmost box under the pointer, or `None`.
///
/// Paint order is parent-before-child, so a reverse scan finds the topmost box
/// first — which is what makes the two absolutely-positioned modal overlays
/// capture the pointer instead of the pane behind them.
pub fn hit_test<S>(boxes: &[Box_<'_, S>], x: f32, y: f32) -> Option<usize> {
    (0..boxes.len()).rev().find(|&i| visible_at(boxes, i, x, y))
}

/// Runs the click handler nearest the pointer, searching the hit box and then
/// its ancestors. Returns whether anything handled it.
///
/// Ancestor search is what lets a click land on a row whose label is the box
/// actually under the pointer — the handler is on the row, not the text.
pub fn dispatch_click<S>(boxes: &[Box_<'_, S>], x: f32, y: f32, state: &mut S) -> bool {
    let Some(hit) = hit_test(boxes, x, y) else {
        return false;
    };
    let mut index = Some(hit);
    while let Some(i) = index {
        if let Some(handler) = boxes[i].node.and_then(|n| n.click_handler()) {
            handler(state);
            return true;
        }
        index = boxes[i].parent;
    }
    false
}

/// The scroll region a wheel event belongs to: the innermost scrollable box
/// under the pointer, with how far it can scroll.
pub fn scroll_target<S>(boxes: &[Box_<'_, S>], x: f32, y: f32) -> Option<(String, f32)> {
    let hit = hit_test(boxes, x, y)?;
    let mut index = Some(hit);
    while let Some(i) = index {
        let b = &boxes[i];
        if b.style.overflow_y_scroll == Some(true)
            && let Some(id) = b.node.and_then(|n| n.element_id())
        {
            return Some((id.to_string(), b.scroll_max));
        }
        index = b.parent;
    }
    None
}

/// Whether the pointer should be a hand here: the nearest `cursor_pointer` on
/// the hit box or an ancestor.
pub fn wants_pointer_cursor<S>(boxes: &[Box_<'_, S>], x: f32, y: f32) -> bool {
    let Some(hit) = hit_test(boxes, x, y) else {
        return false;
    };
    let mut index = Some(hit);
    while let Some(i) = index {
        if boxes[i].style.cursor_pointer == Some(true) {
            return true;
        }
        index = boxes[i].parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::element::{Element, div, v_flex};
    use crate::ui::layout::{FixedMetrics, ScrollState, layout};
    use crate::ui::style::Styled;
    use crate::ui::units::px;

    /// Counts clicks, standing in for app state.
    #[derive(Default)]
    struct Clicks {
        row: u32,
        backdrop: u32,
    }

    fn lay<'a>(
        tree: &'a Element<Clicks>,
        viewport: (f32, f32),
        scroll: &ScrollState,
    ) -> Vec<Box_<'a, Clicks>> {
        layout(tree, viewport, &mut FixedMetrics::default(), scroll)
    }

    /// A click on a row's label must reach the row's handler, because that is
    /// where `app.rs` puts it — on the row, not on the text inside it.
    #[test]
    fn a_click_bubbles_from_the_label_to_the_rows_handler() {
        let tree: Element<Clicks> = v_flex()
            .w(px(200.))
            .child(
                div()
                    .w(px(200.))
                    .h(px(40.))
                    .on_click(|c: &mut Clicks| c.row += 1)
                    .child("Some video title"),
            )
            .into_any_element();

        let boxes = lay(&tree, (200.0, 200.0), &ScrollState::default());
        let mut clicks = Clicks::default();
        assert!(dispatch_click(&boxes, 50.0, 20.0, &mut clicks));
        assert_eq!(clicks.row, 1);
    }

    /// The modal overlay covers the window, so a click anywhere must go to it and
    /// never to the pane underneath.
    #[test]
    fn an_overlay_captures_clicks_from_what_it_covers() {
        let tree: Element<Clicks> = div()
            .w(px(400.))
            .h(px(300.))
            .child(
                div()
                    .w(px(400.))
                    .h(px(300.))
                    .on_click(|c: &mut Clicks| c.row += 1),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .on_click(|c: &mut Clicks| c.backdrop += 1),
            )
            .into_any_element();

        let boxes = lay(&tree, (400.0, 300.0), &ScrollState::default());
        let mut clicks = Clicks::default();
        dispatch_click(&boxes, 200.0, 150.0, &mut clicks);
        assert_eq!((clicks.row, clicks.backdrop), (0, 1), "the overlay wins");
    }

    /// A scroll offset moves the content and nothing else, and is clamped to the
    /// content's overflow so a short list cannot be scrolled at all.
    #[test]
    fn scrolling_shifts_content_and_clamps_to_the_overflow() {
        let tree = || -> Element<Clicks> {
            v_flex()
                .id("job-list")
                .w(px(200.))
                .h(px(100.))
                .overflow_y_scroll()
                .children((0..10).map(|_| div().w(px(200.)).h(px(30.)).flex_shrink_0()))
                .into_any_element()
        };

        let t = tree();
        let boxes = lay(&t, (200.0, 100.0), &ScrollState::default());
        let region = &boxes[0];
        // 10 rows of 30 in a 100-tall box: 300 of content, 200 of overflow.
        assert_eq!(region.scroll_max, 200.0);
        assert_eq!(boxes[1].bounds.y, 0.0, "first row starts at the top");

        let mut scroll = ScrollState::default();
        assert!(scroll.scroll_by("job-list", 50.0, region.scroll_max));
        let boxes = lay(&t, (200.0, 100.0), &scroll);
        assert_eq!(boxes[1].bounds.y, -50.0, "content moved up");
        assert_eq!(boxes[0].bounds.y, 0.0, "the region itself did not move");

        // Past the end clamps, and a second attempt at the limit reports no move.
        scroll.scroll_by("job-list", 9999.0, region.scroll_max);
        assert!(!scroll.scroll_by("job-list", 10.0, region.scroll_max));
        let boxes = lay(&t, (200.0, 100.0), &scroll);
        assert_eq!(boxes[1].bounds.y, -200.0, "clamped to the overflow");
    }

    /// A row scrolled out of its region must not be clickable, or an invisible
    /// row steals clicks meant for whatever is drawn there.
    #[test]
    fn a_row_scrolled_out_of_view_is_not_clickable() {
        let tree: Element<Clicks> = v_flex()
            .id("job-list")
            .w(px(200.))
            .h(px(100.))
            .overflow_y_scroll()
            .child(
                div()
                    .w(px(200.))
                    .h(px(30.))
                    .flex_shrink_0()
                    .on_click(|c: &mut Clicks| c.row += 1),
            )
            .children((0..9).map(|_| div().w(px(200.)).h(px(30.)).flex_shrink_0()))
            .into_any_element();

        let mut scroll = ScrollState::default();
        scroll.scroll_by("job-list", 60.0, 200.0);
        let boxes = lay(&tree, (200.0, 100.0), &scroll);

        let mut clicks = Clicks::default();
        // The first row now sits at y -60..-30, above the region.
        dispatch_click(&boxes, 50.0, 10.0, &mut clicks);
        assert_eq!(clicks.row, 0, "the scrolled-away row must not receive it");
    }
}
