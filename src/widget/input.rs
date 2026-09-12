//! Single-line text fields.
//!
//! Built on `cosmic_text::Editor`, which owns the fiddly parts: click-to-position
//! and drag-selection against shaped glyphs, grapheme-correct motion and
//! deletion, and a caret position that is computed from the glyph run rather than
//! from a prefix width — so it stays correct inside an RTL run, where the caret
//! is emphatically not at the prefix's advance.
//!
//! Every field in the app is single-line, so `Wrap::None` is set and `Enter` is
//! never issued; pasted newlines are stripped rather than accepted.
//!
//! Buffers here are shaped against the same `FontSystem` the painter uses
//! (`Shaper::fonts_mut`), so the caret is measured with the metrics the text is
//! actually drawn with.

use cosmic_text::{
    Action, Attrs, Buffer, Edit, Editor, FontSystem, Metrics, Motion, Shaping, Wrap,
};

use crate::ui::color::Rgba;
use crate::ui::element::{Element, IntoElement, SharedString, div, h_flex};
use crate::ui::style::Styled;
use crate::ui::theme::{BASE_FONT_SIZE, BASE_LINE_HEIGHT, theme};
use crate::ui::units::px;

/// Horizontal padding inside the field, and so the caret's resting offset.
const PAD_X: f32 = 8.0;
const HEIGHT: f32 = 32.0;
const CARET_W: f32 = 1.0;

pub struct InputState {
    editor: Editor<'static>,
    placeholder: SharedString,
    focused: bool,
}

impl InputState {
    pub fn new(fonts: &mut FontSystem) -> Self {
        let metrics = Metrics::new(BASE_FONT_SIZE.0, BASE_FONT_SIZE.0 * BASE_LINE_HEIGHT);
        let mut buffer = Buffer::new(fonts, metrics);
        // Single line: never wrap, and never accept a newline.
        buffer.set_wrap(fonts, Wrap::None);
        buffer.set_text(fonts, "", &Attrs::new(), Shaping::Advanced);
        InputState {
            editor: Editor::new(buffer),
            placeholder: SharedString::from(""),
            focused: false,
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.editor.set_selection(cosmic_text::Selection::None);
        }
    }

    /// The field's text. Single-line, so this is the first buffer line.
    pub fn value(&self) -> String {
        self.editor.with_buffer(|b| {
            b.lines.first().map(|l| l.text().to_string()).unwrap_or_default()
        })
    }

    pub fn set_value(&mut self, fonts: &mut FontSystem, value: &str) {
        let clean = strip_newlines(value);
        self.editor.with_buffer_mut(|b| {
            b.set_text(fonts, &clean, &Attrs::new(), Shaping::Advanced)
        });
        self.editor.action(fonts, Action::Motion(Motion::End));
        self.editor.set_selection(cosmic_text::Selection::None);
    }

    /// A typed character. Newlines are dropped: these fields are single-line, and
    /// the old ones reacted to Enter by submitting rather than by growing.
    pub fn insert_char(&mut self, fonts: &mut FontSystem, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        self.editor.action(fonts, Action::Insert(c));
    }

    pub fn backspace(&mut self, fonts: &mut FontSystem) {
        self.editor.action(fonts, Action::Backspace);
    }

    pub fn delete(&mut self, fonts: &mut FontSystem) {
        self.editor.action(fonts, Action::Delete);
    }

    pub fn motion(&mut self, fonts: &mut FontSystem, motion: Motion) {
        self.editor.action(fonts, Action::Motion(motion));
    }

    /// Places the caret from a click at `x` relative to the field's text origin.
    pub fn click(&mut self, fonts: &mut FontSystem, x: f32) {
        self.editor.action(fonts, Action::Click { x: x as i32, y: 0 });
    }

    /// Extends the selection to `x` — the drag half of click-and-drag selection.
    pub fn drag(&mut self, fonts: &mut FontSystem, x: f32) {
        self.editor.action(fonts, Action::Drag { x: x as i32, y: 0 });
    }

    pub fn select_all(&mut self, fonts: &mut FontSystem) {
        self.editor.action(fonts, Action::Motion(Motion::Home));
        self.editor.set_selection(cosmic_text::Selection::Normal(self.editor.cursor()));
        self.editor.action(fonts, Action::Motion(Motion::End));
    }

    pub fn copy(&self) -> Option<String> {
        self.editor.copy_selection()
    }

    /// Pastes, stripping newlines. This is the field's main interaction: the
    /// primary one reads "Paste a video or playlist URL".
    pub fn paste(&mut self, fonts: &mut FontSystem, text: &str) {
        let clean = strip_newlines(text);
        self.editor.delete_selection();
        self.editor.insert_string(&clean, None);
        self.editor.shape_as_needed(fonts, false);
    }

    pub fn cut(&mut self, fonts: &mut FontSystem) -> Option<String> {
        let copied = self.editor.copy_selection();
        if self.editor.delete_selection() {
            self.editor.shape_as_needed(fonts, false);
        }
        copied
    }

    /// The caret's offset from the text origin, in pixels.
    ///
    /// Taken from cosmic-text's own cursor positioning, which walks the glyph run
    /// and accounts for RTL, rather than measuring the prefix.
    pub fn caret_x(&self) -> f32 {
        self.editor.cursor_position().map(|(x, _)| x as f32).unwrap_or(0.0)
    }

    /// The selected range as pixel offsets from the text origin, if any.
    ///
    /// Derived by walking the line's glyphs, which is exact for a single LTR run.
    /// A selection spanning a bidi boundary would be drawn as one span rather
    /// than as the two visual runs it really occupies — the app's fields hold
    /// URLs, paths and yt-dlp arguments, so that case is documented rather than
    /// handled.
    pub fn selection_span(&self) -> Option<(f32, f32)> {
        let (start, end) = self.editor.selection_bounds()?;
        if start.index == end.index {
            return None;
        }
        let x = |index: usize| -> f32 {
            self.editor.with_buffer(|b| {
                let mut edge: f32 = 0.0;
                for run in b.layout_runs() {
                    for g in run.glyphs {
                        if g.start < index {
                            edge = edge.max(g.x + g.w);
                        }
                    }
                }
                edge
            })
        };
        Some((x(start.index), x(end.index)))
    }
}

fn strip_newlines(s: &str) -> String {
    s.chars().filter(|c| *c != '\n' && *c != '\r').collect()
}

/// Renders an `InputState`.
pub struct Input<'a> {
    id: SharedString,
    state: &'a InputState,
}

impl<'a> Input<'a> {
    pub fn new(id: impl Into<SharedString>, state: &'a InputState) -> Self {
        Input { id: id.into(), state }
    }
}

impl<S> IntoElement<S> for Input<'_> {
    fn into_element(self) -> Element<S> {
        let t = theme();
        let value = self.state.value();
        let empty = value.is_empty();

        // The text layer is relative to the field's padding box, so the caret and
        // the selection share the text's origin.
        let text_color: Rgba = if empty { t.muted_foreground } else { t.foreground };
        let shown: SharedString = if empty {
            self.state.placeholder.clone()
        } else {
            SharedString::from(value)
        };

        let mut layer = div().relative().flex_1().min_w_0().h_full().flex().items_center();

        if let Some((from, to)) = self.state.selection_span() {
            layer = layer.child(
                div()
                    .absolute()
                    .top(px(6.))
                    .ml(from)
                    .w(px((to - from).max(1.0)))
                    .h(px(20.))
                    .bg(t.accent),
            );
        }

        layer = layer.child(div().truncate().text_color(text_color).child(shown));

        if self.state.focused {
            layer = layer.child(
                div()
                    .absolute()
                    .top(px(6.))
                    .ml(self.state.caret_x())
                    .w(px(CARET_W))
                    .h(px(20.))
                    .bg(t.foreground),
            );
        }

        h_flex()
            .id(self.id)
            .w_full()
            .h(px(HEIGHT))
            .px(PAD_X)
            .items_center()
            .rounded(t.radius)
            .border_1()
            .border_color(if self.state.focused { t.ring } else { t.input_border })
            .bg(t.background)
            .child(layer)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (FontSystem, InputState) {
        let mut fonts = FontSystem::new();
        let input = InputState::new(&mut fonts).placeholder("Paste a video or playlist URL");
        (fonts, input)
    }

    /// Typing and deleting have to round-trip through the editor's own buffer,
    /// since that buffer is the single source of truth for the field.
    #[test]
    fn typing_and_backspace_round_trip() {
        let (mut fonts, mut s) = state();
        for c in "abc".chars() {
            s.insert_char(&mut fonts, c);
        }
        assert_eq!(s.value(), "abc");
        s.backspace(&mut fonts);
        assert_eq!(s.value(), "ab");
    }

    /// The field is single-line: a typed newline is dropped and a pasted one is
    /// stripped, so a multi-line clipboard cannot turn one field into two lines.
    #[test]
    fn newlines_never_enter_a_single_line_field() {
        let (mut fonts, mut s) = state();
        s.insert_char(&mut fonts, 'a');
        s.insert_char(&mut fonts, '\n');
        s.insert_char(&mut fonts, 'b');
        assert_eq!(s.value(), "ab");

        s.set_value(&mut fonts, "");
        s.paste(&mut fonts, "https://x/y\nhttps://a/b\r\n");
        assert_eq!(s.value(), "https://x/yhttps://a/b");
    }

    /// Pasting is the field's main interaction, so it has to land at the caret
    /// and replace whatever was selected.
    #[test]
    fn paste_replaces_the_selection() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "old");
        s.select_all(&mut fonts);
        s.paste(&mut fonts, "https://example/v");
        assert_eq!(s.value(), "https://example/v");
    }

    /// set_value leaves the caret at the end, which is what the preset editor
    /// needs when it loads a preset into the fields.
    #[test]
    fn set_value_leaves_the_caret_at_the_end() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "1080");
        s.insert_char(&mut fonts, 'p');
        assert_eq!(s.value(), "1080p");
    }

    /// The caret advances as text is typed, and Home returns it to the origin.
    #[test]
    fn the_caret_tracks_the_cursor() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "some text");
        let at_end = s.caret_x();
        assert!(at_end > 0.0, "caret should have advanced past the origin");
        s.motion(&mut fonts, Motion::Home);
        assert_eq!(s.caret_x(), 0.0);
    }

    /// Non-ASCII has to behave: deleting removes one grapheme, not one byte, so
    /// a CJK filename cannot be corrupted mid-character.
    #[test]
    fn backspace_deletes_a_whole_character_not_a_byte() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "日本語");
        s.backspace(&mut fonts);
        assert_eq!(s.value(), "日本");
    }

    /// Select-all then copy yields the whole field, which is what Ctrl+A Ctrl+C
    /// has to do.
    #[test]
    fn select_all_then_copy_returns_everything() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "C:/dl/video.mkv");
        s.select_all(&mut fonts);
        assert_eq!(s.copy().as_deref(), Some("C:/dl/video.mkv"));
        assert!(s.selection_span().is_some(), "a selection should have a span");
    }

    /// Cut removes the selection and hands the text back.
    #[test]
    fn cut_empties_the_field_and_returns_the_text() {
        let (mut fonts, mut s) = state();
        s.set_value(&mut fonts, "abc");
        s.select_all(&mut fonts);
        assert_eq!(s.cut(&mut fonts).as_deref(), Some("abc"));
        assert_eq!(s.value(), "");
    }
}
