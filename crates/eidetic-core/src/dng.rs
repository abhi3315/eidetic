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
//! a SubIFD instead. That case isn't handled here yet. We'd add SubIFD
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

    // Bound the declared payload against the actual file BEFORE allocating.
    // offset and length are attacker-controlled u32s from the TIFF header:
    // without this check a 74-byte crafted file can declare a ~4 GiB preview
    // and the buffer is allocated first, aborting the process on allocation
    // failure instead of failing as one bad asset. `read_exact` would catch
    // the lie eventually, but only after the allocation already happened.
    let file_size = f
        .metadata()
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    match picked.offset.checked_add(picked.length) {
        Some(end) if end <= file_size => {}
        _ => {
            return Err(Error::CorruptPreview {
                path: path.to_path_buf(),
                reason: format!(
                    "preview claims bytes {}..{} but the file is {} bytes",
                    picked.offset,
                    picked.offset.saturating_add(picked.length),
                    file_size
                ),
            });
        }
    }

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

    // JPEG SOI marker check. Guards against the IFD pointing at garbage
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

    /// Minimal little-endian TIFF whose one IFD declares a JPEG preview
    /// (`Compression = 7`) at the given offset/length. This is the entire
    /// attacker-controlled surface for the allocation bound: nothing about
    /// the declared payload has to exist.
    fn tiff_declaring_preview(preview_offset: u32, preview_length: u32) -> Vec<u8> {
        let mut b = Vec::new();
        let u16le = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
        let u32le = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        // Header: "II", magic 42, first IFD at byte 8.
        b.extend_from_slice(b"II");
        u16le(&mut b, 42);
        u32le(&mut b, 8);
        // One IFD, 5 entries, tags ascending as TIFF requires.
        u16le(&mut b, 5);
        let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, value: u32| {
            u16le(b, tag);
            u16le(b, typ); // 3 = SHORT, 4 = LONG
            u32le(b, 1); // count
            u32le(b, value); // SHORT values sit in the low bytes, LE
        };
        entry(&mut b, 256, 4, 100); // ImageWidth
        entry(&mut b, 257, 4, 100); // ImageLength
        entry(&mut b, 259, 3, 7); // Compression = JPEG
        entry(&mut b, 513, 4, preview_offset); // JPEGInterchangeFormat
        entry(&mut b, 514, 4, preview_length); // JPEGInterchangeFormatLength
        u32le(&mut b, 0); // no next IFD
        b
    }

    /// F-03 from the 2026-09 review: a 74-byte file declaring a ~4 GiB
    /// preview must fail as one corrupt asset, not allocate the buffer (the
    /// old code allocated before any bound check — an OOM abort on hosts
    /// without lazy zero pages).
    #[test]
    fn preview_declared_past_eof_errors_without_allocating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hostile.dng");

        std::fs::write(&path, tiff_declaring_preview(8, u32::MAX - 8)).expect("write");
        let err = extract_largest_jpeg_preview(&path).unwrap_err();
        assert!(
            matches!(&err, Error::CorruptPreview { reason, .. } if reason.contains("but the file is")),
            "expected the bound check, got: {err:?}"
        );

        // Offset + length overflowing u32 arithmetic must also be caught
        // (they are widened to u64, so the sum must simply exceed the file).
        std::fs::write(&path, tiff_declaring_preview(u32::MAX, u32::MAX)).expect("write");
        let err = extract_largest_jpeg_preview(&path).unwrap_err();
        assert!(matches!(err, Error::CorruptPreview { .. }));
    }

    /// The bound must not reject a legitimate payload that fits exactly.
    #[test]
    fn preview_inside_the_file_still_extracts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ok.dng");

        let mut bytes = tiff_declaring_preview(0, 0); // placeholders
        let offset = bytes.len() as u32;
        let payload = [0xFFu8, 0xD8, 0xFF, 0xE0]; // JPEG SOI + marker
        bytes.extend_from_slice(&payload);
        // Rewrite the two placeholder values in entries 4 and 5 (bytes
        // 10 + 3*12 + 8 and 10 + 4*12 + 8).
        bytes[54..58].copy_from_slice(&offset.to_le_bytes());
        bytes[66..70].copy_from_slice(&(payload.len() as u32).to_le_bytes());

        std::fs::write(&path, &bytes).expect("write");
        let got = extract_largest_jpeg_preview(&path).expect("extract");
        assert_eq!(got, payload);
    }
}
