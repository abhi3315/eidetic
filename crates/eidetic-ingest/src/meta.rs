use crate::{Error, Result};
use chrono::{DateTime, Utc};
use std::io::Read;
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
}
