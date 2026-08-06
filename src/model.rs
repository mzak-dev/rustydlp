use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic-ish, sortable, dependency-free id.
/// ponytail: millis + counter instead of a ulid/uuid crate. Collides only if the
/// clock jumps backwards more than the counter advances; swap in `ulid` if ids
/// ever need to be globally unique across machines.
pub fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{ms:013}{n:04}")
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A named bundle of yt-dlp options, chosen in the New Download modal.
///
/// Presets *are* the yt-dlp configuration — there is no separate global
/// options layer, so what a download did is always explainable from the preset
/// it names plus any per-run override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub is_default: bool,
    pub options: crate::ytdlp::YtdlpOptions,
}

impl Preset {
    /// Seeds mirroring yt-dlp's own built-in aliases, so a fresh install is
    /// immediately usable without opening Settings.
    pub fn seeds(download_dir: &str) -> Vec<Preset> {
        use crate::ytdlp::{FormatMode, YtdlpOptions};
        let base = YtdlpOptions {
            download_dir: download_dir.to_string(),
            embed_thumbnail: true,
            embed_metadata: true,
            ..Default::default()
        };
        vec![
            Preset {
                name: "Best (1080p mp4)".into(),
                is_default: true,
                options: YtdlpOptions {
                    max_height: Some(1080),
                    container: Some("mp4".into()),
                    ..base.clone()
                },
            },
            Preset {
                name: "Best available".into(),
                is_default: false,
                options: base.clone(),
            },
            Preset {
                name: "MP3 audio".into(),
                is_default: false,
                options: YtdlpOptions {
                    format: FormatMode::AudioOnly {
                        codec: "mp3".into(),
                    },
                    ..base.clone()
                },
            },
            Preset {
                name: "M4A audio".into(),
                is_default: false,
                options: YtdlpOptions {
                    format: FormatMode::AudioOnly {
                        codec: "m4a".into(),
                    },
                    ..base
                },
            },
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Probing,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Probing => "probing",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "probing" => Self::Probing,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            // Unknown state means a newer version wrote this row; treat as
            // terminal rather than silently resuming something we don't grasp.
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Video,
    Audio,
    Subtitle,
    Thumbnail,
    Info,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Subtitle => "subtitle",
            Self::Thumbnail => "thumbnail",
            Self::Info => "info",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "audio" => Self::Audio,
            "subtitle" => Self::Subtitle,
            "thumbnail" => Self::Thumbnail,
            "info" => Self::Info,
            _ => Self::Video,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub id: String,
    pub path: String,
    pub kind: FileKind,
    pub format_id: Option<String>,
    pub bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub index: i64,
    pub title: String,
    pub duration: Option<f64>,
    pub thumb_path: Option<String>,
    pub webpage_url: String,
    pub files: Vec<File>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub url: String,
    pub title: String,
    pub preset: String,
    pub state: JobState,
    pub error: Option<String>,
    pub created_at: i64,
    pub items: Vec<Item>,
}

impl Job {
    pub fn new(url: impl Into<String>, preset: impl Into<String>) -> Self {
        Self {
            id: new_id("job"),
            url: url.into(),
            title: String::new(),
            preset: preset.into(),
            state: JobState::Queued,
            error: None,
            created_at: now_secs(),
            items: Vec::new(),
        }
    }
}
