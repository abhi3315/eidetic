use crate::{Error, Result};
use chrono::{DateTime, Utc};
use exif::{In, Reader, Tag, Value};
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Default)]
pub struct ExifData {
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

pub fn detect_mime(path: &Path) -> Result<Option<String>> {
    let mut buf = [0u8; 512];
    let mut file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let n = file.read(&mut buf).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(infer::get(&buf[..n]).map(|t| t.mime_type().to_string()))
}

pub fn extract_exif(path: &Path) -> ExifData {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!("cannot open {:?} for EXIF: {e}", path);
            return ExifData::default();
        }
    };
    let exif = match Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("no parseable EXIF in {:?}: {e}", path);
            return ExifData::default();
        }
    };

    ExifData {
        date_taken: read_datetime(&exif),
        latitude: read_gps_coord(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, &['S']),
        longitude: read_gps_coord(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, &['W']),
        camera_make: read_ascii(&exif, Tag::Make),
        camera_model: read_ascii(&exif, Tag::Model),
    }
}

fn read_ascii(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match &field.value {
        Value::Ascii(vecs) => vecs.first().and_then(|bytes| {
            std::str::from_utf8(bytes)
                .ok()
                .map(|s| s.trim_end_matches('\0').trim().to_string())
        }),
        _ => None,
    }
}

fn read_datetime(exif: &exif::Exif) -> Option<DateTime<Utc>> {
    let field = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY)?;
    match &field.value {
        Value::Ascii(vecs) => vecs.first().and_then(|bytes| {
            let s = std::str::from_utf8(bytes).ok()?.trim_end_matches('\0');
            chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S")
                .ok()
                .map(|ndt| ndt.and_utc())
        }),
        _ => None,
    }
}

fn read_gps_coord(
    exif: &exif::Exif,
    coord_tag: Tag,
    ref_tag: Tag,
    negate_refs: &[char],
) -> Option<f64> {
    let coord = exif.get_field(coord_tag, In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, In::PRIMARY)?;

    let rationals = match &coord.value {
        Value::Rational(v) if v.len() >= 3 => v,
        _ => return None,
    };

    let d = rationals[0].num as f64 / rationals[0].denom as f64;
    let m = rationals[1].num as f64 / rationals[1].denom as f64;
    let s = rationals[2].num as f64 / rationals[2].denom as f64;
    let decimal = d + m / 60.0 + s / 3600.0;

    let ref_char = match &ref_field.value {
        Value::Ascii(vecs) => vecs
            .first()
            .and_then(|b| b.first())
            .and_then(|&b| char::from_u32(b as u32))?,
        _ => return None,
    };

    if negate_refs.contains(&ref_char) {
        Some(-decimal)
    } else {
        Some(decimal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn write_bytes(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(content)
            .unwrap();
        path
    }

    #[test]
    fn jpeg_magic_returns_image_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_bytes(tmp.path(), "img.jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
        assert_eq!(detect_mime(&path).unwrap(), Some("image/jpeg".to_string()));
    }

    #[test]
    fn png_magic_returns_image_png() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_bytes(
            tmp.path(),
            "img.png",
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        );
        assert_eq!(detect_mime(&path).unwrap(), Some("image/png".to_string()));
    }

    #[test]
    fn mp4_magic_returns_video_type() {
        let tmp = tempfile::tempdir().unwrap();
        // ftyp box: 4-byte length + "ftyp" + "isom"
        let mp4: &[u8] = &[
            0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6F, 0x6D, 0x00, 0x00,
            0x02, 0x00,
        ];
        let path = write_bytes(tmp.path(), "video.mp4", mp4);
        let mime = detect_mime(&path).unwrap();
        assert!(
            mime.as_deref()
                .map(|m| m.starts_with("video/"))
                .unwrap_or(false),
            "expected video/* MIME, got {mime:?}"
        );
    }

    #[test]
    fn unknown_bytes_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_bytes(tmp.path(), "blob.bin", b"hello world this is not media");
        assert_eq!(detect_mime(&path).unwrap(), None);
    }

    #[test]
    fn nonexistent_path_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        let result = detect_mime(&tmp.path().join("ghost.jpg"));
        assert!(result.is_err());
    }

    #[test]
    fn extract_exif_returns_default_on_random_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_bytes(tmp.path(), "blob.bin", b"random bytes not exif at all here");
        let data = extract_exif(&path);
        assert!(data.date_taken.is_none());
        assert!(data.latitude.is_none());
        assert!(data.longitude.is_none());
        assert!(data.camera_make.is_none());
        assert!(data.camera_model.is_none());
    }

    #[test]
    fn extract_exif_is_non_fatal_on_corrupt_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        // JPEG SOI + truncated/corrupt APP1
        let corrupt: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x10, 0x00, 0x00, 0xFF];
        let path = write_bytes(tmp.path(), "corrupt.jpg", corrupt);
        let data = extract_exif(&path);
        assert!(data.camera_make.is_none());
    }

    #[test]
    fn extract_exif_reads_make_from_minimal_exif() {
        let tmp = tempfile::tempdir().unwrap();
        // Hand-crafted JPEG with a single EXIF tag: Make = "TestCam"
        // Structure: SOI + APP1 (Exif header + little-endian TIFF with 1 IFD entry) + EOI
        const JPEG_WITH_MAKE: &[u8] = &[
            0xFF, 0xD8, // JPEG SOI
            0xFF, 0xE1, 0x00, 0x2A, // APP1 marker, length=42
            0x45, 0x78, 0x69, 0x66, 0x00, 0x00, // "Exif\0\0"
            0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, // TIFF LE, IFD0 at offset 8
            0x01, 0x00, // 1 IFD entry
            0x0F, 0x01, 0x02, 0x00, 0x08, 0x00, 0x00, 0x00, // Make: type ASCII, count 8
            0x1A, 0x00, 0x00, 0x00, // value at TIFF offset 26
            0x00, 0x00, 0x00, 0x00, // next IFD = none
            0x54, 0x65, 0x73, 0x74, 0x43, 0x61, 0x6D, 0x00, // "TestCam\0"
            0xFF, 0xD9, // JPEG EOI
        ];
        let path = write_bytes(tmp.path(), "make_only.jpg", JPEG_WITH_MAKE);
        let data = extract_exif(&path);
        assert_eq!(data.camera_make.as_deref(), Some("TestCam"));
        // No other tags in this fixture
        assert!(data.date_taken.is_none());
        assert!(data.latitude.is_none());
    }
}
