//! Thumbnail generation for images.
//!
//! Two JPEG sizes (256/1024 longest edge), stored under
//! `<library_dir>/.thumbs/{s,m}/<hash[..2]>/<hash[2..4]>/<hash>.jpg`.

use crate::{Error, Result};
use eidetic_core::Sha256;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use std::path::{Path, PathBuf};

/// JPEG quality used for all sizes.
///
/// 85 is the classic photo-thumbnail value: visually indistinguishable
/// from quality 95 at human distances, ~30% smaller. The personal-use
/// trade-off favours bytes saved.
const JPEG_QUALITY: u8 = 85;

/// Thumbnail size variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbSize {
    /// 256px longest edge — grid previews, search result strips.
    Small,
    /// 1024px longest edge — single-photo card view.
    Medium,
}

impl ThumbSize {
    /// Directory segment under `<library_dir>/.thumbs/`. Stable for the
    /// lifetime of the data — adding a new size adds a new letter, never
    /// renames an existing one.
    pub fn dir_segment(self) -> &'static str {
        match self {
            ThumbSize::Small => "s",
            ThumbSize::Medium => "m",
        }
    }

    /// Longest edge in pixels. Aspect ratio is preserved during resize.
    pub fn longest_edge(self) -> u32 {
        match self {
            ThumbSize::Small => 256,
            ThumbSize::Medium => 1024,
        }
    }
}

/// On-disk path for a thumbnail. Derived purely from `hash` and `size`;
/// no DB lookup needed to locate the file.
pub fn thumbnail_path(library_dir: &Path, hash: &Sha256, size: ThumbSize) -> PathBuf {
    let hex = hash.to_string();
    library_dir
        .join(".thumbs")
        .join(size.dir_segment())
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.jpg"))
}

/// Generate both 256px and 1024px JPEG thumbnails for `src`.
///
/// Synchronous; CPU-bound. Call inside `tokio::task::spawn_blocking`.
///
/// Returns `Ok(())` only when both thumbnails wrote successfully. On
/// failure of either size, returns `Err`; partial files (if any) are
/// left on disk for a future run to overwrite.
pub fn generate_thumbnails(src: &Path, hash: &Sha256, library_dir: &Path) -> Result<()> {
    let img = load_image_with_limits(src)?;

    for size in [ThumbSize::Small, ThumbSize::Medium] {
        let dest = thumbnail_path(library_dir, hash, size);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let edge = size.longest_edge();
        // Only downscale. If the source already fits inside the bounding box
        // on both axes, write the original dimensions — upscaling makes files
        // larger for no quality gain.
        let resized = if img.width() <= edge && img.height() <= edge {
            img.clone()
        } else {
            img.resize(edge, edge, FilterType::Lanczos3)
        };

        let rgb = resized.to_rgb8();

        let mut bytes: Vec<u8> = Vec::new();
        let encoder = JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY);
        rgb.write_with_encoder(encoder)
            .map_err(|source| Error::JpegEncode {
                path: dest.clone(),
                source,
            })?;

        std::fs::write(&dest, &bytes).map_err(|source| Error::Io { path: dest, source })?;
    }

    Ok(())
}

/// Open and decode an image with the same decompression-bomb limits the
/// ML preprocessing uses. Identical guards: 512 MB allocation cap, 16384
/// max width/height.
fn load_image_with_limits(path: &Path) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    reader = reader.with_guessed_format().map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);

    reader.decode().map_err(|source| Error::ImageDecode {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::Sha256;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::path::PathBuf;

    fn fixture_hash() -> Sha256 {
        Sha256::from_hex("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899")
            .expect("test hash is valid 64-char hex")
    }

    fn make_real_jpeg(dir: &Path, w: u32, h: u32) -> PathBuf {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let path = dir.join("src.jpg");
        img.save_with_format(&path, ImageFormat::Jpeg)
            .expect("write fixture jpeg");
        path
    }

    #[test]
    fn thumb_size_dir_segment_is_stable() {
        assert_eq!(ThumbSize::Small.dir_segment(), "s");
        assert_eq!(ThumbSize::Medium.dir_segment(), "m");
    }

    #[test]
    fn thumb_size_longest_edge() {
        assert_eq!(ThumbSize::Small.longest_edge(), 256);
        assert_eq!(ThumbSize::Medium.longest_edge(), 1024);
    }

    #[test]
    fn thumbnail_path_derivation() {
        let hash = fixture_hash();
        let lib = PathBuf::from("/library");
        let small = thumbnail_path(&lib, &hash, ThumbSize::Small);
        let medium = thumbnail_path(&lib, &hash, ThumbSize::Medium);

        assert_eq!(
            small,
            PathBuf::from(
                "/library/.thumbs/s/aa/bb/aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899.jpg"
            )
        );
        assert_eq!(
            medium,
            PathBuf::from(
                "/library/.thumbs/m/aa/bb/aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899.jpg"
            )
        );
    }

    #[test]
    fn generate_writes_both_sizes() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_real_jpeg(tmp.path(), 800, 600);
        let library = tmp.path().join("library");
        let hash = fixture_hash();

        generate_thumbnails(&src, &hash, &library).expect("generate");

        let small = thumbnail_path(&library, &hash, ThumbSize::Small);
        let medium = thumbnail_path(&library, &hash, ThumbSize::Medium);
        assert!(small.exists(), "small thumbnail missing at {small:?}");
        assert!(medium.exists(), "medium thumbnail missing at {medium:?}");

        let s_img = image::ImageReader::open(&small).unwrap().decode().unwrap();
        let m_img = image::ImageReader::open(&medium).unwrap().decode().unwrap();
        assert_eq!(s_img.width(), 256);
        assert_eq!(s_img.height(), 192);
        assert_eq!(m_img.width(), 800);
        assert_eq!(m_img.height(), 600);
    }

    #[test]
    fn generate_fails_on_corrupt_input() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = tmp.path().join("corrupt.jpg");
        std::fs::write(&src, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        let library = tmp.path().join("library");
        let hash = fixture_hash();

        let result = generate_thumbnails(&src, &hash, &library);
        assert!(
            result.is_err(),
            "expected Err on corrupt input, got {result:?}"
        );
    }

    #[test]
    fn generate_creates_intermediate_directories() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_real_jpeg(tmp.path(), 100, 100);
        let nested_library = tmp.path().join("deeply").join("nested").join("library");
        let hash = fixture_hash();

        generate_thumbnails(&src, &hash, &nested_library).expect("generate");
        assert!(thumbnail_path(&nested_library, &hash, ThumbSize::Small).exists());
        assert!(thumbnail_path(&nested_library, &hash, ThumbSize::Medium).exists());
    }
}
