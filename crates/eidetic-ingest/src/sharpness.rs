//! Blur detection for the reel's quality floor (goals-v0.8.md phase 0).
//!
//! Variance of the Laplacian — the standard cheap focus measure: a sharp
//! image has strong second derivatives everywhere, a blurry one doesn't.
//! Scores are computed on a ≤640px-wide grayscale copy so numbers are
//! comparable across sources (a 4K frame and a 1024px thumbnail of the
//! same scene otherwise score wildly differently).
//!
//! The score is content-dependent (a clean sky scores near zero even in
//! perfect focus), so callers use it as a *relative* gate over candidates
//! with a conservative floor, never as an absolute verdict.

use crate::error::{Error, Result};
use crate::video;
use image::{DynamicImage, GrayImage, imageops::FilterType};
use std::path::Path;

/// Width sharpness is measured at. Images narrower than this are used
/// as-is (upscaling adds no information and softens the measure).
const MEASURE_WIDTH: u32 = 640;

/// Variance of the 4-neighbour Laplacian over the interior pixels.
pub fn laplacian_variance(gray: &GrayImage) -> f64 {
    let (w, h) = gray.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let px = |x: u32, y: u32| gray.get_pixel(x, y).0[0] as f64;
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let n = ((w - 2) * (h - 2)) as f64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let lap = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1) - 4.0 * px(x, y);
            sum += lap;
            sum_sq += lap * lap;
        }
    }
    let mean = sum / n;
    (sum_sq / n - mean * mean).max(0.0)
}

/// Sharpness of an image file (a photo, or a photo's thumbnail).
pub fn image_sharpness(path: &Path) -> Result<f64> {
    crate::ensure_heic_registered();
    let img = image::open(path).map_err(|source| Error::ImageDecode {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(measure(&img))
}

/// Sharpness of the video frame at `ts_secs`. `Ok(None)` without ffmpeg.
pub fn video_sharpness(path: &Path, ts_secs: f64) -> Result<Option<f64>> {
    Ok(video::extract_frame(path, ts_secs)?.map(|f| measure(&f)))
}

fn measure(img: &DynamicImage) -> f64 {
    let img = if img.width() > MEASURE_WIDTH {
        let h = (img.height() as u64 * MEASURE_WIDTH as u64 / img.width() as u64).max(1) as u32;
        img.resize_exact(MEASURE_WIDTH, h, FilterType::Triangle)
    } else {
        img.clone()
    };
    laplacian_variance(&img.to_luma8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops;

    /// A checkerboard is as sharp as an image gets; blurring it must
    /// collapse the score by orders of magnitude.
    #[test]
    fn blur_collapses_the_score() {
        let sharp = GrayImage::from_fn(128, 128, |x, y| {
            image::Luma([if (x / 8 + y / 8) % 2 == 0 { 230 } else { 20 }])
        });
        let blurred = imageops::blur(&sharp, 4.0);
        let s = laplacian_variance(&sharp);
        let b = laplacian_variance(&blurred);
        assert!(s > 1000.0, "checkerboard variance: {s}");
        assert!(b < s / 20.0, "blurred {b} vs sharp {s}");
    }

    #[test]
    fn flat_image_scores_zero() {
        let flat = GrayImage::from_pixel(64, 64, image::Luma([128]));
        assert_eq!(laplacian_variance(&flat), 0.0);
    }

    #[test]
    fn tiny_images_do_not_panic() {
        let tiny = GrayImage::from_pixel(2, 2, image::Luma([128]));
        assert_eq!(laplacian_variance(&tiny), 0.0);
    }

    /// Calibration diagnostic, not an assertion: prints the score of every
    /// image under EIDETIC_SHARPNESS_DIR (and each video's 2s frame) so a
    /// gate threshold can be sanity-checked against real media.
    /// Run: EIDETIC_SHARPNESS_DIR=<dir> cargo test -p eidetic-ingest \
    ///      calibrate_sharpness -- --ignored --nocapture
    #[test]
    #[ignore]
    fn calibrate_sharpness_on_real_media() {
        let Some(dir) = std::env::var_os("EIDETIC_SHARPNESS_DIR") else {
            eprintln!("set EIDETIC_SHARPNESS_DIR to run");
            return;
        };
        let mut entries: Vec<_> = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect();
        entries.sort();
        for path in entries {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let score = match ext.as_str() {
                "jpg" | "jpeg" | "png" | "webp" | "heic" => {
                    image_sharpness(&path).map(Some).unwrap_or(None)
                }
                "mp4" | "mov" | "mkv" => video_sharpness(&path, 2.0).unwrap_or(None),
                _ => None,
            };
            if let Some(s) = score {
                println!("{s:>10.1}  {}", path.display());
            }
        }
    }
}
