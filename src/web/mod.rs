//! A rendering-only experiment: `ui`/`widget` running in a browser tab.
//!
//! See `docs/adr/0005-wasm-render-experiment.md` for why this exists and what
//! it deliberately does not attempt. In short: `core/` (yt-dlp, ffmpeg, the
//! library database, real audio) is native-only and none of it is reachable
//! from a browser sandbox, so this is a second, much smaller frontend --
//! taffy layout and the same element tree, painted with tiny-skia instead of
//! Skia -- driving a handful of static screens with fabricated data instead
//! of `RustyDlp`. It answers one question: does this app's own UI framework
//! (as opposed to any one widget) run on wasm32 at all.
//!
//! `mod.rs` owns the demo screens and the winit web event loop; `text.rs` and
//! `paint.rs` are the render backend, standing in for `ui/text.rs`,
//! `ui/paint.rs` and `render/`.

mod backend;
mod paint;
mod text;

use std::sync::Arc;

use wasm_bindgen::prelude::*;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
use winit::window::{Window, WindowId};

use crate::ui::element::{AnyElement, color_svg, div, h_flex, v_flex};
use crate::ui::event::dispatch_click;
use crate::ui::layout::{ScrollState, layout};
use crate::ui::style::{FluentBuilder as _, Styled as _};
use crate::ui::theme::theme;
use crate::ui::units::{px, relative};
use crate::widget::{Button, IconName, Switch, Tab, TabBar};

use backend::WasmBackend;
use paint::Painter;

/// Matches the native build's default window size (`shell.rs::WINDOW_SIZE`)
/// so the two are visually comparable. Fixed rather than responsive: see the
/// module doc in `docs/adr/0005-wasm-render-experiment.md` for why resize/DPI
/// handling was left out of scope.
const CANVAS_SIZE: (f32, f32) = (1180.0, 760.0);

/// Wide enough that every hand-wrapped line in `Demo::NOTICE_LINES` fits at
/// `.text_xs()` -- see that constant's doc comment for why they're
/// hand-wrapped at all.
const NOTICE_WIDTH: f32 = 300.0;

#[derive(PartialEq, Eq, Clone, Copy)]
enum DemoTab {
    Home,
    InProgress,
}

impl DemoTab {
    fn index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::InProgress => 1,
        }
    }

    fn from_index(ix: usize) -> Self {
        if ix == 1 {
            Self::InProgress
        } else {
            Self::Home
        }
    }
}

/// A fabricated library entry -- there is no store, no yt-dlp, so this is
/// data the demo screen was built with rather than anything downloaded.
struct FakeJob {
    title: &'static str,
    subtitle: &'static str,
}

const FAKE_JOBS: &[FakeJob] = &[
    FakeJob {
        title: "Conference talk — opening keynote",
        subtitle: "1 video",
    },
    FakeJob {
        title: "Weekly livestream archive, part 3",
        subtitle: "1 video",
    },
    FakeJob {
        title: "Album, full playlist rip",
        subtitle: "12 videos",
    },
];

/// All the state this screen needs -- the wasm32 stand-in for `RustyDlp`.
/// Deliberately tiny: no jobs queue, no worker threads (there is no
/// `std::thread::spawn` on this target to run them on), no persistence.
struct Demo {
    tab: DemoTab,
    settings_open: bool,
    autoplay_previews: bool,
    selected: Option<usize>,
    in_progress_cancelled: bool,
    notice_dismissed: bool,
}

impl Demo {
    fn new() -> Self {
        Demo {
            tab: DemoTab::Home,
            settings_open: false,
            autoplay_previews: true,
            selected: None,
            in_progress_cancelled: false,
            notice_dismissed: false,
        }
    }

    fn render(&self) -> AnyElement<Demo> {
        v_flex()
            .w_full()
            .h_full()
            .bg(theme().background)
            .child(self.navbar())
            .child(if self.settings_open {
                self.settings_panel()
            } else {
                self.library()
            })
            .when(!self.notice_dismissed, |el| el.child(self.notice()))
            .into_any_element()
    }

    fn navbar(&self) -> AnyElement<Demo> {
        let tabs = TabBar::new("navbar-tabs")
            .segmented()
            .selected_index(self.tab.index())
            .children([Tab::new().label("Home"), Tab::new().label("In Progress")])
            .on_click(|this: &mut Demo, ix| this.tab = DemoTab::from_index(ix));

        h_flex()
            .id("navbar")
            .w_full()
            .flex_shrink_0()
            .h(px(48.))
            .pl(16.0)
            .items_center()
            .bg(theme().sidebar)
            .border_b_1()
            .border_color(theme().sidebar_border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(132.))
                    .child(color_svg("icons/rustydlp-logo.svg").w(px(120.)).h(px(26.))),
            )
            .child(div().flex_shrink_0().child(tabs))
            .child(
                div()
                    .flex_1()
                    .min_w(px(132.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr(16.0)
                    .child(
                        Button::new("settings")
                            .ghost()
                            .selected(self.settings_open)
                            .icon(IconName::Settings)
                            .on_click(|this: &mut Demo| this.settings_open = !this.settings_open),
                    ),
            )
            .into_any_element()
    }

    fn library(&self) -> AnyElement<Demo> {
        match self.tab {
            DemoTab::Home => v_flex()
                .id("home-list")
                .flex_1()
                .p_3()
                .gap_1()
                .children((0..FAKE_JOBS.len()).map(|i| self.job_row(i)))
                .into_any_element(),
            DemoTab::InProgress => v_flex()
                .flex_1()
                .p_3()
                .gap_1()
                .when(!self.in_progress_cancelled, |el| {
                    el.child(self.in_progress_row())
                })
                .when(self.in_progress_cancelled, |el| {
                    el.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme().muted_foreground)
                            .child("Cancelled."),
                    )
                })
                .into_any_element(),
        }
    }

    fn job_row(&self, i: usize) -> AnyElement<Demo> {
        let job = &FAKE_JOBS[i];
        let active = self.selected == Some(i);
        let bg = if active {
            theme().sidebar_accent
        } else {
            theme().sidebar_accent.opacity(0.0)
        };

        v_flex()
            .id(format!("job-{i}"))
            .w_full()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .rounded(theme().radius)
            .bg(bg)
            .hover(|s| s.bg(theme().list_hover))
            .cursor_pointer()
            .on_click(move |this: &mut Demo| this.selected = Some(i))
            .child(
                div()
                    .text_sm()
                    .text_color(theme().sidebar_foreground)
                    .child(job.title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme().muted_foreground)
                    .child(job.subtitle),
            )
            .into_any_element()
    }

    fn in_progress_row(&self) -> AnyElement<Demo> {
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(theme().radius)
            .bg(theme().surface)
            .child(
                div()
                    .text_sm()
                    .text_color(theme().foreground)
                    .child("Conference talk — closing panel"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme().muted_foreground)
                    .child("6.1 MB/s"),
            )
            .child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(theme().muted)
                    .child(
                        div()
                            .w(relative(0.62))
                            .h_full()
                            .rounded(px(2.))
                            .bg(theme().progress_bar),
                    ),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(|this: &mut Demo| this.in_progress_cancelled = true),
                ),
            )
            .into_any_element()
    }

    fn settings_panel(&self) -> AnyElement<Demo> {
        v_flex()
            .flex_1()
            .p_4()
            .gap_3()
            .child(
                Button::new("back")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .label("Back")
                    .on_click(|this: &mut Demo| this.settings_open = false),
            )
            .child(
                v_flex()
                    .max_w(px(420.))
                    .p_3()
                    .gap_2()
                    .rounded(theme().radius)
                    .bg(theme().surface)
                    .child(
                        Switch::new("autoplay")
                            .checked(self.autoplay_previews)
                            .label("Autoplay hover previews")
                            .on_click(|this: &mut Demo, checked| this.autoplay_previews = checked),
                    )
                    .child(
                        v_flex().gap_0p5().children(
                            [
                                "The only setting this experiment has",
                                "state for. Every other Settings control",
                                "edits a preset stored in core/, which",
                                "this build does not have.",
                            ]
                            .map(|line| {
                                div()
                                    .text_xs()
                                    .text_color(theme().muted_foreground)
                                    .child(line)
                            }),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// This UI framework shapes text one unbroken line at a time (see
    /// `ui::layout::Inherited::truncate` and `web::text`'s module doc) — the
    /// only wrapping it has is `.truncate()`'s ellipsis, and every real call
    /// site is a title or a label short enough never to need more. This
    /// notice is the first genuinely multi-line prose text either backend
    /// has ever had to draw, so it wraps itself, by hand, into lines short
    /// enough to fit `NOTICE_WIDTH` -- the same thing a native caller with a
    /// paragraph to lay out would have to do.
    const NOTICE_LINES: &'static [&'static str] = &[
        "WASM rendering experiment.",
        "Layout and paint are real;",
        "downloads and playback are not —",
        "core/ needs subprocesses a browser",
        "sandbox cannot have. See ADR-0005.",
    ];

    fn notice(&self) -> AnyElement<Demo> {
        v_flex()
            .id("notice")
            .absolute()
            .right(px(16.))
            .bottom(px(16.))
            .w(px(NOTICE_WIDTH))
            .p_3()
            .gap_2()
            .rounded(theme().radius)
            .border_1()
            .border_color(theme().border)
            .bg(theme().sidebar)
            .child(
                v_flex()
                    .gap_0p5()
                    .children(Self::NOTICE_LINES.iter().map(|line| {
                        div()
                            .text_xs()
                            .text_color(theme().muted_foreground)
                            .child(*line)
                    })),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("dismiss-notice")
                        .ghost()
                        .small()
                        .label("Dismiss")
                        .on_click(|this: &mut Demo| this.notice_dismissed = true),
                ),
            )
            .into_any_element()
    }
}

struct App {
    demo: Demo,
    painter: Painter,
    scroll: ScrollState,
    backend: Option<WasmBackend>,
    window: Option<Arc<Window>>,
    pointer: Option<(f32, f32)>,
    size: (u32, u32),
    dirty: bool,
}

impl App {
    fn new() -> Self {
        App {
            demo: Demo::new(),
            painter: Painter::default(),
            scroll: ScrollState::default(),
            backend: None,
            window: None,
            pointer: None,
            size: (CANVAS_SIZE.0 as u32, CANVAS_SIZE.1 as u32),
            dirty: true,
        }
    }

    fn redraw(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        backend.begin_frame(w, h, theme().background);
        let tree = self.demo.render();
        let boxes = layout(
            &tree,
            (w as f32, h as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );
        paint::paint(
            backend.pixmap_mut(),
            &boxes,
            &mut self.painter,
            self.pointer,
        );
        backend.present();
        self.dirty = false;
    }

    /// Mirrors `shell.rs::Shell::on_click`, minus the slider/titlebar-drag/
    /// text-focus handling that has nothing to click on this screen -- the
    /// demo has no sliders and no text inputs.
    fn on_click(&mut self, x: f32, y: f32) {
        let tree = self.demo.render();
        let boxes = layout(
            &tree,
            (self.size.0 as f32, self.size.1 as f32),
            &mut self.painter.shaper,
            &self.scroll,
        );
        if dispatch_click(&boxes, x, y, &mut self.demo) {
            self.dirty = true;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("rustyDLP — wasm experiment")
            .with_inner_size(LogicalSize::new(CANVAS_SIZE.0, CANVAS_SIZE.1))
            // Winit creates the `<canvas>` itself and appends it to `<body>`,
            // so `web/index.html` needs none of its own.
            .with_append(true);
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        self.backend = WasmBackend::new(window.clone()).ok();
        let size = window.inner_size();
        self.size = (size.width.max(1), size.height.max(1));
        self.window = Some(window);
        self.dirty = true;
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::Resized(size) => {
                self.size = (size.width.max(1), size.height.max(1));
                self.dirty = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some((position.x as f32, position.y as f32));
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((x, y)) = self.pointer {
                    self.on_click(x, y);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.dirty
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}

/// Entry point: called once the wasm module is instantiated (see
/// `web/index.html`).
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let event_loop = EventLoop::new().expect("winit event loop");
    // Not `run_app`: that blocks the calling thread, which on the web is the
    // one thread the page has to keep responding to input and to paint at
    // all. `spawn_app` returns immediately and drives the app from browser
    // callbacks instead (see `winit::platform::web::EventLoopExtWebSys`).
    event_loop.spawn_app(App::new());
}
