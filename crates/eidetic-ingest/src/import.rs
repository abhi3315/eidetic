use crate::{
    Error, Result, hash_file,
    repo::{AssetIndex, NewAsset},
    store_file,
};
use eidetic_core::{AssetId, Paths};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug)]
pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Skipped,
    Failed(Error),
}

pub async fn import_file(path: &Path, index: &impl AssetIndex, config: &Paths) -> ImportOutcome {
    let mime_type = match crate::meta::detect_mime(path) {
        Ok(Some(m)) => m,
        Ok(None) => return ImportOutcome::Skipped,
        Err(e) => return ImportOutcome::Failed(e),
    };
    if !mime_type.starts_with("image/") && !mime_type.starts_with("video/") {
        return ImportOutcome::Skipped;
    }

    let file_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(source) => {
            return ImportOutcome::Failed(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let hash = match hash_file(path) {
        Ok(h) => h,
        Err(e) => return ImportOutcome::Failed(e),
    };

    match index.find_by_hash(&hash).await {
        Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
        Ok(None) => {}
        Err(e) => return ImportOutcome::Failed(e),
    }

    let storage_path = match store_file(path, &hash, &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let original_filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let exif = if mime_type.starts_with("image/") {
        crate::meta::extract_exif(path)
    } else {
        crate::meta::ExifData::default()
    };

    let new_asset = NewAsset {
        hash,
        original_filename,
        storage_path,
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
    };

    match index.insert_asset(new_asset).await {
        Ok(id) => ImportOutcome::Imported(id),
        Err(e) => ImportOutcome::Failed(e),
    }
}

pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub skipped: u32,
    pub failed: Vec<(PathBuf, Error)>,
}

pub async fn import_dir(
    dir: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> Result<ImportSummary> {
    if !dir.is_dir() {
        return Err(Error::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "path does not exist or is not a directory",
            ),
        });
    }

    let mut summary = ImportSummary {
        imported: 0,
        duplicates: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    for entry in WalkDir::new(dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let path = e.path().unwrap_or(dir).to_path_buf();
                let io_err = e
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("directory walk error"));
                summary.failed.push((
                    path.clone(),
                    Error::Io {
                        path,
                        source: io_err,
                    },
                ));
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        match import_file(entry.path(), index, config).await {
            ImportOutcome::Imported(_) => summary.imported += 1,
            ImportOutcome::Duplicate(_) => summary.duplicates += 1,
            ImportOutcome::Skipped => summary.skipped += 1,
            ImportOutcome::Failed(e) => {
                summary.failed.push((entry.path().to_path_buf(), e));
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::test_support::MockAssetIndex;
    use eidetic_core::{AssetId, Paths};
    use std::io::Write;
    use std::path::Path;

    fn make_paths(tmp: &tempfile::TempDir) -> Paths {
        Paths {
            import_dir: tmp.path().join("import"),
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        }
    }

    /// Write stub bytes that `infer` detects as JPEG (not a real JPEG — EXIF parsers will return defaults).
    /// Each call with a different `tag` produces a file with a different hash.
    fn write_jpeg(dir: &Path, name: &str, tag: &[u8]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        f.write_all(tag).unwrap();
        path
    }

    /// Write a file with non-media content (no recognizable magic bytes).
    fn write_non_media(dir: &Path, name: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"this is not a media file at all").unwrap();
        path
    }

    #[tokio::test]
    async fn new_file_is_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
        let index = MockAssetIndex::new();

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Imported(_)));
    }

    #[tokio::test]
    async fn known_hash_returns_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
        let index = MockAssetIndex::new();
        let hash = crate::hash_file(&src).unwrap();
        let existing_id = AssetId::new();
        index.seed(&hash, existing_id);

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Duplicate(id) if id == existing_id));
    }

    #[tokio::test]
    async fn missing_file_returns_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let index = MockAssetIndex::new();
        let nonexistent = tmp.path().join("nope.jpg");

        let outcome = import_file(&nonexistent, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn non_media_file_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_non_media(tmp.path(), "document.txt");
        let index = MockAssetIndex::new();

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Skipped));
    }

    #[tokio::test]
    async fn imported_file_has_jpeg_mime_type() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
        let index = MockAssetIndex::new();

        import_file(&src, &index, &paths).await;

        let inserted = index.all_inserted();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].mime_type, Some("image/jpeg".to_string()));
    }

    #[tokio::test]
    async fn dir_imports_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        write_jpeg(&src_dir, "a.jpg", b"a");
        write_jpeg(&src_dir, "b.jpg", b"b");
        write_jpeg(&src_dir, "c.jpg", b"c");
        let index = MockAssetIndex::new();

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 3);
        assert_eq!(summary.duplicates, 0);
        assert_eq!(summary.skipped, 0);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn dir_counts_duplicates_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        let file = write_jpeg(&src_dir, "photo.jpg", b"a");
        let index = MockAssetIndex::new();
        let hash = crate::hash_file(&file).unwrap();
        index.seed(&hash, AssetId::new());

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.duplicates, 1);
        assert_eq!(summary.skipped, 0);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn dir_counts_non_media_as_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("mixed");
        write_jpeg(&src_dir, "photo.jpg", b"a");
        write_non_media(&src_dir, "notes.txt");
        write_non_media(&src_dir, "archive.zip");
        let index = MockAssetIndex::new();

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 1);
        assert_eq!(summary.skipped, 2);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn dir_not_found_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let index = MockAssetIndex::new();
        let missing = tmp.path().join("does_not_exist");

        let result = import_dir(&missing, &index, &paths).await;
        assert!(matches!(result, Err(crate::Error::Io { .. })));
    }

    #[tokio::test]
    async fn dir_recurses_into_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        write_jpeg(&src_dir, "good.jpg", b"a");
        let subdir = src_dir.join("subdir");
        write_jpeg(&subdir, "nested.jpg", b"b");
        let index = MockAssetIndex::new();

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.duplicates, 0);
        assert!(summary.failed.is_empty());
    }
}
