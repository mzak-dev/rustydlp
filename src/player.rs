//! Native in-app video playback: ffmpeg piped as raw frames + raw PCM,
//! decoded off-thread and blitted straight into a `gpui::RenderImage`.
//!
//! ponytail: frames are paced by a wall-clock timer at the source's own fps
//! rather than off the audio callback's sample count. The two pipes are
//! started from a shared zero point (the video thread waits for the first
//! sample to actually reach the sound card, see `audio_started`) and both then
//! run at real time, so drift is limited to the difference between the system
//! clock and the audio device's clock. If that becomes noticeable on long
//! playback, drive frame presentation off the audio clock instead.

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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runner::base_command;

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Total length, when the container reports one. Needed for the seek bar:
    /// a converted file has no `Item.duration` recorded from the download.
    pub duration: Option<f64>,
}

#[derive(Deserialize)]
struct ProbeOut {
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: String,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

/// Resolution, frame rate and length for sizing the raw-video pipe and the
/// seek bar.
fn probe_video(ffprobe: &Path, source: &Path) -> Result<VideoInfo> {
    let out = base_command(ffprobe)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(source)
        .stdin(Stdio::null())
        .output()
        .context("failed to run ffprobe")?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe exited with an error"));
    }
    parse_probe(&out.stdout)
}

/// Split out from `probe_video` so the parsing rules — which duration wins,
/// what a missing one means — are testable without an ffprobe binary.
fn parse_probe(json: &[u8]) -> Result<VideoInfo> {
    let parsed: ProbeOut =
        serde_json::from_slice(json).context("ffprobe returned unparseable JSON")?;
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
    // The container's own duration is the reliable one; a stream-level
    // duration only shows up for some formats, so it is the fallback. Each is
    // parsed before falling through, because a container that has the field
    // but fills it with "N/A" is exactly the case the fallback exists for.
    let duration = parsed
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(parse_secs)
        .or_else(|| stream.duration.as_deref().and_then(parse_secs));

    Ok(VideoInfo {
        width,
        height,
        fps,
        duration,
    })
}

/// A duration ffprobe couldn't measure comes back as "N/A" or as zero; both
/// mean the same thing to the seek bar, which has to divide by it.
fn parse_secs(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|d| *d > 0.0)
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

/// Output gain in `0.0..=1.0`, shared with the cpal callback. Kept as f32 bits
/// in an atomic so the realtime callback reads it without taking a lock (and
/// so moving the volume slider never has to touch the audio thread).
#[derive(Clone)]
struct Volume(Arc<AtomicU32>);

impl Volume {
    fn new(value: f32) -> Self {
        let this = Self(Arc::new(AtomicU32::new(0)));
        this.set(value);
        this
    }

    fn set(&self, value: f32) {
        self.0.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// The control side of a playback run: pause/resume/volume/stop. Kept in UI
/// state for as long as the run is loaded. The frame stream (`PlayerEvent`s)
/// is consumed separately by whoever called `load`, so it can be driven inside
/// an async loop without needing `&mut` access to whatever holds this.
pub struct PlayerControl {
    video_child: Arc<Mutex<Option<Child>>>,
    paused: Arc<AtomicBool>,
    /// Tears down the audio side. Both audio threads poll it: the reader
    /// thread would otherwise sit forever waiting for room in a ring nobody
    /// drains any more, keeping its ffmpeg alive with it.
    stopped: Arc<AtomicBool>,
    volume: Volume,
}

impl PlayerControl {
    /// Pause = stop draining the ffmpeg pipes (which naturally blocks ffmpeg
    /// on its own output once the OS pipe buffer fills, so decode itself
    /// idles) and output silence instead of draining the audio ring buffer.
    /// Resume just clears the flag; nothing needs to be re-spawned.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Takes effect on the next audio callback — no re-spawn, no glitch.
    pub fn set_volume(&self, volume: f32) {
        self.volume.set(volume);
    }

    /// Idempotent: seeking stops the old run explicitly and then drops it.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // Unblock a paused run so its threads reach the stop check instead of
        // sleeping on the pause flag.
        self.paused.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = self.video_child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

/// Dropping the control is the same thing as stopping: without this, any path
/// that lets a `PlayerControl` go without calling `stop` leaks two ffmpeg
/// processes and three threads for the rest of the session.
impl Drop for PlayerControl {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Resolves the bundled ffmpeg/ffprobe, probes the source, and starts
/// playback from `start_at_secs` at `volume`. Runs entirely on a background
/// thread (ffprobe and the initial ffmpeg spawn are blocking calls) and
/// reports back once playback has actually started.
pub fn load(
    source: PathBuf,
    start_at_secs: f64,
    volume: f32,
) -> oneshot::Receiver<Result<(VideoInfo, PlayerControl, mpsc::UnboundedReceiver<PlayerEvent>)>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let ffmpeg = crate::runner::ffmpeg_path(None)?;
            let ffprobe = crate::runner::ffprobe_path(None)?;
            let info = probe_video(&ffprobe, &source)?;
            let (control, events) = spawn_player(&ffmpeg, &source, info, start_at_secs, volume)?;
            Ok((info, control, events))
        })();
        let _ = tx.send(result);
    });
    rx
}

/// How long the video thread will wait for audio to actually start before
/// giving up and pacing itself. Only reached when the file has no audio
/// stream, or the machine has no working output device.
const AUDIO_START_TIMEOUT: Duration = Duration::from_millis(1500);

fn spawn_player(
    ffmpeg: &Path,
    source: &Path,
    info: VideoInfo,
    start_at_secs: f64,
    volume: f32,
) -> Result<(PlayerControl, mpsc::UnboundedReceiver<PlayerEvent>)> {
    let (tx, rx) = mpsc::unbounded();
    let paused = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let volume = Volume::new(volume);

    // Audio goes first so its start gate exists before the video thread that
    // waits on it. Both ffmpegs get the same -ss, so they decode the same
    // content position; the gate is what lines up *when* they start.
    let audio_started = spawn_audio(
        ffmpeg,
        source,
        start_at_secs,
        Arc::clone(&paused),
        Arc::clone(&stopped),
        volume.clone(),
    )
    .ok();

    // Anything that fails from here on has to take the already-running audio
    // side down with it.
    let stop_audio_on_err = |e: anyhow::Error| {
        stopped.store(true, Ordering::Relaxed);
        e
    };

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
        .context("failed to spawn ffmpeg (video)")
        .map_err(stop_audio_on_err)?;

    let stdout = video_child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child stdout was not captured"))
        .map_err(stop_audio_on_err)?;
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
            // Don't start the presentation clock until sound is actually
            // coming out, otherwise the video runs ahead by however long
            // ffmpeg + the audio device took to spin up.
            if let Some(started) = audio_started {
                let deadline = Instant::now() + AUDIO_START_TIMEOUT;
                while !started.load(Ordering::Relaxed) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let mut start = Instant::now();
            let mut n: u32 = 0;
            loop {
                if paused.load(Ordering::Relaxed) {
                    // The schedule is relative to real playback time, so the
                    // time spent paused has to be added back to the origin —
                    // otherwise resuming decodes at full speed until `n`
                    // catches up with the wall clock, fast-forwarding the
                    // picture away from the audio.
                    let paused_at = Instant::now();
                    while paused.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    start += paused_at.elapsed();
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
                // video in a fraction of a second.
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

    Ok((
        PlayerControl {
            video_child,
            paused,
            stopped,
            volume,
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
/// `Send`, so it can never leave the thread that created it). Returns the
/// flag the video thread waits on before starting its own clock.
fn spawn_audio(
    ffmpeg: &Path,
    source: &Path,
    start_at_secs: f64,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    volume: Volume,
) -> Result<Arc<AtomicBool>> {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    /// How much decoded audio may sit in the ring at once. Long enough to
    /// ride out scheduling jitter, short enough that pausing doesn't leave a
    /// stale tail queued in front of the resume.
    const BUFFER_SECS: f64 = 0.5;

    let capacity = (SAMPLE_RATE as f64 * CHANNELS as f64 * BUFFER_SECS) as usize;

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

    // Ring buffer bridging the pipe-reading thread and the cpal callback
    // thread. i16 samples, interleaved stereo.
    let ring: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));

    // Opened by the audio callback once sound is genuinely reaching the
    // device — or by either audio thread giving up, which means no sound is
    // ever coming (a file with no audio track, a machine with no output
    // device) and the picture should not sit there waiting for it.
    let started = Arc::new(AtomicBool::new(false));

    {
        let ring = Arc::clone(&ring);
        let paused = Arc::clone(&paused);
        let stopped = Arc::clone(&stopped);
        let started = Arc::clone(&started);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            // Bytes of a half-read sample carried over from the last read: a
            // pipe read can end mid-sample, and silently dropping that byte
            // shifts every following sample by one and turns the rest of the
            // track into noise.
            let mut carry = 0usize;
            loop {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                // Backpressure, and the whole reason audio used to play only
                // its last few seconds: ffmpeg decodes far faster than real
                // time, so a reader that never waits races the pipe to EOF
                // and everything but the tail is gone before the sound card
                // asks for it. Waiting for room makes the cpal callback —
                // i.e. real time — set the pace instead.
                match ring.lock() {
                    Ok(guard) => {
                        if guard.len() >= capacity {
                            drop(guard);
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                    }
                    Err(_) => break,
                }
                match pcm_out.read(&mut chunk[carry..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let filled = carry + n;
                        let usable = filled - filled % 2;
                        let mut guard = match ring.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        for pair in chunk[..usable].chunks_exact(2) {
                            guard.push_back(i16::from_le_bytes([pair[0], pair[1]]));
                        }
                        drop(guard);
                        carry = filled - usable;
                        if carry == 1 {
                            chunk[0] = chunk[usable];
                        }
                    }
                }
            }
            // Nothing more is coming down this pipe, so release the video
            // thread even if not one sample ever played.
            started.store(true, Ordering::Relaxed);
            let _ = audio_child.kill();
        });
    }

    {
        let started = Arc::clone(&started);
        std::thread::spawn(move || {
            if let Err(e) = run_audio_output(
                ring,
                SAMPLE_RATE,
                CHANNELS,
                paused,
                stopped,
                volume,
                Arc::clone(&started),
            ) {
                eprintln!("player: audio output failed: {e}");
                started.store(true, Ordering::Relaxed);
            }
        });
    }

    Ok(started)
}

#[allow(clippy::too_many_arguments)]
fn run_audio_output(
    ring: Arc<Mutex<VecDeque<i16>>>,
    sample_rate: u32,
    channels: u16,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    volume: Volume,
    started: Arc<AtomicBool>,
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
            let gain = volume.get();
            let mut guard = match ring.lock() {
                Ok(g) => g,
                // Never hand the device an unwritten buffer: whatever was in
                // it last is not silence.
                Err(_) => {
                    data.fill(0);
                    return;
                }
            };
            // Rounded down to a whole frame: `data` is interleaved, so
            // handing back an odd number of samples on an underrun would
            // swap the channels for every callback after it.
            let frame = channels as usize;
            let available = guard.len().min(data.len()) / frame * frame;
            for (i, sample) in data.iter_mut().enumerate() {
                *sample = if i < available {
                    (guard.pop_front().unwrap_or(0) as f32 * gain) as i16
                } else {
                    0
                };
            }
            // Frame pacing hangs off this: the video thread holds its first
            // frame until sound is genuinely reaching the device.
            if available > 0 {
                started.store(true, Ordering::Relaxed);
            }
        },
        |err| eprintln!("player: audio stream error: {err}"),
        None,
    )?;
    stream.play()?;

    // Parked here for the run's whole lifetime: `stream` must not drop (and
    // this thread must not exit) while audio should keep playing.
    while !stopped.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Imported one by one rather than with a glob: `use super::*` would pull
    // in anything gpui re-exports, including its own `test` attribute macro,
    // which then shadows the built-in one (see the note in app.rs).
    use super::{parse_fraction, parse_probe};

    /// ffprobe reports frame rate as a fraction, and NTSC rates are the whole
    /// reason: 30000/1001 is not 30.
    #[test]
    fn frame_rate_parses_as_a_fraction() {
        assert_eq!(parse_fraction("30/1"), Some(30.0));
        assert_eq!(parse_fraction("25"), Some(25.0));
        assert!((parse_fraction("30000/1001").unwrap() - 29.97).abs() < 0.01);
        // 0/0 is what ffprobe reports for a stream with no meaningful rate.
        assert_eq!(parse_fraction("0/0"), None);
        assert_eq!(parse_fraction("nonsense"), None);
    }

    #[test]
    fn probe_reads_size_and_rate_off_the_video_stream() {
        let info = parse_probe(
            br#"{"streams":[
                  {"codec_type":"audio"},
                  {"codec_type":"video","width":1920,"height":1080,"r_frame_rate":"30/1"}
                ]}"#,
        )
        .unwrap();
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.fps, 30.0);
    }

    /// The seek bar is drawn against this, so which duration wins matters:
    /// the container's covers the whole file, a stream's need not.
    #[test]
    fn container_duration_wins_over_the_streams_own() {
        let info = parse_probe(
            br#"{"streams":[{"codec_type":"video","width":640,"height":360,"duration":"12.5"}],
                 "format":{"duration":"120.25"}}"#,
        )
        .unwrap();
        assert_eq!(info.duration, Some(120.25));
    }

    #[test]
    fn stream_duration_is_the_fallback_when_the_container_has_none() {
        let info = parse_probe(
            br#"{"streams":[{"codec_type":"video","width":640,"height":360,"duration":"12.5"}]}"#,
        )
        .unwrap();
        assert_eq!(info.duration, Some(12.5));

        // Present but unmeasured is the same as absent — this is the case the
        // fallback exists for, so it must not swallow the stream's answer.
        let na = parse_probe(
            br#"{"streams":[{"codec_type":"video","width":640,"height":360,"duration":"12.5"}],
                 "format":{"duration":"N/A"}}"#,
        )
        .unwrap();
        assert_eq!(na.duration, Some(12.5));
    }

    /// No duration is not a failure — playback still works, the seek bar just
    /// has nothing to scale against and renders disabled.
    #[test]
    fn a_source_with_no_duration_still_probes() {
        let info = parse_probe(
            br#"{"streams":[{"codec_type":"video","width":640,"height":360}],
                 "format":{"duration":"N/A"}}"#,
        )
        .unwrap();
        assert_eq!(info.duration, None);
        // A zero duration is the same as none: dividing by it is what the
        // seek bar would do next.
        let zero = parse_probe(
            br#"{"streams":[{"codec_type":"video","width":640,"height":360}],
                 "format":{"duration":"0.000000"}}"#,
        )
        .unwrap();
        assert_eq!(zero.duration, None);
    }

    #[test]
    fn a_source_with_no_video_stream_is_an_error() {
        assert!(parse_probe(br#"{"streams":[{"codec_type":"audio"}]}"#).is_err());
        // Missing dimensions can't size the raw-video pipe.
        assert!(parse_probe(br#"{"streams":[{"codec_type":"video"}]}"#).is_err());
    }

    /// Frame rate has a default because a missing one is recoverable;
    /// guessing wrong only paces playback slightly off.
    #[test]
    fn a_missing_frame_rate_falls_back_to_30() {
        let info =
            parse_probe(br#"{"streams":[{"codec_type":"video","width":8,"height":8}]}"#).unwrap();
        assert_eq!(info.fps, 30.0);
    }
}
