//! Prompt-driven reel generation (the goals.md stretch goal; ADR-0011).
//!
//! A reel is a consumer of data the library already has: `search_similar`
//! ranks photos and video *moments* (frame timestamps), and ffmpeg cuts the
//! winners into one mp4. Nothing here talks to a model — the prompt was
//! embedded once by the caller.
//!
//! Rendering strategy is deliberately boring: every selected segment is
//! normalised to the same codec/size/fps in its own ffmpeg run, then joined
//! losslessly with the concat demuxer. One subprocess per segment beats one
//! giant filter graph on debuggability, and hard cuts beat xfade on
//! robustness (a failed segment names its source file).

use anyhow::{Context, Result, bail};
use eidetic_db::SearchResult;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How much of a video is cut around the best-matching frame: 1.5s of lead-in
/// so the moment doesn't open the clip cold, 4s total.
const VIDEO_LEAD_SECS: f64 = 1.5;
const VIDEO_CLIP_SECS: f64 = 4.0;
/// Photos hold for 3s with a slow Ken Burns push-in.
const PHOTO_CLIP_SECS: f64 = 3.0;
/// A hit scoring below this fraction of the best hit is padding, not content.
const RELATIVE_SCORE_FLOOR: f64 = 0.35;

pub struct ReelPlan {
    pub segments: Vec<Segment>,
    pub total_secs: f64,
}

pub enum Segment {
    /// (source, clip start seconds, clip length seconds)
    VideoClip(PathBuf, f64, f64),
    /// (source photo)
    Photo(PathBuf),
}

impl Segment {
    fn secs(&self) -> f64 {
        match self {
            Segment::VideoClip(_, _, len) => *len,
            Segment::Photo(_) => PHOTO_CLIP_SECS,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Segment::VideoClip(p, start, len) => {
                format!("{} [{start:.1}s +{len:.1}s]", p.display())
            }
            Segment::Photo(p) => format!("{} [photo, {PHOTO_CLIP_SECS:.0}s]", p.display()),
        }
    }
}

/// Turn ranked search results into a cut list no longer than `target_secs`.
///
/// Results are consumed in score order. Weak tails are dropped: anything
/// scoring below zero, or below [`RELATIVE_SCORE_FLOOR`] of the top hit,
/// never enters the reel even if there is room.
///
/// `library_dir` locates photo thumbnails — see [`photo_source`].
pub fn plan(results: &[SearchResult], target_secs: f64, library_dir: &Path) -> ReelPlan {
    let top = results.first().map(|r| r.score as f64).unwrap_or(0.0);
    let floor = (top * RELATIVE_SCORE_FLOOR).max(0.0);

    let mut segments = Vec::new();
    let mut total = 0.0f64;
    for r in results {
        if (r.score as f64) < floor || r.score <= 0.0 {
            break; // results are sorted; everything after is weaker
        }
        let seg = match r.frame_ts {
            Some(ts) => {
                let duration = r.duration_secs.unwrap_or(f64::MAX);
                let start = (ts - VIDEO_LEAD_SECS).max(0.0);
                let len = VIDEO_CLIP_SECS.min(duration - start).max(1.0);
                Segment::VideoClip(r.storage_path.clone(), start, len)
            }
            None => Segment::Photo(photo_source(r, library_dir)),
        };
        if total + seg.secs() > target_secs && !segments.is_empty() {
            break;
        }
        total += seg.secs();
        segments.push(seg);
    }
    ReelPlan {
        segments,
        total_secs: total,
    }
}

/// Render the plan to `output` at `width`x`height`, 30fps, H.264.
pub fn render(plan: &ReelPlan, output: &Path, width: u32, height: u32) -> Result<()> {
    let Some(ffmpeg) = eidetic_ingest::video::ffmpeg() else {
        bail!("ffmpeg not found; install it (or set EIDETIC_FFMPEG_PATH) to render reels");
    };
    if plan.segments.is_empty() {
        bail!("nothing matched confidently enough to cut a reel");
    }

    let workdir = tempfile::tempdir().context("create reel work dir")?;
    let mut list = String::new();

    for (i, seg) in plan.segments.iter().enumerate() {
        let seg_path = workdir.path().join(format!("seg_{i:03}.mp4"));
        let status = match seg {
            Segment::VideoClip(src, start, len) => Command::new(ffmpeg)
                .args(["-y", "-v", "error", "-ss", &format!("{start:.3}")])
                .arg("-i")
                .arg(src)
                .args(["-t", &format!("{len:.3}")])
                .args(["-vf", &normalize_filter(width, height)])
                .args(["-an", "-c:v", "libx264", "-preset", "veryfast"])
                .arg(&seg_path)
                .status(),
            Segment::Photo(src) => Command::new(ffmpeg)
                .args(["-y", "-v", "error", "-loop", "1"])
                .arg("-i")
                .arg(src)
                .args(["-t", &format!("{PHOTO_CLIP_SECS:.1}")])
                .args(["-vf", &ken_burns_filter(width, height)])
                .args(["-an", "-c:v", "libx264", "-preset", "veryfast"])
                .arg(&seg_path)
                .status(),
        }
        .context("run ffmpeg for a segment")?;
        if !status.success() {
            bail!("ffmpeg failed on segment {}: {}", i + 1, seg.describe());
        }
        // concat-demuxer syntax; single quotes inside paths are escaped as '\''
        list.push_str(&format!(
            "file '{}'\n",
            seg_path.display().to_string().replace('\'', "'\\''")
        ));
    }

    let list_path = workdir.path().join("concat.txt");
    std::fs::write(&list_path, list).context("write concat list")?;

    let status = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0"])
        .arg("-i")
        .arg(&list_path)
        .args(["-c", "copy"])
        .arg(output)
        .status()
        .context("run ffmpeg concat")?;
    if !status.success() {
        bail!("ffmpeg concat failed");
    }
    Ok(())
}

/// The file ffmpeg should read for a photo segment.
///
/// The original can be HEIC or DNG (which ffmpeg does not decode) and its
/// EXIF orientation would be ignored even for JPEG. The medium thumbnail is
/// an upright 1024px JPEG, so prefer it whenever it exists; fall back to the
/// original for the rare un-thumbnailed photo.
fn photo_source(r: &SearchResult, library_dir: &Path) -> PathBuf {
    if r.thumbnails_generated
        && let Some(hash) = r
            .storage_path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(eidetic_core::Sha256::from_hex)
    {
        return eidetic_ingest::thumbnail::thumbnail_path(
            library_dir,
            &hash,
            eidetic_ingest::thumbnail::ThumbSize::Medium,
        );
    }
    r.storage_path.clone()
}

/// Fit any input inside the frame, pad to exact size, normalise fps/pixfmt so
/// concat's `-c copy` join is legal across segments.
fn normalize_filter(w: u32, h: u32) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps=30,format=yuv420p"
    )
}

/// Ken Burns for stills: cover-crop to the frame, then a slow centred
/// push-in. `d` is in *output frames* (30fps x 3s).
fn ken_burns_filter(w: u32, h: u32) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},\
         zoompan=z='min(zoom+0.0016,1.15)':\
         x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':\
         d={frames}:s={w}x{h}:fps=30,format=yuv420p",
        frames = (PHOTO_CLIP_SECS * 30.0) as u32
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::AssetId;

    fn lib() -> PathBuf {
        PathBuf::from("/lib")
    }

    fn hit(score: f32, frame_ts: Option<f64>, duration: Option<f64>) -> SearchResult {
        SearchResult {
            id: AssetId::new(),
            storage_path: PathBuf::from("/x"),
            score,
            frame_ts,
            duration_secs: duration,
            mime_type: None,
            file_size: 0,
            thumbnails_generated: false,
            date_taken: None,
            camera_make: None,
            camera_model: None,
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn plan_respects_target_duration() {
        // Ten strong photo hits at 3s each against a 10s target: 3 fit.
        let results: Vec<_> = (0..10).map(|_| hit(0.5, None, None)).collect();
        let p = plan(&results, 10.0, &lib());
        assert_eq!(p.segments.len(), 3);
        assert!((p.total_secs - 9.0).abs() < 1e-9);
    }

    #[test]
    fn plan_drops_weak_tail_even_with_room() {
        let results = vec![hit(0.5, None, None), hit(0.1, None, None)];
        let p = plan(&results, 60.0, &lib());
        assert_eq!(p.segments.len(), 1, "0.1 < 35% of 0.5 is padding");
    }

    #[test]
    fn plan_drops_non_positive_scores() {
        let results = vec![hit(0.0, None, None), hit(-0.2, None, None)];
        assert!(plan(&results, 30.0, &lib()).segments.is_empty());
    }

    #[test]
    fn video_clip_leads_into_the_moment_and_respects_bounds() {
        // Moment at 9s of a 10s video: start = 7.5, len clamped to 2.5.
        let results = vec![hit(0.5, Some(9.0), Some(10.0))];
        let p = plan(&results, 30.0, &lib());
        match &p.segments[0] {
            Segment::VideoClip(_, start, len) => {
                assert!((start - 7.5).abs() < 1e-9);
                assert!((len - 2.5).abs() < 1e-9);
            }
            _ => panic!("expected a video clip"),
        }

        // Moment at 0.5s: lead-in clamps to the file start.
        let results = vec![hit(0.5, Some(0.5), Some(10.0))];
        match &plan(&results, 30.0, &lib()).segments[0] {
            Segment::VideoClip(_, start, len) => {
                assert_eq!(*start, 0.0);
                assert!((len - 4.0).abs() < 1e-9);
            }
            _ => panic!("expected a video clip"),
        }
    }

    #[test]
    fn photos_render_from_the_medium_thumbnail_when_present() {
        let hash = "ab".repeat(32);
        let mut thumbed = hit(0.5, None, None);
        thumbed.storage_path = PathBuf::from(format!("/lib/ab/ab/{hash}.heic"));
        thumbed.thumbnails_generated = true;
        let p = plan(&[thumbed], 30.0, &lib());
        match &p.segments[0] {
            Segment::Photo(src) => {
                let s = src.to_str().unwrap();
                assert!(s.contains(".thumbs/m/"), "expected thumb path, got {s}");
                assert!(s.ends_with(".jpg"));
            }
            _ => panic!("expected a photo"),
        }

        // No thumbnail -> the original is used as-is.
        let raw = hit(0.5, None, None);
        match &plan(&[raw], 30.0, &lib()).segments[0] {
            Segment::Photo(src) => assert_eq!(src, &PathBuf::from("/x")),
            _ => panic!("expected a photo"),
        }
    }

    #[test]
    fn plan_always_takes_at_least_one_strong_hit() {
        // Target shorter than the first segment still yields that segment.
        let results = vec![hit(0.5, Some(5.0), Some(60.0))];
        assert_eq!(plan(&results, 1.0, &lib()).segments.len(), 1);
    }
}
