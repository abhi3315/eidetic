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

use crate::subprocess::{self, Budget};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use image::DynamicImage;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

// Deadlines for every ffmpeg/ffprobe run here. They exist to catch wedged
// decoders and stalled disks, not as performance targets, so they sit far
// past plausible real speeds. Budgets scale with the media's duration.
const VERSION_DEADLINE: Duration = Duration::from_secs(30);
const PROBE_DEADLINE: Duration = Duration::from_secs(30);
const FRAME_DEADLINE: Duration = Duration::from_secs(30);
/// Audio decode at as slow as 0.5x realtime.
const AUDIO: Budget = Budget::new(Duration::from_secs(120), 2.0);
/// A software H.264 transcode at as slow as ~0.033x realtime.
const PLAYBACK: Budget = Budget::new(Duration::from_secs(600), 30.0);
/// A full decode through the scene filter.
const SCENES: Budget = Budget::new(Duration::from_secs(300), 20.0);

/// Most samples reserved up front for an audio decode (128 MiB of f32), so
/// a lying duration header can't force a huge allocation.
const RESERVE_CAP_SAMPLES: usize = 1 << 25;

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
    /// Average frame rate of the video stream, e.g. 29.97. The OTIO export
    /// expresses clip ranges at each source's real rate.
    pub frame_rate: Option<f64>,
    /// Clockwise display rotation from the stream's displaymatrix (0, 90,
    /// 180 or 270); None without a video stream. ffmpeg applies it on
    /// decode, so it describes the container, not the pixels: a phone
    /// recording with orientation lock can still hold sideways content at 0.
    pub rotation_cw: Option<u16>,
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
    match subprocess::run(
        Command::new(name).arg("-version"),
        Path::new(name),
        VERSION_DEADLINE,
    ) {
        Ok(_) => Some(PathBuf::from(name)),
        Err(e @ Error::SubprocessTimeout { .. }) => {
            // The answer is cached for the process, so say why it is absent.
            tracing::warn!(
                "{name} -version took over {VERSION_DEADLINE:?}; treating {name} as \
                 unavailable for this process: {e}"
            );
            None
        }
        Err(_) => None,
    }
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
    let out = subprocess::run(
        Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path),
        path,
        PROBE_DEADLINE,
    )?;
    Ok(Some(parse_probe(&probe_json(path, &out.stdout)?)))
}

fn probe_json(path: &Path, stdout: &[u8]) -> Result<serde_json::Value> {
    serde_json::from_slice(stdout).map_err(|e| Error::VideoProbe {
        path: path.to_path_buf(),
        detail: format!("unparseable ffprobe JSON: {e}"),
    })
}

/// The duration that sizes a full-decode deadline for `path`: `None` when
/// it can't be read, so the budget assumes a long file. A probe that timed
/// out is returned as the error: a file whose header won't parse in that
/// time is wedged, and decoding it would wedge too.
fn deadline_duration(path: &Path) -> Result<Option<f64>> {
    match probe(path) {
        Ok(p) => Ok(p.and_then(|p| p.duration_secs)),
        Err(e @ Error::SubprocessTimeout { .. }) => Err(e),
        Err(_) => Ok(None),
    }
}

/// Pull the fields we keep out of ffprobe's JSON. Split from [`probe`] so it
/// is testable without running ffprobe.
fn parse_probe(json: &serde_json::Value) -> VideoProbe {
    let format = &json["format"];
    let tags = &format["tags"];

    let duration_secs = parse_duration(json);

    // First stream whose codec_type is "video" — audio-only containers have
    // none, and then codec/width/height stay None.
    let video_stream = json["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["codec_type"].as_str() == Some("video"));

    let (video_codec, width, height, frame_rate, rotation_cw) = match video_stream {
        Some(s) => {
            let rotation = stream_rotation(s);
            let rotated = rotation % 180 != 0;
            let (w, h) = (s["width"].as_i64(), s["height"].as_i64());
            (
                s["codec_name"].as_str().map(str::to_string),
                if rotated { h } else { w },
                if rotated { w } else { h },
                s["avg_frame_rate"]
                    .as_str()
                    .or_else(|| s["r_frame_rate"].as_str())
                    .and_then(parse_rational)
                    .filter(|r| *r > 0.0),
                // displaymatrix angles are counter-clockwise.
                Some(((360 - rotation) % 360) as u16),
            )
        }
        None => (None, None, None, None, None),
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
        frame_rate,
        rotation_cw,
    }
}

/// Parse ffprobe's rational rate strings: `"30000/1001"` or `"25"`.
/// `"0/0"` (streams with no rate) yields None via the caller's filter.
fn parse_rational(s: &str) -> Option<f64> {
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
            (d != 0.0).then(|| n / d)
        }
        None => s.parse().ok(),
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
    let frame_err = |detail: String| Error::FrameExtract {
        path: path.to_path_buf(),
        ts_secs,
        detail,
    };
    let out = subprocess::run(
        Command::new(ffmpeg)
            .args(["-v", "error", "-ss", &format!("{ts_secs:.3}")])
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]),
        path,
        FRAME_DEADLINE,
    )
    // Keep the timestamp in the message: callers log it per frame.
    .map_err(|e| match e {
        Error::SubprocessFailed { .. } | Error::SubprocessTimeout { .. } => {
            frame_err(e.to_string())
        }
        other => other,
    })?;
    if out.stdout.is_empty() {
        return Err(frame_err(subprocess::stderr_tail(&out.stderr)));
    }
    let img = image::load_from_memory(&out.stdout).map_err(|source| Error::ImageDecode {
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
    let duration = deadline_duration(src)?;
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
    if let Err(e) = subprocess::run(cmd.arg(&tmp), src, PLAYBACK.deadline(duration)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(Some(()))
}

/// Does this file carry an audio stream at all, and how long is it?
///
/// Header-only ffprobe, so it costs nothing next to a decode. Split out
/// because "no audio" and "audio we failed to decode" need opposite
/// treatment and ffmpeg's exit status alone cannot tell them apart. The
/// duration sizes the decode's deadline. `Ok(None)` when ffprobe is missing.
fn probe_audio(path: &Path) -> Result<Option<(bool, Option<f64>)>> {
    let Some(ffprobe) = ffprobe() else {
        return Ok(None);
    };
    let out = subprocess::run(
        Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=codec_type:format=duration",
                "-print_format",
                "json",
            ])
            .arg(path),
        path,
        PROBE_DEADLINE,
    )?;
    let json = probe_json(path, &out.stdout)?;
    Ok(Some((parse_has_audio(&json), parse_duration(&json))))
}

/// ffprobe prints `format.duration` as a decimal string.
fn parse_duration(json: &serde_json::Value) -> Option<f64> {
    json["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
}

/// Pull the audio-stream verdict out of ffprobe's JSON. Split from
/// [`probe_audio`] so it is testable without running ffprobe.
fn parse_has_audio(json: &serde_json::Value) -> bool {
    json["streams"]
        .as_array()
        .is_some_and(|streams| !streams.is_empty())
}

/// Decode a video's audio track to 16 kHz mono f32 PCM — whisper's input
/// format. `Ok(Some(vec![]))` for a video with no (or empty) audio stream;
/// `Ok(None)` when ffmpeg is missing.
pub fn extract_audio_pcm(path: &Path) -> Result<Option<Vec<f32>>> {
    extract_audio_pcm_at(path, 16_000)
}

/// [`extract_audio_pcm`] at another sample rate — Beat This wants 22.05 kHz,
/// and upsampling the 16 kHz decode would leave its top mel bands empty.
pub fn extract_audio_pcm_at(path: &Path, sample_rate: u32) -> Result<Option<Vec<f32>>> {
    let Some(ffmpeg) = ffmpeg() else {
        return Ok(None);
    };

    // Settle "is there audio here?" before decoding. Deciding it afterwards
    // from ffmpeg's exit status is what made a corrupt or unsupported file
    // indistinguishable from a genuinely silent one: both exit non-zero with
    // nothing on stdout, and the caller stored the failure as a completed
    // zero-segment transcript that no later run would ever retry.
    // Without ffprobe there is no way to make that distinction, so fall
    // through and let a no-audio file fail: it stays pending and retries,
    // which is the recoverable direction to be wrong in.
    let duration = match probe_audio(path)? {
        Some((false, _)) => return Ok(Some(Vec::new())),
        Some((true, duration)) => duration,
        None => None,
    };

    // Saturating casts: NaN or negative durations reserve nothing.
    let reserve = duration.map_or(0, |d| (d * f64::from(sample_rate)).ceil() as usize);
    let reserve = reserve.min(RESERVE_CAP_SAMPLES);
    // Past the probe, any non-zero exit is a real decode failure: it must
    // surface so the asset stays pending and a later run can retry it.
    let out = subprocess::run_with(
        Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vn", "-f", "f32le", "-ac", "1", "-ar"])
            .arg(sample_rate.to_string())
            .arg("-"),
        path,
        AUDIO.deadline(duration),
        move |stdout| read_f32le(stdout, reserve),
    )?;
    Ok(Some(out.stdout))
}

/// Decode a little-endian f32 stream straight into one sample buffer, never
/// holding the bytes as well. A trailing partial sample is dropped.
fn read_f32le(mut r: impl Read, reserve: usize) -> io::Result<Vec<f32>> {
    let mut pcm = Vec::new();
    // Only a size hint: a failed reservation just means the Vec grows.
    let _ = pcm.try_reserve_exact(reserve);
    let mut buf = vec![0u8; 64 * 1024];
    // Bytes of a sample split across two reads, kept at the buffer's front.
    let mut carry = 0;
    loop {
        let n = match r.read(&mut buf[carry..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let filled = carry + n;
        let whole = filled - filled % 4;
        pcm.extend(
            buf[..whole]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
        buf.copy_within(whole..filled, 0);
        carry = filled - whole;
    }
    Ok(pcm)
}

/// Cheap voice-activity proxy: is there enough energy to be worth running
/// speech recognition? Filters out silent tracks so whisper never sees them
/// (it hallucinates text on silence).
pub fn has_audible_content(pcm: &[f32]) -> bool {
    if pcm.len() < 16_000 {
        return false; // under a second of audio
    }
    let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    rms > 0.01
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
    let duration = deadline_duration(path)?;
    let out = subprocess::run(
        Command::new(ffmpeg)
            .args(["-v", "info", "-i"])
            .arg(path)
            .args([
                "-vf",
                &format!("select='gt(scene,{SCENE_THRESHOLD})',showinfo"),
                "-f",
                "null",
                "-",
            ]),
        path,
        SCENES.deadline(duration),
    )?;
    // showinfo logs one line per selected frame; pts_time is the timestamp.
    // The runner only returns once stderr reached EOF, so this is complete.
    let stderr = String::from_utf8_lossy(&out.stderr);
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

    const FIXTURE_DEADLINE: Duration = Duration::from_secs(60);

    /// Yields its bytes a few at a time, so samples straddle reads.
    struct Trickle(Vec<u8>, usize);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(3).min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }

    #[test]
    fn read_f32le_reassembles_samples_split_across_reads() {
        let samples = [0.5f32, -1.25, 3.0, f32::MIN_POSITIVE, 7.75];
        let mut bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        bytes.push(0xAB); // a trailing partial sample is dropped
        let pcm = read_f32le(Trickle(bytes, 0), 2).unwrap();
        assert_eq!(pcm, samples);
    }

    /// Audio extraction keeps "no audio stream" (empty) apart from "decode
    /// failed" (error), and decodes a real track. Skips without ffmpeg.
    #[test]
    fn audio_extraction_separates_silence_from_failure() {
        let (Some(_), Some(ffmpeg_bin)) = (ffprobe(), ffmpeg()) else {
            eprintln!("skipping: ffmpeg/ffprobe not installed");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let synth = |name: &str, source: &str| {
            let path = dir.path().join(name);
            subprocess::run(
                Command::new(ffmpeg_bin)
                    .args(["-v", "error", "-f", "lavfi", "-i", source])
                    .arg(&path),
                &path,
                FIXTURE_DEADLINE,
            )
            .expect("ffmpeg synthesises the fixture");
            path
        };

        let silent = synth("silent.mp4", "testsrc=duration=1:size=64x48:rate=10");
        let pcm = extract_audio_pcm(&silent).unwrap().expect("ffmpeg present");
        assert!(pcm.is_empty(), "no audio stream decodes to nothing");

        let sine = synth("sine.wav", "sine=frequency=440:duration=1");
        let (has_audio, duration) = probe_audio(&sine).unwrap().expect("ffprobe present");
        assert!(has_audio);
        let d = duration.expect("ffprobe reports the duration");
        assert!((d - 1.0).abs() < 0.05, "duration ~1s, got {d}");
        let pcm = extract_audio_pcm(&sine).unwrap().expect("ffmpeg present");
        assert!(pcm.len().abs_diff(16_000) < 200, "{} samples", pcm.len());
        assert!(has_audible_content(&pcm));

        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        assert!(extract_audio_pcm(&bad).is_err(), "corrupt is not silent");
    }

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
        assert_eq!(
            p.rotation_cw,
            Some(90),
            "-90 counter-clockwise = 90 clockwise"
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
    fn parse_has_audio_separates_silence_from_a_real_track() {
        // `-select_streams a` returns only audio streams, so an empty array
        // is the genuine no-audio case — the only one allowed to be stored
        // as a completed zero-segment transcript.
        let none: serde_json::Value = serde_json::json!({ "streams": [] });
        assert!(!parse_has_audio(&none), "no audio stream");

        let some: serde_json::Value = serde_json::json!({
            "streams": [{"codec_type": "audio"}]
        });
        assert!(parse_has_audio(&some), "one audio stream");
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
        subprocess::run(
            Command::new(ffmpeg_bin)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=2:size=64x48:rate=10",
                ])
                .args(["-pix_fmt", "yuv420p"])
                .arg(&clip),
            &clip,
            FIXTURE_DEADLINE,
        )
        .expect("ffmpeg synthesises the clip");

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
    fn audible_content_gate() {
        assert!(!has_audible_content(&[]), "empty");
        assert!(!has_audible_content(&vec![0.0f32; 32_000]), "silence");
        assert!(!has_audible_content(&vec![0.5f32; 8_000]), "under a second");
        let loudish: Vec<f32> = (0..32_000).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
        assert!(has_audible_content(&loudish));
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
        let made_hevc = subprocess::run(
            Command::new(ffmpeg_bin)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=1:size=64x48:rate=10",
                ])
                .args(["-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p"])
                .arg(&hevc),
            &hevc,
            FIXTURE_DEADLINE,
        )
        .is_ok();
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
        subprocess::run(
            Command::new(ffmpeg_bin)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=1:size=64x48:rate=10",
                ])
                .args(["-pix_fmt", "yuv420p"])
                .arg(&h264),
            &h264,
            FIXTURE_DEADLINE,
        )
        .expect("ffmpeg synthesises the H.264 clip");
        let mkv = dir.path().join("clip.mkv");
        subprocess::run(
            Command::new(ffmpeg_bin)
                .args(["-v", "error"])
                .arg("-i")
                .arg(&h264)
                .args(["-c", "copy"])
                .arg(&mkv),
            &h264,
            FIXTURE_DEADLINE,
        )
        .expect("ffmpeg remuxes to MKV");
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
