//! Everything that turns intent into a yt-dlp argv, and yt-dlp's stdout back
//! into structured events.
//!
//! ponytail: args + events live in one file until process spawning lands; split
//! when it does.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Options -> argv
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatMode {
    /// Best video + best audio, merged.
    BestVideoAudio,
    /// Strip to audio and transcode to `codec` (mp3, m4a, opus, ...).
    AudioOnly { codec: String },
    /// Raw `-f` selector, for people who know yt-dlp's format language.
    Custom(String),
}

impl Default for FormatMode {
    fn default() -> Self {
        Self::BestVideoAudio
    }
}

/// The curated slice of yt-dlp's surface that gets real widgets, plus
/// `extra_args` — which is what makes "configures all options" true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YtdlpOptions {
    pub format: FormatMode,
    /// Cap on vertical resolution, e.g. 1080.
    pub max_height: Option<u32>,
    /// `--merge-output-format`, e.g. "mp4".
    pub container: Option<String>,
    pub output_template: String,
    pub download_dir: String,
    /// Empty means don't fetch subtitles at all.
    pub subtitle_langs: Vec<String>,
    pub embed_subs: bool,
    pub embed_thumbnail: bool,
    pub embed_metadata: bool,
    /// e.g. "sponsor,selfpromo"
    pub sponsorblock_remove: Option<String>,
    /// `-r`, e.g. "2M".
    pub rate_limit: Option<String>,
    pub concurrent_fragments: u8,
    /// `--cookies-from-browser`, e.g. "firefox".
    pub cookies_from_browser: Option<String>,
    /// Skip anything already recorded in the archive file.
    pub download_archive: bool,
    pub retries: u8,
    pub extra_args: Vec<String>,
}

impl Default for YtdlpOptions {
    fn default() -> Self {
        Self {
            format: FormatMode::default(),
            max_height: None,
            container: None,
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            download_dir: String::new(),
            subtitle_langs: Vec::new(),
            embed_subs: false,
            embed_thumbnail: false,
            embed_metadata: false,
            sponsorblock_remove: None,
            rate_limit: None,
            concurrent_fragments: 1,
            cookies_from_browser: None,
            download_archive: false,
            retries: 10,
            extra_args: Vec::new(),
        }
    }
}

fn s(v: impl Into<String>) -> String {
    v.into()
}

impl YtdlpOptions {
    /// The single place options become CLI arguments.
    ///
    /// Bugs here are invisible: a wrong argv still downloads *something*, just
    /// not what was asked for. Hence the golden test below.
    pub fn to_args(&self) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();

        match &self.format {
            FormatMode::Custom(sel) => {
                a.push(s("-f"));
                a.push(sel.clone());
            }
            FormatMode::AudioOnly { codec } => {
                a.push(s("-f"));
                a.push(s("ba/b"));
                a.push(s("-x"));
                a.push(s("--audio-format"));
                a.push(codec.clone());
            }
            FormatMode::BestVideoAudio => {
                a.push(s("-f"));
                // Fall back progressively: capped merge -> capped muxed ->
                // uncapped, so an unusual site never yields "no formats".
                a.push(match self.max_height {
                    Some(h) => format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b"),
                    None => s("bv*+ba/b"),
                });
            }
        }

        if let Some(c) = &self.container {
            a.push(s("--merge-output-format"));
            a.push(c.clone());
        }

        a.push(s("-o"));
        a.push(self.output_template.clone());

        if !self.download_dir.is_empty() {
            a.push(s("-P"));
            a.push(self.download_dir.clone());
        }

        if !self.subtitle_langs.is_empty() {
            a.push(s("--write-subs"));
            a.push(s("--sub-langs"));
            a.push(self.subtitle_langs.join(","));
            if self.embed_subs {
                a.push(s("--embed-subs"));
            }
        }

        if self.embed_thumbnail {
            a.push(s("--embed-thumbnail"));
        }
        if self.embed_metadata {
            a.push(s("--embed-metadata"));
        }

        if let Some(cats) = &self.sponsorblock_remove {
            a.push(s("--sponsorblock-remove"));
            a.push(cats.clone());
        }
        if let Some(r) = &self.rate_limit {
            a.push(s("-r"));
            a.push(r.clone());
        }
        if self.concurrent_fragments > 1 {
            a.push(s("-N"));
            a.push(self.concurrent_fragments.to_string());
        }
        if let Some(b) = &self.cookies_from_browser {
            a.push(s("--cookies-from-browser"));
            a.push(b.clone());
        }
        if self.download_archive && !self.download_dir.is_empty() {
            a.push(s("--download-archive"));
            a.push(format!("{}/archive.txt", self.download_dir));
        }
        a.push(s("--retries"));
        a.push(self.retries.to_string());

        a.extend(self.extra_args.iter().cloned());
        a
    }
}

/// Flags applied to *every* invocation, preset or not.
///
/// `--ignore-config` matters more than it looks: without it a user's global
/// yt-dlp config silently overrides the preset, producing behaviour we cannot
/// reproduce from the app's own state.
pub fn base_args() -> Vec<String> {
    vec![
        s("--ignore-config"),
        s("--no-color"),
        // Windows consoles default to cp1252, which mangles non-ASCII titles.
        s("--encoding"),
        s("utf-8"),
    ]
}

/// Full argv for a download run.
pub fn download_args(url: &str, opts: &YtdlpOptions) -> Vec<String> {
    let mut a = base_args();
    a.extend([
        // `--print` implies BOTH --quiet and --simulate. Without these two
        // counter-flags yt-dlp downloads nothing and reports no progress, while
        // still exiting 0 and printing a plausible filepath — a green run that
        // produced no file. Do not remove.
        s("--no-simulate"),
        s("--progress"),
        s("--newline"),
        s("--progress-template"),
        format!("download:{PROGRESS_PREFIX} %(progress)j"),
        s("--print"),
        format!(
            "video:{INFO_PREFIX} %(.{{id,title,duration,thumbnail,webpage_url,playlist_index}})j"
        ),
        s("--print"),
        format!("after_move:{FILE_PREFIX} %(filepath)s"),
        // Sidecar metadata keeps the library rebuildable if the DB is lost.
        s("--write-info-json"),
        // Cover art for the grid. Left in yt-dlp's native webp: zed enables the
        // `webp` feature on the image crate, so gpui decodes it directly and we
        // avoid an ffmpeg conversion per video.
        s("--write-thumbnail"),
    ]);
    a.extend(opts.to_args());
    a.push(s(url));
    a
}

/// Full argv for the cheap metadata probe run when a URL is pasted.
/// `--flat-playlist` keeps a 500-entry playlist to roughly one request.
pub fn probe_args(url: &str) -> Vec<String> {
    let mut a = base_args();
    a.extend([
        s("-J"),
        s("--flat-playlist"),
        s("--no-warnings"),
        s("--no-progress"),
        s(url),
    ]);
    a
}

// ---------------------------------------------------------------------------
// stdout -> events
// ---------------------------------------------------------------------------

pub const PROGRESS_PREFIX: &str = "RDLP_PROG";
pub const INFO_PREFIX: &str = "RDLP_INFO";
pub const FILE_PREFIX: &str = "RDLP_FILE";

/// One `%(progress)j` object.
///
/// Every field is Option on purpose: yt-dlp omits keys depending on the
/// downloader in use, and a schema change must surface as `None` (or a logged
/// deserialize error) rather than a progress bar wedged at 0%.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Progress {
    pub status: Option<String>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub total_bytes_estimate: Option<f64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub filename: Option<String>,
    pub fragment_index: Option<u64>,
    pub fragment_count: Option<u64>,
}

impl Progress {
    /// 0.0..=1.0, preferring the exact total and falling back to the estimate.
    /// `None` when the size is genuinely unknown (live streams, some HLS).
    pub fn fraction(&self) -> Option<f32> {
        let done = self.downloaded_bytes? as f64;
        let total = match (self.total_bytes, self.total_bytes_estimate) {
            (Some(t), _) if t > 0 => t as f64,
            (_, Some(e)) if e > 0.0 => e,
            _ => return None,
        };
        Some((done / total).clamp(0.0, 1.0) as f32)
    }

    pub fn is_finished(&self) -> bool {
        self.status.as_deref() == Some("finished")
    }
}

/// Metadata emitted once per video via `--print video:`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InfoLine {
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub webpage_url: Option<String>,
    pub playlist_index: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Progress(Progress),
    Info(InfoLine),
    /// A finished output file, post-move.
    File(String),
    /// Anything unrecognised — kept for the failure log tail.
    Log(String),
}

/// Classifies one line of yt-dlp stdout.
///
/// Unparseable JSON on a known prefix degrades to `Log` rather than erroring:
/// a malformed progress line should never abort a download that is working.
pub fn parse_line(line: &str) -> Event {
    let line = line.trim_end_matches(['\r', '\n']);

    if let Some(rest) = line.strip_prefix(PROGRESS_PREFIX) {
        return match serde_json::from_str::<Progress>(rest.trim()) {
            Ok(p) => Event::Progress(p),
            Err(e) => Event::Log(format!("unparsed progress ({e}): {rest}")),
        };
    }
    if let Some(rest) = line.strip_prefix(INFO_PREFIX) {
        return match serde_json::from_str::<InfoLine>(rest.trim()) {
            Ok(i) => Event::Info(i),
            Err(e) => Event::Log(format!("unparsed info ({e}): {rest}")),
        };
    }
    if let Some(rest) = line.strip_prefix(FILE_PREFIX) {
        return Event::File(rest.trim().to_string());
    }
    Event::Log(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_argv_for_a_typical_1080p_preset() {
        let opts = YtdlpOptions {
            format: FormatMode::BestVideoAudio,
            max_height: Some(1080),
            container: Some("mp4".into()),
            output_template: "%(title)s.%(ext)s".into(),
            download_dir: "D:/Videos".into(),
            subtitle_langs: vec!["en".into(), "pl".into()],
            embed_subs: true,
            embed_thumbnail: true,
            embed_metadata: true,
            sponsorblock_remove: Some("sponsor".into()),
            rate_limit: Some("2M".into()),
            concurrent_fragments: 4,
            cookies_from_browser: Some("firefox".into()),
            download_archive: true,
            retries: 10,
            extra_args: vec!["--no-mtime".into()],
        };

        assert_eq!(
            opts.to_args(),
            vec![
                "-f",
                "bv*[height<=1080]+ba/b[height<=1080]/bv*+ba/b",
                "--merge-output-format",
                "mp4",
                "-o",
                "%(title)s.%(ext)s",
                "-P",
                "D:/Videos",
                "--write-subs",
                "--sub-langs",
                "en,pl",
                "--embed-subs",
                "--embed-thumbnail",
                "--embed-metadata",
                "--sponsorblock-remove",
                "sponsor",
                "-r",
                "2M",
                "-N",
                "4",
                "--cookies-from-browser",
                "firefox",
                "--download-archive",
                "D:/Videos/archive.txt",
                "--retries",
                "10",
                "--no-mtime",
            ]
        );
    }

    #[test]
    fn audio_only_preset_extracts_and_transcodes() {
        let opts = YtdlpOptions {
            format: FormatMode::AudioOnly {
                codec: "mp3".into(),
            },
            output_template: "%(title)s.%(ext)s".into(),
            ..Default::default()
        };
        let a = opts.to_args();
        assert!(a.windows(2).any(|w| w == ["-x", "--audio-format"]));
        assert!(a.contains(&"mp3".to_string()));
        // A max_height left set must not leak into an audio-only run.
        assert!(!a.iter().any(|x| x.contains("height")));
    }

    #[test]
    fn defaults_emit_no_optional_flags() {
        let a = YtdlpOptions::default().to_args();
        for flag in [
            "--merge-output-format",
            "--write-subs",
            "--embed-thumbnail",
            "--embed-metadata",
            "--sponsorblock-remove",
            "-r",
            "-N",
            "--cookies-from-browser",
            "--download-archive",
            "-P",
        ] {
            assert!(!a.contains(&flag.to_string()), "{flag} leaked into defaults");
        }
    }

    #[test]
    fn every_run_ignores_the_users_global_config() {
        // Without this a stray ~/.config/yt-dlp/config silently overrides the
        // preset and the resulting bug is unreproducible from app state.
        let a = download_args("https://x/y", &YtdlpOptions::default());
        assert!(a.contains(&"--ignore-config".to_string()));
        assert!(a.contains(&"--encoding".to_string()));
        assert_eq!(a.last().unwrap(), "https://x/y");
    }

    /// Regression guard. `--print` implies --quiet and --simulate, so dropping
    /// either counter-flag silently turns every download into a no-op that
    /// still exits 0. Cost us one green-but-empty test run already.
    #[test]
    fn print_flags_are_neutralised_so_downloads_actually_happen() {
        let a = download_args("https://x/y", &YtdlpOptions::default());
        assert!(a.contains(&"--print".to_string()), "test premise changed");
        assert!(
            a.contains(&"--no-simulate".to_string()),
            "--print implies --simulate; downloads would be skipped"
        );
        assert!(
            a.contains(&"--progress".to_string()),
            "--print implies --quiet; progress would be suppressed"
        );
    }

    #[test]
    fn probe_is_flat_and_json() {
        let a = probe_args("https://x/list");
        assert!(a.contains(&"-J".to_string()));
        assert!(a.contains(&"--flat-playlist".to_string()));
    }

    #[test]
    fn parses_a_real_progress_line() {
        let line = r#"RDLP_PROG {"status": "downloading", "downloaded_bytes": 5242880, "total_bytes": 20971520, "speed": 3145728.0, "eta": 5, "filename": "video.mp4", "fragment_index": null}"#;
        match parse_line(line) {
            Event::Progress(p) => {
                assert_eq!(p.downloaded_bytes, Some(5_242_880));
                assert_eq!(p.fraction(), Some(0.25));
                assert!(!p.is_finished());
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn progress_without_a_known_size_has_no_fraction() {
        // Live streams report downloaded bytes but no total — must not render 100%.
        let line = r#"RDLP_PROG {"status":"downloading","downloaded_bytes":1024}"#;
        match parse_line(line) {
            Event::Progress(p) => assert_eq!(p.fraction(), None),
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn estimate_is_used_when_exact_total_is_absent() {
        let line = r#"RDLP_PROG {"downloaded_bytes":50,"total_bytes_estimate":200.0}"#;
        match parse_line(line) {
            Event::Progress(p) => assert_eq!(p.fraction(), Some(0.25)),
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn parses_info_and_file_lines() {
        let info = r#"RDLP_INFO {"id":"abc","title":"Zażółć gęślą","duration":212.5,"playlist_index":3}"#;
        match parse_line(info) {
            Event::Info(i) => {
                assert_eq!(i.title.as_deref(), Some("Zażółć gęślą"));
                assert_eq!(i.playlist_index, Some(3));
            }
            other => panic!("expected Info, got {other:?}"),
        }

        match parse_line(r"RDLP_FILE C:\dl\Zażółć.mp4") {
            Event::File(p) => assert_eq!(p, r"C:\dl\Zażółć.mp4"),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_degrades_to_log_not_panic() {
        // A yt-dlp schema change must not abort a download that is otherwise fine.
        match parse_line("RDLP_PROG {not json") {
            Event::Log(m) => assert!(m.contains("unparsed progress")),
            other => panic!("expected Log, got {other:?}"),
        }
        match parse_line("[download] Destination: foo.mp4") {
            Event::Log(m) => assert!(m.starts_with("[download]")),
            other => panic!("expected Log, got {other:?}"),
        }
    }
}
