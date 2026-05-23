//! DNG (Adobe Digital Negative) preview extraction.
//!
//! DNG is a TIFF-based RAW container. The `image` crate's TIFF decoder rejects
//! the color types DNGs declare (`Unknown(24)` for 16-bit Linear RAW), so we
//! sidestep that path and pull the embedded JPEG preview out directly.
//!
//! Verified 2026-05-23 against Apple ProRAW DNGs (iPhone 15 Pro Max): the
//! primary IFD's data is a JPEG (Compression = 7) at full sensor resolution
//! (12 MP or 48 MP). Raw Bayer data lives in SubIFDs (TIFF tag 330) which we
//! ignore.
//!
//! Non-Apple DNGs may store raw data in the primary IFD with the preview in
//! a SubIFD instead — that case isn't handled here yet. We'd add SubIFD
//! walking when such a file shows up.

use exif::{In, Reader, Tag, Value};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Extract the largest embedded JPEG preview from a DNG file.
///
/// Returns the raw JPEG bytes (with `FFD8FF` magic header) suitable for
/// `image::load_from_memory` or `image::ImageReader::new(Cursor::new(_))`.
///
/// # Errors
///
/// - `Error::Io` if the file can't be opened or read.
/// - `Error::Exif` if the TIFF/EXIF parse fails.
/// - `Error::NoJpegPreview` if no IFD has `Compression = 7` (JPEG).
/// - `Error::CorruptPreview` if the JPEG payload's magic bytes don't match.
pub fn extract_largest_jpeg_preview(path: &Path) -> Result<Vec<u8>, Error> {
    let file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let exif = Reader::new()
        .read_from_container(&mut reader)
        .map_err(|source| Error::Exif {
            path: path.to_path_buf(),
            source,
        })?;

    // Walk every IFD kamadak parses (PRIMARY, THUMBNAIL, EXIF, GPS, INTEROP)
    // and pick the largest JPEG-compressed candidate by pixel area. In
    // practice for Apple ProRAW this is always IFD 0.
    let mut ifds = std::collections::BTreeSet::<u16>::new();
    for f in exif.fields() {
        ifds.insert(f.ifd_num.index());
    }

    let mut best: Option<JpegCandidate> = None;
    for idx in ifds {
        let ifd = In(idx);
        let compression = exif.get_field(Tag::Compression, ifd).and_then(field_u32);
        if compression != Some(7) {
            continue;
        }
        let w = exif
            .get_field(Tag::ImageWidth, ifd)
            .and_then(field_u32)
            .unwrap_or(0);
        let h = exif
            .get_field(Tag::ImageLength, ifd)
            .and_then(field_u32)
            .unwrap_or(0);
        let (offset, length) = match (
            exif.get_field(Tag::StripOffsets, ifd).and_then(field_u32),
            exif.get_field(Tag::StripByteCounts, ifd)
                .and_then(field_u32),
        ) {
            (Some(o), Some(l)) => (u64::from(o), u64::from(l)),
            _ => match (
                exif.get_field(Tag::JPEGInterchangeFormat, ifd)
                    .and_then(field_u32),
                exif.get_field(Tag::JPEGInterchangeFormatLength, ifd)
                    .and_then(field_u32),
            ) {
                (Some(o), Some(l)) => (u64::from(o), u64::from(l)),
                _ => continue,
            },
        };
        if length == 0 {
            continue;
        }
        let area = u64::from(w) * u64::from(h);
        if best.as_ref().is_none_or(|b| area > b.area) {
            best = Some(JpegCandidate {
                area,
                offset,
                length,
            });
        }
    }

    let Some(picked) = best else {
        return Err(Error::NoJpegPreview {
            path: path.to_path_buf(),
        });
    };

    // Seek + slice the JPEG payload out of the DNG file. Use a fresh handle
    // since the BufReader above advanced through the EXIF parse.
    let mut f = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    f.seek(SeekFrom::Start(picked.offset))
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let len: usize = picked
        .length
        .try_into()
        .map_err(|_| Error::CorruptPreview {
            path: path.to_path_buf(),
            reason: format!("preview length {} exceeds usize", picked.length),
        })?;
    let mut bytes = vec![0u8; len];
    f.read_exact(&mut bytes).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;

    // JPEG SOI marker check — guards against the IFD pointing at garbage
    // (corrupt DNG, mistyped Compression tag, etc.).
    if bytes.len() < 3 || bytes[0] != 0xFF || bytes[1] != 0xD8 || bytes[2] != 0xFF {
        return Err(Error::CorruptPreview {
            path: path.to_path_buf(),
            reason: format!(
                "expected JPEG SOI (FFD8FF), got {:02X?}",
                &bytes[..bytes.len().min(4)]
            ),
        });
    }

    Ok(bytes)
}

/// Returns `true` if the path has a `.dng` extension (case-insensitive).
/// Cheap check for callers that want to route DNG inputs through this module.
pub fn is_dng_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
}

struct JpegCandidate {
    area: u64,
    offset: u64,
    length: u64,
}

fn field_u32(f: &exif::Field) -> Option<u32> {
    match &f.value {
        Value::Short(v) => v.first().copied().map(u32::from),
        Value::Long(v) => v.first().copied(),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read DNG at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse TIFF/EXIF in DNG at {path}")]
    Exif {
        path: PathBuf,
        #[source]
        source: exif::Error,
    },
    #[error("no JPEG preview found in DNG at {path}")]
    NoJpegPreview { path: PathBuf },
    #[error("corrupt JPEG preview in DNG at {path}: {reason}")]
    CorruptPreview { path: PathBuf, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_dng_path_case_insensitive() {
        assert!(is_dng_path(Path::new("photo.dng")));
        assert!(is_dng_path(Path::new("photo.DNG")));
        assert!(is_dng_path(Path::new("photo.Dng")));
        assert!(is_dng_path(Path::new("/abs/path/photo.dng")));
        assert!(!is_dng_path(Path::new("photo.jpg")));
        assert!(!is_dng_path(Path::new("photo")));
        assert!(!is_dng_path(Path::new("dng")));
    }

    #[test]
    fn extract_fails_on_missing_file() {
        let err = extract_largest_jpeg_preview(Path::new("/nonexistent/file.dng")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
    }
}
