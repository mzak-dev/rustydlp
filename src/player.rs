//! Native in-app video playback: ffmpeg piped as raw frames + raw PCM,
//! decoded off-thread and blitted straight into a `gpui::RenderImage`.
//!
//! ponytail: frames are paced by a wall-clock timer at the source's own fps,
//! and audio is a second, independently-started ffmpeg pipe — there is no
//! audio-clock-driven frame pacing. Good enough for a first cut; if drift
//! becomes noticeable on long playback, drive frame presentation off the
//! audio callback's sample count instead of a timer.

use anyhow::{Context, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use gpui::RenderImage;
use image::{Frame, RgbaImage};
use serde::Deserialize;
use smallvec::SmallVec;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runner::base_command;

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Deserialize)]
struct ProbeOut {
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: String,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
}

/// Resolution + frame rate for sizing the raw-video pipe. Duration is not
/// probed here — it already lives on `Item.duration` from the download.
fn probe_video(ffprobe: &Path, source: &Path) -> Result<VideoInfo> {
    let out = base_command(ffprobe)
        .args(["-v", "quiet", "-print_format", "json", "-show_streams"])
        .arg(source)
        .stdin(Stdio::null())
        .output()
        .context("failed to run ffprobe")?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe exited with an error"));
    }
    let parsed: ProbeOut =
        serde_json::from_slice(&out.stdout).context("ffprobe returned unparseable JSON")?;
    let stream = parsed
        .streams
        .iter()
        .find(|s| s.codec_type == "video")
        .ok_or_else(|| anyhow!("no video stream found"))?;
    let width = stream.width.ok_or_else(|| anyhow!("video stream has no width"))?;
    let height = stream.height.ok_or_else(|| anyhow!("video stream has no height"))?;
    let fps = stream
        .r_frame_rate
        .as_deref()
        .and_then(parse_fraction)
        .filter(|f| *f > 0.0)
        .unwrap_or(30.0);

    Ok(VideoInfo { width, height, fps })
}

/// ffprobe reports frame rate as "30000/1001" rather than a decimal.
fn parse_fraction(s: &str) -> Option<f64> {
    match s.split_once('/') {
        Some((num, den)) => {
            let num: f64 = num.parse().ok()?;
            let den: f64 = den.parse().ok()?;
            (den != 0.0).then_some(num / den)
        }
        None => s.parse().ok(),
    }
}

pub enum PlayerEvent {
    Frame(Arc<RenderImage>, f64),
    Ended,
}

/// The control side of a playback run: pause/resume/stop. Kept in UI state
/// for as long as the run is loaded. The frame stream (`PlayerEvent`s) is
/// consumed separately by whoever called `load`, so it can be driven inside
/// an async loop without needing `&mut` access to whatever holds this.
pub struct PlayerControl {
    video_child: Arc<Mutex<Option<Child>>>,
    audio_stop: Option<std_mpsc::Sender<()>>,
    paused: Arc<AtomicBool>,
}

impl PlayerControl {
    /// Pause = stop draining the ffmpeg pipes (which naturally blocks ffmpeg
    /// on its own output once the OS pipe buffer fills, so decode itself
    /// idles) and output silence instead of draining the audio ring buffer.
    /// Resume just clears the flag; nothing needs to be re-spawned.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        if let Ok(mut guard) = self.video_child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
        if let Some(tx) = &self.audio_stop {
            let _ = tx.send(());
        }
    }
}

/// Resolves the bundled ffmpeg/ffprobe, probes the source, and starts
/// playback from `start_at_secs`. Runs entirely on a background thread (ffprobe
/// and the initial ffmpeg spawn are blocking calls) and reports back once
/// playback has actually started.
pub fn load(
    source: PathBuf,
    start_at_secs: f64,
) -> oneshot::Receiver<Result<(VideoInfo, PlayerControl, mpsc::UnboundedReceiver<PlayerEvent>)>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let ffmpeg = crate::runner::ffmpeg_path(None)?;
            let ffprobe = crate::runner::ffprobe_path(None)?;
            let info = probe_video(&ffprobe, &source)?;
            let (control, events) = spawn_player(&ffmpeg, &source, info, start_at_secs)?;
            Ok((info, control, events))
        })();
        let _ = tx.send(result);
    });
    rx
}

fn spawn_player(
    ffmpeg: &Path,
    source: &Path,
    info: VideoInfo,
    start_at_secs: f64,
) -> Result<(PlayerControl, mpsc::UnboundedReceiver<PlayerEvent>)> {
    let (tx, rx) = mpsc::unbounded();
    let paused = Arc::new(AtomicBool::new(false));

    let mut video_child = base_command(ffmpeg)
        .args(["-ss", &start_at_secs.to_string(), "-i"])
        .arg(source)
        .args([
            "-an",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-vf",
            &format!("fps={}", info.fps),
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ffmpeg (video)")?;

    let stdout = video_child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child stdout was not captured"))?;
    let video_child = Arc::new(Mutex::new(Some(video_child)));

    {
        let video_child = Arc::clone(&video_child);
        let paused = Arc::clone(&paused);
        let frame_interval = Duration::from_secs_f64(1.0 / info.fps.max(1.0));
        let width = info.width;
        let height = info.height;
        std::thread::spawn(move || {
            let mut reader = stdout;
            let frame_len = (width as usize) * (height as usize) * 4;
            let mut buf = vec![0u8; frame_len];
            let start = Instant::now();
            let mut n: u32 = 0;
            loop {
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                if !read_exact_or_eof(&mut reader, &mut buf) {
                    break;
                }
                if let Some(image) = RgbaImage::from_raw(width, height, buf.clone()) {
                    let render =
                        Arc::new(RenderImage::new(SmallVec::from_elem(Frame::new(image), 1)));
                    let pts = start_at_secs + n as f64 / info.fps;
                    if tx.unbounded_send(PlayerEvent::Frame(render, pts)).is_err() {
                        break; // receiver dropped
                    }
                }
                n += 1;
                // Pace to real time so decode speed (which can outrun
                // playback speed by a lot) doesn't burn through the whole
                // video in a fraction of a second. A pause simply stalls
                // `n`, so the schedule picks back up correctly on resume.
                let target = start + frame_interval * n;
                let now = Instant::now();
                if target > now {
                    std::thread::sleep(target - now);
                }
            }
            let _ = video_child.lock().map(|mut g| {
                if let Some(c) = g.as_mut() {
                    let _ = c.wait();
                }
            });
            let _ = tx.unbounded_send(PlayerEvent::Ended);
        });
    }

    let audio_stop = spawn_audio(ffmpeg, source, start_at_secs, Arc::clone(&paused)).ok();

    Ok((
        PlayerControl {
            video_child,
            audio_stop,
            paused,
        },
        rx,
    ))
}

/// Reads exactly `buf.len()` bytes, or reports a clean EOF if the stream
/// ends before that (the last frame of a file, or ffmpeg exiting on seek
/// past the end).
fn read_exact_or_eof(reader: &mut ChildStdout, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(_) => return false,
        }
    }
    true
}

/// Spawns a second ffmpeg piping raw PCM into a dedicated audio thread that
/// owns the cpal stream for its whole lifetime (cpal's `Stream` is not
/// `Send`, so it can never leave the thread that created it). Returns a
/// sender whose drop (or a message) tells that thread to tear down.
fn spawn_audio(
    ffmpeg: &Path,
    source: &Path,
    start_at_secs: f64,
    paused: Arc<AtomicBool>,
) -> Result<std_mpsc::Sender<()>> {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    let mut audio_child = base_command(ffmpeg)
        .args(["-ss", &start_at_secs.to_string(), "-i"])
        .arg(source)
        .args([
            "-vn",
            "-loglevel",
            "error",
            "-f",
            "s16le",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-ac",
            &CHANNELS.to_string(),
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ffmpeg (audio)")?;

    let mut pcm_out = audio_child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("audio child stdout was not captured"))?;

    // Small ring buffer bridging the pipe-reading thread and the cpal
    // callback thread. i16 samples, interleaved stereo.
    let ring: Arc<Mutex<VecDeque<i16>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(SAMPLE_RATE as usize)));

    {
        let ring = Arc::clone(&ring);
        let paused = Arc::clone(&paused);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                match pcm_out.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = match ring.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        for pair in chunk[..n].chunks_exact(2) {
                            guard.push_back(i16::from_le_bytes([pair[0], pair[1]]));
                        }
                        // Cap so a paused/slow consumer doesn't grow this
                        // unboundedly; drop the oldest audio instead.
                        while guard.len() > SAMPLE_RATE as usize * CHANNELS as usize * 5 {
                            guard.pop_front();
                        }
                    }
                }
            }
            let _ = audio_child.kill();
        });
    }

    let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(e) = run_audio_output(ring, SAMPLE_RATE, CHANNELS, paused, stop_rx) {
            eprintln!("player: audio output failed: {e}");
        }
    });

    Ok(stop_tx)
}

fn run_audio_output(
    ring: Arc<Mutex<VecDeque<i16>>>,
    sample_rate: u32,
    channels: u16,
    paused: Arc<AtomicBool>,
    stop_rx: std_mpsc::Receiver<()>,
) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no default audio output device"))?;
    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _| {
            if paused.load(Ordering::Relaxed) {
                data.fill(0);
                return;
            }
            let mut guard = match ring.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            // ponytail: fixed starting volume, no UI control yet — add a
            // slider wired to this scale factor if adjustable volume is needed.
            const START_VOLUME: f32 = 0.45;
            for sample in data.iter_mut() {
                let raw = guard.pop_front().unwrap_or(0);
                *sample = (raw as f32 * START_VOLUME) as i16;
            }
        },
        |err| eprintln!("player: audio stream error: {err}"),
        None,
    )?;
    stream.play()?;

    // Parked here for the session's whole lifetime: `stream` must not drop
    // (and this thread must not exit) while audio should keep playing.
    let _ = stop_rx.recv();
    Ok(())
}
