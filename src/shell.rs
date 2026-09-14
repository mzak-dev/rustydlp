//! The window and its event loop.
//!
//! winit 0.30's `ApplicationHandler` rather than the old run-closure:
//! `resumed` creates the window and surface, `window_event` routes input,
//! `user_event` is woken when a worker has posted an update, and
//! `about_to_wait` asks for the next frame when something has changed.
//!
//! Redraws are demand-driven, as they were under gpui: the app is a static
//! picture between events, and a frame is only built when an event, an update
//! from a worker, or live playback has actually changed something.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use cosmic_text::{FontSystem, Motion};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

use crate::app::{RustyDlp, SliderKind, Update, Updates, WindowAction};
use crate::render::Backend;
use crate::render::soft::SoftBackend;
use crate::ui::element::SharedString;
use crate::ui::event::{dispatch_click, dispatch_hover, hovered_id, scroll_target, wants_pointer_cursor};
use crate::ui::layout::{ScrollState, layout};
use crate::ui::paint::{Painter, paint};
use crate::ui::text::Shaper;
use crate::ui::theme::theme;

/// The size `main.rs` opened the gpui window at.
pub const WINDOW_SIZE: (f32, f32) = (1180.0, 760.0);

/// The floor `drag_resize_window` (see `resize_direction_at`) can shrink the
/// window to. Below roughly this, the navbar's three columns -- each with
/// their own minimum width -- have nowhere left to give and start fighting
/// each other, and the sidebar's fixed-width action buttons run out of room
/// alongside the job list above them. Rounded well above both of those
/// measured floors rather than pared to the exact pixel, since text metrics
/// (and so the navbar's actual minimum) depend on the host's fonts.
const MIN_WINDOW_SIZE: (f32, f32) = (760.0, 480.0);

/// How often to rebuild a frame while the player is running. The decode threads
/// pace themselves to the source's own fps; this only bounds how often the
/// picture on screen is refreshed.
const PLAYBACK_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// How close to the window's own edge still grabs it for resizing, in logical
/// pixels. Undecorated windows (see `with_decorations(false)` below) come with
/// no OS-drawn resize border at all, so without this the window can only ever
/// be dragged, never resized, once the native chrome is gone.
const RESIZE_BORDER: f32 = 6.0;

/// Which edge or corner `(x, y)` falls within `RESIZE_BORDER` of, if any.
/// Corners are checked first so the last couple of pixels at a corner resize
/// diagonally rather than only ever picking one axis.
fn resize_direction_at(size: (u32, u32), x: f32, y: f32) -> Option<ResizeDirection> {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let (left, right) = (x <= RESIZE_BORDER, x >= w - RESIZE_BORDER);
    let (top, bottom) = (y <= RESIZE_BORDER, y >= h - RESIZE_BORDER);
    use ResizeDirection::*;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(NorthWest),
        (_, true, true, _) => Some(NorthEast),
        (true, _, _, true) => Some(SouthWest),
        (_, true, _, true) => Some(SouthEast),
        (true, false, false, false) => Some(West),
        (false, true, false, false) => Some(East),
        (false, false, true, false) => Some(North),
        (false, false, false, true) => Some(South),
        _ => None,
    }
}

pub struct Shell {
    app: RustyDlp,
    painter: Painter,
    scroll: ScrollState,
    /// D3D12 when a hardware adapter is available, CPU + softbuffer otherwise.
    /// Created with the window in `resumed`.
    backend: Option<Box<dyn Backend>>,
    window: Option<Arc<Window>>,
    updates: futures_channel::mpsc::UnboundedReceiver<Update>,
    fonts: Rc<RefCell<FontSystem>>,
    pointer: Option<(f32, f32)>,
    modifiers: ModifiersState,
    /// Set while the pointer is down inside a slider, so a drag keeps steering
    /// the same one after the pointer leaves its box.
    dragging: Option<SliderKind>,
    dirty: bool,
    size: (u32, u32),
    /// When the last frame started, so the playback timer counts from there
    /// rather than from after a vsync-blocked present.
    last_redraw: std::time::Instant,
    /// The id of the box a hover handler last fired "entered" for, so a move
    /// that leaves it can fire "left" — see `crate::ui::event::dispatch_hover`.
    hovered: Option<SharedString>,
}

impl Shell {
    pub fn new() -> Self {
        let fonts = Rc::new(RefCell::new(FontSystem::new()));
        let painter = Painter {
            shaper: Shaper::with_shared_fonts(fonts.clone()),
            svg: Default::default(),
            ..Default::default()
        };
        let (updates, rx) = Updates::channel();
        let (w, h) = (WINDOW_SIZE.0 as u32, WINDOW_SIZE.1 as u32);
        Shell {
            app: RustyDlp::new(fonts.clone(), updates),
            painter,
            scroll: ScrollState::default(),
            backend: None,
            window: None,
            updates: rx,
            fonts,
            pointer: None,
            modifiers: ModifiersState::empty(),
            dragging: None,
            dirty: true,
            size: (w, h),
            last_redraw: std::time::Instant::now(),
            hovered: None,
        }
    }


    /// Applies everything workers have posted since the last frame.
    fn drain_updates(&mut self) {
        // try_recv, not await: this runs on the thread that owns the state, and
        // must never block the window.
        while let Ok(update) = self.updates.try_recv() {
            update(&mut self.app);
            self.dirty = true;
        }
    }

    fn redraw(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        self.last_redraw = std::time::Instant::now();
        // The window can be un/maximized by a title-bar double-click, a
        // Windows snap shortcut, or our own restore button -- polling it here
        // rather than reacting to a specific request is what catches all
        // three with one code path.
        let (Some(window), Some(backend)) = (self.window.as_ref(), self.backend.as_mut()) else {
            return;
        };
        self.app.set_window_maximized(window.is_maximized());
        // The popover morphs between two rectangles it has to compute itself
        // (see `library_popover`), and only the shell knows how big the window
        // currently is. Pushed in like `window_maximized` rather than threaded
        // through `render`, so the signature every screen is built from stays
        // as it was.
        self.app.set_viewport(w as f32, h as f32);
        backend.begin_frame(w, h, theme().background);
        let tree = self.app.render();
        let boxes = layout(
            &tree,
            (w as f32, h as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );

        // Resolved against this frame's own box list rather than a bespoke
        // extra render+layout pass on every pointer move — an earlier version
        // of this did exactly that (in `CursorMoved`/`CursorLeft`), which
        // meant every mouse move cost a full second tree build and text-shape
        // pass on top of the one below, on a CPU-only raster pipeline that
        // was already the bottleneck. A transition picked up here instead
        // lands in `self.app` one frame late (this frame still paints the old
        // state), which is imperceptible — it's the same lag `drain_updates`
        // already has relative to whatever a worker just posted.
        let current = self.pointer.and_then(|(x, y)| hovered_id(&boxes, x, y));
        if current != self.hovered {
            self.hovered = dispatch_hover(&boxes, &self.hovered, current, &mut self.app);
            self.dirty = true;
        }

        // The library grid wraps in taffy, so how many tiles fit across is
        // only known once a frame has been laid out -- and `library` needs
        // that count to break a row around an expanded playlist. Measured
        // here and handed back for the next frame; a changed width (a resize)
        // asks for that frame directly, since `dirty` is cleared below.
        if let Some(width) = box_width(&boxes, "library-grid")
            && self.app.set_grid_width(width)
        {
            window.request_redraw();
        }

        paint(backend.canvas(), &boxes, &mut self.painter, self.pointer);
        window.pre_present_notify();
        backend.present();
        self.dirty = false;
    }

    /// Routes a click, letting the app's own handlers run.
    fn on_click(&mut self, event_loop: &ActiveEventLoop, x: f32, y: f32) {
        let tree = self.app.render();
        let boxes = layout(
            &tree,
            (self.size.0 as f32, self.size.1 as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );
        // A slider under the pointer takes the press, so the drag can steer it.
        self.dragging = self.app.slider_at(&boxes, x, y);
        if let Some(drag) = self.dragging {
            self.app.slider_drag(drag_is_seek(drag), &boxes, x, y, false);
        } else if let Some(clicked) = dispatch_click(&boxes, x, y, &mut self.app) {
            // What the click was *on*, kept for whatever the handler opened to
            // animate out of -- a library tile hands the popover the rectangle
            // it grows from.
            self.app.set_click_origin(clicked);
            self.dirty = true;
        } else if self.app.is_titlebar_drag_area(&boxes, x, y)
            && let Some(window) = &self.window
        {
            // Must be called synchronously from the button-press event that
            // starts it -- winit hands this straight to the OS's own move
            // loop, which is also why there is no separate drag-move/release
            // handling to wire up here, unlike the sliders.
            let _ = window.drag_window();
        }
        self.app.focus_input_at(&boxes, x, y);
        self.apply_window_action(event_loop);
        self.dirty = true;
    }

    /// Carries out whichever caption button was clicked, if any. Split out
    /// from `on_click` because closing needs `event_loop`, which the app's
    /// own click handlers never see -- they only ever touch `&mut RustyDlp`.
    fn apply_window_action(&mut self, event_loop: &ActiveEventLoop) {
        let Some(action) = self.app.take_window_action() else {
            return;
        };
        let Some(window) = &self.window else { return };
        match action {
            WindowAction::Minimize => window.set_minimized(true),
            WindowAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowAction::Close => event_loop.exit(),
        }
    }

    fn on_scroll(&mut self, dy: f32) {
        let Some((x, y)) = self.pointer else { return };
        let tree = self.app.render();
        let boxes = layout(
            &tree,
            (self.size.0 as f32, self.size.1 as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );
        if let Some((id, max)) = scroll_target(&boxes, x, y)
            && self.scroll.scroll_by(&id, -dy, max)
        {
            self.dirty = true;
        }
        if wants_pointer_cursor(&boxes, x, y) {
            // Cursor shape is set on move; nothing to do here beyond the hover
            // repaint the caller already asked for.
        }
    }

    /// Feeds a key to whichever text field has focus.
    fn on_key(&mut self, key: &Key, text: Option<&str>) {
        let fonts = self.fonts.clone();
        let mut fonts = fonts.borrow_mut();
        let Some(input) = self.app.focused_input_mut() else {
            return;
        };
        let ctrl = self.modifiers.control_key();
        match key {
            Key::Named(NamedKey::Backspace) => input.backspace(&mut fonts),
            Key::Named(NamedKey::Delete) => input.delete(&mut fonts),
            Key::Named(NamedKey::ArrowLeft) => input.motion(&mut fonts, Motion::Left),
            Key::Named(NamedKey::ArrowRight) => input.motion(&mut fonts, Motion::Right),
            Key::Named(NamedKey::Home) => input.motion(&mut fonts, Motion::Home),
            Key::Named(NamedKey::End) => input.motion(&mut fonts, Motion::End),
            Key::Character(c) if ctrl => match c.as_str() {
                "a" | "A" => input.select_all(&mut fonts),
                "c" | "C" => {
                    if let Some(text) = input.copy() {
                        set_clipboard(&text);
                    }
                }
                "x" | "X" => {
                    if let Some(text) = input.cut(&mut fonts) {
                        set_clipboard(&text);
                    }
                }
                "v" | "V" => {
                    if let Some(text) = clipboard() {
                        input.paste(&mut fonts, &text);
                    }
                }
                _ => {}
            },
            _ => {
                if let Some(text) = text {
                    for c in text.chars() {
                        if !c.is_control() {
                            input.insert_char(&mut fonts, c);
                        }
                    }
                }
            }
        }
        drop(fonts);
        // The gpui build reacted to InputEvent::Change with a debounced probe;
        // this is where that now happens.
        self.app.url_input_changed();
        self.dirty = true;
    }
}

/// D3D12 first; the CPU path when there is no hardware adapter (or off Windows).
fn make_backend(window: &Arc<Window>) -> Option<Box<dyn Backend>> {
    #[cfg(windows)]
    match crate::render::d3d::D3dBackend::new(window) {
        Ok(backend) => return Some(Box::new(backend)),
        Err(e) => eprintln!("rustydlp: D3D12 unavailable, rendering on the CPU: {e:#}"),
    }
    match SoftBackend::new(window.clone()) {
        Ok(backend) => Some(Box::new(backend)),
        Err(e) => {
            eprintln!("rustydlp: no way to present frames: {e:#}");
            None
        }
    }
}

/// The seek bar reports continuously but only *acts* on release: each seek
/// re-spawns two ffmpeg processes, so doing that per mouse-move would thrash.
fn drag_is_seek(drag: SliderKind) -> bool {
    drag == SliderKind::Seek
}

#[cfg(windows)]
fn clipboard() -> Option<String> {
    clipboard_win::get_clipboard_string().ok()
}

#[cfg(windows)]
fn set_clipboard(text: &str) {
    let _ = clipboard_win::set_clipboard_string(text);
}

/// The laid-out width of the box with this id, if it is on screen.
fn box_width<S>(boxes: &[crate::ui::layout::Box_<'_, S>], id: &str) -> Option<f32> {
    boxes
        .iter()
        .find(|b| b.node.and_then(|n| n.element_id()).is_some_and(|i| &**i == id))
        .map(|b| b.bounds.width)
}

/// The clipboard is Windows-only, like the app. Elsewhere the interface still
/// runs and renders; paste is simply unavailable.
#[cfg(not(windows))]
fn clipboard() -> Option<String> {
    None
}

#[cfg(not(windows))]
fn set_clipboard(_text: &str) {}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationHandler for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("rustyDLP")
            .with_inner_size(LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
            .with_min_inner_size(LogicalSize::new(MIN_WINDOW_SIZE.0, MIN_WINDOW_SIZE.1))
            // The navbar draws its own minimize/maximize/close buttons and is
            // itself the drag handle (see `RustyDlp::is_titlebar_drag_area`),
            // so the OS's own title bar would just be a second, redundant one
            // sitting on top of it.
            .with_decorations(false);
        let Ok(window) = event_loop.create_window(attrs) else {
            eprintln!("rustydlp: could not create a window");
            event_loop.exit();
            return;
        };
        let window = Arc::new(window);
        self.backend = make_backend(&window);
        let size = window.inner_size();
        self.size = (size.width.max(1), size.height.max(1));
        self.window = Some(window);
        self.dirty = true;
    }

    /// Woken by a worker posting an update.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.drain_updates();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = (size.width.max(1), size.height.max(1));
                self.dirty = true;
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x as f32, position.y as f32);
                self.pointer = Some((x, y));
                if let Some(drag) = self.dragging {
                    let tree = self.app.render();
                    let boxes = layout(
                        &tree,
                        (self.size.0 as f32, self.size.1 as f32),
                        &mut self.painter.shaper,
                        &self.scroll,
                    );
                    self.app.slider_drag(drag_is_seek(drag), &boxes, x, y, false);
                } else if let Some(window) = &self.window {
                    // The affordance for the invisible resize border below --
                    // an edge that neither looks nor feels grabbable until the
                    // cursor changes over it is not discoverable.
                    let cursor = match resize_direction_at(self.size, x, y) {
                        Some(dir) => CursorIcon::from(dir),
                        None => CursorIcon::Default,
                    };
                    window.set_cursor(cursor);
                }
                // Hover *styling* is resolved at paint time from the pointer
                // and always needs a frame regardless; hover *handlers* (see
                // `redraw`) are resolved there too now, off the same box list,
                // rather than a second render+layout pass here on every move.
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                if let Some(window) = &self.window {
                    window.set_cursor(CursorIcon::Default);
                }
                self.dirty = true;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if let Some((x, y)) = self.pointer {
                            let resize = resize_direction_at(self.size, x, y);
                            match (resize, &self.window) {
                                (Some(dir), Some(window)) => {
                                    // Same "must be called synchronously from
                                    // the press that starts it" contract as
                                    // `drag_window` -- see `on_click`.
                                    let _ = window.drag_resize_window(dir);
                                }
                                _ => self.on_click(event_loop, x, y),
                            }
                        }
                    }
                    ElementState::Released => {
                        // Release is where a seek actually happens.
                        if let Some(drag) = self.dragging.take()
                            && let Some((x, y)) = self.pointer
                        {
                            let tree = self.app.render();
                            let boxes = layout(
                                &tree,
                                (self.size.0 as f32, self.size.1 as f32),
                                &mut self.painter.shaper,
                                &self.scroll,
                            );
                            self.app.slider_drag(drag_is_seek(drag), &boxes, x, y, true);
                            self.dirty = true;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.on_scroll(dy);
                self.dirty = true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed =>
            {
                let text = event.text.as_ref().map(|t| t.as_str());
                self.on_key(&event.logical_key, text);
            }
            WindowEvent::RedrawRequested => {
                self.drain_updates();
                self.redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.drain_updates();
        // A tween has no worker posting new frames to mark things dirty --
        // its whole state is "time has passed since render() last looked" --
        // so unlike video playback (dirty from each arriving `Update`) it has
        // to force its own next redraw here, or the animation registered
        // during the render that triggered it would just sit at whatever
        // in-between value that first frame left it at.
        if self.app.is_animating() {
            self.dirty = true;
        }
        if self.dirty && let Some(window) = &self.window {
            window.request_redraw();
        }
        // While the player is running new frames keep arriving, and while a
        // tween (sidebar collapse, row hover/selection, a pane entrance)
        // hasn't settled it needs the same steady wake-up to keep advancing.
        // Otherwise the loop sleeps until the next event -- the app is a
        // static picture between them, exactly as it was under gpui.
        if self.app.is_playing() || self.app.is_animating() {
            // WaitUntil, not wait_duration: D3D12's present already blocked
            // for vsync, and waiting a further 16ms on top of that halved the
            // refresh rate and beat against the video's own frame rate.
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                self.last_redraw + PLAYBACK_FRAME_INTERVAL,
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

/// Opens the window and runs until it closes.
pub fn run() -> anyhow::Result<()> {
    let event_loop: EventLoop<()> = EventLoop::with_user_event().build()?;
    // Held so workers can wake the loop when they post an update.
    let _proxy: EventLoopProxy<()> = event_loop.create_proxy();
    let mut shell = Shell::new();
    event_loop.run_app(&mut shell)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (800, 600);

    /// Regression guard for resizing having no way in at all once the native
    /// chrome (and its OS-drawn resize border) was removed: every edge and
    /// corner has to actually resolve to a direction, not just the ones near
    /// the origin.
    #[test]
    fn every_edge_and_corner_resolves_a_direction() {
        use ResizeDirection::*;
        let (w, h) = (SIZE.0 as f32, SIZE.1 as f32);
        let cases = [
            (0.0, 0.0, NorthWest),
            (w - 1.0, 0.0, NorthEast),
            (0.0, h - 1.0, SouthWest),
            (w - 1.0, h - 1.0, SouthEast),
            (w / 2.0, 0.0, North),
            (w / 2.0, h - 1.0, South),
            (0.0, h / 2.0, West),
            (w - 1.0, h / 2.0, East),
        ];
        for (x, y, expected) in cases {
            assert_eq!(
                resize_direction_at(SIZE, x, y),
                Some(expected),
                "at ({x}, {y})"
            );
        }
    }

    /// Anywhere well clear of an edge must not fight normal clicks and drags
    /// for the press -- only the thin border should ever claim a resize.
    #[test]
    fn the_interior_is_not_a_resize_zone() {
        assert_eq!(resize_direction_at(SIZE, 400.0, 300.0), None);
        // Just past the border, one pixel in from the edge case above.
        assert_eq!(resize_direction_at(SIZE, RESIZE_BORDER + 1.0, 300.0), None);
    }
}
