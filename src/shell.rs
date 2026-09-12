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
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;

use cosmic_text::{FontSystem, Motion};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::app_skia::{RustyDlp, SliderKind, Update, Updates};
use crate::render::Backend;
use crate::render::raster::RasterBackend;
use crate::ui::event::{dispatch_click, scroll_target, wants_pointer_cursor};
use crate::ui::layout::{ScrollState, layout};
use crate::ui::paint::{Painter, paint};
use crate::ui::text::Shaper;
use crate::ui::theme::theme;

/// The size `main.rs` opened the gpui window at.
const WINDOW_SIZE: (f32, f32) = (1180.0, 760.0);

/// How often to rebuild a frame while the player is running. The decode threads
/// pace themselves to the source's own fps; this only bounds how often the
/// picture on screen is refreshed.
const PLAYBACK_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

pub struct Shell {
    app: RustyDlp,
    painter: Painter,
    scroll: ScrollState,
    backend: RasterBackend,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    updates: futures::channel::mpsc::UnboundedReceiver<Update>,
    fonts: Rc<RefCell<FontSystem>>,
    pointer: Option<(f32, f32)>,
    modifiers: ModifiersState,
    /// Set while the pointer is down inside a slider, so a drag keeps steering
    /// the same one after the pointer leaves its box.
    dragging: Option<SliderKind>,
    dirty: bool,
    size: (u32, u32),
}

impl Shell {
    pub fn new() -> Self {
        let fonts = Rc::new(RefCell::new(FontSystem::new()));
        let painter = Painter {
            shaper: Shaper::with_shared_fonts(fonts.clone()),
            svg: Default::default(),
        };
        let (updates, rx) = Updates::channel();
        let (w, h) = (WINDOW_SIZE.0 as u32, WINDOW_SIZE.1 as u32);
        Shell {
            app: RustyDlp::new(fonts.clone(), updates),
            painter,
            scroll: ScrollState::default(),
            backend: RasterBackend::new(w, h),
            window: None,
            surface: None,
            updates: rx,
            fonts,
            pointer: None,
            modifiers: ModifiersState::empty(),
            dragging: None,
            dirty: true,
            size: (w, h),
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
        self.backend.begin_frame(w, h, theme().background);
        let tree = self.app.render();
        let boxes = layout(
            &tree,
            (w as f32, h as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );
        paint(
            self.backend.canvas(),
            &boxes,
            &mut self.painter,
            self.pointer,
        );

        if let (Some(surface), Some(window)) = (self.surface.as_mut(), self.window.as_ref()) {
            let rgba = self.backend.read_rgba();
            if let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h))
                && surface.resize(nw, nh).is_ok()
                && let Ok(mut buffer) = surface.buffer_mut()
            {
                // softbuffer wants 0RGB in a u32 per pixel.
                for (dst, px) in buffer.iter_mut().zip(rgba.chunks_exact(4)) {
                    *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
                }
                let _ = buffer.present();
            }
            window.pre_present_notify();
        }
        self.dirty = false;
    }

    /// Routes a click, letting the app's own handlers run.
    fn on_click(&mut self, x: f32, y: f32) {
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
        } else if dispatch_click(&boxes, x, y, &mut self.app) {
            self.dirty = true;
        }
        self.app.focus_input_at(&boxes, x, y);
        self.dirty = true;
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
            .with_inner_size(LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
        let Ok(window) = event_loop.create_window(attrs) else {
            eprintln!("rustydlp: could not create a window");
            event_loop.exit();
            return;
        };
        let window = Arc::new(window);
        if let Ok(context) = softbuffer::Context::new(window.clone())
            && let Ok(surface) = softbuffer::Surface::new(&context, window.clone())
        {
            self.surface = Some(surface);
        }
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
                self.pointer = Some((position.x as f32, position.y as f32));
                if let Some(drag) = self.dragging {
                    let tree = self.app.render();
                    let boxes = layout(
                        &tree,
                        (self.size.0 as f32, self.size.1 as f32),
                        &mut self.painter.shaper,
                        &self.scroll,
                    );
                    let (x, y) = (position.x as f32, position.y as f32);
                    self.app.slider_drag(drag_is_seek(drag), &boxes, x, y, false);
                }
                // Hover styling is resolved at paint time from the pointer, so a
                // move always needs a frame.
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if let Some((x, y)) = self.pointer {
                            self.on_click(x, y);
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
        if self.dirty && let Some(window) = &self.window {
            window.request_redraw();
        }
        // While the player is running new frames keep arriving, so the loop
        // wakes on a timer. Otherwise it sleeps until the next event -- the app
        // is a static picture between them, exactly as it was under gpui.
        if self.app.is_playing() {
            event_loop.set_control_flow(ControlFlow::wait_duration(PLAYBACK_FRAME_INTERVAL));
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
