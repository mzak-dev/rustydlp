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
    pub options: crate::core::ytdlp::YtdlpOptions,
}

impl Preset {
    /// Seeds mirroring yt-dlp's own built-in aliases, so a fresh install is
    /// immediately usable without opening Settings.
    pub fn seeds(download_dir: &str) -> Vec<Preset> {
        use crate::core::ytdlp::{FormatMode, YtdlpOptions};
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

/// What kind of work a job represents. Download jobs probe/fetch a URL;
/// convert jobs re-encode an existing local file. Both share the same
/// state machine and progress plumbing, so they live in one table
/// distinguished only by this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Download,
    Convert,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Convert => "convert",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "convert" => Self::Convert,
            _ => Self::Download,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    /// Download: the source URL. Convert: the source file path.
    pub url: String,
    pub title: String,
    /// Download: the preset name. Convert: the target format key
    /// (see `ConvertFormat::as_str`).
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
            kind: JobKind::Download,
            url: url.into(),
            title: String::new(),
            preset: preset.into(),
            state: JobState::Queued,
            error: None,
            created_at: now_secs(),
            items: Vec::new(),
        }
    }

    pub fn new_convert(source_path: impl Into<String>, format: impl Into<String>) -> Self {
        Self {
            kind: JobKind::Convert,
            ..Self::new(source_path, format)
        }
    }
}

/// Finds the cover written by `--write-thumbnail` next to a media file.
/// png first: `--convert-thumbnails png` targets it, and it's a format
/// skia-safe's default codecs can actually decode. webp is checked last, as
/// a fallback for a run where the conversion didn't happen — this build
/// can't paint it, but at least the file is still findable.
pub fn sibling_thumbnail(media_path: &str) -> Option<String> {
    let path = std::path::Path::new(media_path);
    let stem = path.file_stem()?;
    let dir = path.parent()?;
    for ext in ["png", "jpg", "jpeg", "webp"] {
        let candidate = dir.join(stem).with_extension(ext);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::sibling_thumbnail;

    /// `after_move` reports only the video file, so the cover has to be located
    /// by stem. png must win over webp: this build can only decode the former.
    #[test]
    fn sibling_thumbnail_finds_the_cover_by_stem() {
        let dir = std::env::temp_dir().join(crate::core::model::new_id("thumb-test"));
        std::fs::create_dir_all(&dir).unwrap();

        let video = dir.join("Some Video [abc123].mp4");
        std::fs::write(&video, b"x").unwrap();
        let video_s = video.to_string_lossy().to_string();

        // No cover on disk yet.
        assert_eq!(sibling_thumbnail(&video_s), None);

        // A leftover .webp alone (conversion failed or was skipped) is still found.
        let webp = dir.join("Some Video [abc123].webp");
        std::fs::write(&webp, b"x").unwrap();
        assert_eq!(sibling_thumbnail(&video_s), Some(webp.to_string_lossy().to_string()));

        // With both present, the decodable png wins.
        let png = dir.join("Some Video [abc123].png");
        std::fs::write(&png, b"x").unwrap();
        assert_eq!(
            sibling_thumbnail(&video_s),
            Some(png.to_string_lossy().to_string())
        );

        // A different video in the same folder must not borrow this cover.
        let other = dir.join("Other.mp4");
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(sibling_thumbnail(&other.to_string_lossy()), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
