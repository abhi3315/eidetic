use crate::{
    Error, hash_file,
    repo::{AssetIndex, NewAsset},
    store_file,
};
use eidetic_core::{AssetId, Paths};
use std::path::Path;

pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Failed(Error),
}

pub async fn import_file(path: &Path, index: &impl AssetIndex, config: &Paths) -> ImportOutcome {
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

    let new_asset = NewAsset {
        hash,
        original_filename,
        storage_path,
        file_size,
        mime_type: None,
    };

    match index.insert_asset(new_asset).await {
        Ok(id) => ImportOutcome::Imported(id),
        Err(e) => ImportOutcome::Failed(e),
    }
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

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[tokio::test]
    async fn new_file_is_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_file(tmp.path(), "photo.jpg", b"fake image");
        let index = MockAssetIndex::new();

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Imported(_)));
    }

    #[tokio::test]
    async fn known_hash_returns_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_file(tmp.path(), "photo.jpg", b"fake image");
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
}
