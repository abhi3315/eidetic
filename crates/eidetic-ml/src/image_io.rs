//! Shared image loading for every model in this crate.
//!
//! Both the SigLIP embedder and the face analyzer need the same thing: decode
//! whatever format the file is (including HEIC and DNG), apply the EXIF
//! rotation, and refuse to allocate absurd amounts of memory. Extracted here
//! so the two pipelines cannot drift apart — a photo must not be upright for
//! semantic search but sideways for face detection.

use crate::{Error, Result};
use image::DynamicImage;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

fn apply_limits_and_decode<R: Read + Seek + std::io::BufRead>(
    mut reader: image::ImageReader<R>,
) -> Result<DynamicImage> {
    // Cap allocation + dimensions before decode so a decompression-bomb file
    // (crafted PNG/JPEG/WEBP/HEIC) can't OOM-kill the loop. 512 MB / 16384 px
    // is well above any real photo and well below "exhaust process memory".
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| Error::Inference(format!("cannot decode image: {e}")))
}

/// Decode the image at `path` and rotate it upright.
///
/// DNG/ProRAW goes through the embedded JPEG preview. HEIC is decoded by the
/// libheif hook when that feature is on, and libheif already applies the `irot`
/// transform, so the EXIF rotation is skipped for those.
pub fn load_oriented_image(path: &Path) -> Result<DynamicImage> {
    crate::ensure_heic_registered();

    let mut img = if eidetic_core::dng::is_dng_path(path) {
        let bytes = eidetic_core::dng::extract_largest_jpeg_preview(path)
            .map_err(|e| Error::Inference(format!("dng preview extraction failed: {e}")))?;
        let reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| Error::Inference(format!("cannot detect image format: {e}")))?;
        apply_limits_and_decode(reader)?
    } else {
        let reader = image::ImageReader::open(path)
            .map_err(|e| Error::Inference(format!("cannot open image: {e}")))?
            .with_guessed_format()
            .map_err(|e| Error::Inference(format!("cannot detect image format: {e}")))?;
        apply_limits_and_decode(reader)?
    };

    if !eidetic_core::exif::is_heic_path(path)
        && let Some(orient) = eidetic_core::exif::read_orientation(path)
        && let Some(transform) = image::metadata::Orientation::from_exif(orient)
        && transform != image::metadata::Orientation::NoTransforms
    {
        img.apply_orientation(transform);
    }

    Ok(img)
}
