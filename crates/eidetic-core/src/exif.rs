//! Decode-time EXIF helpers.
//!
//! Image-decoder code paths (`eidetic-ingest::thumbnail::load_image_with_limits`
//! and `eidetic-ml::siglip::preprocess_image`) read the EXIF Orientation tag
//! through this module to apply the right rotation/flip transform on top of
//! the decoded pixel buffer. iPhone JPEGs and DNGs typically store landscape
//! sensor data with an `Orientation = 6` tag; without this step the photo
//! renders sideways in the grid.
//!
//! HEIC files don't go through this — libheif applies the `irot` transform
//! during decode, so reading the EXIF tag again would double-rotate. Callers
//! check the file extension and skip HEIC.

use exif::{In, Reader, Tag, Value};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Read the EXIF `Orientation` tag (TIFF tag 0x0112) from `path`.
///
/// Returns the raw EXIF value (1–8) when present; `None` if the file has no
/// EXIF, the tag is missing, or the value can't be parsed. Callers convert
/// via [`image::metadata::Orientation::from_exif`].
///
/// Valid EXIF orientation values per the TIFF/EPS spec:
///
/// | Value | Display transform |
/// |---|---|
/// | 1 | identity (already upright)        |
/// | 2 | flip horizontally                 |
/// | 3 | rotate 180°                       |
/// | 4 | flip vertically                   |
/// | 5 | transpose (flip + 90° CW)         |
/// | 6 | rotate 90° clockwise              |
/// | 7 | transverse (flip + 90° CCW)       |
/// | 8 | rotate 90° counter-clockwise      |
pub fn read_orientation(path: &Path) -> Option<u8> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif.get_field(Tag::Orientation, In::PRIMARY)?;
    let raw: u16 = match &field.value {
        Value::Short(v) => v.first().copied()?,
        Value::Long(v) => v.first().copied().and_then(|n| u16::try_from(n).ok())?,
        _ => return None,
    };
    if (1..=8).contains(&raw) {
        Some(raw as u8)
    } else {
        None
    }
}

/// Returns `true` if the path's extension is `.heic` or `.heif`
/// (case-insensitive).
///
/// HEIC inputs go through libheif's decoder hook which applies the `irot`
/// transform before returning the `DynamicImage`. Calling
/// [`read_orientation`] + `apply_orientation` on top of that would
/// double-rotate; callers should branch on this helper.
pub fn is_heic_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_heic_path_case_insensitive() {
        assert!(is_heic_path(Path::new("photo.heic")));
        assert!(is_heic_path(Path::new("photo.HEIC")));
        assert!(is_heic_path(Path::new("photo.Heic")));
        assert!(is_heic_path(Path::new("photo.heif")));
        assert!(is_heic_path(Path::new("photo.HEIF")));
        assert!(!is_heic_path(Path::new("photo.jpg")));
        assert!(!is_heic_path(Path::new("photo.dng")));
        assert!(!is_heic_path(Path::new("photo")));
    }

    #[test]
    fn read_orientation_returns_none_for_missing_file() {
        assert_eq!(read_orientation(Path::new("/nonexistent.jpg")), None);
    }
}
