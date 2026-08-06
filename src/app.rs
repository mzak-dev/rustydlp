use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, IconName,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    switch::Switch,
    tab::{Tab, TabBar},
    *,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::model::{File as MFile, FileKind, Item, Job, JobKind, JobState, Preset, new_id};
use crate::player;
use crate::runner::{self, ConvertEvent, ConvertFormat, Probe};
use crate::store::Store;
use crate::ytdlp::{Event, FormatMode, YtdlpOptions};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Route {
    Library,
    Settings,
}

/// Which top-navbar mode is active, and therefore which jobs the sidebar
/// list shows.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SidebarTab {
    /// Download jobs, newest first.
    Download,
    /// Convert jobs, newest first.
    Convert,
    /// Jobs of either kind that are still working. Shown in the main pane,
    /// not the sidebar — see `main_pane`.
    InProgress,
}

impl SidebarTab {
    fn index(self) -> usize {
        match self {
            Self::Download => 0,
            Self::Convert => 1,
            Self::InProgress => 2,
        }
    }

    fn from_index(ix: usize) -> Self {
        match ix {
            1 => Self::Convert,
            2 => Self::InProgress,
            _ => Self::Download,
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Download => "No recent downloads",
            Self::Convert => "No recent conversions",
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
    url_input: Entity<InputState>,
    /// Extra yt-dlp args applied to this download only, never saved back to the
    /// preset. Keeps the merge two layers deep: preset, then this run.
    override_input: Entity<InputState>,
    probe: ProbeState,
    /// Bumped on every keystroke; a debounced probe only fires if it still
    /// matches when its timer expires.
    probe_gen: u64,
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
    /// Manually toggled. Also forced true whenever `tab == InProgress`, since
    /// that list moves into the main pane and the sidebar has nothing to show.
    sidebar_collapsed: bool,
    player: Option<PlayerState>,
    /// Path currently being probed/spawned, if a load is in flight — set the
    /// instant `ensure_player` decides a (re)load is needed, cleared once
    /// `player` is populated (or the load fails). Prevents re-triggering a
    /// second load on every render while the first is still starting up.
    player_pending: Option<String>,
    /// Bumped on every load/seek so a stray event from a just-replaced
    /// playback run can't clobber the state of the run that replaced it.
    player_gen: u64,
    /// Source file path awaiting a target-format choice, if the format
    /// picker is open. Set by both the per-item Convert button and the
    /// Convert tab's "Convert File" picker.
    convert_picker: Option<String>,
    _subs: Vec<Subscription>,
}

/// Live native-playback state for whichever item is currently open in
/// `detail()`. Torn down (killing the ffmpeg children) whenever the open
/// item changes or the player is closed.
struct PlayerState {
    control: player::PlayerControl,
    path: String,
    frame: Option<Arc<gpui::RenderImage>>,
    position_secs: f64,
    playing: bool,
}

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
    name: Entity<InputState>,
    max_height: Entity<InputState>,
    container: Entity<InputState>,
    output: Entity<InputState>,
    dir: Entity<InputState>,
    subs: Entity<InputState>,
    extra: Entity<InputState>,
    codec: Entity<InputState>,
    custom_fmt: Entity<InputState>,
}

impl PresetForm {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        let mk = |window: &mut Window, cx: &mut App, ph: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        Self {
            name: mk(window, cx, "Preset name"),
            max_height: mk(window, cx, "1080 (blank = no cap)"),
            container: mk(window, cx, "mp4 / mkv (blank = leave alone)"),
            output: mk(window, cx, "%(title)s [%(id)s].%(ext)s"),
            dir: mk(window, cx, "Download folder"),
            subs: mk(window, cx, "en,pl (blank = no subtitles)"),
            extra: mk(window, cx, "--cookies-from-browser firefox"),
            codec: mk(window, cx, "mp3 / m4a / opus"),
            custom_fmt: mk(window, cx, "bv*+ba/b"),
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
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Paste a video or playlist URL")
        });
        let override_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Extra args for this download only")
        });

        let _subs = vec![cx.subscribe_in(&url_input, window, {
            let input = url_input.clone();
            move |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.schedule_probe(value, window, cx);
                }
            }
        })];

        let mut this = Self {
            ytdlp: None,
            store: None,
            jobs: Vec::new(),
            live: HashMap::new(),
            selected: None,
            open_item: None,
            route: Route::Library,
            tab: SidebarTab::Download,
            modal: false,
            url_input,
            override_input,
            probe: ProbeState::Idle,
            probe_gen: 0,
            startup_error: None,
            pending: VecDeque::new(),
            launcher_active: false,
            cancels: HashMap::new(),
            update_status: None,
            presets: Vec::new(),
            chosen_preset: None,
            editing: None,
            form: PresetForm::new(window, cx),
            sidebar_collapsed: false,
            player: None,
            player_pending: None,
            player_gen: 0,
            convert_picker: None,
            _subs,
        };
        this.refresh_ytdlp();
        this.open_store(cx);
        this
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

    fn open_store(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let dir = db_path();
            if let Err(e) = std::fs::create_dir_all(&dir) {
                let msg = format!("cannot create {}: {e}", dir.display());
                let _ = this.update(cx, |this, cx| {
                    this.startup_error = Some(msg);
                    cx.notify();
                });
                return;
            }
            let path = dir.join("library.db");
            match Store::open(&path.to_string_lossy()).await {
                Ok(store) => {
                    let jobs = store.load_jobs().await.unwrap_or_default();

                    // Seed on first run so a fresh install has usable presets
                    // without anyone opening Settings.
                    let mut presets = store.load_presets().await.unwrap_or_default();
                    if presets.is_empty() {
                        for p in Preset::seeds(&dirs_download().to_string_lossy()) {
                            let _ = store.save_preset(&p).await;
                        }
                        presets = store.load_presets().await.unwrap_or_default();
                    }

                    let _ = this.update(cx, |this, cx| {
                        this.store = Some(Arc::new(store));
                        this.jobs = jobs;
                        this.chosen_preset = presets
                            .iter()
                            .find(|p| p.is_default)
                            .or_else(|| presets.first())
                            .map(|p| p.name.clone());
                        this.presets = presets;
                        cx.notify();
                    });
                }
                Err(e) => {
                    let msg = format!("could not open library: {e}");
                    let _ = this.update(cx, |this, cx| {
                        this.startup_error = Some(msg);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    // -- probe -------------------------------------------------------------

    /// Debounced so a probe does not fire on every keystroke while the user is
    /// still typing or pasting.
    fn schedule_probe(&mut self, url: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.probe_gen += 1;
        let generation = self.probe_gen;

        if !url.trim_start().starts_with("http") {
            self.probe = ProbeState::Idle;
            cx.notify();
            return;
        }
        let Some(exe) = self.ytdlp.clone() else {
            return;
        };

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            // Superseded by a newer keystroke — drop this one.
            let still_current = this
                .read_with(cx, |this, _| this.probe_gen == generation)
                .unwrap_or(false);
            if !still_current {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.probe = ProbeState::Running;
                cx.notify();
            });

            let result = runner::probe(exe, url.trim().to_string()).await;
            let _ = this.update(cx, |this, cx| {
                if this.probe_gen != generation {
                    return;
                }
                this.probe = match result {
                    Ok(Ok(p)) => ProbeState::Ok(Box::new(p)),
                    Ok(Err(e)) => ProbeState::Err(e.to_string()),
                    Err(_) => ProbeState::Err("probe cancelled".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    // -- download ----------------------------------------------------------

    fn start_download(&mut self, cx: &mut Context<Self>) {
        let url = self.url_input.read(cx).value().to_string();
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
        self.persist(job, cx);

        let mut opts = preset.map(|p| p.options).unwrap_or_else(default_options);
        if opts.download_dir.trim().is_empty() {
            opts.download_dir = dirs_download().to_string_lossy().to_string();
        }
        // This-run override is appended after the preset's own extra args, so
        // later flags win — same precedence yt-dlp gives a repeated option.
        opts.extra_args.extend(
            self.override_input
                .read(cx)
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
        self.ensure_launcher(cx);
        cx.notify();
    }

    /// Drains `pending`, spacing launches by [`LAUNCH_STAGGER`]. There is no cap
    /// on how many downloads end up running at once — only on how fast they
    /// start. A failed spawn is requeued with growing backoff, so a transient
    /// resource limit delays a job rather than killing it.
    fn ensure_launcher(&mut self, cx: &mut Context<Self>) {
        if self.launcher_active {
            return;
        }
        self.launcher_active = true;

        cx.spawn(async move |this, cx| {
            loop {
                let next = this
                    .update(cx, |this, _| this.pending.pop_front())
                    .ok()
                    .flatten();
                let Some(mut pending) = next else { break };

                let launched = this
                    .update(cx, |this, cx| this.launch(&pending, cx))
                    .unwrap_or(Ok(()));

                match launched {
                    Ok(()) => {
                        cx.background_executor().timer(LAUNCH_STAGGER).await;
                    }
                    Err(_) if pending.attempts + 1 < MAX_LAUNCH_ATTEMPTS => {
                        pending.attempts += 1;
                        let wait = LAUNCH_BACKOFF * pending.attempts;
                        // Back of the queue so one sick job cannot starve the rest.
                        let _ = this.update(cx, |this, cx| {
                            this.pending.push_back(pending);
                            cx.notify();
                        });
                        cx.background_executor().timer(wait).await;
                    }
                    Err(e) => {
                        let msg = format!("could not start yt-dlp after retries: {e}");
                        let id = pending.job_id.clone();
                        let _ = this.update(cx, |this, cx| this.fail_job(&id, msg, cx));
                    }
                }
            }

            let _ = this.update(cx, |this, _| this.launcher_active = false);
        })
        .detach();
    }

    /// Spawns one job and starts pumping its events. Errors here are retryable.
    fn launch(&mut self, pending: &PendingLaunch, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let exe = runner::ytdlp_path(None)?;
        let download = runner::spawn_download(&exe, &pending.url, &pending.opts)?;

        let job_id = pending.job_id.clone();
        self.cancels
            .insert(job_id.clone(), download.cancel_handle());
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Running;
            let snapshot = job.clone();
            self.persist(snapshot, cx);
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut events = download.events;
            while let Some(event) = events.next().await {
                let ended = this
                    .update(cx, |this, cx| this.apply_event(&job_id, event, cx))
                    .unwrap_or(true);
                if ended {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| this.finish_job(&job_id, cx));
        })
        .detach();

        Ok(())
    }

    /// Folds one yt-dlp event into job state. Returns true when the run ended.
    fn apply_event(&mut self, job_id: &str, event: Event, cx: &mut Context<Self>) -> bool {
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
                if line == crate::runner::EXIT_OK {
                    ended = true;
                } else if let Some(code) = line.strip_prefix(crate::runner::EXIT_FAIL_PREFIX) {
                    let tail = self
                        .live
                        .get(job_id)
                        .map(|l| l.log.iter().rev().take(3).cloned().collect::<Vec<_>>().join(" | "))
                        .unwrap_or_default();
                    self.fail_job(job_id, format!("yt-dlp exited {code}: {tail}"), cx);
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
        cx.notify();
        ended
    }

    fn fail_job(&mut self, job_id: &str, error: String, cx: &mut Context<Self>) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            // Killing the child makes yt-dlp exit non-zero, which would
            // otherwise be reported as a failure. A deliberate cancel wins.
            job.state = state_after_failure(job.state);
            if job.state == JobState::Failed {
                job.error = Some(error);
            }
            let snapshot = job.clone();
            self.persist(snapshot, cx);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
        cx.notify();
    }

    fn finish_job(&mut self, job_id: &str, cx: &mut Context<Self>) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            if job.state == JobState::Running {
                job.state = JobState::Done;
            }
            let snapshot = job.clone();
            self.persist(snapshot, cx);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
        cx.notify();
    }

    /// Marks the job cancelled first, then kills the process — so the non-zero
    /// exit that follows is recognised as intentional.
    fn cancel_job(&mut self, job_id: &str, cx: &mut Context<Self>) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Cancelled;
            job.error = None;
            let snapshot = job.clone();
            self.persist(snapshot, cx);
        }
        // Drop it from the launch queue too, in case it never started.
        self.pending.retain(|p| p.job_id != job_id);
        if let Some(handle) = self.cancels.remove(job_id) {
            handle.cancel();
        }
        self.live.remove(job_id);
        cx.notify();
    }

    /// Removes a job from the list and library. Only for jobs that aren't
    /// actively downloading — cancel first.
    fn delete_job(&mut self, job_id: &str, cx: &mut Context<Self>) {
        self.jobs.retain(|j| j.id != job_id);
        self.live.remove(job_id);
        self.cancels.remove(job_id);
        if self.selected.as_deref() == Some(job_id) {
            self.selected = None;
        }
        if let Some(store) = self.store.clone() {
            let job_id = job_id.to_string();
            cx.background_spawn(async move {
                if let Err(e) = store.delete_job(&job_id).await {
                    eprintln!("rustydlp: failed to delete job {job_id}: {e}");
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Re-queues a failed or cancelled job. Partial `.part` files survive a
    /// kill, so yt-dlp resumes rather than starting the transfer again.
    fn retry_job(&mut self, job_id: &str, cx: &mut Context<Self>) {
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
            self.persist(snapshot, cx);
        }
        self.live.insert(job_id.to_string(), Live::default());
        self.pending.push_back(PendingLaunch {
            job_id: job_id.to_string(),
            url,
            opts,
            attempts: 0,
        });
        self.ensure_launcher(cx);
        cx.notify();
    }

    fn update_ytdlp(&mut self, cx: &mut Context<Self>) {
        let Some(exe) = self.ytdlp.clone() else {
            return;
        };
        self.update_status = Some("Updating…".into());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = runner::update_ytdlp(exe).await;
            let msg = match result {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => format!("Update failed: {e}"),
                Err(_) => "Update task cancelled".into(),
            };
            let _ = this.update(cx, |this, cx| {
                this.update_status = Some(msg);
                this.refresh_ytdlp();
                cx.notify();
            });
        })
        .detach();
    }

    /// Writes a job snapshot off the UI thread. Called only on transitions.
    fn persist(&self, job: Job, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.background_spawn(async move {
            if let Err(e) = store.save_job(&job).await {
                eprintln!("rustydlp: failed to save job {}: {e}", job.id);
            }
        })
        .detach();
    }

    // -- presets -----------------------------------------------------------

    /// Copies a preset into the editor's text fields.
    fn edit_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        let o = &preset.options;
        let set = |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
            e.update(cx, |s, cx| s.set_value(v, window, cx));
        };
        set(&self.form.name, preset.name.clone(), window, cx);
        set(
            &self.form.max_height,
            o.max_height.map(|h| h.to_string()).unwrap_or_default(),
            window,
            cx,
        );
        set(
            &self.form.container,
            o.container.clone().unwrap_or_default(),
            window,
            cx,
        );
        set(&self.form.output, o.output_template.clone(), window, cx);
        set(&self.form.dir, o.download_dir.clone(), window, cx);
        set(&self.form.subs, o.subtitle_langs.join(","), window, cx);
        set(&self.form.extra, o.extra_args.join(" "), window, cx);
        set(
            &self.form.codec,
            match &o.format {
                FormatMode::AudioOnly { codec } => codec.clone(),
                _ => String::new(),
            },
            window,
            cx,
        );
        set(
            &self.form.custom_fmt,
            match &o.format {
                FormatMode::Custom(s) => s.clone(),
                _ => String::new(),
            },
            window,
            cx,
        );
        self.editing = Some(preset);
        cx.notify();
    }

    /// Reads the text fields back onto the preset being edited and saves it.
    fn save_editing(&mut self, cx: &mut Context<Self>) {
        let Some(mut preset) = self.editing.clone() else {
            return;
        };
        let read = |e: &Entity<InputState>, cx: &Context<Self>| e.read(cx).value().trim().to_string();

        let name = read(&self.form.name, cx);
        if name.is_empty() {
            return;
        }
        let previous_name = preset.name.clone();
        preset.name = name;

        let o = &mut preset.options;
        // A non-numeric height is treated as "no cap" rather than rejected —
        // the field is advisory and a modal error here would be worse.
        o.max_height = read(&self.form.max_height, cx).parse::<u32>().ok();
        o.container = Some(read(&self.form.container, cx)).filter(|s| !s.is_empty());
        let output = read(&self.form.output, cx);
        o.output_template = if output.is_empty() {
            YtdlpOptions::default().output_template
        } else {
            output
        };
        o.download_dir = read(&self.form.dir, cx);
        o.subtitle_langs = read(&self.form.subs, cx)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Split on whitespace: this is a CLI fragment, so quoting is the
        // user's problem, same as typing it into a shell.
        o.extra_args = read(&self.form.extra, cx)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        o.format = match &o.format {
            FormatMode::AudioOnly { .. } => FormatMode::AudioOnly {
                codec: {
                    let c = read(&self.form.codec, cx);
                    if c.is_empty() { "mp3".into() } else { c }
                },
            },
            FormatMode::Custom(_) => FormatMode::Custom({
                let c = read(&self.form.custom_fmt, cx);
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
            cx.background_spawn(async move {
                if renamed {
                    let _ = store.delete_preset(&previous_name).await;
                }
                if let Err(e) = store.save_preset(&preset).await {
                    eprintln!("rustydlp: failed to save preset: {e}");
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn delete_editing(&mut self, cx: &mut Context<Self>) {
        let Some(preset) = self.editing.take() else {
            return;
        };
        self.presets.retain(|p| p.name != preset.name);
        if self.chosen_preset.as_deref() == Some(preset.name.as_str()) {
            self.chosen_preset = self.presets.first().map(|p| p.name.clone());
        }
        if let Some(store) = self.store.clone() {
            let name = preset.name.clone();
            cx.background_spawn(async move {
                let _ = store.delete_preset(&name).await;
            })
            .detach();
        }
        cx.notify();
    }

    // -- rendering ---------------------------------------------------------

    /// Jobs the current tab should list.
    fn visible_jobs(&self) -> Vec<&Job> {
        filter_jobs(&self.jobs, self.tab)
    }

    /// Whether the sidebar shows a job list right now, or just the icon
    /// rail. In progress has nothing for the sidebar to show (its list lives
    /// in the main pane, see `main_pane`), so it always collapses.
    fn sidebar_is_collapsed(&self) -> bool {
        self.sidebar_collapsed || self.tab == SidebarTab::InProgress
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sidebar_is_collapsed() {
            return v_flex()
                .w(px(56.))
                .h_full()
                .flex_shrink_0()
                .items_center()
                .bg(cx.theme().sidebar)
                .border_r_1()
                .border_color(cx.theme().sidebar_border)
                .child(v_flex().flex_1())
                .child(self.sidebar_actions(true, cx))
                .into_any_element();
        }

        let visible = self.visible_jobs();
        let rows: Vec<AnyElement> = visible.iter().map(|job| self.job_row(job, cx)).collect();
        let is_empty = rows.is_empty();

        v_flex()
            .w(px(260.))
            .h_full()
            .flex_shrink_0()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    // .id() is required before .overflow_y_scroll(): the scroll
                    // offset is retained state and needs identity across frames.
                    .id("job-list")
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_1()
                    .overflow_y_scroll()
                    .when(is_empty, |this| {
                        this.child(
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(cx.theme().sidebar_foreground.opacity(0.55))
                                .child(self.tab.empty_message()),
                        )
                    })
                    .children(rows),
            )
            .child(self.sidebar_actions(false, cx))
            .into_any_element()
    }

    /// The bottom action row, shared between the full sidebar and its
    /// collapsed icon rail — `icon_only` drops the labels and shrinks the
    /// buttons to fit the narrow rail.
    fn sidebar_actions(&self, icon_only: bool, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .p_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                Button::new("new-download")
                    .primary()
                    .when(!icon_only, |b| b.w_full())
                    .icon(IconName::Plus)
                    .when(!icon_only, |b| b.label("New download"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.modal = true;
                        this.probe = ProbeState::Idle;
                        // Re-probe here so installing yt-dlp while the app is
                        // open takes effect without a restart.
                        this.refresh_ytdlp();
                        // Overrides are per-download; never carry one
                        // silently into the next job.
                        this.override_input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.url_input.update(cx, |s, cx| s.focus(window, cx));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("settings")
                    .ghost()
                    .when(!icon_only, |b| b.w_full())
                    .icon(IconName::Settings)
                    .when(!icon_only, |b| b.label("Settings"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.route = if this.route == Route::Settings {
                            Route::Library
                        } else {
                            Route::Settings
                        };
                        cx.notify();
                    })),
            )
            .when(self.tab != SidebarTab::InProgress, |this| {
                this.child(
                    Button::new("collapse-sidebar")
                        .ghost()
                        .when(!icon_only, |b| b.w_full())
                        .icon(if icon_only {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronLeft
                        })
                        .when(!icon_only, |b| b.label("Collapse"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_collapsed = !this.sidebar_collapsed;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// Full-width row above the sidebar+main split: wordmark pinned left,
    /// Download/Convert/In progress tabs true-centered regardless of the
    /// wordmark's width.
    fn navbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let tabs = TabBar::new("navbar-tabs")
            .segmented()
            .selected_index(self.tab.index())
            .children([
                Tab::new().label("Download"),
                Tab::new().label("Convert"),
                Tab::new().label("In Progress"),
            ])
            // TabBar::on_click hands back &mut App rather than &mut
            // Context<Self>, so cx.listener is unusable here; go through the
            // entity handle instead.
            .on_click(move |ix, _window, cx| {
                let tab = SidebarTab::from_index(*ix);
                entity.update(cx, |this, cx| {
                    // In progress has nothing to show in the (now narrow)
                    // sidebar — collapse it automatically so the main pane's
                    // in-progress list gets the room instead. Manually
                    // re-expanding is still available on the other tabs.
                    if tab == SidebarTab::InProgress {
                        this.sidebar_collapsed = true;
                    }
                    this.tab = tab;
                    this.selected = None;
                    this.open_item = None;
                    cx.notify();
                });
            });

        h_flex()
            .w_full()
            .flex_shrink_0()
            .h(px(48.))
            .px_4()
            .items_center()
            .bg(cx.theme().sidebar)
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            // Three equal-width columns: wordmark left, tabs centered
            // independent of the wordmark's own width, empty spacer right to
            // balance the layout.
            .child(
                div()
                    .flex_1()
                    .font_bold()
                    .text_color(cx.theme().sidebar_foreground)
                    .child("rustyDLP"),
            )
            .child(div().flex_shrink_0().child(tabs.w(px(340.))))
            .child(div().flex_1())
            .into_any_element()
    }

    fn job_row(&self, job: &Job, cx: &mut Context<Self>) -> AnyElement {
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

        v_flex()
            .id(SharedString::from(job.id.clone()))
            .w_full()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .rounded(cx.theme().radius)
            .when(active, |this| this.bg(cx.theme().sidebar_accent))
            .hover(|this| this.bg(cx.theme().list_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(id.clone());
                this.open_item = None;
                this.route = Route::Library;
                cx.notify();
            }))
            .child(
                div()
                    .text_sm()
                    .truncate()
                    .text_color(cx.theme().sidebar_foreground)
                    .child(job.title.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if job.state == JobState::Failed {
                        cx.theme().danger
                    } else {
                        cx.theme().sidebar_foreground.opacity(0.55)
                    })
                    .child(subtitle),
            )
            .when_some(live.and_then(|l| l.fraction), |this, pct| {
                this.child(
                    div()
                        .mt_1()
                        .w_full()
                        .h(px(3.))
                        .rounded_full()
                        .bg(cx.theme().muted)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(cx.theme().progress_bar)
                                .w(relative(pct.clamp(0.0, 1.0))),
                        ),
                )
            })
            .into_any_element()
    }

    fn main_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.route == Route::Settings {
            self.ensure_player(None, cx);
            return self.settings(cx);
        }
        // A selected job always wins over the tab's own landing view — this
        // is how clicking a row in the in-progress list (which lives here in
        // the main pane, not the sidebar) drills into that job's detail/
        // player view.
        //
        // Cloned rather than borrowed: `detail` needs `&mut self` (to manage
        // the player), which would conflict with holding a `&Job` borrowed
        // out of `self.jobs` for the same call.
        let Some(job) = self
            .selected
            .as_ref()
            .and_then(|id| self.jobs.iter().find(|j| &j.id == id))
            .cloned()
        else {
            self.ensure_player(None, cx);
            return match self.tab {
                SidebarTab::InProgress => self.in_progress_pane(cx),
                SidebarTab::Convert => self.convert_page(cx),
                SidebarTab::Download => self.empty_state(cx),
            };
        };

        // Job -> Item -> File: one item goes straight to detail, many show a grid.
        match job.items.len() {
            0 => self.detail(&job, None, cx),
            1 => {
                let item = job.items.first().cloned();
                self.detail(&job, item.as_ref(), cx)
            }
            _ => match self.open_item.clone() {
                Some(item_id) => {
                    let item = job.items.iter().find(|i| i.id == item_id).cloned();
                    self.detail(&job, item.as_ref(), cx)
                }
                None => {
                    self.ensure_player(None, cx);
                    self.grid(&job, cx)
                }
            },
        }
    }

    /// In progress moved out of the sidebar and into here because it mixes
    /// download and convert jobs and needs room for a progress bar per row —
    /// the 260px sidebar column was too cramped for that.
    fn in_progress_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let jobs = filter_jobs(&self.jobs, SidebarTab::InProgress);
        let is_empty = jobs.is_empty();
        let rows: Vec<AnyElement> = jobs.iter().map(|job| self.job_row(job, cx)).collect();

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
                        .text_color(cx.theme().muted_foreground)
                        .child(SidebarTab::InProgress.empty_message()),
                )
            })
            .children(rows)
            .into_any_element()
    }

    /// The Convert tab's landing view when nothing is selected: recent
    /// conversions plus the "Convert File" action that makes conversion work
    /// on any local file, not just something this app downloaded.
    fn convert_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let jobs = filter_jobs(&self.jobs, SidebarTab::Convert);
        let is_empty = jobs.is_empty();
        let rows: Vec<AnyElement> = jobs.iter().map(|job| self.job_row(job, cx)).collect();

        v_flex()
            .id("convert-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_5()
            .gap_3()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(div().font_bold().child("Convert"))
                    .child(
                        Button::new("convert-file")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Convert File")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pick_file_to_convert(cx);
                            })),
                    ),
            )
            .child(div().font_bold().text_sm().mt_2().child("Recent conversions"))
            .when(is_empty, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SidebarTab::Convert.empty_message()),
                )
            })
            .children(rows)
            .into_any_element()
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap_5()
            .child(
                svg()
                    .path("icons/empty-downloads.svg")
                    .w(px(128.))
                    .h(px(96.))
                    .text_color(cx.theme().muted_foreground.opacity(0.5)),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No selected download, select or download new video"),
            )
            .into_any_element()
    }

    fn field(&self, label: &str, input: &Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.to_string()),
            )
            .child(Input::new(input))
            .into_any_element()
    }

    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let preset_rows: Vec<AnyElement> = self
            .presets
            .iter()
            .map(|p| {
                let preset = p.clone();
                let active = self.editing.as_ref().map(|e| &e.name) == Some(&p.name);
                h_flex()
                    .id(SharedString::from(format!("preset-{}", p.name)))
                    .w_full()
                    .justify_between()
                    .items_center()
                    .px_2()
                    .py_1p5()
                    .rounded(cx.theme().radius)
                    .when(active, |t| t.bg(cx.theme().sidebar_accent))
                    .hover(|t| t.bg(cx.theme().list_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.edit_preset(preset.clone(), window, cx);
                    }))
                    .child(div().text_sm().truncate().child(p.name.clone()))
                    .when(p.is_default, |t| {
                        t.child(
                            div()
                                .text_xs()
                                .px_1p5()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted)
                                .text_color(cx.theme().muted_foreground)
                                .child("Default"),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let editor: AnyElement = match &self.editing {
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select a preset to edit, or create a new one.")
                .into_any_element(),
            Some(editing) => {
                let o = &editing.options;
                let is_audio = matches!(o.format, FormatMode::AudioOnly { .. });
                let is_custom = matches!(o.format, FormatMode::Custom(_));
                let is_best = matches!(o.format, FormatMode::BestVideoAudio);
                let is_default = editing.is_default;

                v_flex()
                    .gap_3()
                    .child(self.field("Name", &self.form.name, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Format"),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("fmt-best")
                                            .small()
                                            .selected(is_best)
                                            .label("Video + audio")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(e) = this.editing.as_mut() {
                                                    e.options.format = FormatMode::BestVideoAudio;
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("fmt-audio")
                                            .small()
                                            .selected(is_audio)
                                            .label("Audio only")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(e) = this.editing.as_mut() {
                                                    e.options.format = FormatMode::AudioOnly {
                                                        codec: "mp3".into(),
                                                    };
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("fmt-custom")
                                            .small()
                                            .selected(is_custom)
                                            .label("Custom -f")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(e) = this.editing.as_mut() {
                                                    e.options.format =
                                                        FormatMode::Custom("bv*+ba/b".into());
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .when(is_audio, |t| {
                        t.child(self.field("Audio codec", &self.form.codec, cx))
                    })
                    .when(is_custom, |t| {
                        t.child(self.field("Format selector", &self.form.custom_fmt, cx))
                    })
                    .when(is_best, |t| {
                        t.child(self.field("Max height", &self.form.max_height, cx))
                            .child(self.field("Container", &self.form.container, cx))
                    })
                    .child(self.field("Output template", &self.form.output, cx))
                    .child(self.field("Download folder", &self.form.dir, cx))
                    .child(self.field("Subtitle languages", &self.form.subs, cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(toggle(
                                "sw-thumb",
                                "Embed thumbnail",
                                o.embed_thumbnail,
                                cx,
                                |o| &mut o.embed_thumbnail,
                            ))
                            .child(toggle(
                                "sw-meta",
                                "Embed metadata",
                                o.embed_metadata,
                                cx,
                                |o| &mut o.embed_metadata,
                            ))
                            .child(toggle("sw-subs", "Embed subtitles", o.embed_subs, cx, |o| {
                                &mut o.embed_subs
                            }))
                            .child(toggle(
                                "sw-archive",
                                "Skip already-downloaded (archive)",
                                o.download_archive,
                                cx,
                                |o| &mut o.download_archive,
                            )),
                    )
                    .child(
                        // The escape hatch that makes "all yt-dlp options" true.
                        self.field("Additional arguments", &self.form.extra, cx),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Switch::new("sw-default")
                                    .checked(is_default)
                                    .label("Use as default")
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        if let Some(e) = this.editing.as_mut() {
                                            e.is_default = *checked;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("preset-save")
                                    .primary()
                                    .label("Save preset")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_editing(cx);
                                    })),
                            )
                            .child(
                                Button::new("preset-delete")
                                    .danger()
                                    .label("Delete")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.delete_editing(cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }
        };

        h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .w(px(260.))
                    .flex_shrink_0()
                    .h_full()
                    .p_4()
                    .gap_2()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(div().font_bold().text_sm().child("Presets"))
                    .child(v_flex().gap_1().children(preset_rows))
                    .child(
                        Button::new("preset-new")
                            .w_full()
                            .icon(IconName::Plus)
                            .label("New preset")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let preset = Preset {
                                    name: "New preset".into(),
                                    is_default: false,
                                    options: default_options(),
                                };
                                this.edit_preset(preset, window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        v_flex()
                            .gap_1()
                            .pt_3()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(div().font_bold().text_sm().child("Application"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "yt-dlp: {}",
                                        self.ytdlp
                                            .as_ref()
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_else(|| "not found".into())
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Downloads start staggered; no concurrency cap."),
                            )
                            .child(
                                Button::new("recheck-bin")
                                    .small()
                                    .w_full()
                                    .label("Re-check binaries")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_ytdlp();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("update-ytdlp")
                                    .small()
                                    .w_full()
                                    .label("Update yt-dlp")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_ytdlp(cx);
                                    })),
                            )
                            .when_some(self.update_status.clone(), |t, msg| {
                                t.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(msg),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("settings-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_5()
                    .gap_4()
                    .overflow_y_scroll()
                    .child(div().font_bold().child("Settings"))
                    .child(editor),
            )
            .into_any_element()
    }

    fn centered_message(&self, msg: &str, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(msg.to_string()),
            )
            .into_any_element()
    }

    fn grid(&self, job: &Job, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = job
            .items
            .iter()
            .map(|item| {
                let item_id = item.id.clone();
                v_flex()
                    .id(SharedString::from(item.id.clone()))
                    .w(px(220.))
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .hover(|this| this.bg(cx.theme().list_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_item = Some(item_id.clone());
                        cx.notify();
                    }))
                    .child(cover(item.thumb_path.as_deref(), px(112.), px(48.), cx))
                    .child(div().text_sm().truncate().child(item.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(match item.duration {
                                Some(d) => human_duration(d),
                                None => format!("{} files", item.files.len()),
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .id("grid-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_5()
            .gap_4()
            .overflow_y_scroll()
            .child(div().font_bold().child(job.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .children(cards),
            )
            .into_any_element()
    }

    // -- player --------------------------------------------------------------

    /// Starts/stops native playback so it always matches whatever file
    /// `detail()` is currently showing. `target` is the playable file path
    /// for the item on screen, or `None` when nothing playable is showing
    /// (multi-item grid, empty state, Settings, ...).
    fn ensure_player(&mut self, target: Option<&str>, cx: &mut Context<Self>) {
        match target {
            None => self.stop_player(),
            Some(path) => {
                let already_loaded = self.player.as_ref().map(|p| p.path.as_str()) == Some(path);
                let already_loading = self.player_pending.as_deref() == Some(path);
                if already_loaded || already_loading {
                    return;
                }
                self.stop_player();
                self.player_pending = Some(path.to_string());
                self.load_player(path.to_string(), 0.0, cx);
            }
        }
    }

    fn stop_player(&mut self) {
        if let Some(p) = self.player.take() {
            p.control.stop();
        }
        self.player_pending = None;
        // Any in-flight load's result will still arrive, but its generation
        // will no longer match — see `load_player`.
        self.player_gen += 1;
    }

    /// Resolves ffmpeg/ffprobe, probes, and starts playback entirely off the
    /// UI thread (both are blocking calls) — see `player::load`.
    fn load_player(&mut self, path: String, start_at_secs: f64, cx: &mut Context<Self>) {
        self.player_gen += 1;
        let generation = self.player_gen;
        let rx = player::load(PathBuf::from(&path), start_at_secs);

        cx.spawn(async move |this, cx| {
            let loaded = rx.await;
            let (control, mut events) = match loaded {
                Ok(Ok((_, control, events))) => (control, events),
                _ => {
                    // ponytail: silent fallback to the static poster +
                    // Open Externally, which always works regardless of why
                    // native playback couldn't start (missing ffmpeg, no
                    // video stream, unsupported codec, ...).
                    let _ = this.update(cx, |this, cx| {
                        if this.player_pending.as_deref() == Some(path.as_str()) {
                            this.player_pending = None;
                            cx.notify();
                        }
                    });
                    return;
                }
            };

            let installed = this
                .update(cx, |this, cx| {
                    // Superseded by a newer load (navigated away, sought
                    // again) before this one finished starting up — tear it
                    // down rather than let it leak an ffmpeg process nobody
                    // is watching.
                    if this.player_gen != generation {
                        control.stop();
                        return false;
                    }
                    this.player_pending = None;
                    this.player = Some(PlayerState {
                        control,
                        path: path.clone(),
                        frame: None,
                        position_secs: start_at_secs,
                        playing: true,
                    });
                    cx.notify();
                    true
                })
                .unwrap_or(false);

            if !installed {
                return;
            }

            while let Some(event) = events.next().await {
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.player_gen != generation {
                            return false;
                        }
                        match event {
                            player::PlayerEvent::Frame(frame, pts) => {
                                if let Some(p) = this.player.as_mut() {
                                    p.frame = Some(frame);
                                    p.position_secs = pts;
                                }
                            }
                            player::PlayerEvent::Ended => {
                                if let Some(p) = this.player.as_mut() {
                                    p.playing = false;
                                }
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.player.as_mut() {
            p.playing = !p.playing;
            p.control.set_paused(!p.playing);
            cx.notify();
        }
    }

    /// The poster/player element that replaces the old static thumbnail:
    /// shows the live decoded frame once playback has started, otherwise
    /// falls back to the plain cover art.
    fn player_view(&self, item: Option<&Item>, cx: &mut Context<Self>) -> AnyElement {
        let thumb = item.and_then(|i| i.thumb_path.as_deref());

        let Some(player) = &self.player else {
            return cover(thumb, px(300.), px(96.), cx);
        };
        let Some(frame) = player.frame.clone() else {
            return cover(thumb, px(300.), px(96.), cx);
        };

        v_flex()
            .gap_2()
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(300.))
                    .rounded(cx.theme().radius)
                    .overflow_hidden()
                    .bg(gpui::black())
                    .child(
                        img(frame)
                            .w_full()
                            .h_full()
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("player-toggle")
                            .small()
                            .icon(if player.playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(human_duration(player.position_secs)),
                    )
                    .child(
                        // ponytail: display-only — shows progress but isn't
                        // click-to-seek yet. Add position math against this
                        // bar's own prepainted bounds (same technique
                        // TabBar's indicator uses) if click-to-seek is
                        // needed.
                        div()
                            .flex_1()
                            .h(px(4.))
                            .rounded_full()
                            .bg(cx.theme().muted)
                            .child(div().h_full().rounded_full().bg(cx.theme().progress_bar)),
                    ),
            )
            .into_any_element()
    }

    fn detail(&mut self, job: &Job, item: Option<&Item>, cx: &mut Context<Self>) -> AnyElement {
        let title = item
            .map(|i| i.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| job.title.clone());
        let multi = job.items.len() > 1;

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
                                    .text_xs()
                                    .truncate()
                                    .when(!present, |t| {
                                        t.text_color(cx.theme().muted_foreground.opacity(0.6))
                                    })
                                    .child(file_name(&f.path)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .flex_shrink_0()
                                    .text_color(if present {
                                        cx.theme().muted_foreground
                                    } else {
                                        cx.theme().danger
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

        self.ensure_player(playable.as_deref(), cx);
        let player_view = self.player_view(item, cx);
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
                    .when(multi, |this| {
                        this.child(
                            Button::new("back-to-grid")
                                .ghost()
                                .small()
                                .label("← All videos")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_item = None;
                                    cx.notify();
                                })),
                        )
                    })
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
                                        .on_click(move |_, _, _| open_path(&open)),
                                )
                                .child(
                                    Button::new("convert")
                                        .small()
                                        .icon(IconName::Replace)
                                        .label("Convert")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.convert_picker = Some(convert_source.clone());
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("reveal")
                                        .small()
                                        .icon(IconName::Folder)
                                        .label("Show in folder")
                                        .on_click(move |_, _, _| reveal_path(&path)),
                                )
                            })
                            .when(running, |this| {
                                let id = job_id.clone();
                                this.child(
                                    Button::new("cancel-job")
                                        .small()
                                        .danger()
                                        .label("Cancel")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.cancel_job(&id, cx);
                                        })),
                                )
                            })
                            .when(retryable, |this| {
                                let id = job_id.clone();
                                this.child(
                                    Button::new("retry-job")
                                        .small()
                                        .primary()
                                        .label("Retry")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.retry_job(&id, cx);
                                        })),
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.delete_job(&id, cx);
                                        })),
                                )
                            }),
                    )
                    .when_some(job.error.clone(), |this, err| {
                        this.child(
                            div()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_xs()
                                .text_color(cx.theme().danger)
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
                                        .bg(cx.theme().muted)
                                        .child(
                                            div()
                                                .h_full()
                                                .rounded_full()
                                                .bg(cx.theme().progress_bar)
                                                .w(relative(pct)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
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
                    .border_color(cx.theme().border)
                    .child(div().font_bold().text_sm().child("Details"))
                    .child(self.kv("State", job.state.as_str(), cx))
                    .child(self.kv("Preset", &job.preset, cx))
                    .child(self.kv("Videos", &job.items.len().to_string(), cx))
                    .child(self.kv("Source", &job.url, cx))
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

    fn kv(&self, key: &str, value: &str, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(key.to_string()),
            )
            .child(div().text_xs().truncate().child(value.to_string()))
            .into_any_element()
    }

    fn modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let preview: AnyElement = match &self.probe {
            ProbeState::Idle => div().into_any_element(),
            ProbeState::Running => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Fetching info…")
                .into_any_element(),
            ProbeState::Err(e) => div()
                .text_xs()
                .text_color(cx.theme().danger)
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
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(p.title.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(sub),
                    )
                    .into_any_element()
            }
        };

        // ponytail: hand-rolled overlay rather than gpui_component::dialog —
        // one absolutely-positioned div, no modal-manager lifecycle to learn.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.5))
            .child(
                v_flex()
                    .w(px(520.))
                    .p_5()
                    .gap_4()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(div().font_bold().child("New download"))
                    .child(Input::new(&self.url_input))
                    .child(preview)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
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
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.chosen_preset = Some(name.clone());
                                                cx.notify();
                                            }))
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Override for this download (optional)"),
                            )
                            .child(Input::new(&self.override_input)),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.modal = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("go")
                                    .primary()
                                    .label("Download")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.start_download(cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    // -- convert -------------------------------------------------------------

    /// Opens the native file picker so conversion works on any local video,
    /// not just something this app downloaded.
    fn pick_file_to_convert(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a video to convert".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let _ = this.update(cx, |this, cx| {
                this.convert_picker = Some(path.to_string_lossy().to_string());
                cx.notify();
            });
        })
        .detach();
    }

    fn convert_format_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(source) = self.convert_picker.clone() else {
            return div().into_any_element();
        };

        // ponytail: hand-rolled overlay, same as the New download modal —
        // one absolutely-positioned div, no modal-manager lifecycle to learn.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.5))
            .child(
                v_flex()
                    .w(px(420.))
                    .p_5()
                    .gap_3()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(div().font_bold().child("Convert to…"))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(file_name(&source)),
                    )
                    .child(v_flex().gap_2().children(ConvertFormat::ALL.into_iter().map(|format| {
                        let source = source.clone();
                        Button::new(SharedString::from(format!("convert-as-{}", format.as_str())))
                            .w_full()
                            .label(format.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_convert(source.clone(), format, cx);
                            }))
                    })))
                    .child(
                        h_flex().justify_end().child(
                            Button::new("cancel-convert")
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.convert_picker = None;
                                    cx.notify();
                                })),
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
    fn start_convert(&mut self, source: String, format: ConvertFormat, cx: &mut Context<Self>) {
        self.convert_picker = None;

        let mut job = Job::new_convert(source.clone(), format.as_str());
        job.title = format!("{} → {}", file_name(&source), format.label());
        job.state = JobState::Queued;
        let job_id = job.id.clone();
        self.jobs.insert(0, job.clone());
        self.live.insert(job_id.clone(), Live::default());
        self.selected = Some(job_id.clone());
        self.open_item = None;
        self.persist(job, cx);
        cx.notify();

        let ffmpeg = match runner::ffmpeg_path(None) {
            Ok(p) => p,
            Err(e) => {
                self.fail_job(&job_id, format!("ffmpeg not available: {e}"), cx);
                return;
            }
        };
        let convert = match runner::spawn_convert(&ffmpeg, std::path::Path::new(&source), format) {
            Ok(c) => c,
            Err(e) => {
                self.fail_job(&job_id, format!("could not start ffmpeg: {e}"), cx);
                return;
            }
        };

        self.cancels.insert(job_id.clone(), convert.cancel_handle());
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.state = JobState::Running;
            let snapshot = job.clone();
            self.persist(snapshot, cx);
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

        cx.spawn(async move |this, cx| {
            let mut events = convert.events;
            while let Some(event) = events.next().await {
                let ended = this
                    .update(cx, |this, cx| {
                        this.apply_convert_event(&job_id, duration, event, cx)
                    })
                    .unwrap_or(true);
                if ended {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.finish_convert(&job_id, &output_path, cx)
            });
        })
        .detach();
    }

    fn apply_convert_event(
        &mut self,
        job_id: &str,
        duration: Option<f64>,
        event: ConvertEvent,
        cx: &mut Context<Self>,
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
                if line == crate::runner::EXIT_OK {
                    ended = true;
                } else if let Some(code) = line.strip_prefix(crate::runner::EXIT_FAIL_PREFIX) {
                    self.fail_job(job_id, format!("ffmpeg exited {code}"), cx);
                    ended = true;
                }
            }
        }
        cx.notify();
        ended
    }

    /// Attaches the converted file as a single-item Job (mirroring what a
    /// download job looks like) so it shows up in the Convert tab's list and
    /// can be played/opened the same way.
    fn finish_convert(&mut self, job_id: &str, output_path: &str, cx: &mut Context<Self>) {
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
            self.persist(snapshot, cx);
        }
        self.live.remove(job_id);
        self.cancels.remove(job_id);
        cx.notify();
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

/// Opens Explorer with the file selected. `/select,` needs the path in the same
/// argument, and Explorer rejects forward slashes here.
fn reveal_path(path: &str) {
    let native = path.replace('/', "\\");
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{native}"))
        .spawn();
}

/// A labelled switch bound to one bool on the preset being edited.
/// Takes an accessor so each toggle is one call rather than a copied closure.
fn toggle(
    id: &'static str,
    label: &'static str,
    checked: bool,
    cx: &mut Context<RustyDlp>,
    field: fn(&mut YtdlpOptions) -> &mut bool,
) -> AnyElement {
    Switch::new(id)
        .checked(checked)
        .label(label)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
            if let Some(editing) = this.editing.as_mut() {
                *field(&mut editing.options) = *checked;
            }
            cx.notify();
        }))
        .into_any_element()
}

/// Cover art box: the real thumbnail when one exists on disk, otherwise the
/// placeholder glyph. Checks `is_file` because the DB may name a cover the user
/// has since deleted, and gpui would otherwise render a broken-image gap.
fn cover(
    thumb: Option<&str>,
    height: Pixels,
    glyph: Pixels,
    cx: &mut Context<RustyDlp>,
) -> AnyElement {
    let existing = thumb.filter(|p| std::path::Path::new(p).is_file());

    match existing {
        Some(path) => img(PathBuf::from(path))
            .w_full()
            .h(height)
            .rounded(cx.theme().radius)
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => div()
            .w_full()
            .h(height)
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path("icons/empty-downloads.svg")
                    .w(glyph)
                    .h(glyph * 0.75)
                    .text_color(cx.theme().muted_foreground.opacity(0.4)),
            )
            .into_any_element(),
    }
}

/// Finds the cover written by `--write-thumbnail` next to a media file.
/// webp first: that is yt-dlp's native output and zed enables webp decoding,
/// so no conversion is needed.
pub fn sibling_thumbnail(media_path: &str) -> Option<String> {
    let path = std::path::Path::new(media_path);
    let stem = path.file_stem()?;
    let dir = path.parent()?;
    for ext in ["webp", "jpg", "png", "jpeg"] {
        let candidate = dir.join(stem).with_extension(ext);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
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

/// Download/Convert list jobs of their own kind, newest first. In progress
/// lists jobs of either kind that are still doing work — it's the only tab
/// that mixes kinds, which is why it renders in the main pane instead of a
/// per-kind sidebar list. Free function so it is testable without
/// constructing a Window.
fn filter_jobs(jobs: &[Job], tab: SidebarTab) -> Vec<&Job> {
    jobs.iter()
        .filter(|j| match tab {
            SidebarTab::Download => j.kind == JobKind::Download,
            SidebarTab::Convert => j.kind == JobKind::Convert,
            SidebarTab::InProgress => is_active(j.state),
        })
        .collect()
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

#[cfg(test)]
mod tests {
    // NOT `use super::*`: that re-globs gpui's own `test` attribute macro over
    // the built-in one, and #[test] then expands into itself until the
    // recursion limit. Import explicitly.
    use super::{
        Job, JobKind, JobState, SidebarTab, filter_jobs, is_active, is_retryable,
        sibling_thumbnail, state_after_failure,
    };

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
    fn download_and_convert_tabs_list_only_their_own_kind() {
        let jobs = vec![
            job_in(JobState::Done),
            job_in(JobState::Running),
            convert_job_in(JobState::Done),
            convert_job_in(JobState::Running),
        ];

        let downloads = filter_jobs(&jobs, SidebarTab::Download);
        assert_eq!(downloads.len(), 2);
        assert!(downloads.iter().all(|j| j.kind == JobKind::Download));

        let converts = filter_jobs(&jobs, SidebarTab::Convert);
        assert_eq!(converts.len(), 2);
        assert!(converts.iter().all(|j| j.kind == JobKind::Convert));
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
    fn tab_index_round_trips_and_defaults_to_download() {
        assert_eq!(SidebarTab::from_index(0), SidebarTab::Download);
        assert_eq!(SidebarTab::from_index(1), SidebarTab::Convert);
        assert_eq!(SidebarTab::from_index(2), SidebarTab::InProgress);
        // TabBar could in principle hand back an out-of-range index.
        assert_eq!(SidebarTab::from_index(99), SidebarTab::Download);
        assert_eq!(SidebarTab::from_index(SidebarTab::InProgress.index()), SidebarTab::InProgress);
    }

    /// `after_move` reports only the video file, so the cover has to be located
    /// by stem. webp must win: it is yt-dlp's native output.
    #[test]
    fn sibling_thumbnail_finds_the_cover_by_stem() {
        let dir = std::env::temp_dir().join(crate::model::new_id("thumb-test"));
        std::fs::create_dir_all(&dir).unwrap();

        let video = dir.join("Some Video [abc123].mp4");
        std::fs::write(&video, b"x").unwrap();
        let video_s = video.to_string_lossy().to_string();

        // No cover on disk yet.
        assert_eq!(sibling_thumbnail(&video_s), None);

        // A .jpg alone is found.
        let jpg = dir.join("Some Video [abc123].jpg");
        std::fs::write(&jpg, b"x").unwrap();
        assert_eq!(sibling_thumbnail(&video_s), Some(jpg.to_string_lossy().to_string()));

        // With both present, webp wins.
        let webp = dir.join("Some Video [abc123].webp");
        std::fs::write(&webp, b"x").unwrap();
        assert_eq!(
            sibling_thumbnail(&video_s),
            Some(webp.to_string_lossy().to_string())
        );

        // A different video in the same folder must not borrow this cover.
        let other = dir.join("Other.mp4");
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(sibling_thumbnail(&other.to_string_lossy()), None);

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(SidebarTab::Download.empty_message(), "No recent downloads");
        assert_eq!(SidebarTab::Convert.empty_message(), "No recent conversions");
        assert_eq!(SidebarTab::InProgress.empty_message(), "Nothing in progress");
    }
}

impl Render for RustyDlp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let navbar = self.navbar(cx);
        let sidebar = self.sidebar(cx);
        let main = self.main_pane(cx);
        let modal = if self.modal { Some(self.modal(cx)) } else { None };
        let convert_modal = self
            .convert_picker
            .is_some()
            .then(|| self.convert_format_picker(cx));
        let banner = self.startup_error.clone();

        div()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .size_full()
                    .when_some(banner, |this, msg| {
                        this.child(
                            h_flex()
                                .w_full()
                                .px_4()
                                .py_2()
                                .gap_3()
                                .items_center()
                                .bg(cx.theme().danger.opacity(0.15))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(cx.theme().danger)
                                        .child(msg),
                                )
                                .child(
                                    Button::new("open-bin-dir")
                                        .small()
                                        .flex_shrink_0()
                                        .icon(IconName::Folder)
                                        .label("Open folder")
                                        .on_click(|_, _, _| open_bin_dir()),
                                )
                                .child(
                                    Button::new("banner-recheck")
                                        .small()
                                        .flex_shrink_0()
                                        .label("Re-check")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.refresh_ytdlp();
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .child(navbar)
                    .child(h_flex().flex_1().min_h_0().child(sidebar).child(main)),
            )
            .children(modal)
            .children(convert_modal)
    }
}
