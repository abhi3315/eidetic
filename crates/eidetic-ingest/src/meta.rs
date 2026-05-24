use crate::{Error, Result};
use chrono::{DateTime, Utc};
use exif::{In, Reader, Tag, Value};
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct ExifData {
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub aperture: Option<f32>,
    pub shutter: Option<String>,
    pub iso: Option<i32>,
    pub orientation: Option<i16>,
    pub altitude: Option<f64>,
    pub gps_direction: Option<f64>,
    pub raw: Option<serde_json::Value>,
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
        lens_make: read_ascii(&exif, Tag::LensMake),
        lens_model: read_ascii(&exif, Tag::LensModel),
        focal_length: read_first_rational(&exif, Tag::FocalLength)
            .map(|(n, d)| n as f32 / d as f32),
        focal_length_35mm: read_first_unsigned(&exif, Tag::FocalLengthIn35mmFilm).map(|n| n as f32),
        aperture: read_first_rational(&exif, Tag::FNumber).map(|(n, d)| n as f32 / d as f32),
        shutter: read_shutter(&exif),
        iso: read_first_unsigned(&exif, Tag::PhotographicSensitivity)
            .or_else(|| read_first_unsigned(&exif, Tag::ISOSpeed))
            .map(|n| n as i32),
        orientation: read_first_unsigned(&exif, Tag::Orientation).map(|n| n as i16),
        altitude: read_altitude(&exif),
        gps_direction: read_first_rational(&exif, Tag::GPSImgDirection)
            .map(|(n, d)| n as f64 / d as f64),
        raw: Some(build_raw_jsonb(&exif)),
    }
}

impl From<ExifData> for eidetic_db::ExifUpdate {
    fn from(d: ExifData) -> Self {
        Self {
            date_taken: d.date_taken,
            latitude: d.latitude,
            longitude: d.longitude,
            camera_make: d.camera_make,
            camera_model: d.camera_model,
            lens_make: d.lens_make,
            lens_model: d.lens_model,
            focal_length: d.focal_length,
            focal_length_35mm: d.focal_length_35mm,
            aperture: d.aperture,
            shutter: d.shutter,
            iso: d.iso,
            orientation: d.orientation,
            altitude: d.altitude,
            gps_direction: d.gps_direction,
            raw: d.raw,
        }
    }
}

fn read_first_rational(exif: &exif::Exif, tag: Tag) -> Option<(u32, u32)> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Rational(r) => r.first().filter(|r| r.denom != 0).map(|r| (r.num, r.denom)),
        _ => None,
    }
}

fn read_first_unsigned(exif: &exif::Exif, tag: Tag) -> Option<u32> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Short(v) => v.first().map(|&n| n as u32),
        Value::Long(v) => v.first().copied(),
        Value::Byte(v) => v.first().map(|&n| n as u32),
        _ => None,
    }
}

fn read_shutter(exif: &exif::Exif) -> Option<String> {
    let (num, denom) = read_first_rational(exif, Tag::ExposureTime)?;
    Some(format_shutter(num, denom))
}

fn format_shutter(num: u32, denom: u32) -> String {
    if num == 1 && denom >= 1 {
        format!("1/{denom}")
    } else {
        let secs = num as f64 / denom as f64;
        format!("{secs:.3}")
    }
}

fn read_altitude(exif: &exif::Exif) -> Option<f64> {
    let (num, denom) = read_first_rational(exif, Tag::GPSAltitude)?;
    let meters = num as f64 / denom as f64;
    let below = matches!(
        exif.get_field(Tag::GPSAltitudeRef, In::PRIMARY)
            .map(|f| &f.value),
        Some(Value::Byte(b)) if b.first() == Some(&1)
    );
    Some(if below { -meters } else { meters })
}

// Per-tag display strings keyed by the tag's Display form (e.g. "LensMake",
// "FNumber"; "Tag(Tiff, 50706)" for tags kamadak doesn't name). IFD 1
// (thumbnail metadata) is omitted because it duplicates IFD 0 and adds the
// JPEG-thumb pointer we don't store. MakerNote is omitted because kamadak
// surfaces it as a multi-KB opaque hex blob.
fn build_raw_jsonb(exif: &exif::Exif) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for field in exif.fields() {
        if field.ifd_num != In::PRIMARY || field.tag == Tag::MakerNote {
            continue;
        }
        let key = field.tag.to_string();
        let value = field.display_value().with_unit(exif).to_string();
        obj.insert(key, serde_json::Value::String(value));
    }
    serde_json::Value::Object(obj)
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
                // EXIF stores local time with no timezone. Treating as UTC is a known
                // imprecision acceptable for a single-user personal library.
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

    if rationals[0].denom == 0 || rationals[1].denom == 0 || rationals[2].denom == 0 {
        return None;
    }
    let d = rationals[0].num as f64 / rationals[0].denom as f64;
    let m = rationals[1].num as f64 / rationals[1].denom as f64;
    let s = rationals[2].num as f64 / rationals[2].denom as f64;
    let decimal = d + m / 60.0 + s / 3600.0;

    let ref_char = match &ref_field.value {
        Value::Ascii(vecs) => vecs
            .first()
            .and_then(|b| b.first())
            .map(|&b| char::from(b))?,
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
    fn extract_exif_is_non_fatal_on_corrupt_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        // JPEG SOI + truncated/corrupt APP1
        let corrupt: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x10, 0x00, 0x00, 0xFF];
        let path = write_bytes(tmp.path(), "corrupt.jpg", corrupt);
        let data = extract_exif(&path);
        assert!(data.camera_make.is_none());
    }

    #[test]
    fn extract_exif_reads_gps_coords() {
        let tmp = tempfile::tempdir().unwrap();
        // Hand-crafted JPEG with Make="TestCam", GPSLatitude=37°46'12"N, GPSLongitude=122°25'9"W
        // TIFF (little-endian): IFD0 → Make + GPSInfoIFD pointer → GPS IFD with 4 entries
        const JPEG_WITH_GPS: &[u8] = &[
            // JPEG SOI
            0xFF, 0xD8, // APP1 marker + length 156 (0x009C)
            0xFF, 0xE1, 0x00, 0x9C, // Exif\0\0
            0x45, 0x78, 0x69, 0x66, 0x00, 0x00, // TIFF LE header, IFD0 at offset 8
            0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, // IFD0: 2 entries
            0x02, 0x00, // Make (0x010F): ASCII, count=8, offset=38
            0x0F, 0x01, 0x02, 0x00, 0x08, 0x00, 0x00, 0x00, 0x26, 0x00, 0x00, 0x00,
            // GPSInfoIFD (0x8825): LONG, count=1, value=46
            0x25, 0x88, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x2E, 0x00, 0x00, 0x00,
            // IFD0 next = 0
            0x00, 0x00, 0x00, 0x00, // "TestCam\0" at offset 38
            0x54, 0x65, 0x73, 0x74, 0x43, 0x61, 0x6D, 0x00,
            // GPS IFD at offset 46: 4 entries
            0x04, 0x00, // GPSLatitudeRef (0x0001): ASCII, count=2, "N\0" inline
            0x01, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x4E, 0x00, 0x00, 0x00,
            // GPSLatitude (0x0002): RATIONAL, count=3, offset=100
            0x02, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
            // GPSLongitudeRef (0x0003): ASCII, count=2, "W\0" inline
            0x03, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x57, 0x00, 0x00, 0x00,
            // GPSLongitude (0x0004): RATIONAL, count=3, offset=124
            0x04, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x7C, 0x00, 0x00, 0x00,
            // GPS next IFD = 0
            0x00, 0x00, 0x00, 0x00,
            // GPSLatitude rationals at offset 100: 37/1, 46/1, 12/1
            0x25, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x2E, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            // GPSLongitude rationals at offset 124: 122/1, 25/1, 9/1
            0x7A, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // JPEG EOI
            0xFF, 0xD9,
        ];
        let path = write_bytes(tmp.path(), "gps.jpg", JPEG_WITH_GPS);
        let data = extract_exif(&path);
        // 37 + 46/60 + 12/3600 = 37.770°
        assert!(
            (data.latitude.unwrap() - 37.770).abs() < 1e-4,
            "lat={:?}",
            data.latitude
        );
        // West → negated: -(122 + 25/60 + 9/3600) ≈ -122.419°
        assert!(
            (data.longitude.unwrap() - (-122.419)).abs() < 1e-3,
            "lon={:?}",
            data.longitude
        );
        assert_eq!(data.camera_make.as_deref(), Some("TestCam"));

        // JSONB walker reflects parsed fields. ASCII values keep their quotes
        // per kamadak's display convention.
        let raw = data.raw.as_ref().expect("raw populated when EXIF parses");
        let obj = raw.as_object().unwrap();
        assert_eq!(
            obj.get("Make").and_then(|v| v.as_str()),
            Some("\"TestCam\"")
        );
        assert!(obj.contains_key("GPSLatitude"));
    }

    #[test]
    fn format_shutter_branches() {
        assert_eq!(format_shutter(1, 25), "1/25");
        assert_eq!(format_shutter(30, 10), "3.000");
    }
}
