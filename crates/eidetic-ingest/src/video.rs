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
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
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

/// Produce a browser-playable MP4 copy of `src` at `dest` (goals-v0.5.md).
///
/// When the video codec is already browser-safe (`codec_is_safe`) only the
/// container is the problem (MKV/AVI), so the video stream is *remuxed*
/// (`-c:v copy`, near-instant). Otherwise it is re-encoded to H.264. Audio
/// is normalised to AAC either way — a remuxed MP4 with PCM audio would
/// play silently. `+faststart` moves the index up front so playback starts
/// before the file finishes downloading.
///
/// Known limit, accepted for now: HDR sources re-encode without tone
/// mapping, so HDR10 HEVC comes out washed. Fixing that needs a tonemap
/// filter chain and per-source colour probing.
///
/// Writes to a temp name in `dest`'s directory and renames, so a killed
/// transcode never leaves a half-written file where the server would find
/// it. `Ok(None)` when ffmpeg is not installed.
pub fn transcode_to_playback(src: &Path, dest: &Path, codec_is_safe: bool) -> Result<Option<()>> {
    let Some(ffmpeg) = ffmpeg() else {
        return Ok(None);
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = dest.with_extension("mp4.part");

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-v", "error"]).arg("-i").arg(src);
    if codec_is_safe {
        cmd.args(["-c:v", "copy"]);
    } else {
        cmd.args([
            "-c:v", "libx264", "-preset", "fast", "-crf", "23", "-pix_fmt", "yuv420p",
        ]);
    }
    // `-f mp4` because the temp name ends in `.part`, which ffmpeg can't
    // infer a muxer from.
    cmd.args([
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ]);
    let output = cmd.arg(&tmp).output().map_err(|source| Error::Io {
        path: src.to_path_buf(),
        source,
    })?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Transcode {
            path: src.to_path_buf(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    std::fs::rename(&tmp, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(Some(()))
}

/// Scene-change threshold for ffmpeg's `scene` score. 0.3 catches soft cuts
/// (matches PySceneDetect's default sensitivity); hard cuts score far above.
const SCENE_THRESHOLD: f64 = 0.3;

/// Detect scene-change timestamps with ffmpeg's scene filter.
///
/// This is a full decode of the video — the one expensive pass — so callers
/// persist the result (`video_scenes`) and every later consumer reads it
/// back. A single-shot video returns an empty Vec, which is a real answer,
/// not a failure. `Ok(None)` when ffmpeg is missing.
pub fn detect_scenes(path: &Path) -> Result<Option<Vec<f64>>> {
    let Some(ffmpeg) = ffmpeg() else {
        return Ok(None);
    };
    let output = Command::new(ffmpeg)
        .args(["-v", "info", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("select='gt(scene,{SCENE_THRESHOLD})',showinfo"),
            "-f",
            "null",
            "-",
        ])
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
    // showinfo logs one line per selected frame; pts_time is the timestamp.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut scenes: Vec<f64> = stderr
        .lines()
        .filter_map(|line| {
            let idx = line.find("pts_time:")?;
            line[idx + "pts_time:".len()..]
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect();
    scenes.sort_by(f64::total_cmp);
    scenes.dedup();
    Ok(Some(scenes))
}

/// One sample per scene, capped and duration-scaled (goals-v0.5.md #3).
///
/// Scene boundaries split `[0, duration)` into shots; each shot contributes
/// its midpoint. When there are more shots than the cap allows, the longest
/// shots win (length is the cheap proxy for "this is where the content is").
/// With no detected scenes — single-shot video, or detection unavailable —
/// falls back to the plain even sampler.
pub fn scene_sample_timestamps(duration_secs: Option<f64>, scenes: &[f64]) -> Vec<f64> {
    let Some(d) = duration_secs.filter(|d| *d > 0.0) else {
        return sample_timestamps(duration_secs);
    };
    if scenes.is_empty() {
        return sample_timestamps(duration_secs);
    }

    // Shot list: [0, s1), [s1, s2), ..., [sn, d). Degenerate slivers under
    // half a second are noise from rapid cuts and are dropped.
    let mut bounds = Vec::with_capacity(scenes.len() + 2);
    bounds.push(0.0);
    bounds.extend(scenes.iter().copied().filter(|s| *s > 0.0 && *s < d));
    bounds.push(d);
    let mut shots: Vec<(f64, f64)> = bounds
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|(a, b)| b - a >= 0.5)
        .collect();
    if shots.is_empty() {
        return sample_timestamps(duration_secs);
    }

    // Cap scales with duration: a 10s clip keeps ~5, a long video up to 24.
    let cap = ((d / 5.0).round() as usize).clamp(5, 24);
    if shots.len() > cap {
        shots.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
        shots.truncate(cap);
    }

    let mut ts: Vec<f64> = shots.iter().map(|(a, b)| (a + b) / 2.0).collect();
    ts.sort_by(f64::total_cmp);
    ts
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
        assert_eq!(
            parse_iso6709("-33.8688+151.2093/"),
            Some((-33.8688, 151.2093))
        );
        assert_eq!(
            parse_iso6709("+28.6139-077.2090/"),
            Some((28.6139, -77.2090))
        );
    }

    #[test]
    fn iso6709_rejects_garbage() {
        assert_eq!(parse_iso6709(""), None);
        assert_eq!(parse_iso6709("28.6"), None);
        assert_eq!(
            parse_iso6709("+99.0+200.0/"),
            None,
            "out-of-range coordinates"
        );
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
        assert_eq!(
            p.date_taken.unwrap().to_rfc3339(),
            "2026-01-02T03:04:05+00:00"
        );
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
        assert_eq!(
            (p.width, p.height),
            (Some(1080), Some(1920)),
            "portrait after rotation"
        );
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
        assert_eq!(
            thumbnail_ts(Some(1.0)),
            0.5,
            "0.5s floor still inside a 1s clip"
        );
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
        assert!(
            (ts[0] - 1.0).abs() < 1e-9 && (ts[4] - 9.0).abs() < 1e-9,
            "middle 80%: {ts:?}"
        );
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
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=64x48:rate=10",
            ])
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

        let frame = extract_frame(&clip, 1.0)
            .expect("extract")
            .expect("ffmpeg present");
        assert_eq!((frame.width(), frame.height()), (64, 48));

        // A corrupt file must error, not panic or hang.
        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        assert!(probe(&bad).is_err());
        assert!(extract_frame(&bad, 0.0).is_err());
    }

    #[test]
    fn scene_sampling_takes_shot_midpoints() {
        // 10s video, cuts at 4 and 7: shots [0,4) [4,7) [7,10) -> midpoints.
        let ts = scene_sample_timestamps(Some(10.0), &[4.0, 7.0]);
        assert_eq!(ts, vec![2.0, 5.5, 8.5]);
    }

    #[test]
    fn scene_sampling_falls_back_without_scenes() {
        assert_eq!(
            scene_sample_timestamps(Some(10.0), &[]),
            sample_timestamps(Some(10.0)),
            "single-shot video uses the even sampler"
        );
        assert_eq!(scene_sample_timestamps(None, &[1.0]), vec![0.0]);
    }

    #[test]
    fn scene_sampling_caps_by_longest_shots() {
        // 20 rapid cuts every 0.6s in a 12s video -> cap is max(5, 12/5)=5;
        // survivors are shots, all >= 0.5s, count capped.
        let scenes: Vec<f64> = (1..20).map(|i| i as f64 * 0.6).collect();
        let ts = scene_sample_timestamps(Some(12.0), &scenes);
        assert!(ts.len() <= 5, "capped: {ts:?}");
        assert!(ts.windows(2).all(|w| w[0] < w[1]), "sorted: {ts:?}");
    }

    #[test]
    fn scene_sampling_drops_sub_half_second_slivers() {
        // Cut at 0.2s: the [0, 0.2) sliver is noise; the rest samples fine.
        let ts = scene_sample_timestamps(Some(10.0), &[0.2]);
        assert_eq!(ts, vec![5.1], "midpoint of [0.2, 10)");
    }

    /// Playback transcode paths: an unsafe codec re-encodes to H.264; a safe
    /// codec in an unsafe container remuxes (stream copy). Skips where
    /// ffmpeg is missing; CI installs it.
    #[test]
    fn transcode_reencodes_hevc_and_remuxes_mkv() {
        let Some(ffmpeg_bin) = ffmpeg() else {
            eprintln!("skipping: ffmpeg not installed");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");

        // HEVC source (skip quietly if this ffmpeg lacks libx265).
        let hevc = dir.path().join("clip_hevc.mp4");
        let made_hevc = Command::new(ffmpeg_bin)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=64x48:rate=10",
            ])
            .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p"])
            .arg(&hevc)
            .status()
            .expect("run ffmpeg")
            .success();
        if made_hevc {
            let out = dir.path().join("play_hevc.mp4");
            transcode_to_playback(&hevc, &out, false)
                .expect("transcode")
                .expect("ffmpeg present");
            let p = probe(&out).expect("probe output").expect("ffprobe present");
            assert_eq!(
                p.video_codec.as_deref(),
                Some("h264"),
                "re-encoded to H.264"
            );
        } else {
            eprintln!("skipping HEVC half: no libx265 in this ffmpeg");
        }

        // H.264 in MKV: container is the only problem, so remux.
        let h264 = dir.path().join("clip.mp4");
        assert!(
            Command::new(ffmpeg_bin)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=1:size=64x48:rate=10"
                ])
                .args(["-pix_fmt", "yuv420p"])
                .arg(&h264)
                .status()
                .expect("run ffmpeg")
                .success()
        );
        let mkv = dir.path().join("clip.mkv");
        assert!(
            Command::new(ffmpeg_bin)
                .args(["-v", "error"])
                .arg("-i")
                .arg(&h264)
                .args(["-c", "copy"])
                .arg(&mkv)
                .status()
                .expect("run ffmpeg")
                .success()
        );
        let out = dir.path().join("play_mkv.mp4");
        transcode_to_playback(&mkv, &out, true)
            .expect("remux")
            .expect("ffmpeg present");
        let p = probe(&out).expect("probe output").expect("ffprobe present");
        assert_eq!(p.video_codec.as_deref(), Some("h264"), "stream-copied");

        // No half-written .part file survives either path.
        assert!(!dir.path().join("play_mkv.mp4.part").exists());
    }
}
