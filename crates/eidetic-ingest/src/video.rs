//! Video probing and frame extraction by shelling out to ffmpeg (ADR-0011).
//!
//! ffmpeg/ffprobe are *runtime-optional*: when the binaries are missing,
//! [`ffmpeg`]/[`ffprobe`] return `None` and callers skip video work with a
//! warning instead of failing. No Cargo feature — the build never changes.
//!
//! Everything here shapes three features at once: the probed duration and
//! dimensions land on the asset row, extracted frames feed both thumbnails
//! and per-frame SigLIP embeddings, and the frame timestamps are the cut
//! points reel generation uses later.

use crate::{Error, Result};
use chrono::{DateTime, Utc};
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Everything worth keeping from one `ffprobe` run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoProbe {
    pub duration_secs: Option<f64>,
    pub video_codec: Option<String>,
    /// Display dimensions: coded width/height with any 90-degree display
    /// rotation already applied, so portrait phone videos report portrait.
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// From the container's `creation_time` tag — the video analogue of
    /// EXIF DateTimeOriginal.
    pub date_taken: Option<DateTime<Utc>>,
    /// From the QuickTime ISO 6709 location tag iPhones write.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// Locate a binary: `$env_var` override first, then trust `PATH`.
///
/// The PATH probe actually executes `<name> -version` once — a name that
/// resolves but can't run (broken install) is treated as absent.
fn resolve(env_var: &str, name: &str) -> Option<PathBuf> {
    if let Ok(v) = std::env::var(env_var) {
        let p = PathBuf::from(v);
        return p.exists().then_some(p);
    }
    let ok = Command::new(name)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from(name))
}

/// Path to ffprobe, or `None` if unavailable. Resolved once per process.
pub fn ffprobe() -> Option<&'static Path> {
    static FFPROBE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FFPROBE
        .get_or_init(|| resolve("EIDETIC_FFPROBE_PATH", "ffprobe"))
        .as_deref()
}

/// Path to ffmpeg, or `None` if unavailable. Resolved once per process.
pub fn ffmpeg() -> Option<&'static Path> {
    static FFMPEG: OnceLock<Option<PathBuf>> = OnceLock::new();
    FFMPEG
        .get_or_init(|| resolve("EIDETIC_FFMPEG_PATH", "ffmpeg"))
        .as_deref()
}

/// Probe a video file. `Ok(None)` when ffprobe is not installed; `Err` only
/// when ffprobe ran and failed (corrupt file, not a video, ...).
pub fn probe(path: &Path) -> Result<Option<VideoProbe>> {
    let Some(ffprobe) = ffprobe() else {
        return Ok(None);
    };
    let output = Command::new(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::VideoProbe {
            path: path.to_path_buf(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| Error::VideoProbe {
            path: path.to_path_buf(),
            detail: format!("unparseable ffprobe JSON: {e}"),
        })?;
    Ok(Some(parse_probe(&json)))
}

/// Pull the fields we keep out of ffprobe's JSON. Split from [`probe`] so it
/// is testable without running ffprobe.
fn parse_probe(json: &serde_json::Value) -> VideoProbe {
    let format = &json["format"];
    let tags = &format["tags"];

    let duration_secs = format["duration"].as_str().and_then(|s| s.parse().ok());

    // First stream whose codec_type is "video" — audio-only containers have
    // none, and then codec/width/height stay None.
    let video_stream = json["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["codec_type"].as_str() == Some("video"));

    let (video_codec, width, height) = match video_stream {
        Some(s) => {
            let rotated = stream_rotation(s) % 180 != 0;
            let (w, h) = (s["width"].as_i64(), s["height"].as_i64());
            (
                s["codec_name"].as_str().map(str::to_string),
                if rotated { h } else { w },
                if rotated { w } else { h },
            )
        }
        None => (None, None, None),
    };

    let date_taken = tags["creation_time"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let (latitude, longitude) = tags["com.apple.quicktime.location.ISO6709"]
        .as_str()
        .or_else(|| tags["location"].as_str())
        .and_then(parse_iso6709)
        .map_or((None, None), |(la, lo)| (Some(la), Some(lo)));

    VideoProbe {
        duration_secs,
        video_codec,
        width,
        height,
        date_taken,
        latitude,
        longitude,
    }
}

/// A stream's display rotation in degrees, from the displaymatrix side data.
/// 0 when absent. ffprobe reports e.g. -90 for typical portrait phone video.
fn stream_rotation(stream: &serde_json::Value) -> i64 {
    stream["side_data_list"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|sd| sd["rotation"].as_i64())
        .unwrap_or(0)
        .rem_euclid(360)
}

/// Parse an ISO 6709 point string like `+28.6139+077.2090+216.000/` into
/// `(lat, lon)`. Altitude and the trailing CRS are ignored.
fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim_end_matches('/');
    // Latitude and longitude each start with a mandatory sign; split on the
    // sign positions after the first character.
    let signs: Vec<usize> = s
        .char_indices()
        .skip(1)
        .filter(|(_, c)| *c == '+' || *c == '-')
        .map(|(i, _)| i)
        .collect();
    let lon_start = *signs.first()?;
    let lon_end = signs.get(1).copied().unwrap_or(s.len());
    let lat: f64 = s[..lon_start].parse().ok()?;
    let lon: f64 = s[lon_start..lon_end].parse().ok()?;
    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
}

/// Decode one frame at `ts_secs` into a [`DynamicImage`].
///
/// ffmpeg applies the display rotation during decode, so the frame comes out
/// upright — the same convention `load_oriented_image` gives photos.
/// `Ok(None)` when ffmpeg is not installed.
pub fn extract_frame(path: &Path, ts_secs: f64) -> Result<Option<DynamicImage>> {
    let Some(ffmpeg) = ffmpeg() else {
        return Ok(None);
    };
    let output = Command::new(ffmpeg)
        .args(["-v", "error", "-ss", &format!("{ts_secs:.3}")])
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::FrameExtract {
            path: path.to_path_buf(),
            ts_secs,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let img = image::load_from_memory(&output.stdout).map_err(|source| Error::ImageDecode {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(img))
}

/// The timestamp the single representative thumbnail frame is taken at:
/// 10% in, but at least half a second (skipping fade-from-black openings),
/// and never past the end.
pub fn thumbnail_ts(duration_secs: Option<f64>) -> f64 {
    match duration_secs {
        Some(d) if d > 0.0 => (d * 0.1).max(0.5).min(d * 0.9),
        _ => 0.0,
    }
}

/// Evenly spaced sample points across the middle 80% of the video, for frame
/// embeddings. Sample count scales with duration (one per ~2s) and is capped
/// at 5 (ADR-0011); a zero/unknown duration falls back to a single t=0 frame.
pub fn sample_timestamps(duration_secs: Option<f64>) -> Vec<f64> {
    let Some(d) = duration_secs.filter(|d| *d > 0.0) else {
        return vec![0.0];
    };
    let count = ((d / 2.0).round() as usize).clamp(1, 5);
    if count == 1 {
        return vec![d * 0.5];
    }
    (0..count)
        .map(|i| d * (0.1 + 0.8 * i as f64 / (count - 1) as f64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709_parses_iphone_location_tag() {
        assert_eq!(
            parse_iso6709("+28.6139+077.2090+216.000/"),
            Some((28.6139, 77.2090))
        );
        assert_eq!(parse_iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(parse_iso6709("+28.6139-077.2090/"), Some((28.6139, -77.2090)));
    }

    #[test]
    fn iso6709_rejects_garbage() {
        assert_eq!(parse_iso6709(""), None);
        assert_eq!(parse_iso6709("28.6"), None);
        assert_eq!(parse_iso6709("+99.0+200.0/"), None, "out-of-range coordinates");
    }

    #[test]
    fn probe_parse_reads_the_video_stream_not_the_audio() {
        let json: serde_json::Value = serde_json::json!({
            "streams": [
                {"codec_type": "audio", "codec_name": "aac"},
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080}
            ],
            "format": {"duration": "12.500000", "tags": {"creation_time": "2026-01-02T03:04:05.000000Z"}}
        });
        let p = parse_probe(&json);
        assert_eq!(p.video_codec.as_deref(), Some("h264"));
        assert_eq!((p.width, p.height), (Some(1920), Some(1080)));
        assert_eq!(p.duration_secs, Some(12.5));
        assert_eq!(p.date_taken.unwrap().to_rfc3339(), "2026-01-02T03:04:05+00:00");
    }

    #[test]
    fn probe_parse_swaps_dimensions_for_rotated_video() {
        let json: serde_json::Value = serde_json::json!({
            "streams": [{
                "codec_type": "video", "codec_name": "hevc",
                "width": 1920, "height": 1080,
                "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]
            }],
            "format": {"duration": "3.0"}
        });
        let p = parse_probe(&json);
        assert_eq!((p.width, p.height), (Some(1080), Some(1920)), "portrait after rotation");
    }

    #[test]
    fn probe_parse_handles_audio_only_container() {
        let json: serde_json::Value = serde_json::json!({
            "streams": [{"codec_type": "audio", "codec_name": "aac"}],
            "format": {"duration": "3.0"}
        });
        let p = parse_probe(&json);
        assert_eq!(p.video_codec, None);
        assert_eq!((p.width, p.height), (None, None));
        assert_eq!(p.duration_secs, Some(3.0));
    }

    #[test]
    fn thumbnail_ts_skips_the_opening_but_stays_in_bounds() {
        assert_eq!(thumbnail_ts(Some(100.0)), 10.0);
        assert_eq!(thumbnail_ts(Some(1.0)), 0.5, "0.5s floor still inside a 1s clip");
        assert_eq!(thumbnail_ts(Some(10.0)), 1.0);
        assert_eq!(thumbnail_ts(None), 0.0);
    }

    #[test]
    fn sample_timestamps_scale_with_duration_and_cap_at_five() {
        assert_eq!(sample_timestamps(Some(1.0)), vec![0.5]);
        assert_eq!(sample_timestamps(Some(4.0)).len(), 2);
        assert_eq!(sample_timestamps(Some(60.0)).len(), 5);
        let ts = sample_timestamps(Some(10.0));
        assert_eq!(ts.len(), 5);
        assert!((ts[0] - 1.0).abs() < 1e-9 && (ts[4] - 9.0).abs() < 1e-9, "middle 80%: {ts:?}");
        assert_eq!(sample_timestamps(None), vec![0.0]);
    }

    /// Real-ffmpeg round trip on a synthesised clip. Skips (with a note)
    /// where ffmpeg isn't installed; CI installs it so the skip never hides
    /// a regression there.
    #[test]
    fn probe_and_extract_roundtrip_on_a_generated_clip() {
        let (Some(_), Some(ffmpeg_bin)) = (ffprobe(), ffmpeg()) else {
            eprintln!("skipping: ffmpeg/ffprobe not installed");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let clip = dir.path().join("test.mp4");
        let status = Command::new(ffmpeg_bin)
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=64x48:rate=10"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("run ffmpeg");
        assert!(status.success(), "ffmpeg failed to synthesise the clip");

        let p = probe(&clip).expect("probe").expect("ffprobe present");
        assert_eq!(p.video_codec.as_deref(), Some("h264"));
        assert_eq!((p.width, p.height), (Some(64), Some(48)));
        let d = p.duration_secs.expect("duration");
        assert!((d - 2.0).abs() < 0.2, "duration ~2s, got {d}");

        let frame = extract_frame(&clip, 1.0).expect("extract").expect("ffmpeg present");
        assert_eq!((frame.width(), frame.height()), (64, 48));

        // A corrupt file must error, not panic or hang.
        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        assert!(probe(&bad).is_err());
        assert!(extract_frame(&bad, 0.0).is_err());
    }
}
