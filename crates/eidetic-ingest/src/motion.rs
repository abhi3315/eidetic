//! Per-moment motion from decoded frames (goals-v0.9.md phase 2).
//!
//! The signal is deliberately simple frame differencing: the sampled frame
//! and a companion decoded [`COMPANION_SECS`] later are both reduced to a
//! small greyscale grid, and the score is their mean absolute difference in
//! 0..1. 0 means the two frames are identical; a hard cut between unrelated
//! shots would be near the top of the range.
//!
//! What it cannot tell apart: subject motion from camera motion (a pan and
//! a sprint both score high), and motion from sudden exposure change or a
//! scene cut that falls inside the companion gap. Frames are sampled at shot
//! midpoints, so the last case is rare. The absolute scale is not
//! calibrated — planners compare moments against each other, not against a
//! fixed threshold.

use crate::Result;
use image::DynamicImage;
use image::imageops::FilterType;
use std::path::Path;

/// Gap between the sampled frame and its companion. Long enough for real
/// movement to show at 30fps (six frames), short enough to stay in the shot.
pub const COMPANION_SECS: f64 = 0.2;

/// Both frames are compared at this size: large enough to see a subject
/// cross the frame, small enough that sensor noise and compression blocks
/// average out.
const GRID: u32 = 64;

/// Mean absolute luma difference of two frames, in 0..1. `None` for an
/// empty frame. Frames of different sizes are compared at the same grid, so
/// the score is resolution independent.
pub fn frame_motion(a: &DynamicImage, b: &DynamicImage) -> Option<f32> {
    if a.width() == 0 || a.height() == 0 || b.width() == 0 || b.height() == 0 {
        return None;
    }
    let small = |img: &DynamicImage| {
        img.resize_exact(GRID, GRID, FilterType::Triangle)
            .to_luma8()
    };
    let (a, b) = (small(a), small(b));
    let sum: u64 = a
        .as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    let score = sum as f64 / (a.as_raw().len() as f64 * 255.0);
    // Finite and bounded by construction; the clamp documents the contract
    // the database CHECK enforces.
    Some(score.clamp(0.0, 1.0) as f32)
}

/// Where the companion frame for a moment at `ts` is decoded: after it when
/// the video continues, before it at the tail. `None` when the video is too
/// short to hold two distinct frames.
pub fn companion_ts(ts: f64, duration_secs: Option<f64>) -> Option<f64> {
    let end = duration_secs.unwrap_or(f64::MAX);
    if ts + COMPANION_SECS < end {
        Some(ts + COMPANION_SECS)
    } else if ts - COMPANION_SECS >= 0.0 {
        Some(ts - COMPANION_SECS)
    } else {
        None
    }
}

/// Motion at the moment `ts` whose frame is already decoded (`frame`), by
/// decoding its companion. `Ok(None)` = not measurable (no ffmpeg, video too
/// short); callers store that as NULL, never as 0.
pub fn motion_at(
    path: &Path,
    frame: &DynamicImage,
    ts: f64,
    duration_secs: Option<f64>,
) -> Result<Option<f32>> {
    let Some(other_ts) = companion_ts(ts, duration_secs) else {
        return Ok(None);
    };
    let Some(other) = crate::video::extract_frame(path, other_ts)? else {
        return Ok(None);
    };
    Ok(frame_motion(frame, &other))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    /// A 160x90 frame: black with a white square at `x`.
    fn square_at(x: u32) -> DynamicImage {
        let mut img = RgbImage::from_pixel(160, 90, Rgb([0, 0, 0]));
        for yy in 30..60 {
            for xx in x..x + 30 {
                img.put_pixel(xx, yy, Rgb([255, 255, 255]));
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn identical_frames_measure_zero_not_unknown() {
        assert_eq!(frame_motion(&square_at(10), &square_at(10)), Some(0.0));
    }

    #[test]
    fn more_movement_scores_higher_and_stays_bounded() {
        let still = frame_motion(&square_at(10), &square_at(10)).unwrap();
        let small = frame_motion(&square_at(10), &square_at(20)).unwrap();
        let large = frame_motion(&square_at(10), &square_at(110)).unwrap();
        assert!(still < small && small < large, "{still} {small} {large}");
        let black = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([0, 0, 0])));
        let white = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([255, 255, 255])));
        let max = frame_motion(&black, &white).unwrap();
        assert!((max - 1.0).abs() < 1e-6 && max.is_finite());
    }

    #[test]
    fn resolution_does_not_change_the_score() {
        let big = |x| square_at(x).resize_exact(640, 360, FilterType::Nearest);
        let a = frame_motion(&square_at(10), &square_at(60)).unwrap();
        let b = frame_motion(&big(10), &big(60)).unwrap();
        assert!((a - b).abs() < 0.02, "{a} vs {b}");
    }

    #[test]
    fn empty_frames_are_unmeasurable() {
        let empty = DynamicImage::ImageRgb8(RgbImage::new(0, 0));
        assert_eq!(frame_motion(&empty, &square_at(0)), None);
    }

    #[test]
    fn companion_stays_inside_the_video() {
        assert_eq!(companion_ts(1.0, Some(10.0)), Some(1.0 + COMPANION_SECS));
        assert_eq!(companion_ts(9.9, Some(10.0)), Some(9.9 - COMPANION_SECS));
        assert_eq!(
            companion_ts(0.05, Some(0.1)),
            None,
            "too short for two frames"
        );
    }

    /// End to end on real decoded frames: a clip of a moving box measures
    /// more motion than a clip of a static one. Skipped without ffmpeg.
    #[test]
    fn decoded_moving_clip_outscores_a_static_one() {
        let Some(ffmpeg) = crate::video::ffmpeg() else {
            eprintln!("skipping: ffmpeg not installed");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, x_expr: &str| {
            let path = dir.path().join(name);
            let ok = std::process::Command::new(ffmpeg)
                .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
                // overlay re-evaluates x per frame (drawbox does not).
                .arg(format!(
                    "color=c=black:s=160x90:d=2:r=30[bg];\
                     color=c=white:s=30x30:d=2:r=30[fg];\
                     [bg][fg]overlay=x='{x_expr}':y=30"
                ))
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&path)
                .status()
                .unwrap()
                .success();
            assert!(ok, "ffmpeg could not synthesise {name}");
            path
        };
        let still = make("still.mp4", "20");
        let moving = make("moving.mp4", "t*60");
        let measure = |p: &Path| {
            let frame = crate::video::extract_frame(p, 1.0).unwrap().unwrap();
            motion_at(p, &frame, 1.0, Some(2.0)).unwrap().unwrap()
        };
        let (s, m) = (measure(&still), measure(&moving));
        assert!(s < 0.005, "static clip measured {s}");
        assert!(m > s * 4.0 && m > 0.005, "moving {m} vs static {s}");
    }
}
