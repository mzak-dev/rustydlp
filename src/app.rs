//! The interface.
//!
//! Ported from the gpui build this replaced, and kept deliberately parallel to
//! it -- same order, same helpers, same comments -- so the two can still be
//! diffed while parity is signed off. The gpui version is at `src/app.rs` in
//! commit 374d659 and earlier.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::{AtomicU64, Ordering};

use cosmic_text::FontSystem;
use futures_util::StreamExt as _;

use crate::ui::element::{
    IntoAnyElement as _, SharedString, color_svg, div, h_flex, img, svg, v_flex,
};
use crate::ui::style::{FluentBuilder as _, ObjectFit, Styled as _};
use crate::ui::theme::theme;
use crate::ui::units::{Bounds, Length, Pixels, px, relative};
use crate::ui::black;
use crate::ui::anim::{SWAP_DISTANCE, morph_rect, page_swap};
use crate::widget::{
    Button, Icon, IconName, Input, InputState, Slider, SliderState, Switch, Tab, TabBar,
};

use crate::core::model::{
    File as MFile, FileKind, Item, Job, JobKind, JobState, Preset, new_id, sibling_thumbnail,
};
use crate::core::player;
use crate::core::runner::{self, ConvertEvent, ConvertFormat, Probe};
use crate::core::store::Store;
use crate::core::ytdlp::{Event, FormatMode, YtdlpOptions};

/// Every element-returning helper in this file hands back one of these, exactly
/// as it did under gpui -- the one line that keeps those ~30 signatures intact.
pub type AnyElement = crate::ui::element::AnyElement<RustyDlp>;

/// A change to apply to the app, on the thread that owns it.
pub type Update = Box<dyn FnOnce(&mut RustyDlp) + Send>;

/// Hands work off to a thread and its results back.
///
/// This replaces gpui's executor. Every async site in the gpui build was the
/// same shape -- do something off-thread, then mutate state -- and `core/`
/// already hands back plain `futures_channel` receivers from threads it spawns
/// itself, so nothing here needs a runtime. Closures rather than an enum of
/// messages keeps the ported bodies looking like the `this.update(..)` blocks
/// they came from.
#[derive(Clone)]
pub struct Updates(futures_channel::mpsc::UnboundedSender<Update>);

impl Updates {
    pub fn channel() -> (Updates, futures_channel::mpsc::UnboundedReceiver<Update>) {
        let (tx, rx) = futures_channel::mpsc::unbounded();
        (Updates(tx), rx)
    }

    /// Queues a mutation. Dropped silently if the app is gone, which is what
    /// `this.update(..)`'s ignored `Result` did.
    pub fn send(&self, f: impl FnOnce(&mut RustyDlp) + Send + 'static) {
        let _ = self.0.unbounded_send(Box::new(f));
    }

    /// Runs `work` on its own thread, handing it a sender for the results.
    pub fn spawn(&self, work: impl FnOnce(Updates) + Send + 'static) {
        let updates = self.clone();
        std::thread::spawn(move || work(updates));
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Route {
    Library,
    Settings,
}

/// A native window chrome action, requested by a click on one of the custom
/// caption buttons. `app.rs` only records which one was wanted -- `shell.rs`
/// is what owns the actual `winit::window::Window` and carries it out, the
/// same split `Updates` draws between "what changed" and "who has the handle
/// to act on it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

/// Which top-navbar mode is active, and therefore what the main pane shows.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SidebarTab {
    /// Every finished download or conversion, newest first, as one flat
    /// grid — see `library`.
    Home,
    /// Jobs of either kind that are still working.
    InProgress,
}

impl SidebarTab {
    fn index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::InProgress => 1,
        }
    }

    fn from_index(ix: usize) -> Self {
        match ix {
            1 => Self::InProgress,
            _ => Self::Home,
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Home => "No downloads or conversions yet",
            Self::InProgress => "Nothing in progress",
        }
    }
}

/// Live, in-memory state for a running job.
///
/// Deliberately NOT persisted: progress ticks arrive ~10x/second and writing
/// each one would mean constant disk churn. Only state transitions reach turso.
#[derive(Default)]
struct Live {
    fraction: Option<f32>,
    speed: Option<f64>,
    eta: Option<f64>,
    log: Vec<String>,
}

enum ProbeState {
    Idle,
    Running,
    Ok(Box<Probe>),
    Err(String),
}

pub struct RustyDlp {
    ytdlp: Option<PathBuf>,
    store: Option<Arc<Store>>,
    jobs: Vec<Job>,
    live: HashMap<String, Live>,
    /// Selected job id.
    selected: Option<String>,
    /// Item id, when drilled into a multi-item job's grid.
    open_item: Option<String>,
    route: Route,
    tab: SidebarTab,
    modal: bool,
    url_input: InputState,
    /// Extra yt-dlp args applied to this download only, never saved back to the
    /// preset. Keeps the merge two layers deep: preset, then this run.
    override_input: InputState,
    probe: ProbeState,
    /// Bumped on every keystroke; a debounced probe only fires if it still
    /// matches when its timer expires.
    probe_gen: Arc<AtomicU64>,
    startup_error: Option<String>,
    /// Accepted but not yet spawned. Drained by a single staggering task.
    pending: VecDeque<PendingLaunch>,
    launcher_active: bool,
    /// Kill switches for currently running jobs, keyed by job id.
    cancels: HashMap<String, runner::CancelHandle>,
    /// Result of the last `yt-dlp -U`, shown in Settings.
    update_status: Option<String>,
    presets: Vec<Preset>,
    /// Preset chosen in the New Download modal.
    chosen_preset: Option<String>,
    /// Preset currently open in the Settings editor.
    editing: Option<Preset>,
    form: PresetForm,
    player: Option<PlayerState>,
    /// Path currently being probed/spawned, if a load is in flight — set the
    /// instant `ensure_player` decides a (re)load is needed, cleared once
    /// `player` is populated (or the load fails). Prevents re-triggering a
    /// second load on every render while the first is still starting up.
    player_pending: Option<String>,
    /// Bumped on every load/seek so a stray event from a just-replaced
    /// playback run can't clobber the state of the run that replaced it.
    player_gen: Arc<AtomicU64>,
    /// Path whose last load attempt failed. Needed because a failed load
    /// notifies, which re-renders, which calls `ensure_player` again — an
    /// unguarded retry spins a fresh ffprobe every frame for as long as the
    /// item stays open. Cleared on navigating away, so re-opening the item
    /// tries again.
    player_failed: Option<String>,
    /// Seek bar position as a fraction of the source's duration, not as
    /// seconds: a `SliderState`'s range is fixed when it is built, and the
    /// duration only becomes known once a file is loaded.
    seek_slider: SliderState,
    /// Output gain, `0.0..=1.0`.
    volume_slider: SliderState,
    /// Kept here rather than on `PlayerState` so it survives seeks (which
    /// re-spawn ffmpeg) and carries over to the next item played.
    volume: f32,
    muted: bool,
    /// Source file path awaiting a target-format choice, if the format
    /// picker is open. Set by both the per-item Convert button and the
    /// Convert tab's "Convert File" picker.
    convert_picker: Option<String>,
    /// Shared with the painter's shaper. Text inputs keep their own cosmic-text
    /// buffers, and they have to be shaped against the same font database the
    /// paint uses or a caret would be measured with different metrics than the
    /// glyphs beside it.
    ///
    /// Borrow discipline: taken only for the duration of one input mutation, and
    /// never held across a shaping call.
    fonts: Rc<RefCell<FontSystem>>,
    updates: Updates,
    /// Which field has the keyboard, if any.
    focus: Option<Focus>,
    /// In-flight width/colour/entrance transitions, keyed by whatever each
    /// call site uses to name "this same animated thing across renders".
    /// `RefCell`-guarded so `sidebar()`/`job_row()` — ported from gpui
    /// signatures that took `&self` — can consult and advance it without
    /// becoming `&mut self`.
    anim: crate::ui::Animator,
    /// Set by a caption-button click, drained by `shell.rs` after routing the
    /// click that set it. There is at most one of these in flight at a time —
    /// a click is a single event -- so "the last one wins" needs no queue.
    pending_window_action: Option<WindowAction>,
    /// Mirrors `winit::window::Window::is_maximized`, pushed in by `shell.rs`
    /// on every redraw: `render()` has no window handle of its own to ask, and
    /// the maximize button's icon needs to know which state it would toggle to.
    window_maximized: bool,
    /// Expands the library popover (see `library_popover`) to near-fullscreen.
    /// Reset whenever the popover closes, so reopening a different item always
    /// starts at the compact size.
    detail_fullscreen: bool,
    /// The window's size in logical pixels, pushed in by `shell.rs` every
    /// frame the way `window_maximized` is. `library_popover` computes the
    /// rectangle it morphs into itself (a fraction of the window, centred), so
    /// unlike every other screen it cannot leave sizing to the layout engine.
    viewport: (f32, f32),
    /// Bounds of the box whose click handler last ran, pushed in by
    /// `shell.rs`. What the popover grows out of.
    click_origin: Option<Bounds>,
    /// The pair of rectangles the popover is travelling between, kept across
    /// frames so the closing collapse can run it backwards.
    morph: Option<Morph>,
    /// Muted, autoplay-looped preview for whichever grid tile the pointer has
    /// rested on — a separate pipeline from `player`/`PlayerState` (the
    /// detail popover's own, full-controls player) rather than a shared one,
    /// since the two can be live at once (hovering a tile behind an already
    /// open popover) and want different lifecycles.
    hover_preview: Option<PreviewState>,
    /// Item id a preview is debounced to start for, cleared once it lands in
    /// `hover_preview` or the pointer leaves before it fires.
    preview_pending: Option<String>,
    /// Bumped on every hover change so a stale debounce timer or in-flight
    /// load from a tile the pointer has already left can't install itself.
    preview_gen: Arc<AtomicU64>,
}

/// The two rectangles the library popover animates between: the tile that
/// opened it, and the card it settles into.
///
/// Kept on the app rather than in the `Animator` because only one of the two
/// is a tween — `to` follows the fullscreen toggle every frame, while `from`
/// is whatever was clicked and must not move under the animation.
#[derive(Clone)]
struct Morph {
    /// Which open job/item this belongs to, or `CLOSED` once the popover has
    /// been dismissed — which is what re-anchors the next open to its own
    /// tile instead of reusing this one's.
    identity: u64,
    from: Bounds,
    to: Bounds,
    /// The item's thumbnail — the one thing the tile and the open card
    /// actually have in common, and so what the growing rectangle carries
    /// while `detail`'s content is still fading in (and what it carries back
    /// on the way out, when there is no content to render at all).
    thumb: Option<String>,
}

/// Live native-playback state for whichever item is currently open in
/// `detail()`. Torn down (killing the ffmpeg children) whenever the open
/// item changes or the player is closed.
struct PlayerState {
    control: player::PlayerControl,
    path: String,
    /// The most recent decoded frame, kept as plain RGBA.
    frame: Option<Frame>,
    position_secs: f64,
    /// Total length: ffprobe's, or the item's recorded one as a fallback.
    /// Without it there is nothing to draw a seek bar against.
    duration_secs: Option<f64>,
    playing: bool,
    /// Playback reached the end of the file. Play then restarts from the top
    /// rather than un-pausing pipes that have nothing left to give.
    ended: bool,
    /// True while the seek bar is being dragged, so arriving frame positions
    /// don't fight the thumb for the slider's value.
    scrubbing: bool,
}

/// Live native-playback state for a grid tile's hover preview: the same
/// decode pipeline `PlayerState` wraps, but without the transport (position,
/// duration, play/pause, scrubbing) a preview has no use for.
struct PreviewState {
    control: player::PlayerControl,
    /// Which item this is previewing, so the tile whose hover it belongs to
    /// (and only that one) paints it instead of its static thumbnail.
    item_id: String,
    frame: Option<Frame>,
}

/// Which text field has the keyboard.
///
/// gpui tracked focus itself; here the interface owns it, so a click has to say
/// which field it landed on and the event loop asks for that field back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Url,
    Override,
    Name,
    MaxHeight,
    Container,
    Output,
    Dir,
    Subs,
    Extra,
    Codec,
    CustomFmt,
}

impl Focus {
    /// The element id the matching `Input` is built with.
    fn element_id(self) -> &'static str {
        match self {
            Focus::Url => "url",
            Focus::Override => "override",
            Focus::Name => "field-name",
            Focus::MaxHeight => "field-max-height",
            Focus::Container => "field-container",
            Focus::Output => "field-output",
            Focus::Dir => "field-dir",
            Focus::Subs => "field-subs",
            Focus::Extra => "field-extra",
            Focus::Codec => "field-codec",
            Focus::CustomFmt => "field-custom-fmt",
        }
    }

    const ALL: [Focus; 11] = [
        Focus::Url,
        Focus::Override,
        Focus::Name,
        Focus::MaxHeight,
        Focus::Container,
        Focus::Output,
        Focus::Dir,
        Focus::Subs,
        Focus::Extra,
        Focus::Codec,
        Focus::CustomFmt,
    ];
}

/// Which of the two player sliders a drag belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderKind {
    Seek,
    Volume,
}

/// One decoded frame, split into what an image source needs.
///
/// The decode thread's `Vec` moves straight into the `Arc`, so no frame is ever
/// copied, and handing it to the element tree each render is an `Arc` bump.
#[derive(Clone)]
struct Frame {
    width: u32,
    height: u32,
    data: Arc<Vec<u8>>,
    /// This frame's presentation timestamp, doubling as the paint-side
    /// cache's identity for it (see `ImageSource::Rgba::id`) -- it is
    /// already unique per decoded frame, so nothing new had to be minted
    /// just to tell one buffer apart from another.
    pts: f64,
}

/// Starting playback volume. Full scale is loud enough to be startling on a
/// first play, and there is no per-file gain to compensate with.
const DEFAULT_VOLUME: f32 = 0.45;

fn db_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("rustyDLP")
}

fn default_options() -> YtdlpOptions {
    let dir = dirs_download().to_string_lossy().to_string();
    YtdlpOptions {
        download_dir: dir,
        container: Some("mp4".into()),
        max_height: Some(1080),
        ..Default::default()
    }
}

/// Real Videos folder via the known-folder API, so a relocated library still
/// resolves correctly. Falls back to the profile only if that lookup fails.
fn dirs_download() -> PathBuf {
    dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("rustyDLP")
}

/// Gap between process launches. Concurrency itself is unbounded — this only
/// stops N jobs spawning in the same instant, which is what makes a site
/// rate-limit you and what makes 30 simultaneous ffmpeg muxes thrash the disk.
const LAUNCH_STAGGER: Duration = Duration::from_millis(400);
/// Extra wait after a launch fails, multiplied by attempt count.
const LAUNCH_BACKOFF: Duration = Duration::from_secs(2);
const MAX_LAUNCH_ATTEMPTS: u32 = 5;

/// Text fields of the preset editor. Toggles and the format mode live on
/// `editing` directly; only free-text needs an InputState.
struct PresetForm {
    name: InputState,
    max_height: InputState,
    container: InputState,
    output: InputState,
    dir: InputState,
    subs: InputState,
    extra: InputState,
    codec: InputState,
    custom_fmt: InputState,
}

impl PresetForm {
    fn new(fonts: &mut FontSystem) -> Self {
        let mut mk = |ph: &'static str| InputState::new(fonts).placeholder(ph);
        Self {
            name: mk("Preset name"),
            max_height: mk("1080 (blank = no cap)"),
            container: mk("mp4 / mkv (blank = leave alone)"),
            output: mk("%(title)s [%(id)s].%(ext)s"),
            dir: mk("Download folder"),
            subs: mk("en,pl (blank = no subtitles)"),
            extra: mk("--cookies-from-browser firefox"),
            codec: mk("mp3 / m4a / opus"),
            custom_fmt: mk("bv*+ba/b"),
        }
    }
}

/// A job accepted by the UI but not yet spawned.
struct PendingLaunch {
    job_id: String,
    url: String,
    opts: YtdlpOptions,
    attempts: u32,
}

fn human_bytes(n: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

fn human_duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl RustyDlp {
    pub fn new(fonts: Rc<RefCell<FontSystem>>, updates: Updates) -> Self {
        let mut this = Self::build(fonts, updates);
        this.refresh_ytdlp();
        this.open_store();
        this
    }

    /// The state alone, with nothing started.
    ///
    /// Rendering needs neither yt-dlp nor the library, and a test that opened the
    /// store would write a real database into the user's app data.
    fn build(fonts: Rc<RefCell<FontSystem>>, updates: Updates) -> Self {
        let (url_input, override_input, form) = {
            let mut f = fonts.borrow_mut();
            (
                InputState::new(&mut f).placeholder("Paste a video or playlist URL"),
                InputState::new(&mut f).placeholder("Extra args for this download only"),
                PresetForm::new(&mut f),
            )
        };

        // Both player sliders work in normalised units: the seek bar as a
        // fraction of the duration, the volume as a gain factor.
        let seek_slider = SliderState::new()
            .min(0.0)
            .max(1.0)
            .step(0.001)
            .default_value(0.0);
        let volume_slider = SliderState::new()
            .min(0.0)
            .max(1.0)
            .step(0.01)
            .default_value(DEFAULT_VOLUME);

        // No subscriptions: the three the gpui build registered (url text
        // changed, seek slider changed/released, volume changed) are now direct
        // calls from the input and slider handling -- `url_changed`, `scrub_to`,
        // `seek_to_fraction`, `set_volume`. The drag/release split is preserved
        // there, because every seek re-spawns two ffmpeg processes and doing
        // that per mouse-move would thrash.

        Self {
            ytdlp: None,
            store: None,
            jobs: Vec::new(),
            live: HashMap::new(),
            selected: None,
            open_item: None,
            route: Route::Library,
            tab: SidebarTab::Home,
            modal: false,
            url_input,
            override_input,
            probe: ProbeState::Idle,
            probe_gen: Arc::new(AtomicU64::new(0)),
            startup_error: None,
            pending: VecDeque::new(),
            launcher_active: false,
            cancels: HashMap::new(),
            update_status: None,
            presets: Vec::new(),
            chosen_preset: None,
            editing: None,
            form,
            player: None,
            player_pending: None,
            player_gen: Arc::new(AtomicU64::new(0)),
            player_failed: None,
            seek_slider,
            volume_slider,
            volume: DEFAULT_VOLUME,
            muted: false,
            convert_picker: None,
            fonts,
            updates,
            focus: None,
            anim: crate::ui::Animator::default(),
            pending_window_action: None,
            window_maximized: false,
            detail_fullscreen: false,
            // Until `shell.rs` says otherwise, the size it opens the window at
            // -- which is also the size the render tests judge layout at.
            viewport: crate::shell::WINDOW_SIZE,
            click_origin: None,
            morph: None,
            hover_preview: None,
            preview_pending: None,
            preview_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    pub fn for_test(fonts: Rc<RefCell<FontSystem>>, updates: Updates) -> Self {
        Self::build(fonts, updates)
    }

    // -- driven by the event loop -----------------------------------------

    /// The field with the keyboard, if any.
    pub fn focused_input_mut(&mut self) -> Option<&mut InputState> {
        self.input_mut(self.focus?)
    }

    /// Moves focus to whichever field was clicked, or clears it.
    ///
    /// The caret is placed from the click's offset into the field, so clicking
    /// into the middle of a URL puts the caret there rather than at the end.
    pub fn focus_input_at<S>(&mut self, boxes: &[crate::ui::layout::Box_<'_, S>], x: f32, y: f32) {
        let hit = crate::ui::event::hit_test(boxes, x, y);
        let mut landed = None;
        if let Some(mut i) = hit {
            'outer: loop {
                if let Some(id) = boxes[i].node.and_then(|n| n.element_id()) {
                    for focus in Focus::ALL {
                        if &**id == focus.element_id() {
                            landed = Some((focus, boxes[i].bounds.x));
                            break 'outer;
                        }
                    }
                }
                match boxes[i].parent {
                    Some(p) => i = p,
                    None => break,
                }
            }
        }

        for focus in Focus::ALL {
            let is_target = landed.map(|(f, _)| f) == Some(focus);
            if let Some(input) = self.input_mut(focus) {
                input.set_focused(is_target);
            }
        }
        self.focus = landed.map(|(f, _)| f);
        if let Some((focus, origin)) = landed {
            // 8px is the field's horizontal padding -- see the Input widget.
            let offset = (x - origin - 8.0).max(0.0);
            let fonts = self.fonts.clone();
            let mut fonts = fonts.borrow_mut();
            if let Some(input) = self.input_mut(focus) {
                input.click(&mut fonts, offset);
            }
        }
    }

    fn input_mut(&mut self, focus: Focus) -> Option<&mut InputState> {
        Some(match focus {
            Focus::Url => &mut self.url_input,
            Focus::Override => &mut self.override_input,
            Focus::Name => &mut self.form.name,
            Focus::MaxHeight => &mut self.form.max_height,
            Focus::Container => &mut self.form.container,
            Focus::Output => &mut self.form.output,
            Focus::Dir => &mut self.form.dir,
            Focus::Subs => &mut self.form.subs,
            Focus::Extra => &mut self.form.extra,
            Focus::Codec => &mut self.form.codec,
            Focus::CustomFmt => &mut self.form.custom_fmt,
        })
    }

    /// Which slider, if either, is under the pointer.
    pub fn slider_at<S>(
        &self,
        boxes: &[crate::ui::layout::Box_<'_, S>],
        x: f32,
        y: f32,
    ) -> Option<SliderKind> {
        let mut i = crate::ui::event::hit_test(boxes, x, y)?;
        loop {
            if let Some(id) = boxes[i].node.and_then(|n| n.element_id()) {
                match &**id {
                    "seek" => return Some(SliderKind::Seek),
                    "volume" => return Some(SliderKind::Volume),
                    _ => {}
                }
            }
            i = boxes[i].parent?;
        }
    }

    /// Whether an unhandled click at this point should move the window --
    /// true for the navbar's own background, false over a button, tab, or
    /// anything else with its own click handler (those are never reached
    /// here: `shell.rs` only asks this when `dispatch_click` found nothing).
    pub fn is_titlebar_drag_area<S>(
        &self,
        boxes: &[crate::ui::layout::Box_<'_, S>],
        x: f32,
        y: f32,
    ) -> bool {
        let Some(mut i) = crate::ui::event::hit_test(boxes, x, y) else {
            return false;
        };
        loop {
            if boxes[i].node.and_then(|n| n.element_id()).is_some_and(|id| &**id == "navbar") {
                return true;
            }
            match boxes[i].parent {
                Some(p) => i = p,
                None => return false,
            }
        }
    }

    /// The window action a caption button asked for since the last time this
    /// was called, if any.
    pub fn take_window_action(&mut self) -> Option<WindowAction> {
        self.pending_window_action.take()
    }

    /// Called by `shell.rs` before every render, so the maximize button's
    /// icon reflects the window's actual state rather than assuming it drove
    /// every change to it (a title-bar double-click or a Windows snap
    /// shortcut changes it without going through `WindowAction` at all).
    pub fn set_window_maximized(&mut self, maximized: bool) {
        self.window_maximized = maximized;
    }

    /// The window's current size in logical pixels. Pushed in by `shell.rs`
    /// each frame; see the field.
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = (width, height);
    }

    /// The bounds of the box whose click handler just ran, pushed in by
    /// `shell.rs` after it routes a click; see the field.
    pub fn set_click_origin(&mut self, rect: Bounds) {
        self.click_origin = Some(rect);
    }

    /// Steers a slider from a pointer position.
    ///
    /// The seek bar deliberately splits drag from release: dragging only moves
    /// the readout, and the seek itself waits for the release, because every seek
    /// re-spawns two ffmpeg processes and doing that per mouse-move would thrash.
    pub fn slider_drag<S>(
        &mut self,
        seek: bool,
        boxes: &[crate::ui::layout::Box_<'_, S>],
        x: f32,
        _y: f32,
        released: bool,
    ) {
        let id = if seek { "seek" } else { "volume" };
        let Some(track) = boxes
            .iter()
            .find(|b| b.node.and_then(|n| n.element_id()).is_some_and(|e| &**e == id))
            .map(|b| b.bounds)
        else {
            return;
        };
        let state = if seek { &mut self.seek_slider } else { &mut self.volume_slider };
        let value = state.value_at(&track, x);
        // The thumb follows the pointer for the whole drag. Nothing else sets
        // it mid-drag: `sync_seek_slider` deliberately stands aside while
        // scrubbing, and the volume slider has no sync at all.
        state.set_value(value);
        if seek {
            if released {
                self.seek_to_fraction(value);
            } else {
                self.scrub_to(value);
            }
        } else {
            self.set_volume(value);
        }
    }

    /// The debounced probe the gpui build ran from `InputEvent::Change`.
    pub fn url_input_changed(&mut self) {
        if self.focus != Some(Focus::Url) {
            return;
        }
        let value = self.url_input.value();
        self.schedule_probe(value);
    }

    pub fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.playing)
    }

    /// Whether any width/colour/entrance transition `render()` just touched
    /// is still short of settling. The event loop uses this exactly like
    /// `is_playing()`: while either is true it keeps waking up on a timer
    /// instead of sleeping until the next input event, since a tween has
    /// nothing else to prod the loop into redrawing the next frame of it.
    pub fn is_animating(&self) -> bool {
        self.anim.is_animating()
    }

    /// Re-probes for the yt-dlp binary and clears/sets the banner accordingly.
    ///
    /// Called at startup, whenever the New Download modal opens, and from the
    /// Re-check button. Resolving once at startup meant an instance launched
    /// before the binary was installed showed "not found" for its whole life
    /// with no way to recover short of a restart.
    fn refresh_ytdlp(&mut self) {
        match runner::ytdlp_path(None) {
            Ok(path) => {
                self.ytdlp = Some(path);
                self.startup_error = None;
            }
            Err(e) => {
                self.ytdlp = None;
                self.startup_error = Some(e.to_string());
            }
        }
    }

    fn open_store(&mut self) {
        self.updates.spawn(|updates| {
            let dir = db_path();
            if let Err(e) = std::fs::create_dir_all(&dir) {
                let msg = format!("cannot create {}: {e}", dir.display());
                updates.send(move |this| {
                    this.startup_error = Some(msg);
                });
                return;
            }
            let path = dir.join("library.db");
            match Store::open(&path.to_string_lossy()) {
                Ok(store) => {
                    let jobs = store.load_jobs().unwrap_or_default();

                    // Seed on first run so a fresh install has usable presets
                    // without anyone opening Settings.
                    let mut presets = store.load_presets().unwrap_or_default();
                    if presets.is_empty() {
                        for p in Preset::seeds(&dirs_download().to_string_lossy()) {
                            let _ = store.save_preset(&p);
                        }
                        presets = store.load_presets().unwrap_or_default();
                    }

                    updates.send(move |this| {
                        this.store = Some(Arc::new(store));
                        this.jobs = jobs;
                        this.chosen_preset = presets
                            .iter()
                            .find(|p| p.is_default)
                            .or_else(|| presets.first())
                            .map(|p| p.name.clone());
                        this.presets = presets;
                    });
                }
                Err(e) => {
                    let msg = format!("could not open library: {e}");
                    updates.send(move |this| {
                        this.startup_error = Some(msg);
                    });
                }
            }
        });
    }

    // -- probe -------------------------------------------------------------

    /// Debounced so a probe does not fire on every keystroke while the user is
    /// still typing or pasting.
    fn schedule_probe(&mut self, url: String) {
        let generation = self.probe_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let counter = self.probe_gen.clone();

        if !url.trim_start().starts_with("http") {
            self.probe = ProbeState::Idle;
            return;
        }
        let Some(exe) = self.ytdlp.clone() else {
            return;
        };

        self.updates.spawn(move |updates| {
            std::thread::sleep(Duration::from_millis(500));
            // Superseded by a newer keystroke — drop this one. Read straight off
            // the shared counter rather than asking the app, so a stale probe
            // costs nothing and never spawns yt-dlp.
            if counter.load(Ordering::SeqCst) != generation {
                return;
            }
            updates.send(|this| {
                this.probe = ProbeState::Running;
            });

            let result = pollster::block_on(runner::probe(exe, url.trim().to_string()));
            updates.send(move |this| {
                if this.probe_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                this.probe = match result {
                    Ok(Ok(p)) => ProbeState::Ok(Box::new(p)),
                    Ok(Err(e)) => ProbeState::Err(e.to_string()),
                    Err(_) => ProbeState::Err("probe cancelled".into()),
                };
            });
        });
    }

    // -- download ----------------------------------------------------------

    fn start_download(&mut self) {
        let url = self.url_input.value().to_string();
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let Some(exe) = self.ytdlp.clone() else {
            return;
        };

        let _ = exe; // presence already checked; the launcher re-resolves it.

        // Resolve the preset now so the job records what it actually ran with,
        // even if the preset is edited later.
        let preset = self
            .chosen_preset
            .as_ref()
            .and_then(|n| self.presets.iter().find(|p| &p.name == n))
            .or_else(|| self.presets.iter().find(|p| p.is_default))
            .cloned();
        let preset_name = preset
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "default".into());

        let mut job = Job::new(&url, preset_name);
        job.state = JobState::Queued;
        if let ProbeState::Ok(p) = &self.probe {
            job.title = p.title.clone().unwrap_or_else(|| url.clone());
        } else {
            job.title = url.clone();
        }

        let job_id = job.id.clone();
        self.jobs.insert(0, job.clone());
        self.live.insert(job_id.clone(), Live::default());
        self.selected = Some(job_id.clone());
        self.open_item = None;
        self.modal = false;
        self.probe = ProbeState::Idle;
        // Deliberately does NOT switch to the In progress tab: Home already
        // lists the new job, so a jump would only move you off what you had.
        self.persist(job);

        let mut opts = preset.map(|p| p.options).unwrap_or_else(default_options);
        if opts.download_dir.trim().is_empty() {
            opts.download_dir = dirs_download().to_string_lossy().to_string();
        }
        // This-run override is appended after the preset's own extra args, so
        // later flags win — same precedence yt-dlp gives a repeated option.
        opts.extra_args.extend(
            self.override_input
                .value()
                .split_whitespace()
                .map(str::to_string),
        );
        let _ = std::fs::create_dir_all(&opts.download_dir);
        self.pending.push_back(PendingLaunch {
            job_id,
            url,
            opts,
            attempts: 0,
        });
        self.ensure_launcher();
    }

    /// Drains `pending`, spacing launches by [`LAUNCH_STAGGER`]. There is no cap
    /// on how many downloads end up running at once — only on how fast they
    /// start. A failed spawn is requeued with growing backoff, so a transient
    /// resource limit delays a job rather than killing it.
    fn ensure_launcher(&mut self) {
        if self.launcher_active {
            return;
        }
        self.launcher_active = true;
        self.launch_next();
    }

    /// One turn of the launch queue.
    ///
    /// The gpui build ran this as a loop on a background task that reached back
    /// into the entity for each step. Here the loop is driven from the thread
    /// that owns the state and only the *waiting* goes to a worker, which keeps
    /// every read and write of `pending` on one thread. The behaviour is the
    /// same: launches are spaced by [`LAUNCH_STAGGER`], a failed spawn goes to
    /// the back of the queue with growing backoff so one sick job cannot starve
    /// the rest, and the queue is only marked idle once it is empty.
    fn launch_next(&mut self) {
        let Some(mut pending) = self.pending.pop_front() else {
            self.launcher_active = false;
            return;
        };

        match self.launch(&pending) {
            Ok(()) => self.resume_launcher_after(LAUNCH_STAGGER),
            Err(_) if pending.attempts + 1 < MAX_LAUNCH_ATTEMPTS => {
                pending.attempts += 1;
                let wait = LAUNCH_BACKOFF * pending.attempts;
                self.pending.push_back(pending);
                self.resume_launcher_after(wait);
            }
            Err(e) => {
                let msg = format!("could not start yt-dlp after retries: {e}");
                let id = pending.job_id.clone();
                self.fail_job(&id, msg);
                self.launch_next();
            }
        }
    }

    fn resume_launcher_after(&self, wait: Duration) {
        self.updates.spawn(move |updates| {
            std::thread::sleep(wait);
            updates.send(|this| this.launch_next());
        });
    }

    /// Spawns one job and starts pumping its events. Errors here are retryable.
    fn launch(&mut self, pending: &PendingLaunch) -> anyhow::Result<()> {
        let exe = runner::ytdlp_path(None)?;
        let download = runner::spawn_download(&exe, &pending.url, &pending.opts)?;

        let job_id = pending.job_id.clone();
        self.cancels
            .insert(job_id.clone(), download.cancel_handle());
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Running;
            let snapshot = job.clone();
            self.persist(snapshot);
        }

        // `download.events` is already a plain futures channel fed by a thread
        // runner.rs spawned, so pumping it needs a thread and a block_on, not an
        // executor. `apply_event` still decides when the run ended; the flag is
        // how that decision gets back here.
        //
        // Deliberately not a round trip per event. This used to send a reply
        // channel with every line and block on it, which put the download's
        // whole event pipe behind the interface's frame rate: one slow repaint
        // stalled the pump, and the progress a repaint was drawing was
        // therefore always older than the one it was waiting on. A flag the
        // handler sets costs nothing and lets yt-dlp's output drain as fast as
        // it arrives. Ordering is unchanged -- `finish_job` is queued behind
        // every event already sent, on the same channel.
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.updates.spawn(move |updates| {
            let mut events = download.events;
            while let Some(event) = pollster::block_on(events.next()) {
                let id = job_id.clone();
                let flag = ended.clone();
                updates.send(move |this| {
                    if this.apply_event(&id, event) {
                        flag.store(true, Ordering::SeqCst);
                    }
                });
                if ended.load(Ordering::SeqCst) {
                    break;
                }
            }
            updates.send(move |this| this.finish_job(&job_id));
        });

        Ok(())
    }

    /// Folds one yt-dlp event into job state. Returns true when the run ended.
    fn apply_event(&mut self, job_id: &str, event: Event) -> bool {
        let mut ended = false;
        match event {
            Event::Progress(p) => {
                let live = self.live.entry(job_id.to_string()).or_default();
                if let Some(f) = p.fraction() {
                    live.fraction = Some(f);
                }
                live.speed = p.speed;
                live.eta = p.eta;
            }
            Event::Info(info) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                    let title = info.title.clone().unwrap_or_default();
                    if job.title.starts_with("http") && !title.is_empty() {
                        job.title = title.clone();
                    }
                    let webpage = info.webpage_url.clone().unwrap_or_default();
                    // One Item per video; re-emitted info for the same video updates in place.
                    if !job.items.iter().any(|i| i.webpage_url == webpage) {
                        job.items.push(Item {
                            id: new_id("itm"),
                            index: info.playlist_index.unwrap_or(job.items.len() as i64),
                            title,
                            duration: info.duration,
                            thumb_path: info.thumbnail.clone(),
                            webpage_url: webpage,
                            files: Vec::new(),
                        });
                    }
                }
            }
            Event::File(path) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                    let kind = classify(&path);
                    let bytes = std::fs::metadata(&path).ok().map(|m| m.len() as i64);
                    let thumb = sibling_thumbnail(&path);

                    // Attach to the most recent item, which is the one yt-dlp
                    // is currently working through.
                    if let Some(item) = job.items.last_mut() {
                        if !item.files.iter().any(|f| f.path == path) {
                            item.files.push(MFile {
                                id: new_id("fil"),
                                path,
                                kind,
                                format_id: None,
                                bytes,
                            });
                        }
                        // --write-thumbnail saves the cover beside the video but
                        // after_move only reports the video itself, so the image
                        // has to be found by stem.
                        if let Some(thumb) = thumb {
                            if !item.files.iter().any(|f| f.path == thumb) {
                                item.files.push(MFile {
                                    id: new_id("fil"),
                                    path: thumb.clone(),
                                    kind: FileKind::Thumbnail,
                                    format_id: None,
                                    bytes: None,
                                });
                            }
                            item.thumb_path = Some(thumb);
                        }
                    }
                }
            }
            Event::Log(line) => {
                if line == crate::core::runner::EXIT_OK {
                    ended = true;
                } else if let Some(code) = line.strip_prefix(crate::core::runner::EXIT_FAIL_PREFIX) {
                    let tail = self
                        .live
                        .get(job_id)
                        .map(|l| l.log.iter().rev().take(3).cloned().collect::<Vec<_>>().join(" | "))
                        .unwrap_or_default();
                    self.fail_job(job_id, format!("yt-dlp exited {code}: {tail}"));
                    ended = true;
                } else {
                    let live = self.live.entry(job_id.to_string()).or_default();
                    live.log.push(line);
                    // Only the tail matters for diagnosing a failure.
                    if live.log.len() > 200 {
                        live.log.drain(0..100);
                    }
                }
            }
        }
        ended
    }

    fn fail_job(&mut self, job_id: &str, error: String) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            // Killing the child makes yt-dlp exit non-zero, which would
            // otherwise be reported as a failure. A deliberate cancel wins.
            job.state = state_after_failure(job.state);
            if job.state == JobState::Failed {
                job.error = Some(error);
            }
            let snapshot = job.clone();
            self.persist(snapshot);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
    }

    fn finish_job(&mut self, job_id: &str) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            if job.state == JobState::Running {
                job.state = JobState::Done;
            }
            let snapshot = job.clone();
            self.persist(snapshot);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
    }

    /// Marks the job cancelled first, then kills the process — so the non-zero
    /// exit that follows is recognised as intentional.
    fn cancel_job(&mut self, job_id: &str) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Cancelled;
            job.error = None;
            let snapshot = job.clone();
            self.persist(snapshot);
        }
        // Drop it from the launch queue too, in case it never started.
        self.pending.retain(|p| p.job_id != job_id);
        if let Some(handle) = self.cancels.remove(job_id) {
            handle.cancel();
        }
        self.live.remove(job_id);
    }

    /// Removes a job from the list and library. Only for jobs that aren't
    /// actively downloading — cancel first.
    fn delete_job(&mut self, job_id: &str) {
        self.jobs.retain(|j| j.id != job_id);
        self.live.remove(job_id);
        self.cancels.remove(job_id);
        if self.selected.as_deref() == Some(job_id) {
            self.selected = None;
        }
        if let Some(store) = self.store.clone() {
            let job_id = job_id.to_string();
            self.updates.spawn(move |_| {
                if let Err(e) = store.delete_job(&job_id) {
                    eprintln!("rustydlp: failed to delete job {job_id}: {e}");
                }
            });
        }
    }

    /// Re-queues a failed or cancelled job. Partial `.part` files survive a
    /// kill, so yt-dlp resumes rather than starting the transfer again.
    fn retry_job(&mut self, job_id: &str) {
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
            return;
        };
        let url = job.url.clone();
        let preset_name = job.preset.clone();

        let mut opts = self
            .presets
            .iter()
            .find(|p| p.name == preset_name)
            .map(|p| p.options.clone())
            .unwrap_or_else(default_options);
        if opts.download_dir.trim().is_empty() {
            opts.download_dir = dirs_download().to_string_lossy().to_string();
        }
        let _ = std::fs::create_dir_all(&opts.download_dir);

        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Queued;
            job.error = None;
            let snapshot = job.clone();
            self.persist(snapshot);
        }
        self.live.insert(job_id.to_string(), Live::default());
        self.pending.push_back(PendingLaunch {
            job_id: job_id.to_string(),
            url,
            opts,
            attempts: 0,
        });
        self.ensure_launcher();
    }

    fn update_ytdlp(&mut self) {
        let Some(exe) = self.ytdlp.clone() else {
            return;
        };
        self.update_status = Some("Updating…".into());

        self.updates.spawn(move |updates| {
            let result = pollster::block_on(runner::update_ytdlp(exe));
            let msg = match result {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => format!("Update failed: {e}"),
                Err(_) => "Update task cancelled".into(),
            };
            updates.send(move |this| {
                this.update_status = Some(msg);
                this.refresh_ytdlp();
            });
        });
    }

    /// Writes a job snapshot off the UI thread. Called only on transitions.
    fn persist(&self, job: Job) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.updates.spawn(move |_| {
            if let Err(e) = store.save_job(&job) {
                eprintln!("rustydlp: failed to save job {}: {e}", job.id);
            }
        });
    }

    // -- presets -----------------------------------------------------------

    /// Copies a preset into the editor's text fields.
    fn edit_preset(&mut self, preset: Preset) {
        let o = &preset.options;
        let fonts = self.fonts.clone();
        let mut fonts = fonts.borrow_mut();
        let mut set = |e: &mut InputState, v: String| e.set_value(&mut fonts, &v);
        set(&mut self.form.name, preset.name.clone());
        set(
            &mut self.form.max_height,
            o.max_height.map(|h| h.to_string()).unwrap_or_default(),
        );
        set(
            &mut self.form.container,
            o.container.clone().unwrap_or_default(),
        );
        set(&mut self.form.output, o.output_template.clone());
        set(&mut self.form.dir, o.download_dir.clone());
        set(&mut self.form.subs, o.subtitle_langs.join(","));
        set(&mut self.form.extra, o.extra_args.join(" "));
        set(
            &mut self.form.codec,
            match &o.format {
                FormatMode::AudioOnly { codec } => codec.clone(),
                _ => String::new(),
            },
        );
        set(
            &mut self.form.custom_fmt,
            match &o.format {
                FormatMode::Custom(s) => s.clone(),
                _ => String::new(),
            },
        );
        self.editing = Some(preset);
    }

    /// Reads the text fields back onto the preset being edited and saves it.
    fn save_editing(&mut self) {
        let Some(mut preset) = self.editing.clone() else {
            return;
        };
        let read = |e: &InputState| e.value().trim().to_string();

        let name = read(&self.form.name);
        if name.is_empty() {
            return;
        }
        let previous_name = preset.name.clone();
        preset.name = name;

        let o = &mut preset.options;
        // A non-numeric height is treated as "no cap" rather than rejected —
        // the field is advisory and a modal error here would be worse.
        o.max_height = read(&self.form.max_height).parse::<u32>().ok();
        o.container = Some(read(&self.form.container)).filter(|s| !s.is_empty());
        let output = read(&self.form.output);
        o.output_template = if output.is_empty() {
            YtdlpOptions::default().output_template
        } else {
            output
        };
        o.download_dir = read(&self.form.dir);
        o.subtitle_langs = read(&self.form.subs)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Split on whitespace: this is a CLI fragment, so quoting is the
        // user's problem, same as typing it into a shell.
        o.extra_args = read(&self.form.extra)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        o.format = match &o.format {
            FormatMode::AudioOnly { .. } => FormatMode::AudioOnly {
                codec: {
                    let c = read(&self.form.codec);
                    if c.is_empty() { "mp3".into() } else { c }
                },
            },
            FormatMode::Custom(_) => FormatMode::Custom({
                let c = read(&self.form.custom_fmt);
                if c.is_empty() { "bv*+ba/b".into() } else { c }
            }),
            FormatMode::BestVideoAudio => FormatMode::BestVideoAudio,
        };

        // Renaming is a delete + insert; the name is the primary key.
        let renamed = previous_name != preset.name;
        if let Some(existing) = self.presets.iter_mut().find(|p| p.name == previous_name) {
            *existing = preset.clone();
        } else {
            self.presets.push(preset.clone());
        }
        if preset.is_default {
            for p in self.presets.iter_mut() {
                p.is_default = p.name == preset.name;
            }
        }
        if self.chosen_preset.as_deref() == Some(previous_name.as_str()) {
            self.chosen_preset = Some(preset.name.clone());
        }
        self.presets.sort_by(|a, b| a.name.cmp(&b.name));
        self.editing = Some(preset.clone());

        if let Some(store) = self.store.clone() {
            self.updates.spawn(move |_| {
                if renamed {
                    let _ = store.delete_preset(&previous_name);
                }
                if let Err(e) = store.save_preset(&preset) {
                    eprintln!("rustydlp: failed to save preset: {e}");
                }
            });
        }
    }

    fn delete_editing(&mut self) {
        let Some(preset) = self.editing.take() else {
            return;
        };
        self.presets.retain(|p| p.name != preset.name);
        if self.chosen_preset.as_deref() == Some(preset.name.as_str()) {
            self.chosen_preset = self.presets.first().map(|p| p.name.clone());
        }
        if let Some(store) = self.store.clone() {
            let name = preset.name.clone();
            self.updates.spawn(move |_| {
                let _ = store.delete_preset(&name);
            });
        }
    }

    // -- rendering ---------------------------------------------------------

    /// Full-width row above the main pane: wordmark pinned left, Home/In
    /// Progress tabs true-centered regardless of the wordmark's width.
    fn navbar(&self) -> AnyElement {
        let tabs = TabBar::new("navbar-tabs")
            .segmented()
            .selected_index(self.tab.index())
            .children([Tab::new().label("Home"), Tab::new().label("In Progress")])
            // The handler gets `&mut Self` directly, so the entity round-trip
            // the gpui build needed here is gone.
            .on_click(|this: &mut Self, ix: usize| {
                this.tab = SidebarTab::from_index(ix);
                // Switching tabs behind an open library popover would leave
                // it pointing at a screen you've navigated away from.
                this.close_popover();
            });

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
            // Three columns: wordmark left, tabs centered independent of the
            // wordmark's own width, caption buttons right. The left and right
            // columns share the same min width so the tabs stay centered
            // rather than drifting toward whichever side has less content.
            .child(
                div()
                    .flex_1()
                    .min_w(px(132.))
                    // The source is 1104x240 -- height-driven at that same
                    // ratio (~4.6:1) so `color_svg` (no object-fit of its
                    // own, unlike `img()`) doesn't stretch it.
                    .child(color_svg("icons/rustydlp-logo.svg").w(px(120.)).h(px(26.))),
            )
            // Deliberately no width: a fixed one was wider than the three
            // labels, and TabBar lays its tabs out from the left, so the
            // slack showed up as dead segmented background hanging off the
            // right-hand end. Left to size itself, the bar shrink-wraps the
            // tabs and the row stays centered.
            .child(div().flex_shrink_0().child(tabs))
            .child(
                div()
                    .flex_1()
                    .min_w(px(132.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("new-download")
                            .primary()
                            .icon(IconName::Plus)
                            .on_click(|this: &mut Self| {
                                this.modal = true;
                                this.probe = ProbeState::Idle;
                                // Re-probe here so installing yt-dlp while the
                                // app is open takes effect without a restart.
                                this.refresh_ytdlp();
                                // Overrides are per-download; never carry one
                                // silently into the next job.
                                let fonts = this.fonts.clone();
                                let mut fonts = fonts.borrow_mut();
                                this.override_input.set_value(&mut fonts, "");
                                this.url_input.set_focused(true);
                            }),
                    )
                    .child(
                        // The Convert tab folded into Home, but converting a
                        // local file that was never downloaded through this
                        // app still needs an entry point somewhere.
                        Button::new("convert-file")
                            .ghost()
                            .icon(IconName::Replace)
                            .on_click(|this: &mut Self| {
                                this.pick_file_to_convert();
                            }),
                    )
                    .child(
                        Button::new("settings")
                            .ghost()
                            .selected(self.route == Route::Settings)
                            .icon(IconName::Settings)
                            .on_click(|this: &mut Self| {
                                this.route = if this.route == Route::Settings {
                                    Route::Library
                                } else {
                                    Route::Settings
                                };
                                this.close_popover();
                            }),
                    )
                    .child(self.window_controls()),
            )
            .into_any_element()
    }

    /// The custom minimize/maximize/close buttons that replace the native
    /// title bar (`shell.rs` opens the window with `decorations(false)`, and
    /// the rest of `navbar()`'s own background is what `shell.rs` drags the
    /// window from, in place of the chrome that used to do both jobs).
    fn window_controls(&self) -> AnyElement {
        h_flex()
            .h_full()
            .items_center()
            .child(
                Button::new("win-minimize")
                    .ghost()
                    .small()
                    .icon(IconName::Minimize)
                    .on_click(|this: &mut Self| {
                        this.pending_window_action = Some(WindowAction::Minimize);
                    }),
            )
            .child(
                Button::new("win-maximize")
                    .ghost()
                    .small()
                    .icon(if self.window_maximized { IconName::Restore } else { IconName::Maximize })
                    .on_click(|this: &mut Self| {
                        this.pending_window_action = Some(WindowAction::ToggleMaximize);
                    }),
            )
            .child(
                Button::new("win-close")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .on_click(|this: &mut Self| {
                        this.pending_window_action = Some(WindowAction::Close);
                    }),
            )
            .into_any_element()
    }

    /// A yt-dlp-not-found (or library-open-failed) notice, floated over
    /// everything else rather than pushed into the layout flow: the old
    /// full-width banner shoved the navbar and every pane down a row, and its
    /// one-line message had no wrap, so the long path-and-flags diagnostic in
    /// `refresh_ytdlp`'s error string ran straight off the window's edge
    /// (visible, not just clipped, since nothing there constrained its
    /// width). Capped width plus `.truncate()` here keeps this a glanceable
    /// toast; `Open folder`/`Re-check` stay the way to actually act on it.
    fn startup_error_toast(&self, msg: String) -> AnyElement {
        v_flex()
            .id("startup-toast")
            .absolute()
            .right(px(16.))
            .bottom(px(16.))
            .max_w(px(340.))
            .p_3()
            .gap_2()
            .rounded(theme().radius)
            .border_1()
            .border_color(theme().danger.opacity(0.4))
            .bg(theme().sidebar)
            .child(div().text_xs().truncate().text_color(theme().danger).child(msg))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-bin-dir")
                            .small()
                            .icon(IconName::Folder)
                            .label("Open folder")
                            .on_click(|_: &mut Self| open_bin_dir()),
                    )
                    .child(
                        Button::new("banner-recheck")
                            .small()
                            .label("Re-check")
                            .on_click(|this: &mut Self| this.refresh_ytdlp()),
                    ),
            )
            .into_any_element()
    }

    fn job_row(&self, job: &Job) -> AnyElement {
        let id = job.id.clone();
        let active = self.selected.as_deref() == Some(job.id.as_str())
            && self.route == Route::Library;
        let live = self.live.get(&job.id);

        let subtitle = match job.state {
            JobState::Failed => "Failed".to_string(),
            JobState::Running => match live.and_then(|l| l.speed) {
                Some(s) => format!("{}/s", human_bytes(s)),
                None => "Starting…".to_string(),
            },
            _ => match job.items.len() {
                0 | 1 => "1 video".to_string(),
                n => format!("{n} videos"),
            },
        };

        // Selection fades in/out rather than snapping, animated against the
        // same hue at zero alpha rather than a plain transparent, so the
        // transition reads as a clean fade instead of a cross-hue blend.
        // Hover stays instant (`.hover()` below): the pointer's own position
        // is resolved at paint time, after the element tree already exists,
        // so there is no app-state moment to key a hover tween off.
        let target_bg =
            if active { theme().sidebar_accent } else { theme().sidebar_accent.opacity(0.0) };
        let bg = self.anim.tween_color(format!("job-bg-{}", job.id), target_bg);

        // `follow_f32`, not `tween_f32`: this bar is a readout of a number
        // yt-dlp keeps revising, not a surface settling after a click. On the
        // widget bounce it never reached one value before the next arrived, so
        // it ran permanently behind the real figure and jittered around it
        // where the overshoot met the next retarget.
        let target_pct = live.and_then(|l| l.fraction).map(|p| p.clamp(0.0, 1.0));
        let pct = target_pct.map(|p| self.anim.follow_f32(format!("job-pct-{}", job.id), p));

        v_flex()
            .id(SharedString::from(job.id.clone()))
            .w_full()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .rounded(theme().radius)
            .bg(bg)
            .hover(|this| this.bg(theme().list_hover))
            .cursor_pointer()
            .on_click(move |this: &mut Self| {
                this.selected = Some(id.clone());
                this.open_item = None;
                this.route = Route::Library;
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .text_color(theme().sidebar_foreground)
                    .child(job.title.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if job.state == JobState::Failed {
                        theme().danger
                    } else {
                        theme().sidebar_foreground.opacity(0.55)
                    })
                    .child(subtitle),
            )
            .when_some(pct, |this, pct| {
                this.child(
                    div()
                        .mt_1()
                        .w_full()
                        .h(px(3.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(theme().muted)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(theme().progress_bar)
                                // `follow_f32` does not overshoot, so this is
                                // already within 0..=1; the track's own
                                // `overflow_hidden` stays as the backstop.
                                .w(relative(pct)),
                        ),
                )
            })
            .into_any_element()
    }

    /// Which screen the main pane is showing, as a position on the one
    /// left-to-right axis the navbar lays its destinations out on: the two
    /// tabs in the order `TabBar` draws them, then Settings, which sits to the
    /// right of both in the toolbar. `main_pane` slides a swap along this
    /// axis, so the direction of travel matches where the thing you clicked
    /// actually is.
    fn pane_index(&self) -> u64 {
        match (self.route, self.tab) {
            (Route::Settings, _) => 2,
            (Route::Library, SidebarTab::Home) => 0,
            (Route::Library, SidebarTab::InProgress) => 1,
        }
    }

    /// The screen at `index` on that axis. Called for the screen arriving and,
    /// while a swap is playing, for the one leaving — both are built from the
    /// same state, so the outgoing one is a live render rather than a snapshot
    /// of the last frame (which this renderer, rebuilding the tree every
    /// frame, keeps nothing of).
    fn pane_at(&mut self, index: u64) -> AnyElement {
        match index {
            2 => self.settings(),
            1 => self.in_progress_pane(),
            _ => self.library(),
        }
    }

    fn main_pane(&mut self) -> AnyElement {
        // Opening the library popover does not change the index — the popover
        // is a separate overlay (see `library_popover`), not a swap of what
        // the main pane itself shows.
        let index = self.pane_index();
        let (leaving, progress) = self.anim.transition("main-pane", index);

        // Both screens are absolutely positioned inside a container that stays
        // normally flexed (so it keeps its slot in the window's column, at a
        // fixed size, while its contents move): this layout engine only
        // resolves an inset offset for an absolutely positioned box — the same
        // trick `stage()` uses to overlay the player frame without disturbing
        // its container's size. `left` alone, with `w_full` rather than
        // `inset_0`, is what makes that offset a slide instead of a squeeze.
        let pane = |offset: f32, opacity: f32, content: AnyElement| {
            div()
                .absolute()
                .top(px(0.))
                .left(px(offset))
                .w_full()
                .h_full()
                .flex()
                .opacity(opacity)
                .child(content)
        };

        let mut container = h_flex().flex_1().min_h_0().relative().overflow_hidden();

        if let Some(from) = leaving {
            // Forward when the destination sits further right on the axis.
            let direction = if index > from { 1.0 } else { -1.0 };
            let swap = page_swap(progress, direction, SWAP_DISTANCE);
            let outgoing = self.pane_at(from);
            let incoming = self.pane_at(index);
            container = container
                // Painted, but inert: the screen on its way out must not
                // answer a click aimed at the one arriving over it, and must
                // not light up under the pointer either.
                .child(
                    pane(swap.outgoing_offset, swap.outgoing_opacity, outgoing)
                        .pointer_events_none(),
                )
                .child(pane(swap.incoming_offset, swap.incoming_opacity, incoming));
        } else {
            container = container.child(pane(0.0, 1.0, self.pane_at(index)));
        }

        container.into_any_element()
    }

    /// Every finished download or conversion, newest first, as one flat grid
    /// — replaces the old sidebar job list, the Convert tab's own list, and
    /// the per-job "Videos" grid a multi-item playlist used to drill into,
    /// all three of which are now just tiles in the same library. Clicking a
    /// tile opens `library_popover`, not a swap of this pane's own content.
    fn library(&mut self) -> AnyElement {
        let mut jobs = filter_jobs(&self.jobs, SidebarTab::Home);
        jobs.sort_by_key(|j| std::cmp::Reverse(j.created_at));

        let mut tiles: Vec<AnyElement> = Vec::new();
        for job in &jobs {
            if job.items.is_empty() {
                tiles.push(self.library_tile(job, None));
            } else {
                for item in &job.items {
                    tiles.push(self.library_tile(job, Some(item)));
                }
            }
        }
        let is_empty = tiles.is_empty();

        v_flex()
            .id("library-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_5()
            .gap_4()
            .overflow_y_scroll()
            .child(div().font_bold().child("Library"))
            .when(is_empty, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h(px(280.))
                        .items_center()
                        .justify_center()
                        .gap_5()
                        .child(
                            svg()
                                .path("icons/empty-downloads.svg")
                                .w(px(128.))
                                .h(px(96.))
                                .text_color(theme().muted_foreground.opacity(0.5)),
                        )
                        .child(
                            div()
                                .text_color(theme().muted_foreground)
                                .child(SidebarTab::Home.empty_message()),
                        ),
                )
            })
            .child(div().flex().flex_wrap().gap_3().children(tiles))
            .into_any_element()
    }

    /// One library tile. `item` is `None` for a job that finished with
    /// nothing to show per-item (a failed/cancelled download before any item
    /// landed) — a convert job always has exactly one, per `finish_convert`.
    fn library_tile(&self, job: &Job, item: Option<&Item>) -> AnyElement {
        let key = item.map(|i| i.id.clone()).unwrap_or_else(|| job.id.clone());
        let title = item
            .map(|i| i.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| job.title.clone());
        let thumb = item.and_then(|i| i.thumb_path.as_deref());
        let duration = item.and_then(|i| i.duration);
        let bytes: i64 = item.map(|i| i.files.iter().filter_map(|f| f.bytes).sum()).unwrap_or(0);
        let playable = item.and_then(|i| {
            i.files
                .iter()
                .find(|f| {
                    matches!(f.kind, FileKind::Video | FileKind::Audio)
                        && std::path::Path::new(&f.path).is_file()
                })
                .map(|f| f.path.clone())
        });

        let subtitle = match (duration, bytes > 0) {
            (Some(d), true) => format!("{} \u{b7} {}", human_duration(d), human_bytes(bytes as f64)),
            (Some(d), false) => human_duration(d),
            (None, true) => human_bytes(bytes as f64),
            (None, false) => String::new(),
        };

        let (chip_label, chip_danger) = job_status_chip(job);
        let preview = self
            .hover_preview
            .as_ref()
            .filter(|p| p.item_id == key)
            .and_then(|p| p.frame.clone());

        let job_id = job.id.clone();
        let item_id = item.map(|i| i.id.clone());
        let hover_key = key.clone();

        v_flex()
            .id(SharedString::from(format!("tile-{key}")))
            .w(px(220.))
            .gap_2()
            .p_2()
            .rounded(theme().radius)
            .border_1()
            .border_color(theme().border)
            .hover(|this| this.bg(theme().list_hover))
            .cursor_pointer()
            .on_click(move |this: &mut Self| {
                this.stop_hover_preview();
                this.selected = Some(job_id.clone());
                this.open_item = item_id.clone();
                this.route = Route::Library;
            })
            .when_some(playable, |t, path| {
                let enter_key = hover_key.clone();
                let leave_key = hover_key.clone();
                t.on_hover(move |this: &mut Self, entered| {
                    if entered {
                        this.schedule_hover_preview(enter_key.clone(), path.clone());
                    } else {
                        this.stop_hover_preview_for(&leave_key);
                    }
                })
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(match preview {
                        Some(frame) => img(crate::ui::element::ImageSource::Rgba {
                            width: frame.width,
                            height: frame.height,
                            data: frame.data,
                            id: frame.pts.to_bits(),
                        })
                        .w_full()
                        .h(px(112.))
                        .rounded(theme().radius)
                        .overflow_hidden()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element(),
                        None => cover(thumb, px(112.), px(48.)),
                    })
                    .when(!chip_label.is_empty(), |t| {
                        t.child(
                            div()
                                .absolute()
                                .top(px(6.))
                                .left(px(6.))
                                .text_xs()
                                .px_1p5()
                                .rounded(theme().radius)
                                .bg(if chip_danger { theme().danger } else { black().opacity(0.6) })
                                .text_color(if chip_danger {
                                    theme().danger_foreground
                                } else {
                                    theme().foreground
                                })
                                .child(chip_label),
                        )
                    }),
            )
            .child(div().w_full().min_w_0().text_sm().truncate().child(title))
            .when(!subtitle.is_empty(), |t| {
                t.child(div().text_xs().text_color(theme().muted_foreground).child(subtitle))
            })
            .into_any_element()
    }

    /// In progress moved out of the sidebar and into here because it mixes
    /// download and convert jobs and needs room for a progress bar per row —
    /// the 260px sidebar column was too cramped for that.
    fn in_progress_pane(&self) -> AnyElement {
        let jobs = filter_jobs(&self.jobs, SidebarTab::InProgress);
        let is_empty = jobs.is_empty();
        let rows: Vec<AnyElement> = jobs.iter().map(|job| self.job_row(job)).collect();

        v_flex()
            .id("in-progress-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_5()
            .gap_2()
            .overflow_y_scroll()
            .child(div().font_bold().child("In Progress"))
            .when(is_empty, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme().muted_foreground)
                        .child(SidebarTab::InProgress.empty_message()),
                )
            })
            .children(rows)
            .into_any_element()
    }

    fn field(&self, label: &str, focus: Focus, input: &InputState) -> AnyElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme().muted_foreground)
                    .child(label.to_string()),
            )
            .child(Input::new(focus.element_id(), input))
            .into_any_element()
    }

    fn settings(&self) -> AnyElement {
        let preset_rows: Vec<AnyElement> = self
            .presets
            .iter()
            .map(|p| {
                let preset = p.clone();
                let active = self.editing.as_ref().map(|e| &e.name) == Some(&p.name);
                h_flex()
                    .id(SharedString::from(format!("preset-{}", p.name)))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .pl(4.0)
                    .pr(8.0)
                    .py_2()
                    .rounded(theme().radius)
                    .when(active, |t| t.bg(theme().sidebar_accent))
                    .hover(|t| t.bg(theme().list_hover))
                    .cursor_pointer()
                    .on_click(move |this: &mut Self| {
                        this.edit_preset(preset.clone());
                    })
                    // Selection rail rather than a full-row highlight alone, so the
                    // active preset stays legible even against the hover state.
                    .child(
                        div()
                            .w(px(3.))
                            .h(px(28.))
                            .rounded_full()
                            .when(active, |t| t.bg(theme().primary)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .truncate()
                                            .child(p.name.clone()),
                                    )
                                    .when(p.is_default, |t| {
                                        t.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_xs()
                                                .px_1p5()
                                                .rounded_full()
                                                .bg(theme().accent)
                                                .text_color(theme().accent_foreground)
                                                .child("Default"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(theme().muted_foreground)
                                    .child(preset_summary(&p.options)),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        let editor: AnyElement = match &self.editing {
            None => v_flex()
                .flex_1()
                .min_w_0()
                .min_h(px(280.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    svg()
                        .path(IconName::Settings.path())
                        .w(px(40.))
                        .h(px(40.))
                        .text_color(theme().muted_foreground.opacity(0.4)),
                )
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_sm().child("No preset selected")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme().muted_foreground)
                        .child("Choose a preset on the left, or create a new one."),
                )
                .into_any_element(),
            Some(editing) => {
                let o = &editing.options;
                let is_audio = matches!(o.format, FormatMode::AudioOnly { .. });
                let is_custom = matches!(o.format, FormatMode::Custom(_));
                let is_best = matches!(o.format, FormatMode::BestVideoAudio);
                let is_default = editing.is_default;

                // Source selector: a recessed track (page-level `background`,
                // darker than this card's own `surface`) reads as a single
                // control with one active segment, instead of three competing
                // buttons — the segmented-control shape that "pick one of
                // these formats" actually calls for.
                let source_control = v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme().muted_foreground)
                            .child("Source"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .p(4.0)
                            .rounded(theme().radius)
                            .bg(theme().background)
                            .border_1()
                            .border_color(theme().border)
                            .child(
                                Button::new("fmt-best")
                                    .ghost()
                                    .small()
                                    .flex_1()
                                    .selected(is_best)
                                    .label("Video + audio")
                                    .on_click(|this: &mut Self| {
                                        if let Some(e) = this.editing.as_mut() {
                                            e.options.format = FormatMode::BestVideoAudio;
                                        }
                                    }),
                            )
                            .child(
                                Button::new("fmt-audio")
                                    .ghost()
                                    .small()
                                    .flex_1()
                                    .selected(is_audio)
                                    .label("Audio only")
                                    .on_click(|this: &mut Self| {
                                        if let Some(e) = this.editing.as_mut() {
                                            e.options.format = FormatMode::AudioOnly {
                                                codec: "mp3".into(),
                                            };
                                        }
                                    }),
                            )
                            .child(
                                Button::new("fmt-custom")
                                    .ghost()
                                    .small()
                                    .flex_1()
                                    .selected(is_custom)
                                    .label("Custom -f")
                                    .on_click(|this: &mut Self| {
                                        if let Some(e) = this.editing.as_mut() {
                                            e.options.format =
                                                FormatMode::Custom("bv*+ba/b".into());
                                        }
                                    }),
                            ),
                    )
                    .into_any_element();

                // Fields that share a row when the editor is wide enough, and
                // wrap to their own line when it isn't — a grid rather than
                // one long single-file column.
                let mut format_fields = vec![
                    field_row(vec![grid_item(240., self.field("Name", Focus::Name, &self.form.name))]),
                    field_row(vec![grid_item(280., source_control)]),
                ];
                if is_audio {
                    format_fields.push(field_row(vec![grid_item(
                        180.,
                        self.field("Audio codec", Focus::Codec, &self.form.codec),
                    )]));
                }
                if is_custom {
                    format_fields.push(field_row(vec![grid_item(
                        280.,
                        self.field("Format selector", Focus::CustomFmt, &self.form.custom_fmt),
                    )]));
                }
                if is_best {
                    format_fields.push(field_row(vec![
                        grid_item(140., self.field("Max height", Focus::MaxHeight, &self.form.max_height)),
                        grid_item(140., self.field("Container", Focus::Container, &self.form.container)),
                    ]));
                }

                v_flex()
                    .gap_4()
                    .child(settings_section("Format", format_fields))
                    .child(settings_section(
                        "Output",
                        vec![
                            field_row(vec![grid_item(
                                280.,
                                self.field("Output template", Focus::Output, &self.form.output),
                            )]),
                            field_row(vec![
                                grid_item(220., self.field("Download folder", Focus::Dir, &self.form.dir)),
                                grid_item(
                                    160.,
                                    self.field("Subtitle languages", Focus::Subs, &self.form.subs),
                                ),
                            ]),
                        ],
                    ))
                    .child(settings_section(
                        "Post-processing",
                        vec![field_row(vec![
                            grid_item(
                                220.,
                                toggle(
                                    "sw-thumb",
                                    "Embed thumbnail",
                                    o.embed_thumbnail,
                                    |o| &mut o.embed_thumbnail,
                                ),
                            ),
                            grid_item(
                                220.,
                                toggle(
                                    "sw-meta",
                                    "Embed metadata",
                                    o.embed_metadata,
                                    |o| &mut o.embed_metadata,
                                ),
                            ),
                            grid_item(
                                220.,
                                toggle("sw-subs", "Embed subtitles", o.embed_subs, |o| {
                                    &mut o.embed_subs
                                }),
                            ),
                            grid_item(
                                220.,
                                toggle(
                                    "sw-archive",
                                    "Skip already-downloaded (archive)",
                                    o.download_archive,
                                    |o| &mut o.download_archive,
                                ),
                            ),
                        ])],
                    ))
                    .child(settings_section(
                        "Advanced",
                        vec![
                            // The escape hatch that makes "all yt-dlp options" true.
                            self.field("Additional arguments", Focus::Extra, &self.form.extra),
                        ],
                    ))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pt_3()
                            .border_t_1()
                            .border_color(theme().border)
                            .child(
                                Switch::new("sw-default")
                                    .checked(is_default)
                                    .label("Use as default")
                                    .on_click(|this: &mut Self, checked: bool| {
                                        if let Some(e) = this.editing.as_mut() {
                                            e.is_default = checked;
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("preset-delete")
                                            .ghost()
                                            .text_color(theme().danger)
                                            .label("Delete")
                                            .on_click(|this: &mut Self| {
                                                this.delete_editing();
                                            }),
                                    )
                                    .child(
                                        Button::new("preset-save")
                                            .primary()
                                            .label("Save preset")
                                            .on_click(|this: &mut Self| {
                                                this.save_editing();
                                            }),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        };

        // General: app-level behavior, not tied to any one preset. Full width
        // and first, since it's the one section every user hits regardless of
        // how many presets they keep.
        let general = settings_section(
            "General",
            vec![
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme().muted_foreground)
                                    .child("yt-dlp"),
                            )
                            .child(div().text_sm().truncate().child(
                                self.ytdlp
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| "Not found".into()),
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .child(
                                Button::new("recheck-bin")
                                    .ghost()
                                    .small()
                                    .border_1()
                                    .border_color(theme().border)
                                    .label("Re-check binaries")
                                    .on_click(|this: &mut Self| {
                                        this.refresh_ytdlp();
                                    }),
                            )
                            .child(
                                Button::new("update-ytdlp")
                                    .ghost()
                                    .small()
                                    .border_1()
                                    .border_color(theme().border)
                                    .label("Update yt-dlp")
                                    .on_click(|this: &mut Self| {
                                        this.update_ytdlp();
                                    }),
                            ),
                    )
                    .into_any_element(),
                div()
                    .text_xs()
                    .text_color(theme().muted_foreground)
                    .child(
                        self.update_status.clone().unwrap_or_else(|| {
                            "Downloads start staggered; no concurrency cap.".into()
                        }),
                    )
                    .into_any_element(),
                settings_row("Minimize to tray on close", coming_soon_badge()),
                settings_row("Theme", coming_soon_badge()),
            ],
        );

        // Presets: a narrow picker beside the editor for whichever one is
        // selected, so switching preset and tweaking it happen side by side
        // instead of losing your place navigating between two pages.
        let presets = settings_section(
            "Presets",
            vec![
                h_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .w(px(220.))
                            .flex_shrink_0()
                            .gap_0p5()
                            .child(v_flex().gap_0p5().children(preset_rows))
                            .child(
                                Button::new("preset-new")
                                    .ghost()
                                    .w_full()
                                    .mt_2()
                                    .border_1()
                                    .border_color(theme().border)
                                    .icon(IconName::Plus)
                                    .label("New preset")
                                    .on_click(|this: &mut Self| {
                                        let preset = Preset {
                                            name: "New preset".into(),
                                            is_default: false,
                                            options: default_options(),
                                        };
                                        this.edit_preset(preset);
                                    }),
                            ),
                    )
                    .child(editor)
                    .into_any_element(),
            ],
        );

        v_flex()
            .id("settings-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_5()
            .gap_5()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    // Settings is the one screen you arrive at from a toolbar
                    // button rather than from the tab bar, so it is also the
                    // one with nowhere obvious to go back to: the tab bar
                    // shows which tab is selected, not that you are three
                    // levels away from it. This is that way back, and it
                    // returns to whichever tab you left, not to a fixed one.
                    .child(
                        Button::new("settings-back")
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .on_click(|this: &mut Self| {
                                this.route = Route::Library;
                                this.close_popover();
                            }),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_size(px(20.), px(28.)).font_bold().child("Settings"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme().muted_foreground)
                                    .child("App behavior, and presets for how yt-dlp fetches and saves a download."),
                            ),
                    ),
            )
            .child(general)
            .child(presets)
            .into_any_element()
    }

    // -- player --------------------------------------------------------------

    /// Starts/stops native playback so it always matches whatever file
    /// `detail()` is currently showing. `target` is the playable file path
    /// for the item on screen, or `None` when nothing playable is showing
    /// (multi-item grid, empty state, Settings, ...). `duration_hint` is the
    /// length recorded at download time, used only when ffprobe can't report
    /// one of its own.
    fn ensure_player(
        &mut self,
        target: Option<&str>,
        duration_hint: Option<f64>,
    ) {
        match target {
            None => self.stop_player(),
            Some(path) => {
                let already_loaded = self.player.as_ref().map(|p| p.path.as_str()) == Some(path);
                let already_loading = self.player_pending.as_deref() == Some(path);
                let failed = self.player_failed.as_deref() == Some(path);
                if already_loaded || already_loading || failed {
                    return;
                }
                self.stop_player();
                self.player_pending = Some(path.to_string());
                self.load_player(path.to_string(), 0.0, duration_hint);
            }
        }
    }

    fn stop_player(&mut self) {
        if let Some(p) = self.player.take() {
            p.control.stop();
        }
        self.player_pending = None;
        self.player_failed = None;
        // Any in-flight load's result will still arrive, but its generation
        // will no longer match — see `load_player`.
        self.player_gen.fetch_add(1, Ordering::SeqCst);
    }

    /// Resolves ffmpeg/ffprobe, probes, and starts playback entirely off the
    /// UI thread (both are blocking calls) — see `player::load`.
    fn load_player(
        &mut self,
        path: String,
        start_at_secs: f64,
        duration_hint: Option<f64>,
    ) {
        let generation = self.player_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let rx = player::load(
            PathBuf::from(&path),
            start_at_secs,
            self.effective_volume(),
        );

        self.updates.spawn(move |updates| {
            let loaded = pollster::block_on(rx);
            let (info, control, mut events) = match loaded {
                Ok(Ok((info, control, events))) => (info, control, events),
                _ => {
                    // ponytail: silent fallback to the static poster +
                    // Open Externally, which always works regardless of why
                    // native playback couldn't start (missing ffmpeg, no
                    // video stream, unsupported codec, ...).
                    updates.send(move |this| {
                        if this.player_pending.as_deref() == Some(path.as_str()) {
                            this.player_pending = None;
                            this.player_failed = Some(path.clone());
                            // A seek that fails to restart leaves the old,
                            // already-stopped run on screen; drop it so the
                            // fallback poster takes over rather than a player
                            // whose buttons do nothing.
                            if this.player.as_ref().is_some_and(|p| p.path == path) {
                                this.player = None;
                            }
                        }
                    });
                    return;
                }
            };

            // The install has to happen on the app's thread but its answer is
            // needed here, so it round-trips through a one-shot.
            let (tx, rx) = std::sync::mpsc::channel();
            let install_path = path.clone();
            updates.send(move |this| {
                    // Superseded by a newer load (navigated away, sought
                    // again) before this one finished starting up — tear it
                    // down rather than let it leak an ffmpeg process nobody
                    // is watching.
                    if this.player_gen.load(Ordering::SeqCst) != generation {
                        control.stop();
                        let _ = tx.send(false);
                        return;
                    }
                    // A seek keeps the outgoing run's last frame on screen so
                    // the view doesn't drop back to the poster (losing the
                    // controls with it) while ffmpeg restarts.
                    let carried_frame = this
                        .player
                        .as_ref()
                        .filter(|p| p.path == install_path)
                        .and_then(|p| p.frame.clone());
                    this.player_pending = None;
                    this.player = Some(PlayerState {
                        control,
                        path: install_path,
                        frame: carried_frame,
                        position_secs: start_at_secs,
                        duration_secs: info.duration.or(duration_hint),
                        playing: true,
                        ended: false,
                        scrubbing: false,
                    });
                    let _ = tx.send(true);
            });

            // A closed channel means the app is gone, so there is nothing left
            // to pump frames to.
            if !rx.recv().unwrap_or(false) {
                return;
            }

            while let Some(mut event) = pollster::block_on(events.next()) {
                // Coalesce a frame backlog: the decode thread paces itself to
                // real time regardless of how fast this loop's round trip to
                // the UI thread runs, so if that round trip is ever slow
                // (a busy repaint, GC-style pauses), frames queue up here —
                // 8MB apiece at 1080p, per the channel being unbounded (see
                // docs/parity-deviations.md). Jump straight to the newest
                // queued frame instead of painting through the backlog one
                // stale frame at a time, which is what "lag" looks like from
                // the outside. Stops as soon as something isn't a Frame (an
                // Ended must still be handled, not skipped) or nothing else
                // is queued right now.
                while let player::PlayerEvent::Frame(..) = event {
                    match events.try_recv() {
                        Ok(newer) => event = newer,
                        Err(_) => break,
                    }
                }
                let (tx, rx) = std::sync::mpsc::channel();
                updates.send(move |this| {
                        if this.player_gen.load(Ordering::SeqCst) != generation {
                            let _ = tx.send(false);
                            return;
                        }
                        match event {
                            player::PlayerEvent::Frame(frame, pts) => {
                                if let Some(p) = this.player.as_mut() {
                                    // The decode threads hand over plain RGBA; the
                                    // interface wraps it as an image source. No
                                    // renderer type crosses into `core/`.
                                    p.frame = Some(Frame {
                                        width: frame.width,
                                        height: frame.height,
                                        data: Arc::new(frame.rgba),
                                        pts,
                                    });
                                    // While the thumb is being dragged the
                                    // readout belongs to it, not to the run
                                    // still playing behind it.
                                    if !p.scrubbing {
                                        p.position_secs = pts;
                                    }
                                }
                            }
                            player::PlayerEvent::Ended => {
                                if let Some(p) = this.player.as_mut() {
                                    p.playing = false;
                                    p.ended = true;
                                }
                            }
                        }
                        let _ = tx.send(true);
                });
                // A closed channel means the app is gone; stop pumping rather
                // than decode frames nobody will draw.
                if !rx.recv().unwrap_or(false) {
                    break;
                }
            }
        });
    }

    // -- hover preview ---------------------------------------------------

    /// Debounced so sweeping the pointer across a row of tiles doesn't spawn
    /// an ffmpeg per tile passed over — only the one the pointer actually
    /// settles on, after it's stayed there a moment.
    fn schedule_hover_preview(&mut self, item_id: String, path: String) {
        let generation = self.preview_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let counter = self.preview_gen.clone();
        self.preview_pending = Some(item_id.clone());

        self.updates.spawn(move |updates| {
            std::thread::sleep(Duration::from_millis(350));
            if counter.load(Ordering::SeqCst) != generation {
                return;
            }
            updates.send(move |this| {
                if this.preview_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                this.start_hover_preview(item_id, path);
            });
        });
    }

    /// A separate, minimal decode pipeline from `load_player`'s — a preview
    /// has no transport (position, duration, play/pause), plays muted, and
    /// never seeks — rather than reusing `PlayerState`/`ensure_player` and
    /// parameterizing away all of that.
    fn start_hover_preview(&mut self, item_id: String, path: String) {
        let generation = self.preview_gen.fetch_add(1, Ordering::SeqCst) + 1;
        // Muted: this is ambient motion behind a thumbnail, not something the
        // pointer resting there for a moment should make audible.
        let rx = player::load(PathBuf::from(&path), 0.0, 0.0);

        self.updates.spawn(move |updates| {
            let loaded = pollster::block_on(rx);
            let Ok(Ok((_info, control, mut events))) = loaded else {
                return;
            };

            let (tx, rx) = std::sync::mpsc::channel();
            let install_id = item_id.clone();
            updates.send(move |this| {
                if this.preview_gen.load(Ordering::SeqCst) != generation {
                    control.stop();
                    let _ = tx.send(false);
                    return;
                }
                this.hover_preview =
                    Some(PreviewState { control, item_id: install_id, frame: None });
                this.preview_pending = None;
                let _ = tx.send(true);
            });
            if !rx.recv().unwrap_or(false) {
                return;
            }

            while let Some(mut event) = pollster::block_on(events.next()) {
                // Same backlog-coalescing as `load_player`: only the newest
                // queued frame matters for a preview.
                while let player::PlayerEvent::Frame(..) = event {
                    match events.try_recv() {
                        Ok(newer) => event = newer,
                        Err(_) => break,
                    }
                }
                let (tx, rx) = std::sync::mpsc::channel();
                updates.send(move |this| {
                    if this.preview_gen.load(Ordering::SeqCst) != generation {
                        let _ = tx.send(false);
                        return;
                    }
                    if let player::PlayerEvent::Frame(frame, pts) = event
                        && let Some(p) = this.hover_preview.as_mut()
                    {
                        p.frame = Some(Frame {
                            width: frame.width,
                            height: frame.height,
                            data: Arc::new(frame.rgba),
                            pts,
                        });
                    }
                    // A preview that reaches the end just holds its last
                    // frame — no loop, no controls to restart it, and a tile
                    // isn't asking for more attention than a first glance.
                    let _ = tx.send(true);
                });
                if !rx.recv().unwrap_or(false) {
                    break;
                }
            }
        });
    }

    fn stop_hover_preview(&mut self) {
        self.preview_pending = None;
        self.preview_gen.fetch_add(1, Ordering::SeqCst);
        if let Some(p) = self.hover_preview.take() {
            p.control.stop();
        }
    }

    /// Only stops the preview if it's still the one `key` started — a leave
    /// event for a tile that's no longer the active preview (superseded by a
    /// faster enter elsewhere before this one's debounce even fired) should
    /// be a no-op, not a wrong stop.
    fn stop_hover_preview_for(&mut self, key: &str) {
        let is_pending = self.preview_pending.as_deref() == Some(key);
        let is_active = self.hover_preview.as_ref().is_some_and(|p| p.item_id == key);
        if is_pending || is_active {
            self.stop_hover_preview();
        }
    }

    fn toggle_play(&mut self) {
        // Once the pipes have run dry there is nothing to un-pause, so Play
        // on a finished file means "play it again".
        if self.player.as_ref().is_some_and(|p| p.ended) {
            self.seek_to(0.0);
            return;
        }
        if let Some(p) = self.player.as_mut() {
            p.playing = !p.playing;
            p.control.set_paused(!p.playing);
        }
    }

    /// Drag feedback only: moves the readout without touching playback, so a
    /// drag across the bar doesn't spawn an ffmpeg per pixel.
    fn scrub_to(&mut self, fraction: f32) {
        let Some(p) = self.player.as_mut() else {
            return;
        };
        let Some(duration) = p.duration_secs else {
            return;
        };
        p.scrubbing = true;
        p.position_secs = (fraction as f64 * duration).clamp(0.0, duration);
    }

    fn seek_to_fraction(&mut self, fraction: f32) {
        let Some(duration) = self.player.as_ref().and_then(|p| p.duration_secs) else {
            return;
        };
        self.seek_to((fraction as f64 * duration).clamp(0.0, duration));
    }

    /// ffmpeg can't seek in place over a pipe, so a seek is a fresh pair of
    /// processes started with a new `-ss`. The old run is stopped up front
    /// (otherwise its audio keeps playing through the restart) but its state
    /// — including the last frame — stays on screen until the new run
    /// delivers.
    fn seek_to(&mut self, secs: f64) {
        let Some(p) = self.player.as_mut() else {
            return;
        };
        let path = p.path.clone();
        let duration = p.duration_secs;
        let secs = secs.max(0.0);

        p.control.stop();
        p.position_secs = secs;
        p.scrubbing = false;
        p.ended = false;
        // A seek always resumes: leaving a frozen frame from the old position
        // on screen while the new one decodes reads as a hang.
        p.playing = true;

        // Without this, the `ensure_player` call in the next render sees no
        // load in flight and restarts the file from the beginning.
        self.player_pending = Some(path.clone());
        self.load_player(path, secs, duration);
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        // Reaching for the slider is also how you come back from mute.
        self.muted = false;
        self.apply_volume();
    }

    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.apply_volume();
    }

    /// What actually reaches the audio callback. Mute is kept separate from
    /// the slider's value so un-muting restores the level you had.
    ///
    /// Cubed rather than passed straight through: ears perceive loudness
    /// roughly logarithmically, but the slider position is linear, so a
    /// linear gain multiplier crams almost all the audible change into the
    /// top of the track and leaves the bottom half sounding barely quieter.
    /// Cubing is the standard cheap taper for this — closer to how volume
    /// controls are expected to feel than true dB math, which would need a
    /// clamp of its own to avoid -infinity at zero.
    fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume.powi(3) }
    }

    fn apply_volume(&mut self) {
        if let Some(p) = self.player.as_ref() {
            p.control.set_volume(self.effective_volume());
        }
    }

    /// The poster/player element that replaces the old static thumbnail:
    /// shows the live decoded frame once playback has started, otherwise
    /// falls back to the plain cover art.
    fn player_view(
        &mut self,
        item: Option<&Item>,
    ) -> AnyElement {
        let thumb = item.and_then(|i| i.thumb_path.as_deref());

        // A load is in flight for this item, or one has started and no frame
        // has come back yet. Either way there is nothing to watch and no
        // percentage to quote, which is exactly what the bar is for.
        let loading = self.player_pending.is_some();

        let Some(player) = &self.player else {
            return self.stage(cover(thumb, relative(1.), px(96.)), loading);
        };
        let Some(frame) = player.frame.clone() else {
            return self.stage(cover(thumb, relative(1.), px(96.)), true);
        };

        // Read out before syncing: the slider is behind `&mut self`, and the
        // borrow of `self.player` above would still be live otherwise.
        let position = player.position_secs;
        let duration = player.duration_secs;
        let scrubbing = player.scrubbing;
        let playing = player.playing;
        self.sync_seek_slider(position, duration, scrubbing);

        v_flex()
            // Takes the detail pane's leftover height so the picture grows
            // with the window; min_h_0 is what lets it shrink again, since a
            // column flex item's automatic minimum is its content height.
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                self.stage(
                    // Positioned against the stage rather than sized in
                    // percentages: an `img` left at Length::Auto gets the
                    // frame's natural size (1920x1080) forced on it, which
                    // would overflow and clip.
                    img(crate::ui::element::ImageSource::Rgba {
                        width: frame.width,
                        height: frame.height,
                        data: frame.data,
                        id: frame.pts.to_bits(),
                    })
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .into_any_element(),
                    // True while a seek is respawning ffmpeg behind the last
                    // frame of the run it replaced.
                    loading,
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new("player-toggle")
                            .small()
                            .icon(if playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            })
                            .on_click(|this: &mut Self| this.toggle_play()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme().muted_foreground)
                            .child(match duration {
                                Some(d) => {
                                    format!("{} / {}", human_duration(position), human_duration(d))
                                }
                                None => human_duration(position),
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            // Nothing to seek against without a duration: a
                            // live stream, or a container ffprobe can't
                            // measure.
                            .child(Slider::new("seek", &self.seek_slider).disabled(duration.is_none())),
                    )
                    .child(self.volume_controls()),
            )
            .into_any_element()
    }

    /// The box the decoded frame — or the poster, before the first frame
    /// arrives — is drawn into.
    ///
    /// Flexes with the window instead of sitting at a fixed height: pinned at
    /// 300px, maximising the window only widened the black surround while a
    /// 1080p frame stayed letterboxed into the same short strip. `Contain`
    /// keeps the whole frame visible whatever shape the pane ends up, and
    /// `relative` is what the frame positions itself against.
    ///
    /// `loading` puts an indeterminate bar over it. Opening a file means
    /// resolving ffmpeg, probing it and waiting for a first decoded frame, all
    /// off this thread — seconds, for a large file on a cold cache — and until
    /// now every one of those seconds looked exactly like a still poster that
    /// had simply decided not to play. Seeking lands here too: the outgoing
    /// run's last frame deliberately stays on screen while the new one spins
    /// up, so without this the picture just sits there, frozen and unexplained.
    fn stage(&self, content: AnyElement, loading: bool) -> AnyElement {
        div()
            .relative()
            .w_full()
            .flex_1()
            // A floor, so a short window shrinks the picture rather than
            // losing it entirely behind the controls.
            .min_h(px(180.))
            .rounded(theme().radius)
            .overflow_hidden()
            .bg(black())
            .child(content)
            .when(loading, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        // Enough of a scrim to read the label against a bright
                        // poster, not enough to hide what is being opened.
                        .bg(black().opacity(0.35))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme().foreground.opacity(0.85))
                                .child("Loading…"),
                        )
                        .child(self.indeterminate_bar("stage-loading", px(140.))),
                )
            })
            .into_any_element()
    }

    /// A bar for work with no measurable progress: a segment sweeping the
    /// track, leaving one end as it enters the other, so the motion never
    /// stops or doubles back.
    ///
    /// Everything else animated in this app is going somewhere — a screen to
    /// its resting place, a card to its open size, a progress bar to a figure
    /// yt-dlp reported. This is the one indicator that admits it has no idea
    /// how far along it is, which is why it loops (`Animator::cycle`) rather
    /// than tweening toward anything.
    fn indeterminate_bar(&self, key: &str, width: Pixels) -> AnyElement {
        let phase = self.anim.cycle(key, LOADING_SWEEP);
        let segment = width.0 * 0.35;
        // Starts fully off the left edge and ends fully off the right, so the
        // wrap at the end of each period is invisible.
        let x = -segment + (width.0 + segment) * phase;
        div()
            .relative()
            .w(width)
            .h(px(3.))
            .rounded_full()
            .overflow_hidden()
            .bg(theme().muted)
            .child(
                div()
                    .absolute()
                    .left(px(x))
                    .w(px(segment))
                    .h_full()
                    .rounded_full()
                    .bg(theme().progress_bar),
            )
            .into_any_element()
    }

    /// Keeps the seek thumb on the playhead. Deliberately conditional:
    /// `set_value` notifies, and notifying every frame for a value that
    /// hasn't moved is a render loop.
    fn sync_seek_slider(
        &mut self,
        position: f64,
        duration: Option<f64>,
        scrubbing: bool,
    ) {
        // While the thumb is under the mouse it owns the value.
        if scrubbing {
            return;
        }
        let fraction = match duration {
            Some(d) if d > 0.0 => (position / d).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        };
        if (self.seek_slider.value() - fraction).abs() <= 0.0005 {
            return;
        }
        self.seek_slider.set_value(fraction);
    }

    fn volume_controls(&self) -> AnyElement {
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .child(
                Button::new("player-mute")
                    .small()
                    .ghost()
                    .icon(Icon::empty().path(if self.muted || self.volume <= 0.0 {
                        "icons/volume-muted.svg"
                    } else {
                        "icons/volume.svg"
                    }))
                    .on_click(|this: &mut Self| this.toggle_mute()),
            )
            .child(div().w(px(88.)).child(Slider::new("volume", &self.volume_slider)))
            .into_any_element()
    }

    fn detail(
        &mut self,
        job: &Job,
        item: Option<&Item>,
    ) -> AnyElement {
        let title = item
            .map(|i| i.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| job.title.clone());

        let files: Vec<AnyElement> = item
            .map(|i| {
                i.files
                    .iter()
                    .map(|f| {
                        // Files get moved and deleted outside the app; showing a
                        // stale name as if it were still there is worse than
                        // saying so. ponytail: stat at render time — a handful of
                        // rows, not a directory walk.
                        let present = std::path::Path::new(&f.path).is_file();
                        h_flex()
                            .w_full()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .truncate()
                                    .when(!present, |t| {
                                        t.text_color(theme().muted_foreground.opacity(0.6))
                                    })
                                    .child(file_name(&f.path)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .flex_shrink_0()
                                    .text_color(if present {
                                        theme().muted_foreground
                                    } else {
                                        theme().danger
                                    })
                                    .child(if present {
                                        match f.bytes {
                                            Some(b) => human_bytes(b as f64),
                                            None => f.kind.as_str().to_string(),
                                        }
                                    } else {
                                        "missing".to_string()
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();

        // The first present video/audio file is what Play and Show act on.
        let playable = item.and_then(|i| {
            i.files
                .iter()
                .find(|f| {
                    matches!(f.kind, FileKind::Video | FileKind::Audio)
                        && std::path::Path::new(&f.path).is_file()
                })
                .map(|f| f.path.clone())
        });
        let job_id = job.id.clone();
        let running = is_active(job.state);
        let retryable = is_retryable(job.state);

        self.ensure_player(playable.as_deref(), item.and_then(|i| i.duration));
        let player_view = self.player_view(item);
        // Borrowed only after the `&mut self` calls above — holding this
        // across them would conflict with the mutable borrow they need.
        let live = self.live.get(&job.id);

        h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_5()
                    .gap_4()
                    .child(player_view)
                    .child(div().font_bold().child(title))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .when_some(playable.clone(), |this, path| {
                                let open = path.clone();
                                let convert_source = path.clone();
                                this.child(
                                    Button::new("open-externally")
                                        .small()
                                        .icon(IconName::Play)
                                        .label("Open Externally")
                                        .on_click(move |_: &mut Self| open_path(&open)),
                                )
                                .child(
                                    Button::new("convert")
                                        .small()
                                        .icon(IconName::Replace)
                                        .label("Convert")
                                        .on_click(move |this: &mut Self| {
                                            this.convert_picker = Some(convert_source.clone());
                                        }),
                                )
                                .child(
                                    Button::new("reveal")
                                        .small()
                                        .icon(IconName::Folder)
                                        .label("Show in folder")
                                        .on_click(move |_: &mut Self| reveal_path(&path)),
                                )
                            })
                            .when(running, |this| {
                                let id = job_id.clone();
                                this.child(
                                    Button::new("cancel-job")
                                        .small()
                                        .danger()
                                        .label("Cancel")
                                        .on_click(move |this: &mut Self| {
                                            this.cancel_job(&id);
                                        }),
                                )
                            })
                            .when(retryable, |this| {
                                let id = job_id.clone();
                                this.child(
                                    Button::new("retry-job")
                                        .small()
                                        .primary()
                                        .label("Retry")
                                        .on_click(move |this: &mut Self| {
                                            this.retry_job(&id);
                                        }),
                                )
                            })
                            .when(!running, |this| {
                                let id = job_id.clone();
                                this.child(
                                    Button::new("delete-job")
                                        .small()
                                        .ghost()
                                        .danger()
                                        .label("Delete")
                                        .on_click(move |this: &mut Self| {
                                            this.delete_job(&id);
                                        }),
                                )
                            }),
                    )
                    .when_some(job.error.clone(), |this, err| {
                        this.child(
                            div()
                                .p_3()
                                .rounded(theme().radius)
                                .bg(theme().danger.opacity(0.12))
                                .text_xs()
                                .text_color(theme().danger)
                                .child(err),
                        )
                    })
                    .when_some(live.and_then(|l| l.fraction), |this, pct| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(6.))
                                        .rounded_full()
                                        .bg(theme().muted)
                                        .child(
                                            div()
                                                .h_full()
                                                .rounded_full()
                                                .bg(theme().progress_bar)
                                                .w(relative(pct)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme().muted_foreground)
                                        .child(format!("{:.0}%", pct * 100.0)),
                                ),
                        )
                    }),
            )
            .child(
                // The collapsible params panel from the spec.
                v_flex()
                    .w(px(280.))
                    .flex_shrink_0()
                    .h_full()
                    .p_4()
                    .gap_3()
                    .border_l_1()
                    .border_color(theme().border)
                    .child(div().font_bold().text_sm().child("Details"))
                    .child(self.kv("State", job.state.as_str()))
                    .child(self.kv("Preset", &job.preset))
                    .child(self.kv("Videos", &job.items.len().to_string()))
                    .child(self.kv("Source", &job.url))
                    .when(!files.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(div().font_bold().text_sm().mt_2().child("Files"))
                                .children(files),
                        )
                    }),
            )
            .into_any_element()
    }

    fn kv(&self, key: &str, value: &str) -> AnyElement {
        v_flex()
            .w_full()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(theme().muted_foreground)
                    .child(key.to_string()),
            )
            .child(div().w_full().min_w_0().text_xs().truncate().child(value.to_string()))
            .into_any_element()
    }

    /// Closes the library popover (if one is open) and stops whatever it was
    /// playing. Called on an explicit close, and from anywhere navigation
    /// makes the popover's target stale (switching tabs, opening Settings).
    fn close_popover(&mut self) {
        if self.selected.is_none() {
            return;
        }
        self.selected = None;
        self.open_item = None;
        self.detail_fullscreen = false;
        // The rectangles stay: `render` runs them backwards to collapse the
        // card into the tile again. Only the identity is dropped, so the next
        // open anchors to whatever *it* was clicked from.
        if let Some(m) = &mut self.morph {
            m.identity = CLOSED;
        }
        self.ensure_player(None, None);
    }

    /// The overlay a library tile or an In Progress row opens into: `detail`'s
    /// content, framed as a card over a dimmed backdrop instead of replacing
    /// the whole main pane, with its own close and fullscreen controls.
    ///
    /// It morphs out of the tile you clicked: the card starts at that tile's
    /// rectangle (`shell.rs` hands it over, since only the laid-out box list
    /// knows where a tile ended up) and grows to its resting size, which is a
    /// fraction of the window, centred. There is no transform in this
    /// renderer, so this is not a scaled-up snapshot the way a compositor
    /// would do it: the card is genuinely laid out at each intermediate size,
    /// its overflow clipped, with `detail`'s content fading in over the second
    /// half once there is room to read it. The fullscreen toggle then resizes
    /// the same card through a retargetable `tween_f32`, so toggling
    /// mid-animation reverses from where it is rather than jumping.
    fn library_popover(&mut self) -> Option<AnyElement> {
        let job_id = self.selected.clone()?;
        let job = self.jobs.iter().find(|j| j.id == job_id)?.clone();
        let item = match &self.open_item {
            Some(id) => job.items.iter().find(|i| &i.id == id).cloned(),
            None => job.items.first().cloned(),
        };

        // Changes only when a *different* job/item opens, not on every
        // re-render of the one already showing — that identity change is
        // what tells `entrance_progress` to replay from zero.
        let identity = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            job.id.hash(&mut h);
            item.as_ref().map(|i| &i.id).hash(&mut h);
            // Low bit forced so an identity can never be `CLOSED`, which
            // `render` parks this same key at while the popover is shut.
            h.finish() | 1
        };
        let progress = self.anim.entrance_progress("popover", identity);

        let fullscreen = self.detail_fullscreen;
        let w = self.anim.tween_f32("popover-w", if fullscreen { 0.97 } else { 0.72 });
        let h = self.anim.tween_f32("popover-h", if fullscreen { 0.95 } else { 0.8 });
        let to = centered_in(self.viewport, w, h);

        // Anchored once per open: `from` is the tile that was clicked and must
        // not move under the animation, while `to` is re-read every frame
        // because the fullscreen tween is still steering it. `close_popover`
        // parks the identity at `CLOSED`, which is what makes the next open
        // take its own tile rather than reuse this one's.
        let anchored = self.morph.as_ref().map(|m| (m.identity, m.from));
        let from = match anchored {
            Some((anchor, from)) if anchor == identity => from,
            // Opened by something that was not a click on a tile (or before
            // the shell ever routed one): grow from the middle of where the
            // card will be, which is the old fade-and-rise in geometry form.
            _ => self.click_origin.take().unwrap_or_else(|| inset_by(to, 0.85)),
        };
        let thumb = item.as_ref().and_then(|i| i.thumb_path.clone());
        self.morph = Some(Morph { identity, from, to, thumb: thumb.clone() });
        let rect = morph_rect(from, to, progress);

        // The dimmed backdrop and the card's own frame come up quickly -- they
        // are what make the growing rectangle legible -- while the content
        // inside waits until the card is nearly full size. Laid out at a
        // tile's 220px, `detail` is a column of clipped fragments; faded in
        // late, the card reads as opening rather than as a screen being
        // rebuilt.
        //
        // The thumbnail carries the card until then, and hands over *fast*:
        // it is full-bleed while `detail` frames the same picture in its
        // stage, so a long cross-fade between them shows the one image twice,
        // at two crops. A short one reads as a dissolve.
        let frame_in = (progress / 0.2).clamp(0.0, 1.0);
        let content_in = ((progress - 0.4) / 0.3).clamp(0.0, 1.0);
        let art_in = 1.0 - ((progress - 0.4) / 0.15).clamp(0.0, 1.0);

        let content = self.detail(&job, item.as_ref());

        Some(
            div()
                .absolute()
                .inset_0()
                // Dimmed in step with the card's growth, rather than arriving
                // as a separate dark sheet over the window.
                .bg(black().opacity(0.55 * frame_in))
                .on_click(|this: &mut Self| this.close_popover())
                .child(
                    div()
                        .id("popover-card")
                        .absolute()
                        .left(px(rect.x))
                        .top(px(rect.y))
                        .w(px(rect.width))
                        .h(px(rect.height))
                        .opacity(frame_in)
                        .rounded(theme().radius)
                        .border_1()
                        .border_color(theme().border)
                        .bg(theme().background)
                        .overflow_hidden()
                        // Swallows the click here instead of letting it fall
                        // through to the backdrop's close handler — see
                        // `dispatch_click`'s ancestor walk.
                        .on_click(|_: &mut Self| {})
                        .children(morph_art(thumb.as_deref(), art_in))
                        .child(div().absolute().inset_0().opacity(content_in).child(content))
                        .child(
                            h_flex()
                                .absolute()
                                .top(px(8.))
                                .right(px(8.))
                                .gap_1()
                                // With the content, not with the frame: a
                                // close button floating in a 220px-wide card
                                // that is still growing is not offering
                                // anything worth clicking yet.
                                .opacity(content_in)
                                .child(
                                    Button::new("popover-fullscreen")
                                        .ghost()
                                        .small()
                                        .icon(if fullscreen {
                                            IconName::Restore
                                        } else {
                                            IconName::Maximize
                                        })
                                        .on_click(|this: &mut Self| {
                                            this.detail_fullscreen = !this.detail_fullscreen;
                                        }),
                                )
                                .child(
                                    Button::new("popover-close")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Close)
                                        .on_click(|this: &mut Self| this.close_popover()),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The popover's exit: the same two rectangles as the entrance, run the
    /// other way, so the card collapses back into the tile it came from.
    ///
    /// The frame and the item's thumbnail — no `detail` content. Rebuilding
    /// that content for a view that is on its way out would restart the
    /// playback `close_popover` just stopped (`detail` is what drives the
    /// player), and at the sizes the last third of this plays at there would
    /// be nothing legible in it anyway. The thumbnail is what makes it a morph
    /// rather than a shrinking box: it is the same picture the tile shows, so
    /// the card visibly goes back to where it came from. Inert, too: the click that closed it has already been answered,
    /// and the window underneath is live again from that same frame.
    fn popover_collapse(&self, morph: &Morph, progress: f32) -> AnyElement {
        let rect = morph_rect(morph.to, morph.from, progress);
        let out = 1.0 - progress.clamp(0.0, 1.0);
        div()
            .absolute()
            .inset_0()
            .pointer_events_none()
            .bg(black().opacity(0.55 * out))
            .child(
                div()
                    .id("popover-collapse")
                    .absolute()
                    .left(px(rect.x))
                    .top(px(rect.y))
                    .w(px(rect.width))
                    .h(px(rect.height))
                    .opacity(out)
                    .rounded(theme().radius)
                    .border_1()
                    .border_color(theme().border)
                    .bg(theme().background)
                    .overflow_hidden()
                    .children(morph_art(morph.thumb.as_deref(), 1.0)),
            )
            .into_any_element()
    }

    fn modal(&self, progress: f32) -> AnyElement {
        let preview: AnyElement = match &self.probe {
            ProbeState::Idle => div().into_any_element(),
            ProbeState::Running => div()
                .text_xs()
                .text_color(theme().muted_foreground)
                .child("Fetching info…")
                .into_any_element(),
            ProbeState::Err(e) => div()
                .text_xs()
                .text_color(theme().danger)
                .child(e.clone())
                .into_any_element(),
            ProbeState::Ok(p) => {
                let count = p.item_count();
                let sub = if p.is_playlist() {
                    format!("Playlist · {count} videos")
                } else {
                    match p.duration {
                        Some(d) => format!("Video · {}", human_duration(d)),
                        None => "Video".to_string(),
                    }
                };
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(p.title.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme().muted_foreground)
                            .child(sub),
                    )
                    .into_any_element()
            }
        };

        let (opacity, offset) = overlay_entrance(progress);

        // ponytail: hand-rolled overlay rather than gpui_component::dialog —
        // one absolutely-positioned div, no modal-manager lifecycle to learn.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(opacity)
            .bg(black().opacity(0.5))
            .child(
                v_flex()
                    .relative()
                    .top(px(offset))
                    .w(px(520.))
                    .p_5()
                    .gap_4()
                    .rounded(theme().radius)
                    .border_1()
                    .border_color(theme().border)
                    .bg(theme().background)
                    .child(div().font_bold().child("New download"))
                    .child(Input::new("url", &self.url_input))
                    .child(preview)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme().muted_foreground)
                                    .child("Preset"),
                            )
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(self.presets.iter().map(|p| {
                                        let name = p.name.clone();
                                        let active =
                                            self.chosen_preset.as_deref() == Some(p.name.as_str());
                                        Button::new(SharedString::from(format!("pick-{}", p.name)))
                                            .small()
                                            .selected(active)
                                            .label(p.name.clone())
                                            .on_click(move |this: &mut Self| {
                                                this.chosen_preset = Some(name.clone());
                                            })
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme().muted_foreground)
                                    .child("Override for this download (optional)"),
                            )
                            .child(Input::new("override", &self.override_input)),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(|this: &mut Self| {
                                        this.modal = false;
                                    }),
                            )
                            .child(
                                Button::new("go")
                                    .primary()
                                    .label("Download")
                                    .on_click(|this: &mut Self| {
                                        this.start_download();
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    // -- convert -------------------------------------------------------------

    /// Opens the native file picker so conversion works on any local video,
    /// not just something this app downloaded.
    fn pick_file_to_convert(&mut self) {
        // On a worker thread: the dialog is modal and blocking, and running it on
        // the thread that owns the interface would freeze the window behind it.
        self.updates.spawn(|updates| {
            let Some(path) = native_file_prompt("Select a video to convert") else {
                return;
            };
            updates.send(move |this| {
                this.convert_picker = Some(path.to_string_lossy().to_string());
            });
        });
    }

    fn convert_format_picker(&self, progress: f32) -> AnyElement {
        let Some(source) = self.convert_picker.clone() else {
            return div().into_any_element();
        };

        let (opacity, offset) = overlay_entrance(progress);

        // ponytail: hand-rolled overlay, same as the New download modal —
        // one absolutely-positioned div, no modal-manager lifecycle to learn.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(opacity)
            .bg(black().opacity(0.5))
            .child(
                v_flex()
                    .relative()
                    .top(px(offset))
                    .w(px(420.))
                    .p_5()
                    .gap_3()
                    .rounded(theme().radius)
                    .border_1()
                    .border_color(theme().border)
                    .bg(theme().background)
                    .child(div().font_bold().child("Convert to…"))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(theme().muted_foreground)
                            .child(file_name(&source)),
                    )
                    .child(v_flex().gap_2().children(ConvertFormat::ALL.into_iter().map(|format| {
                        let source = source.clone();
                        Button::new(SharedString::from(format!("convert-as-{}", format.as_str())))
                            .w_full()
                            .label(format.label())
                            .on_click(move |this: &mut Self| {
                                this.start_convert(source.clone(), format);
                            })
                    })))
                    .child(
                        h_flex().justify_end().child(
                            Button::new("cancel-convert")
                                .ghost()
                                .label("Cancel")
                                .on_click(|this: &mut Self| {
                                    this.convert_picker = None;
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// Creates and immediately launches a Convert job. No staggering queue
    /// like downloads get — conversions are expected to run one at a time in
    /// practice, and ffmpeg re-encodes are CPU-bound rather than
    /// network-rate-limited, so there is no equivalent reason to space them
    /// out. ponytail: if concurrent converts become common, give this the
    /// same PendingLaunch/backoff treatment as downloads.
    fn start_convert(&mut self, source: String, format: ConvertFormat) {
        self.convert_picker = None;

        let mut job = Job::new_convert(source.clone(), format.as_str());
        job.title = format!("{} → {}", file_name(&source), format.label());
        job.state = JobState::Queued;
        let job_id = job.id.clone();
        self.jobs.insert(0, job.clone());
        self.live.insert(job_id.clone(), Live::default());
        self.selected = Some(job_id.clone());
        self.open_item = None;
        self.persist(job);

        let ffmpeg = match runner::ffmpeg_path(None) {
            Ok(p) => p,
            Err(e) => {
                self.fail_job(&job_id, format!("ffmpeg not available: {e}"));
                return;
            }
        };
        let convert = match runner::spawn_convert(&ffmpeg, std::path::Path::new(&source), format) {
            Ok(c) => c,
            Err(e) => {
                self.fail_job(&job_id, format!("could not start ffmpeg: {e}"));
                return;
            }
        };

        self.cancels.insert(job_id.clone(), convert.cancel_handle());
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Running;
            let snapshot = job.clone();
            self.persist(snapshot);
        }

        // Duration is only known once ffmpeg reports it isn't — fall back to
        // an indeterminate progress bar (no fraction) if the source has none
        // recorded (e.g. a file picked outside any downloaded item).
        let duration = self
            .jobs
            .iter()
            .flat_map(|j| j.items.iter())
            .find(|i| i.files.iter().any(|f| f.path == source))
            .and_then(|i| i.duration);
        let output_path = convert.output_path.to_string_lossy().to_string();

        // No round trip per event, for the reason `launch` gives.
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.updates.spawn(move |updates| {
            let mut events = convert.events;
            while let Some(event) = pollster::block_on(events.next()) {
                let id = job_id.clone();
                let flag = ended.clone();
                updates.send(move |this| {
                    if this.apply_convert_event(&id, duration, event) {
                        flag.store(true, Ordering::SeqCst);
                    }
                });
                if ended.load(Ordering::SeqCst) {
                    break;
                }
            }
            updates.send(move |this| this.finish_convert(&job_id, &output_path));
        });
    }

    fn apply_convert_event(
        &mut self,
        job_id: &str,
        duration: Option<f64>,
        event: ConvertEvent,
    ) -> bool {
        let mut ended = false;
        match event {
            ConvertEvent::Progress(p) => {
                let live = self.live.entry(job_id.to_string()).or_default();
                live.fraction = match (p.out_time_secs, duration) {
                    (Some(t), Some(d)) if d > 0.0 => Some((t / d).clamp(0.0, 1.0) as f32),
                    _ => None,
                };
                live.speed = p.speed;
            }
            ConvertEvent::Log(line) => {
                if line == crate::core::runner::EXIT_OK {
                    ended = true;
                } else if let Some(code) = line.strip_prefix(crate::core::runner::EXIT_FAIL_PREFIX) {
                    self.fail_job(job_id, format!("ffmpeg exited {code}"));
                    ended = true;
                }
            }
        }
        ended
    }

    /// Attaches the converted file as a single-item Job (mirroring what a
    /// download job looks like) so it shows up in the Convert tab's list and
    /// can be played/opened the same way.
    fn finish_convert(&mut self, job_id: &str, output_path: &str) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            if job.state == JobState::Running {
                job.state = JobState::Done;
                let bytes = std::fs::metadata(output_path).ok().map(|m| m.len() as i64);
                job.items = vec![Item {
                    id: new_id("itm"),
                    index: 0,
                    title: job.title.clone(),
                    duration: None,
                    thumb_path: None,
                    webpage_url: String::new(),
                    files: vec![MFile {
                        id: new_id("fil"),
                        path: output_path.to_string(),
                        kind: classify(output_path),
                        format_id: None,
                        bytes,
                    }],
                }];
            }
            let snapshot = job.clone();
            self.persist(snapshot);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
    }
}

/// Opens the app-data folder yt-dlp is expected in.
///
/// Creates it first: the failure this button accompanies is often that the
/// directory doesn't resolve, and pointing Explorer at a missing path just
/// produces a second error instead of somewhere to drop the binary.
fn open_bin_dir() {
    let dir = runner::bin_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
}

/// Opens a file with whatever the shell has associated with it.
/// `start` is a cmd builtin, hence the `cmd /C`; the empty "" is the window
/// title argument, without which a quoted path is swallowed as the title.
fn open_path(path: &str) {
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", path]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd.spawn();
}

/// The raw `/select,"path"` argument for Explorer. Split out from
/// `reveal_path` so the quoting -- the actual bug it exists to fix -- is
/// testable without spawning a real Explorer process.
///
/// `Command::arg` was quoting the *whole* `/select,<path>` string whenever
/// the path had a space in it -- true of nearly every download, since
/// yt-dlp's output template embeds the title -- which puts the opening
/// quote *before* `/select,` in the actual command line Explorer sees.
/// Explorer's own parser only recognises `/select,` unquoted, so that never
/// matched and it silently fell back to its default window instead of the
/// file. Quoting only the path here, then handing the whole thing to
/// Explorer as one pre-built argument (see `reveal_path`'s `raw_arg`), is
/// the syntax Explorer actually expects.
fn select_arg(path: &str) -> String {
    let native = path.replace('/', "\\");
    format!("/select,\"{native}\"")
}

/// Opens Explorer with the file selected. `/select,` needs the path in the
/// same argument, and Explorer rejects forward slashes here.
fn reveal_path(path: &str) {
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(select_arg(path));
    }
    #[cfg(not(windows))]
    cmd.arg(select_arg(path));
    let _ = cmd.spawn();
}

/// A labelled switch bound to one bool on the preset being edited.
/// Takes an accessor so each toggle is one call rather than a copied closure.
fn toggle(
    id: &'static str,
    label: &'static str,
    checked: bool,
    field: fn(&mut YtdlpOptions) -> &mut bool,
) -> AnyElement {
    Switch::new(id)
        .checked(checked)
        .label(label)
        .on_click(move |this: &mut RustyDlp, checked: bool| {
            if let Some(editing) = this.editing.as_mut() {
                *field(&mut editing.options) = checked;
            }
        })
        .into_any_element()
}

/// A raised card grouping related preset fields, so the editor reads as
/// distinct sections (what to fetch, where it goes, what happens after)
/// instead of one long flat list of inputs.
/// The item's thumbnail, full-bleed, for the card to carry while it is
/// morphing between the tile and its open size — the one thing both ends of
/// the morph actually show, and what turns a growing rectangle into the
/// picture you clicked opening up.
///
/// `None` when there is no thumbnail on disk, rather than `cover`'s
/// placeholder box: that box is `muted`, and a card-sized sheet of it flashing
/// over the window is a worse start than the card's own background, which is
/// what the detail view behind it is anyway.
fn morph_art(thumb: Option<&str>, opacity: f32) -> Option<AnyElement> {
    let path = thumb.filter(|p| std::path::Path::new(p).is_file())?;
    Some(
        div()
            .absolute()
            .inset_0()
            .opacity(opacity)
            .child(img(PathBuf::from(path)).size_full().object_fit(ObjectFit::Cover))
            .into_any_element(),
    )
}

/// A box of `w` x `h` of the viewport, centred in it — where the library
/// popover comes to rest, and what it morphs back into the tile from.
fn centered_in(viewport: (f32, f32), w: f32, h: f32) -> Bounds {
    let (width, height) = (viewport.0 * w, viewport.1 * h);
    Bounds {
        x: (viewport.0 - width) / 2.0,
        y: (viewport.1 - height) / 2.0,
        width,
        height,
    }
}

/// `rect` scaled about its own centre. The fallback origin for a popover that
/// was opened by something other than a click on a tile: with no rectangle to
/// come from, it grows a little out of where it is going to be.
fn inset_by(rect: Bounds, scale: f32) -> Bounds {
    let (width, height) = (rect.width * scale, rect.height * scale);
    Bounds {
        x: rect.x + (rect.width - width) / 2.0,
        y: rect.y + (rect.height - height) / 2.0,
        width,
        height,
    }
}

/// The identity an overlay's entrance is parked at while it is closed. Any
/// real overlay identity is a hash, and `library_popover` forces the low bit
/// so none of them can collide with it.
const CLOSED: u64 = 0;

/// The entrance every overlay plays: a fade of the whole thing, backdrop
/// included, with the card itself rising the last few pixels into place.
///
/// One function so the popover and both modals move identically — three
/// overlays that appeared three different ways is most of what "disconnected"
/// meant. Returns `(opacity, offset)`; the offset eases with the page-swap
/// curve rather than the widget bounce, for the reason `ease_out_cubic`
/// gives.
fn overlay_entrance(progress: f32) -> (f32, f32) {
    let t = progress.clamp(0.0, 1.0);
    (t, OVERLAY_RISE - OVERLAY_RISE * crate::ui::Animator::ease_out(t))
}

/// One pass of the indeterminate bar across its track. Slow enough to read as
/// deliberate rather than frantic; fast enough that a glance catches movement.
const LOADING_SWEEP: Duration = Duration::from_millis(1100);

/// How far an overlay's card rises into place.
const OVERLAY_RISE: f32 = 12.0;

fn settings_section(title: &str, children: Vec<AnyElement>) -> AnyElement {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(theme().radius)
        .bg(theme().surface)
        .border_1()
        .border_color(theme().border)
        .child(div().text_sm().font_bold().child(title.to_string()))
        .children(children)
        .into_any_element()
}

/// Lets a field claim `min_w` before it's forced to wrap onto its own line,
/// so `field_row` reads as a real grid rather than evenly-split columns that
/// crush a long path or format string.
fn grid_item(min_w: f32, el: AnyElement) -> AnyElement {
    div().flex_1().min_w(px(min_w)).child(el).into_any_element()
}

/// A row of fields that sit side by side when the editor is wide enough and
/// stack when it isn't, instead of every field taking the full card width
/// regardless of how short its value is.
fn field_row(items: Vec<AnyElement>) -> AnyElement {
    h_flex().flex_wrap().gap_3().children(items).into_any_element()
}

/// A label-plus-control row for a general setting, e.g. pairing "Theme" with
/// its (possibly not-yet-real) control.
fn settings_row(label: &str, control: AnyElement) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .child(div().text_sm().child(label.to_string()))
        .child(control)
        .into_any_element()
}

/// Marks a setting that's laid out but not wired up yet, rather than faking a
/// toggle that looks live but does nothing when clicked.
fn coming_soon_badge() -> AnyElement {
    div()
        .text_xs()
        .px_1p5()
        .rounded_full()
        .bg(theme().muted)
        .text_color(theme().muted_foreground)
        .child("Coming soon")
        .into_any_element()
}

/// One-line gist of what a preset actually does, shown under its name in the
/// sidebar so picking the right preset doesn't require opening each one.
fn preset_summary(o: &YtdlpOptions) -> String {
    match &o.format {
        FormatMode::BestVideoAudio => match (o.max_height, &o.container) {
            (Some(h), Some(c)) => format!("Up to {h}p \u{b7} {c}"),
            (Some(h), None) => format!("Up to {h}p"),
            (None, Some(c)) => format!("Best quality \u{b7} {c}"),
            (None, None) => "Best video + audio".into(),
        },
        FormatMode::AudioOnly { codec } => format!("Audio only \u{b7} {}", codec.to_uppercase()),
        FormatMode::Custom(_) => "Custom format selector".into(),
    }
}

/// Cover art box: the real thumbnail when one exists on disk, otherwise the
/// placeholder glyph. Checks `is_file` because the DB may name a cover the user
/// has since deleted, and gpui would otherwise render a broken-image gap.
///
/// `height` is a `Length` rather than `Pixels` so the same box serves both the
/// fixed-size grid cards and the player's stage, which fills whatever height
/// the window gives it.
fn cover(
    thumb: Option<&str>,
    height: impl Into<Length>,
    glyph: Pixels,
) -> AnyElement {
    let existing = thumb.filter(|p| std::path::Path::new(p).is_file());
    // Converted up front: `Styled::h` takes its argument by value and both
    // arms need it.
    let height: Length = height.into();

    // Always builds the placeholder box, then lays the real thumbnail over
    // it when one exists on disk, rather than choosing one or the other.
    // This element tree has no decode-failure hook (unlike gpui's
    // `with_fallback`): `draw_image` just paints nothing when
    // `skia_safe::Image::from_encoded` fails, so without the placeholder
    // underneath, a corrupt or unsupported sidecar image (an interrupted
    // write, a webp variant this build can't decode) would show as an empty
    // box instead of the glyph. `is_file()` above only proves the path
    // exists, not that it decodes.
    div()
        .w_full()
        .h(height)
        .rounded(theme().radius)
        .bg(theme().muted)
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path("icons/empty-downloads.svg")
                .w(glyph)
                .h(glyph * 0.75)
                .text_color(theme().muted_foreground.opacity(0.4)),
        )
        .when_some(existing, |this, path| {
            this.child(
                img(PathBuf::from(path))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .rounded(theme().radius)
                    .object_fit(ObjectFit::Cover),
            )
        })
        .into_any_element()
}

/// Single definition of "still working". The sidebar filter and the detail
/// pane's Cancel button previously each had their own copy and had already
/// drifted — one counted Probing, the other didn't.
fn is_active(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued | JobState::Probing | JobState::Running
    )
}

/// Only a job that stopped short can be retried; Done has nothing to redo.
fn is_retryable(state: JobState) -> bool {
    matches!(state, JobState::Failed | JobState::Cancelled)
}

/// Killing the child makes yt-dlp exit non-zero, which the event pump reports
/// as a failure. A deliberate cancel must not be relabelled by its own kill.
fn state_after_failure(current: JobState) -> JobState {
    if current == JobState::Cancelled {
        JobState::Cancelled
    } else {
        JobState::Failed
    }
}

/// Home lists every job (either kind) that has finished one way or another;
/// In Progress lists whatever is still doing work. Free function so it is
/// testable without constructing a Window.
fn filter_jobs(jobs: &[Job], tab: SidebarTab) -> Vec<&Job> {
    jobs.iter()
        .filter(|j| match tab {
            SidebarTab::Home => !is_active(j.state),
            SidebarTab::InProgress => is_active(j.state),
        })
        .collect()
}

/// The small badge a library tile shows over its thumbnail: what happened to
/// this job, in the one word there's room for.
fn job_status_chip(job: &Job) -> (&'static str, bool) {
    match job.state {
        JobState::Failed => ("Failed", true),
        JobState::Cancelled => ("Cancelled", true),
        _ => match job.kind {
            JobKind::Convert => ("Converted", false),
            JobKind::Download => ("Downloaded", false),
        },
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn classify(path: &str) -> FileKind {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "srt" | "vtt" | "ass" => FileKind::Subtitle,
        "jpg" | "jpeg" | "png" | "webp" => FileKind::Thumbnail,
        "json" => FileKind::Info,
        "m4a" | "mp3" | "opus" | "ogg" | "flac" | "wav" => FileKind::Audio,
        _ => FileKind::Video,
    }
}

impl RustyDlp {
    /// Builds this frame's element tree.
    ///
    /// A plain method rather than a trait impl: there is no framework to satisfy,
    /// and the caller is the event loop, which lays the tree out and paints it.
    pub fn render(&mut self) -> AnyElement {
        let navbar = self.navbar();
        let main = self.main_pane();
        // `library_popover` calls `detail`, which starts/stops playback to
        // match whatever's open; with no popover open there's nothing to
        // drive that, so this is the one place that has to say "stop"
        // explicitly, covering every path that closes it (the close button,
        // switching tabs, opening Settings) in one spot.
        let popover = self.library_popover();
        // Resets the popover's entrance while nothing is open. Without a
        // closed-state poll its identity would still be the last item's, so
        // reopening that same tile would find nothing changed and skip the
        // animation entirely — the case where a transition is most obviously
        // missing is the one you repeat. It also gives the collapse below the
        // progress to run on.
        let closing = if popover.is_none() {
            self.ensure_player(None, None);
            let (was_open, progress) = self.anim.transition("popover", CLOSED);
            match (was_open, self.morph.as_ref()) {
                (Some(_), Some(morph)) => Some(self.popover_collapse(morph, progress)),
                _ => None,
            }
        } else {
            None
        };
        // Same reason these are polled every frame rather than only while
        // their overlay is up.
        let modal_in = self.anim.entrance_progress("modal", self.modal as u64);
        let convert_in = self
            .anim
            .entrance_progress("convert-modal", self.convert_picker.is_some() as u64);
        let modal = self.modal.then(|| self.modal(modal_in));
        let convert_modal = self
            .convert_picker
            .is_some()
            .then(|| self.convert_format_picker(convert_in));
        let toast = self.startup_error.clone().map(|msg| self.startup_error_toast(msg));

        div()
            .relative()
            .size_full()
            .bg(theme().background)
            .text_color(theme().foreground)
            .child(v_flex().size_full().child(navbar).child(main))
            .children(popover)
            .children(closing)
            .children(modal)
            .children(convert_modal)
            .children(toast)
            .into_any_element()
    }
}

/// The native open-file dialog.
///
/// Windows-only: rfd's Linux backend is xdg-portal, which pulls in tokio — the
/// runtime this crate deliberately keeps out. The app ships for Windows; builds
/// elsewhere exist to run the interface's tests, where no dialog is opened.
fn native_file_prompt(title: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        rfd::FileDialog::new().set_title(title).pick_file()
    }
    #[cfg(not(windows))]
    {
        let _ = title;
        None
    }
}

#[cfg(test)]
mod tests {
    // Explicit rather than a glob. Under gpui this was mandatory -- `use super::*`
    // re-globbed gpui's own `test` attribute macro over the built-in one and
    // `#[test]` expanded into itself until the recursion limit. That hazard is
    // gone with gpui; the explicit list stays because it documents what is
    // actually under test.
    use super::{
        AnyElement, Job, JobKind, JobState, LOADING_SWEEP, Route, RustyDlp, SidebarTab, Updates,
        filter_jobs, is_active, is_retryable, select_arg, state_after_failure,
    };
    use crate::core::model::{File as MFile, FileKind, Item};
    use crate::render::Backend;
    use crate::render::raster::RasterBackend;
    use crate::ui::element::div;
    use crate::ui::event::{dispatch_click, wants_pointer_cursor};
    use crate::ui::style::Styled as _;
    use crate::ui::units::px;
    use crate::ui::layout::{Box_, ScrollState, layout};
    use crate::ui::paint::{Painter, paint};
    use crate::ui::text::Shaper;
    use crate::ui::theme::theme;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The window size `main.rs` opened with, and so the size parity is judged at.
    const WINDOW: (u32, u32) = (1180, 760);

    /// An app with no store and no yt-dlp, which is all a render needs.
    fn fixture() -> (RustyDlp, Painter) {
        let fonts = Rc::new(RefCell::new(cosmic_text::FontSystem::new()));
        let painter = Painter {
            shaper: Shaper::with_shared_fonts(fonts.clone()),
            svg: Default::default(),
            ..Default::default()
        };
        let (updates, rx) = Updates::channel();
        // The receiver is what the event loop drains; leaking it here keeps any
        // queued update from cancelling the sender mid-test.
        std::mem::forget(rx);
        (RustyDlp::for_test(fonts, updates), painter)
    }

    /// Renders the whole window and hands back the surface for pixel checks.
    fn render(app: &mut RustyDlp, painter: &mut Painter) -> RasterBackend {
        let (w, h) = WINDOW;
        let mut backend = RasterBackend::new(w, h);
        backend.begin_frame(w, h, theme().background);
        let tree = app.render();
        let boxes = layout(
            &tree,
            (w as f32, h as f32),
            &mut painter.shaper,
            &ScrollState::default(),
        );
        paint(backend.canvas(), &boxes, painter, None);
        backend
    }

    fn download_job(title: &str, state: JobState) -> Job {
        let mut j = Job::new("https://x/y", "default");
        j.title = title.to_string();
        j.state = state;
        j.items.push(Item {
            id: "i1".into(),
            index: 0,
            title: title.to_string(),
            duration: Some(61.0),
            thumb_path: None,
            webpage_url: "https://x/y".into(),
            files: vec![MFile {
                id: "f1".into(),
                path: "C:/dl/video.mp4".into(),
                kind: FileKind::Video,
                format_id: None,
                bytes: Some(1024 * 1024),
            }],
        });
        j
    }

    /// The shell renders at the window size the app opens with, and the sidebar
    /// and main pane land where the gpui build put them.
    #[test]
    fn the_library_screen_renders_at_the_shipped_window_size() {
        let (mut app, mut painter) = fixture();
        app.jobs = vec![download_job("Some Video", JobState::Done)];
        let mut r = render(&mut app, &mut painter);

        // Inside the 260px sidebar, below the navbar.
        assert_eq!(r.pixel(10, 120).3, 0xff, "sidebar is painted");
        // The main pane, well clear of the sidebar.
        assert_eq!(r.pixel(700, 400).3, 0xff, "main pane is painted");
        assert!(!r.encode_png().is_empty());
    }

    /// The window has no OS title bar (see `shell.rs`'s `with_decorations`),
    /// so its own navbar is the only thing left to drag it by. A regression
    /// here either makes the window immovable, or -- if the id lookup ever
    /// matched too broadly -- would fight every other click handler in the
    /// window for the press.
    #[test]
    fn the_navbar_is_a_drag_region_and_nothing_below_it_is() {
        let (mut app, mut painter) = fixture();
        let tree = app.render();
        let boxes = layout(
            &tree,
            (WINDOW.0 as f32, WINDOW.1 as f32),
            &mut painter.shaper,
            &ScrollState::default(),
        );
        // Navbar is 48px tall and starts flush with the window's top edge.
        assert!(app.is_titlebar_drag_area(&boxes, 400.0, 20.0), "navbar background");
        assert!(!app.is_titlebar_drag_area(&boxes, 400.0, 60.0), "just below the navbar");
        assert!(!app.is_titlebar_drag_area(&boxes, 10.0, 120.0), "the sidebar");
    }

    /// Regression guard for the old full-width banner, which pushed the
    /// navbar and every pane down a row and let a long diagnostic string run
    /// off the window's edge with nothing to cap or wrap it. The toast must
    /// float over the corner instead, at a bounded width, with the rest of
    /// the screen laid out exactly as it would be with no error at all.
    #[test]
    fn a_startup_error_floats_as_a_bounded_corner_toast() {
        let (mut app, mut painter) = fixture();
        app.startup_error = Some(
            "yt-dlp not found. app-data: \"C:\\Users\\matis\\AppData\\Local\\rustyDLP\\bin\\yt-dlp.exe\" \
             (exists false, is_file false) | dir: [<empty>] | portable: None"
                .to_string(),
        );
        let tree = app.render();
        let boxes = layout(
            &tree,
            (WINDOW.0 as f32, WINDOW.1 as f32),
            &mut painter.shaper,
            &ScrollState::default(),
        );

        let find = |id: &str| {
            boxes
                .iter()
                .find(|b| b.node.and_then(|n| n.element_id()).is_some_and(|i| &**i == id))
                .unwrap_or_else(|| panic!("no box with id {id}"))
        };
        assert_eq!(find("navbar").bounds.y, 0.0, "an error must not shove the navbar down");

        let toast = find("startup-toast").bounds;
        assert!(toast.width <= 340.0, "capped width, not the old full-width bar: {}", toast.width);
        let (w, h) = (WINDOW.0 as f32, WINDOW.1 as f32);
        assert!(toast.x + toast.width > w - 340.0, "anchored toward the right edge");
        assert!(toast.y + toast.height > h - 340.0, "anchored toward the bottom edge");
    }

    /// Every screen has to render without panicking, including the empty states,
    /// because an empty list is what a fresh install shows.
    #[test]
    fn every_route_and_tab_renders() {
        for tab in [SidebarTab::Home, SidebarTab::InProgress] {
            for jobs in [vec![], vec![download_job("A", JobState::Running)]] {
                let (mut app, mut painter) = fixture();
                app.tab = tab;
                app.jobs = jobs;
                let mut r = render(&mut app, &mut painter);
                assert!(!r.read_rgba().is_empty(), "{tab:?} rendered nothing");
            }
        }
        let (mut app, mut painter) = fixture();
        app.route = Route::Settings;
        let mut r = render(&mut app, &mut painter);
        assert!(!r.read_rgba().is_empty(), "settings rendered nothing");
    }

    /// Lays this frame out the way `render` paints it, for the tests that need
    /// to look at boxes rather than pixels.
    fn boxes_of<'a>(tree: &'a AnyElement, painter: &mut Painter) -> Vec<Box_<'a, RustyDlp>> {
        layout(
            tree,
            (WINDOW.0 as f32, WINDOW.1 as f32),
            &mut painter.shaper,
            &ScrollState::default(),
        )
    }

    fn find_text<'a, 'b>(boxes: &'b [Box_<'a, RustyDlp>], text: &str) -> Option<&'b Box_<'a, RustyDlp>> {
        boxes.iter().find(|b| b.text == Some(text))
    }

    fn find_id<'a, 'b>(boxes: &'b [Box_<'a, RustyDlp>], id: &str) -> Option<&'b Box_<'a, RustyDlp>> {
        boxes
            .iter()
            .find(|b| b.node.and_then(|n| n.element_id()).is_some_and(|i| &**i == id))
    }

    /// The complaint this motion answers: the screen you left used to vanish
    /// on the first frame, so a swap started from an empty window. Both have
    /// to be on screen, offset from each other, while it plays.
    #[test]
    fn both_screens_are_drawn_while_a_page_swap_plays() {
        let (mut app, mut painter) = fixture();
        app.jobs = vec![download_job("Some Video", JobState::Done)];
        // Settles the main pane on Library first -- the very first render of
        // a screen has nothing to come from and deliberately does not animate.
        render(&mut app, &mut painter);

        app.route = Route::Settings;
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);

        let leaving = find_text(&boxes, "Library").expect("the screen being left is still drawn");
        let arriving = find_text(&boxes, "Settings").expect("the screen arriving is drawn too");
        assert!(
            leaving.inherited.opacity > arriving.inherited.opacity,
            "the fades are staggered: leaving {} vs arriving {}",
            leaving.inherited.opacity,
            arriving.inherited.opacity
        );
        assert!(
            arriving.bounds.x > leaving.bounds.x,
            "and Settings, which sits right of the tabs, slides in from the right"
        );
        assert!(!leaving.inherited.interactive, "the outgoing screen is inert");
        assert!(arriving.inherited.interactive);
    }

    /// Going back reverses the direction of travel, so the motion says which
    /// way you moved rather than playing the same animation both ways.
    #[test]
    fn going_back_from_settings_slides_the_other_way() {
        let (mut app, mut painter) = fixture();
        app.route = Route::Settings;
        render(&mut app, &mut painter);

        app.route = Route::Library;
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);

        let leaving = find_text(&boxes, "Settings").expect("settings still drawn");
        let arriving = find_text(&boxes, "Library").expect("library drawn");
        assert!(
            arriving.bounds.x < leaving.bounds.x,
            "the returning screen comes back in from the left"
        );
    }

    /// A click during the crossfade belongs to the screen arriving, never to
    /// the half-faded one it is replacing.
    #[test]
    fn the_outgoing_screen_takes_no_clicks_during_a_swap() {
        let (mut app, mut painter) = fixture();
        let job = download_job("Some Video", JobState::Done);
        app.jobs = vec![job.clone()];
        render(&mut app, &mut painter);

        app.route = Route::Settings;
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        let tile = find_id(&boxes, &format!("tile-{}", job.items[0].id))
            .expect("the outgoing library's tile is still drawn")
            .bounds;
        let (x, y) = (tile.x + tile.width / 2.0, tile.y + tile.height / 2.0);
        assert!(!wants_pointer_cursor(&boxes, x, y), "no hand cursor over a screen on its way out");

        drop(boxes);
        drop(tree);
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        let mut app2 = app;
        let _ = dispatch_click(&boxes, x, y, &mut app2);
        assert!(app2.selected.is_none(), "the tile underneath must not open");
    }

    /// Settings is the one screen reached from a toolbar button rather than
    /// from the tab bar, so it carries its own way back, top left of its title.
    #[test]
    fn settings_has_a_back_button_left_of_its_title() {
        let (mut app, mut painter) = fixture();
        app.route = Route::Settings;
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);

        let back = find_id(&boxes, "settings-back").expect("no back button").bounds;
        let title = find_text(&boxes, "Settings").expect("no settings title").bounds;
        assert!(back.x < title.x, "to the left of the label: {} vs {}", back.x, title.x);
        assert!(back.y < 160.0, "and at the top of the screen: {}", back.y);

        let (cx, cy) = (back.x + back.width / 2.0, back.y + back.height / 2.0);
        assert!(dispatch_click(&boxes, cx, cy, &mut app).is_some(), "the back button is clickable");
        assert_eq!(app.route, Route::Library, "and it goes back to the library");
    }

    /// The popover is not a panel that appears over the library: it is the
    /// tile you clicked, growing. The first frame of it has to be at the
    /// tile's rectangle, and the last at its resting one.
    #[test]
    fn the_popover_grows_out_of_the_tile_that_opened_it() {
        let (mut app, mut painter) = fixture();
        app.jobs = vec![download_job("Some Video", JobState::Done)];
        render(&mut app, &mut painter);

        let tile = {
            let tree = app.render();
            let boxes = boxes_of(&tree, &mut painter);
            find_id(&boxes, "tile-i1").expect("no tile").bounds
        };
        click_id(&mut app, &mut painter, "tile-i1");

        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        let opening = find_id(&boxes, "popover-card").expect("no popover card").bounds;
        assert!(
            (opening.x - tile.x).abs() < 60.0 && (opening.y - tile.y).abs() < 60.0,
            "starts on the tile, not centred: {opening:?} vs {tile:?}"
        );
        assert!(
            opening.width < tile.width * 2.0,
            "and at roughly the tile's size: {}",
            opening.width
        );
        drop(boxes);
        drop(tree);

        std::thread::sleep(crate::ui::anim::DURATION + std::time::Duration::from_millis(20));
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        let settled = find_id(&boxes, "popover-card").expect("no popover card").bounds;
        let (vw, vh) = (WINDOW.0 as f32, WINDOW.1 as f32);
        assert!((settled.width - vw * 0.72).abs() < 1.0, "settles at 72% wide: {settled:?}");
        assert!((settled.height - vh * 0.8).abs() < 1.0, "and 80% tall: {settled:?}");
        assert!(
            (settled.x - (vw - settled.width) / 2.0).abs() < 1.0,
            "centred once it gets there: {settled:?}"
        );
    }

    /// And it goes back the same way. The collapsing card carries no content
    /// (see `popover_collapse`) and must not take clicks: the one that closed
    /// it has already been answered.
    #[test]
    fn closing_collapses_the_card_back_toward_the_tile() {
        let (mut app, mut painter) = fixture();
        app.jobs = vec![download_job("Some Video", JobState::Done)];
        render(&mut app, &mut painter);
        let tile = {
            let tree = app.render();
            let boxes = boxes_of(&tree, &mut painter);
            find_id(&boxes, "tile-i1").expect("no tile").bounds
        };
        click_id(&mut app, &mut painter, "tile-i1");
        render(&mut app, &mut painter);
        std::thread::sleep(crate::ui::anim::DURATION + std::time::Duration::from_millis(20));
        render(&mut app, &mut painter);

        app.close_popover();
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        let ghost = find_id(&boxes, "popover-collapse").expect("nothing collapsing");
        assert!(
            ghost.bounds.width > tile.width * 2.0,
            "starts at the open card's size: {:?}",
            ghost.bounds
        );
        assert!(!ghost.inherited.interactive, "and answers no clicks on the way out");
        assert!(
            find_id(&boxes, "popover-card").is_none(),
            "the real popover is gone -- this is only its shape"
        );

        drop(boxes);
        drop(tree);
        std::thread::sleep(crate::ui::anim::DURATION + std::time::Duration::from_millis(20));
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        assert!(find_id(&boxes, "popover-collapse").is_none(), "and is gone once it lands");
    }

    /// Opening a file means resolving ffmpeg, probing it and waiting for a
    /// first decoded frame — seconds, on a cold cache — and every one of them
    /// used to look like a poster that had simply decided not to play.
    #[test]
    fn the_stage_shows_a_loading_bar_while_a_file_is_opening() {
        let dir = std::env::temp_dir().join("rustydlp-stage-loading");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let video = dir.join("clip.mp4");
        std::fs::write(&video, b"not a real video, only a path that exists").expect("write");

        let (mut app, mut painter) = fixture();
        let mut job = download_job("Some Video", JobState::Done);
        job.items[0].files[0].path = video.to_string_lossy().into_owned();
        app.selected = Some(job.id.clone());
        app.jobs = vec![job];

        // The render is what calls `ensure_player`, which starts the load and
        // so sets the pending state the bar keys off, before `player_view`
        // builds the stage in that same pass.
        let tree = app.render();
        let boxes = boxes_of(&tree, &mut painter);
        assert!(app.player_pending.is_some(), "a load should be in flight");
        assert!(
            find_text(&boxes, "Loading…").is_some(),
            "the stage says nothing about the wait"
        );
        assert!(app.is_animating(), "and the indeterminate bar keeps redrawing");

        let _ = std::fs::remove_file(&video);
    }

    /// A selected job opens the detail pane, which is the densest screen: title,
    /// metadata, the action row and the poster.
    #[test]
    fn the_detail_pane_renders_for_a_selected_job() {
        let (mut app, mut painter) = fixture();
        let job = download_job("Some Video", JobState::Done);
        app.selected = Some(job.id.clone());
        app.jobs = vec![job];
        let mut r = render(&mut app, &mut painter);
        assert_eq!(r.pixel(700, 300).3, 0xff);
    }

    /// Both overlays cover the window, and the one on top is the one that draws.
    #[test]
    fn the_modals_render_over_the_window() {
        let (mut app, mut painter) = fixture();
        app.modal = true;
        let mut r = render(&mut app, &mut painter);
        // The backdrop dims the whole window, including over the sidebar.
        assert_eq!(r.pixel(10, 400).3, 0xff);

        let (mut app, mut painter) = fixture();
        app.convert_picker = Some("C:/dl/a.mkv".into());
        let mut r = render(&mut app, &mut painter);
        assert_eq!(r.pixel(590, 380).3, 0xff);
    }

    // -- transition contact sheets -----------------------------------------

    /// Frame size in the sheet, as a fraction of the window.
    const SHEET_SCALE: f32 = 0.5;
    /// Where a transition is sampled, as a fraction of `anim::DURATION`.
    const SHEET_STEPS: [f32; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];

    /// Renders `app` at each of `SHEET_STEPS` along a transition that starts
    /// now, sleeping between frames so the animator -- which reads the clock,
    /// having no notion of a frame number -- is sampled at those points.
    fn transition_frames(
        app: &mut RustyDlp,
        painter: &mut Painter,
        over: std::time::Duration,
    ) -> Vec<(f32, Vec<u8>)> {
        let start = std::time::Instant::now();
        let mut out = Vec::new();
        for step in SHEET_STEPS {
            let due = start + over.mul_f32(step);
            if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
            out.push((step, render(app, painter).read_rgba()));
        }
        out
    }

    /// Lays the frames out in a grid, captioned with where in the transition
    /// each one sits. Captions go through the app's own text pipeline, so the
    /// sheet needs no font handling of its own.
    fn contact_sheet(
        frames: &[(f32, Vec<u8>)],
        painter: &mut Painter,
        over: std::time::Duration,
    ) -> RasterBackend {
        use crate::render::Backend;
        use crate::ui::rgb;

        const PAD: f32 = 16.0;
        const CAPTION: f32 = 18.0;
        const COLS: usize = 3;

        let (fw, fh) = (WINDOW.0 as f32 * SHEET_SCALE, WINDOW.1 as f32 * SHEET_SCALE);
        let rows = frames.len().div_ceil(COLS);
        let width = PAD + COLS as f32 * (fw + PAD);
        let height = PAD + rows as f32 * (CAPTION + fh + PAD);

        let mut sheet = RasterBackend::new(width as u32, height as u32);
        sheet.begin_frame(width as u32, height as u32, rgb(0x0a0a0a));

        for (i, (step, rgba)) in frames.iter().enumerate() {
            let x = PAD + (i % COLS) as f32 * (fw + PAD);
            let y = PAD + (i / COLS) as f32 * (CAPTION + fh + PAD);

            let caption: AnyElement = div()
                .w(px(fw))
                .text_xs()
                .text_color(theme().muted_foreground)
                .child(format!("{:.0}ms", step * over.as_millis() as f32))
                .into_any_element();
            let boxes = layout(
                &caption,
                (fw, CAPTION),
                &mut painter.shaper,
                &ScrollState::default(),
            );
            let canvas = sheet.canvas();
            let restore = canvas.save();
            canvas.translate((x, y));
            paint(canvas, &boxes, painter, None);
            canvas.restore_to_count(restore);

            let info = skia_safe::ImageInfo::new(
                (WINDOW.0 as i32, WINDOW.1 as i32),
                skia_safe::ColorType::RGBA8888,
                skia_safe::AlphaType::Unpremul,
                None,
            );
            let image = skia_safe::images::raster_from_data(
                &info,
                skia_safe::Data::new_copy(rgba),
                WINDOW.0 as usize * 4,
            )
            .expect("frame image");
            let dst = skia_safe::Rect::from_xywh(x, y + CAPTION, fw, fh);
            let mut opts = skia_safe::Paint::default();
            opts.set_anti_alias(true);
            sheet.canvas().draw_image_rect(&image, None, dst, &opts);
        }
        sheet
    }

    /// What a transition case does to get the app into its before/after
    /// state. Given the painter too, so a case can route a real click through
    /// a real laid-out frame rather than reaching past it into the fields the
    /// handler would have set.
    type Navigate = Box<dyn Fn(&mut RustyDlp, &mut Painter)>;

    /// Clicks the centre of the box with this id, the way `shell.rs` would:
    /// handler first, then the clicked rectangle handed back to the app, which
    /// is what the popover morphs out of.
    fn click_id(app: &mut RustyDlp, painter: &mut Painter, id: &str) {
        let tree = app.render();
        let boxes = boxes_of(&tree, painter);
        let rect = find_id(&boxes, id).unwrap_or_else(|| panic!("no box with id {id}")).bounds;
        let (x, y) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        let clicked = dispatch_click(&boxes, x, y, app).expect("nothing handled the click");
        app.set_click_origin(clicked);
    }

    /// A recognisable stand-in for a real thumbnail, written next to the
    /// sheets. The morph's whole claim is that the same picture is on screen
    /// at both ends of it, and a fixture whose items have no art on disk
    /// cannot show that either way.
    fn fixture_thumb(out: &std::path::Path) -> String {
        use crate::render::Backend;
        use crate::ui::rgb;

        let (w, h) = (480u32, 270u32);
        let mut r = RasterBackend::new(w, h);
        r.begin_frame(w, h, rgb(0x1d3557));
        let canvas = r.canvas();
        let mut paint = skia_safe::Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(skia_safe::Color::from_rgb(0xf1, 0xa2, 0x08));
        canvas.draw_circle((360.0, 80.0), 42.0, &paint);
        paint.set_color(skia_safe::Color::from_rgb(0x2a, 0x9d, 0x8f));
        canvas.draw_rect(skia_safe::Rect::from_xywh(0.0, 170.0, 480.0, 100.0), &paint);
        paint.set_color(skia_safe::Color::from_rgb(0x26, 0x46, 0x53));
        let mut hill = skia_safe::Path::new();
        hill.move_to((0.0, 200.0));
        hill.line_to((150.0, 110.0));
        hill.line_to((300.0, 200.0));
        hill.close();
        canvas.draw_path(&hill, &paint);
        let path = out.join("fixture-thumb.png");
        std::fs::write(&path, r.encode_png()).expect("write thumbnail");
        path.to_string_lossy().into_owned()
    }

    /// Writes one PNG contact sheet per transition, so the motion can be
    /// looked at rather than only asserted about. Ignored by default: it
    /// sleeps through five transitions in real time and writes files.
    ///
    /// Run with: cargo test --lib -- --ignored transition_contact_sheets
    #[test]
    #[ignore = "renders in real time and writes PNGs to target/transitions"]
    fn transition_contact_sheets() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/transitions");
        std::fs::create_dir_all(&out).expect("output dir");

        let thumb = fixture_thumb(&out);
        let mut job = download_job("Big Buck Bunny (1080p)", JobState::Done);
        job.items[0].thumb_path = Some(thumb);
        let running = {
            let mut j = download_job("Sintel trailer", JobState::Running);
            j.id = "running-1".into();
            j.items[0].id = "running-item".into();
            j
        };

        // Each case renders once to settle the screen it starts on -- the very
        // first render of a screen has nothing to come from -- then mutates
        // exactly what the click would have, and samples the result.
        let noop: fn() -> Navigate = || Box::new(|_: &mut RustyDlp, _: &mut Painter| {});
        let open_popover: fn() -> Navigate =
            || Box::new(|a: &mut RustyDlp, p: &mut Painter| click_id(a, p, "tile-i1"));
        let swap = crate::ui::anim::DURATION;

        // (name, what the sheet spans, how the screen it starts on is reached,
        // what starts the animation). The third runs before the settling
        // render below, the fourth right after it.
        let cases: Vec<(&str, std::time::Duration, Navigate, Navigate)> = vec![
            (
                "library-to-settings",
                swap,
                noop(),
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.route = Route::Settings),
            ),
            (
                "settings-to-library",
                swap,
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.route = Route::Settings),
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.route = Route::Library),
            ),
            (
                "home-to-in-progress",
                swap,
                noop(),
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.tab = SidebarTab::InProgress),
            ),
            ("tile-to-popover", swap, noop(), open_popover()),
            (
                "popover-to-tile",
                swap,
                open_popover(),
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.close_popover()),
            ),
            (
                "new-download-modal",
                swap,
                noop(),
                Box::new(|a: &mut RustyDlp, _: &mut Painter| a.modal = true),
            ),
            // Not a transition: a loop, sampled across one full sweep. The
            // item's file has to exist for the player to try opening it at
            // all, so the case points it at a stand-in written next to the
            // sheets -- nothing decodes it here, the load never completes,
            // which is exactly the state being photographed.
            (
                "player-loading",
                LOADING_SWEEP,
                open_popover(),
                noop(),
            ),
        ];

        for (name, over, before, act) in cases {
            let (mut app, mut painter) = fixture();
            let mut first = job.clone();
            first.id = "j-1".into();
            if name == "player-loading" {
                let stand_in = out.join("fixture-clip.mp4");
                std::fs::write(&stand_in, b"a path that exists, nothing more").expect("write");
                first.items[0].files[0].path = stand_in.to_string_lossy().into_owned();
            }
            app.jobs = vec![first, running.clone()];
            before(&mut app, &mut painter);
            // Twice, a full duration apart: whatever `before` started has to
            // be settled before the transition under test begins, or it plays
            // over the top of it.
            render(&mut app, &mut painter);
            std::thread::sleep(crate::ui::anim::DURATION);
            render(&mut app, &mut painter);
            act(&mut app, &mut painter);

            let frames = transition_frames(&mut app, &mut painter, over);
            let png = contact_sheet(&frames, &mut painter, over).encode_png();
            let path = out.join(format!("{name}.png"));
            std::fs::write(&path, png).expect("write sheet");
            println!("wrote {}", path.display());
        }
    }

    /// This is a yt-dlp client, so titles are not ASCII. Non-Latin and emoji have
    /// to shape and truncate rather than render tofu or panic -- the case that
    /// drove the cosmic-text choice.
    #[test]
    fn non_latin_and_emoji_titles_render() {
        for title in [
            "日本語のタイトルです",
            "Видео на русском",
            "العربية",
            "emoji 🎬🔥 in a title",
            "a very long title that will certainly have to be truncated in the sidebar column",
        ] {
            let (mut app, mut painter) = fixture();
            app.jobs = vec![download_job(title, JobState::Done)];
            let mut r = render(&mut app, &mut painter);
            assert!(!r.read_rgba().is_empty(), "{title} rendered nothing");
        }
    }

    fn job_in(state: JobState) -> Job {
        let mut j = Job::new("https://x/y", "default");
        j.state = state;
        j
    }

    fn convert_job_in(state: JobState) -> Job {
        let mut j = Job::new_convert("C:/dl/a.mkv", "mp4");
        j.state = state;
        j
    }

    #[test]
    fn home_lists_finished_jobs_of_either_kind() {
        let jobs = vec![
            job_in(JobState::Done),
            job_in(JobState::Running),
            convert_job_in(JobState::Done),
            convert_job_in(JobState::Running),
            job_in(JobState::Failed),
        ];

        let home = filter_jobs(&jobs, SidebarTab::Home);
        assert_eq!(home.len(), 3, "both kinds, but only what's no longer active");
        assert!(home.iter().any(|j| j.kind == JobKind::Download));
        assert!(home.iter().any(|j| j.kind == JobKind::Convert));
        assert!(!home.iter().any(|j| is_active(j.state)));
    }

    #[test]
    fn in_progress_mixes_kinds_but_only_active_ones() {
        let jobs = vec![
            job_in(JobState::Running),
            job_in(JobState::Done),
            job_in(JobState::Queued),
            job_in(JobState::Failed),
            job_in(JobState::Probing),
            job_in(JobState::Cancelled),
            convert_job_in(JobState::Running),
            convert_job_in(JobState::Done),
        ];

        let active = filter_jobs(&jobs, SidebarTab::InProgress);
        assert_eq!(active.len(), 4, "queued/probing/running of either kind are in progress");
        for j in active {
            assert!(
                !matches!(
                    j.state,
                    JobState::Done | JobState::Failed | JobState::Cancelled
                ),
                "a terminal job leaked into In progress"
            );
        }
    }

    #[test]
    fn tab_index_round_trips_and_defaults_to_home() {
        assert_eq!(SidebarTab::from_index(0), SidebarTab::Home);
        assert_eq!(SidebarTab::from_index(1), SidebarTab::InProgress);
        // TabBar could in principle hand back an out-of-range index.
        assert_eq!(SidebarTab::from_index(99), SidebarTab::Home);
        assert_eq!(SidebarTab::from_index(SidebarTab::InProgress.index()), SidebarTab::InProgress);
    }

    /// Cancelling kills the child, which makes yt-dlp exit non-zero, which the
    /// event pump delivers as a failure. Without this rule every cancelled job
    /// would immediately relabel itself "Failed" with a spurious error.
    #[test]
    fn cancelling_survives_the_failure_its_own_kill_causes() {
        assert_eq!(
            state_after_failure(JobState::Cancelled),
            JobState::Cancelled
        );
        assert_eq!(state_after_failure(JobState::Running), JobState::Failed);
        assert_eq!(state_after_failure(JobState::Queued), JobState::Failed);
    }

    #[test]
    fn active_and_retryable_partition_the_states() {
        for s in [JobState::Queued, JobState::Probing, JobState::Running] {
            assert!(is_active(s), "{s:?} should be active");
            assert!(!is_retryable(s), "{s:?} is still working, not retryable");
        }
        for s in [JobState::Failed, JobState::Cancelled] {
            assert!(!is_active(s), "{s:?} should not be active");
            assert!(is_retryable(s), "{s:?} should be retryable");
        }
        // Done is neither: nothing to cancel, nothing to redo.
        assert!(!is_active(JobState::Done));
        assert!(!is_retryable(JobState::Done));
    }

    /// The sidebar filter and the Cancel button must agree on "in progress",
    /// which is why both go through is_active.
    #[test]
    fn sidebar_filter_and_cancel_button_agree() {
        let jobs: Vec<Job> = [
            JobState::Queued,
            JobState::Probing,
            JobState::Running,
            JobState::Done,
            JobState::Failed,
            JobState::Cancelled,
        ]
        .into_iter()
        .map(job_in)
        .collect();

        for job in filter_jobs(&jobs, SidebarTab::InProgress) {
            assert!(
                is_active(job.state),
                "{:?} listed as in progress but has no Cancel button",
                job.state
            );
        }
    }

    #[test]
    fn empty_message_differs_per_tab() {
        assert_eq!(SidebarTab::Home.empty_message(), "No downloads or conversions yet");
        assert_eq!(SidebarTab::InProgress.empty_message(), "Nothing in progress");
    }

    /// Regression guard for "Show in folder" landing on a default Explorer
    /// window (Documents, in practice) instead of the actual file: the quote
    /// has to wrap only the path, not the `/select,` flag ahead of it, or
    /// Explorer's parser never recognises the flag at all. A downloaded
    /// file's name almost always has a space in it -- yt-dlp's output
    /// template embeds the title -- which is exactly the case that used to
    /// trip this.
    #[test]
    fn reveal_path_quotes_only_the_path_not_the_select_flag() {
        let arg = select_arg("C:/Users/matis/Videos/Some Title [abc123].mp4");
        assert_eq!(arg, "/select,\"C:\\Users\\matis\\Videos\\Some Title [abc123].mp4\"");
        assert!(arg.starts_with("/select,\""), "the flag itself must stay unquoted: {arg}");
    }
}
