//! Locating the bundled binaries, and running them.
//!
//! ponytail: one std::thread per running process, parked on a blocking
//! read_line, rather than an async-process dependency. A thread blocked on a
//! pipe costs ~8 KB and no CPU, and the concurrency ceiling here is ~3. Events
//! cross to the UI thread over a futures channel, which a gpui background task
//! awaits and forwards via cx.update().

use crate::core::ytdlp::{Event, YtdlpOptions, download_args, parse_line, probe_args};
use anyhow::{Context, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `%LOCALAPPDATA%\rustyDLP\bin` — deliberately not Program Files, so
/// `yt-dlp.exe -U` can overwrite itself without elevation.
///
/// Resolved through the known-folder API first, not the raw environment
/// variable: an inherited LOCALAPPDATA can carry trailing whitespace or quoting
/// that yields `...\Local \rustyDLP\bin` and fails with ERROR_PATH_NOT_FOUND,
/// while printing indistinguishably from the correct value.
pub fn bin_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
        .join("rustyDLP")
        .join("bin")
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// `<dir containing our exe>\bin` — the portable layout, and the one that
/// keeps working when the environment or known-folder lookup misbehaves.
pub fn exe_relative_bin_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("bin"))
}

/// Resolution order: explicit override -> next to the exe -> app-data bin ->
/// PATH. Exe-relative comes before app-data so a portable copy wins over a
/// stale global install; app-data stays in the chain because that is the
/// location `yt-dlp -U` can write to.
pub fn resolve(stem: &str, override_path: Option<&Path>) -> Option<PathBuf> {
    let name = exe_name(stem);

    if let Some(p) = override_path
        && p.is_file()
    {
        return Some(p.to_path_buf());
    }
    if let Some(portable) = exe_relative_bin_dir().map(|d| d.join(&name))
        && portable.is_file()
    {
        return Some(portable);
    }
    let bundled = bin_dir().join(&name);
    if bundled.is_file() {
        return Some(bundled);
    }
    which_on_path(&name)
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

pub fn ytdlp_path(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(found) = resolve("yt-dlp", override_path) {
        return Ok(found);
    }

    // Self-diagnosing on purpose: "not found" against a path the user can see
    // in Explorer is unactionable. `exists` true with `is_file` false means a
    // permissions or reparse-point problem, not a missing download.
    let dir = bin_dir();
    let bundled = dir.join(exe_name("yt-dlp"));
    let listing = std::fs::read_dir(&dir)
        .map(|rd| {
            let mut names: Vec<String> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            if names.is_empty() {
                "<empty>".to_string()
            } else {
                names.join(", ")
            }
        })
        .unwrap_or_else(|e| format!("<unreadable: {e}>"));

    Err(anyhow!(
        "yt-dlp not found.  app-data: {:?} (exists {}, is_file {})  |  dir: [{}]  |  portable: {:?}  |  known-folder: {:?}  |  env LOCALAPPDATA: {:?}",
        bundled,
        bundled.exists(),
        bundled.is_file(),
        listing,
        exe_relative_bin_dir().map(|d| d.join(exe_name("yt-dlp"))),
        // Both printed with Debug so trailing whitespace or embedded quoting in
        // either source is actually visible rather than rendering identically.
        dirs::data_local_dir(),
        std::env::var("LOCALAPPDATA").ok(),
    ))
}

/// ffmpeg is expected bundled alongside yt-dlp (yt-dlp already shells out to
/// it by name for muxing), so it resolves through the same search order.
pub fn ffmpeg_path(override_path: Option<&Path>) -> Result<PathBuf> {
    resolve("ffmpeg", override_path)
        .ok_or_else(|| anyhow!("ffmpeg not found in bin dir, next to the exe, or on PATH"))
}

/// ffprobe ships in the same archive as ffmpeg in every common distribution.
pub fn ffprobe_path(override_path: Option<&Path>) -> Result<PathBuf> {
    resolve("ffprobe", override_path)
        .ok_or_else(|| anyhow!("ffprobe not found in bin dir, next to the exe, or on PATH"))
}

pub(crate) fn base_command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    // Without this a console window flashes on every spawn — including the
    // debounced probe that fires while the user is still typing a URL.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    // yt-dlp shells out to ffmpeg by name; make the bundled one win.
    c.env(
        "PATH",
        prepend_path(&bin_dir()).unwrap_or_else(|| bin_dir().into_os_string()),
    );
    c
}

fn prepend_path(dir: &Path) -> Option<std::ffi::OsString> {
    let existing = std::env::var_os("PATH")?;
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(&existing));
    std::env::join_paths(dirs).ok()
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// The subset of `yt-dlp -J --flat-playlist` the New Download modal needs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Probe {
    /// "video" or "playlist".
    #[serde(rename = "_type")]
    pub kind: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub webpage_url: Option<String>,
    pub uploader: Option<String>,
    pub playlist_count: Option<i64>,
    pub entries: Option<Vec<ProbeEntry>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProbeEntry {
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub url: Option<String>,
    pub thumbnails: Option<Vec<Thumbnail>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Thumbnail {
    pub url: Option<String>,
}

impl Probe {
    pub fn is_playlist(&self) -> bool {
        self.kind.as_deref() == Some("playlist")
    }

    /// How many videos this URL will produce.
    pub fn item_count(&self) -> usize {
        if self.is_playlist() {
            self.entries
                .as_ref()
                .map(|e| e.len())
                .or(self.playlist_count.map(|c| c as usize))
                .unwrap_or(0)
        } else {
            1
        }
    }
}

/// Runs the metadata probe off-thread. Resolves once yt-dlp exits.
pub fn probe(exe: PathBuf, url: String) -> oneshot::Receiver<Result<Probe>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe_blocking(&exe, &url));
    });
    rx
}

fn probe_blocking(exe: &Path, url: &str) -> Result<Probe> {
    let out = base_command(exe)
        .args(probe_args(url))
        .stdin(Stdio::null())
        .output()
        .context("failed to run yt-dlp for probe")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // yt-dlp's own message is far more useful than anything we'd invent
        // ("Unsupported URL", "Private video", "Sign in to confirm your age").
        let tail = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        return Err(anyhow!("yt-dlp probe failed: {tail}"));
    }
    serde_json::from_slice(&out.stdout).context("probe returned unparseable JSON")
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// A running download. Dropping this does NOT kill the process.
pub struct Download {
    pub events: mpsc::UnboundedReceiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
}

/// Kill switch for a running download, detachable from the `Download` itself so
/// the event stream can be moved into a task while the UI keeps the ability to
/// cancel.
#[derive(Clone)]
pub struct CancelHandle(Arc<Mutex<Option<Child>>>);

impl CancelHandle {
    /// Kills the child. Partial `.part` files survive, so a retry resumes from
    /// where it stopped rather than starting over.
    pub fn cancel(&self) {
        if let Ok(mut guard) = self.0.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    }
}

impl Download {
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(Arc::clone(&self.child))
    }
}

/// Runs `yt-dlp -U`. Works because the binary lives under LOCALAPPDATA and can
/// therefore overwrite itself without elevation.
pub fn update_ytdlp(exe: PathBuf) -> oneshot::Receiver<Result<String>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let out = base_command(&exe)
                .arg("-U")
                .stdin(Stdio::null())
                .output()
                .context("failed to run yt-dlp -U")?;
            let text = String::from_utf8_lossy(&out.stdout);
            let err = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{text}{err}");
            let last = combined
                .lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("yt-dlp reported nothing")
                .to_string();
            if out.status.success() {
                Ok(last)
            } else {
                Err(anyhow!("{last}"))
            }
        })();
        let _ = tx.send(result);
    });
    rx
}

/// Terminal event appended to the stream so consumers know the run ended.
pub const EXIT_OK: &str = "RDLP_EXIT ok";
pub const EXIT_FAIL_PREFIX: &str = "RDLP_EXIT fail ";

pub fn spawn_download(exe: &Path, url: &str, opts: &YtdlpOptions) -> Result<Download> {
    let mut child = base_command(exe)
        .args(download_args(url, opts))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn yt-dlp")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child stdout was not captured"))?;
    let stderr = child.stderr.take();

    let (tx, rx) = mpsc::unbounded();
    let child = Arc::new(Mutex::new(Some(child)));

    // stderr carries the real reason for a failure; fold it into the same
    // stream as Log events so the failure tail is one ordered buffer.
    if let Some(stderr) = stderr {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.unbounded_send(Event::Log(line));
            }
        });
    }

    {
        let child = Arc::clone(&child);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.unbounded_send(parse_line(&line)).is_err() {
                    break; // receiver dropped; nobody is listening any more
                }
            }
            // Reap the process so it does not linger as a zombie, and report
            // how it ended.
            let status = child.lock().ok().and_then(|mut g| g.as_mut().map(|c| c.wait()));
            let msg = match status {
                Some(Ok(s)) if s.success() => EXIT_OK.to_string(),
                Some(Ok(s)) => format!("{EXIT_FAIL_PREFIX}{}", s.code().unwrap_or(-1)),
                _ => format!("{EXIT_FAIL_PREFIX}unknown"),
            };
            let _ = tx.unbounded_send(Event::Log(msg));
        });
    }

    Ok(Download { events: rx, child })
}

// ---------------------------------------------------------------------------
// Convert
// ---------------------------------------------------------------------------

/// A fixed target format for the Convert feature. No codec picker — each
/// variant bakes in a codec pair that actually works together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertFormat {
    Mp4H264Aac,
    MkvH264Aac,
    WebmVp9Opus,
    Mp3Audio,
}

impl ConvertFormat {
    pub const ALL: [ConvertFormat; 4] = [
        Self::Mp4H264Aac,
        Self::MkvH264Aac,
        Self::WebmVp9Opus,
        Self::Mp3Audio,
    ];

    /// Stored in `Job.preset` for a convert job, and used to round-trip from
    /// the DB.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp4H264Aac => "mp4",
            Self::MkvH264Aac => "mkv",
            Self::WebmVp9Opus => "webm",
            Self::Mp3Audio => "mp3",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "mkv" => Self::MkvH264Aac,
            "webm" => Self::WebmVp9Opus,
            "mp3" => Self::Mp3Audio,
            _ => Self::Mp4H264Aac,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4H264Aac => "MP4 (H.264/AAC)",
            Self::MkvH264Aac => "MKV (H.264/AAC)",
            Self::WebmVp9Opus => "WebM (VP9/Opus)",
            Self::Mp3Audio => "MP3 (audio only)",
        }
    }

    fn extension(self) -> &'static str {
        self.as_str()
    }

    /// Always a full re-encode — no remux-first branching — so it works
    /// regardless of the source's own codec.
    fn ffmpeg_codec_args(self) -> Vec<&'static str> {
        match self {
            Self::Mp4H264Aac | Self::MkvH264Aac => {
                vec!["-c:v", "libx264", "-c:a", "aac"]
            }
            Self::WebmVp9Opus => vec!["-c:v", "libvpx-vp9", "-c:a", "libopus"],
            Self::Mp3Audio => vec!["-vn", "-c:a", "libmp3lame"],
        }
    }
}

/// Same directory as the source, named so a same-extension conversion (e.g.
/// mp4 -> mp4) can never collide with the file it was made from.
pub fn convert_output_path(source: &Path, format: ConvertFormat) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "converted".to_string());
    source.with_file_name(format!("{stem} [{}].{}", format.as_str(), format.extension()))
}

#[derive(Debug, Clone, Default)]
pub struct ConvertProgress {
    pub out_time_secs: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum ConvertEvent {
    Progress(ConvertProgress),
    Log(String),
}

/// A running conversion. Mirrors `Download`'s shape exactly — same
/// child-tracking, same cancel handle — just driven by ffmpeg's own
/// `-progress` key=value stream instead of yt-dlp's JSON.
pub struct Convert {
    pub events: mpsc::UnboundedReceiver<ConvertEvent>,
    pub output_path: PathBuf,
    child: Arc<Mutex<Option<Child>>>,
}

impl Convert {
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(Arc::clone(&self.child))
    }
}

pub fn spawn_convert(ffmpeg: &Path, source: &Path, format: ConvertFormat) -> Result<Convert> {
    let output_path = convert_output_path(source, format);

    let mut args: Vec<std::ffi::OsString> = vec![
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        source.into(),
    ];
    args.extend(format.ffmpeg_codec_args().into_iter().map(Into::into));
    args.extend(
        ["-progress", "pipe:1", "-nostats"]
            .into_iter()
            .map(std::ffi::OsString::from),
    );
    args.push(output_path.clone().into_os_string());

    let mut child = base_command(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child stdout was not captured"))?;
    let stderr = child.stderr.take();

    let (tx, rx) = mpsc::unbounded();
    let child = Arc::new(Mutex::new(Some(child)));

    if let Some(stderr) = stderr {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.unbounded_send(ConvertEvent::Log(line));
            }
        });
    }

    {
        let child = Arc::clone(&child);
        std::thread::spawn(move || {
            let mut acc = ConvertProgress::default();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                match key {
                    "out_time_ms" => {
                        acc.out_time_secs = value.trim().parse::<f64>().ok().map(|us| us / 1_000_000.0);
                    }
                    "speed" => {
                        acc.speed = value.trim().trim_end_matches('x').parse().ok();
                    }
                    "progress" => {
                        // One "frame" of key=value pairs ends with
                        // progress=continue|end — that's the signal to emit.
                        let _ = tx.unbounded_send(ConvertEvent::Progress(acc.clone()));
                        if value.trim() == "end" {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let status = child.lock().ok().and_then(|mut g| g.as_mut().map(|c| c.wait()));
            let msg = match status {
                Some(Ok(s)) if s.success() => EXIT_OK.to_string(),
                Some(Ok(s)) => format!("{EXIT_FAIL_PREFIX}{}", s.code().unwrap_or(-1)),
                _ => format!("{EXIT_FAIL_PREFIX}unknown"),
            };
            let _ = tx.unbounded_send(ConvertEvent::Log(msg));
        });
    }

    Ok(Convert { events: rx, output_path, child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_an_explicit_override() {
        // Any real file works as a stand-in for a user-supplied binary path.
        let me = std::env::current_exe().unwrap();
        let got = resolve("yt-dlp", Some(&me)).unwrap();
        assert_eq!(got, me);
    }

    #[test]
    fn resolve_ignores_an_override_that_does_not_exist() {
        let bogus = PathBuf::from("Z:/definitely/not/here/yt-dlp.exe");
        // Falls through to the bundled dir or PATH rather than returning bogus.
        assert_ne!(resolve("yt-dlp", Some(&bogus)), Some(bogus));
    }

    #[test]
    fn bin_dir_is_user_writable_not_program_files() {
        let d = bin_dir().to_string_lossy().to_lowercase();
        assert!(!d.contains("program files"), "yt-dlp -U could not self-update");
    }

    #[test]
    fn probe_json_classifies_single_video_and_playlist() {
        let single: Probe = serde_json::from_str(
            r#"{"_type":"video","id":"a","title":"One","duration":635.0}"#,
        )
        .unwrap();
        assert!(!single.is_playlist());
        assert_eq!(single.item_count(), 1);

        let list: Probe = serde_json::from_str(
            r#"{"_type":"playlist","title":"Mix","entries":[
                 {"id":"a","title":"1"},{"id":"b","title":"2"},{"id":"c","title":"3"}]}"#,
        )
        .unwrap();
        assert!(list.is_playlist());
        assert_eq!(list.item_count(), 3);
    }

    /// Exercises the real binary over the real network: resolve -> spawn with
    /// CREATE_NO_WINDOW -> parse. The unit tests above all use synthetic JSON,
    /// so this is the only thing that would catch a wrong argv or a yt-dlp
    /// output change. Run with: cargo test -- --ignored
    #[test]
    #[ignore = "requires network and the bundled yt-dlp"]
    fn probe_a_real_youtube_url() {
        let exe = ytdlp_path(None).expect("bundled yt-dlp should be present");
        let rx = probe(exe, "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into());
        let probe = pollster::block_on(rx).expect("probe thread died").unwrap();

        assert!(!probe.is_playlist());
        assert_eq!(probe.item_count(), 1);
        assert!(
            probe.title.as_deref().unwrap_or("").contains("Big Buck Bunny"),
            "unexpected title: {:?}",
            probe.title
        );
        assert!(probe.duration.unwrap_or(0.0) > 600.0);
        assert!(probe.thumbnail.is_some());
    }

    /// The whole pipeline against the real binary: spawn -> stream stdout ->
    /// classify events -> reap. Asserts we actually observe progress, a final
    /// file that exists on disk, and a clean exit.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "requires network; downloads ~28 MB"]
    fn download_a_real_video_end_to_end() {
        use crate::core::ytdlp::FormatMode;
        use futures::StreamExt as _;

        let exe = ytdlp_path(None).expect("bundled yt-dlp");
        let dir = std::env::temp_dir().join(crate::core::model::new_id("rustydlp-dl"));
        std::fs::create_dir_all(&dir).unwrap();

        let opts = YtdlpOptions {
            format: FormatMode::Custom("worst".into()),
            download_dir: dir.to_string_lossy().to_string(),
            output_template: "t.%(ext)s".into(),
            ..Default::default()
        };

        let dl = spawn_download(&exe, "https://www.youtube.com/watch?v=aqz-KE-bpKQ", &opts)
            .expect("spawn");

        let (mut progress_ticks, mut infos, mut files, mut clean_exit) = (0, 0, vec![], false);
        pollster::block_on(async {
            let mut events = dl.events;
            while let Some(e) = events.next().await {
                match e {
                    Event::Progress(p) if p.fraction().is_some() => progress_ticks += 1,
                    Event::Info(_) => infos += 1,
                    Event::File(f) => files.push(f),
                    Event::Log(l) if l == EXIT_OK => {
                        clean_exit = true;
                        break;
                    }
                    Event::Log(l) if l.starts_with(EXIT_FAIL_PREFIX) => break,
                    _ => {}
                }
            }
        });

        assert!(clean_exit, "yt-dlp did not exit cleanly");
        assert!(progress_ticks > 3, "only {progress_ticks} usable progress ticks");
        assert_eq!(infos, 1, "expected exactly one info line for a single video");
        assert!(!files.is_empty(), "no output file was reported");
        assert!(
            std::path::Path::new(&files[0]).exists(),
            "reported file does not exist: {}",
            files[0]
        );

        // --write-thumbnail must leave a cover the grid can find by stem.
        // after_move never reports it, so this is the only thing that proves
        // the flag is doing its job.
        let cover = crate::core::model::sibling_thumbnail(&files[0]);
        assert!(
            cover.is_some(),
            "no sibling thumbnail beside {}; dir held: {:?}",
            files[0],
            std::fs::read_dir(&dir)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect::<Vec<_>>())
                .unwrap_or_default()
        );
        assert!(std::path::Path::new(&cover.unwrap()).is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real yt-dlp output omits keys entirely rather than sending nulls; every
    /// field must therefore be optional.
    #[test]
    fn probe_tolerates_a_sparse_payload() {
        let p: Probe = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        assert_eq!(p.item_count(), 1);
        assert!(p.title.is_none());
    }
}
