use crate::{
    Error, Result, hash_file,
    store::{commit_staged, stage_file},
};
use eidetic_core::{AssetId, Paths};
use eidetic_db::{InsertOutcome, NewAsset, PgAssetsRepo};
use std::path::{Path, PathBuf};
use tracing::{debug, info};
use walkdir::WalkDir;

#[derive(Debug)]
pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Skipped,
    Failed(Error),
}

pub async fn import_file(path: &Path, repo: &PgAssetsRepo, config: &Paths) -> ImportOutcome {
    // MIME detection reads only 512 bytes — acceptable before staging.
    let mime_type = match crate::meta::detect_mime(path) {
        Ok(Some(m)) => m,
        Ok(None) => return ImportOutcome::Skipped,
        Err(e) => return ImportOutcome::Failed(e),
    };
    if !mime_type.starts_with("image/") && !mime_type.starts_with("video/") {
        return ImportOutcome::Skipped;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    let original_filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    // Hash the source directly so we can short-circuit duplicates before
    // copying the file to staging. On a re-import of a synced photo dir
    // this avoids gigabytes of pointless I/O per duplicate.
    let hash = match hash_file(path) {
        Ok(h) => h,
        Err(e) => return ImportOutcome::Failed(e),
    };
    let hash_hex = hash.to_string();

    match repo.find_by_hash(&hash_hex).await {
        Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
        Ok(None) => {}
        Err(e) => return ImportOutcome::Failed(Error::Db(e)),
    }

    // Past the dedup gate. Stage a stable copy so EXIF and the CAS commit
    // both read the same bytes that will end up canonical at the CAS path.
    let stage = match stage_file(path, &config.library_dir) {
        Ok(s) => s,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let file_size = match std::fs::metadata(stage.path()) {
        Ok(m) => m.len(),
        Err(source) => {
            return ImportOutcome::Failed(Error::Io {
                path: stage.path().to_path_buf(),
                source,
            });
        }
    };

    let exif = if mime_type.starts_with("image/") {
        crate::meta::extract_exif(stage.path())
    } else {
        crate::meta::ExifData::default()
    };

    let storage_path = match commit_staged(stage, &hash, ext.as_deref(), &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let thumbnails_generated = if mime_type.starts_with("image/") {
        let src = storage_path.clone();
        let hash_clone = hash.clone();
        let lib = config.library_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::thumbnail::generate_thumbnails(&src, &hash_clone, &lib)
        })
        .await
        .expect("thumbnail thread panicked");
        match result {
            Ok(()) => true,
            Err(e) => {
                tracing::info!(
                    path = %path.display(),
                    error = %e,
                    "thumbnail generation failed; asset stored without thumbnails (eidetic thumbnail will retry)"
                );
                false
            }
        }
    } else {
        false
    };

    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path: storage_path.clone(),
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
        thumbnails_generated,
    };

    match repo.insert_asset(new_asset).await {
        Ok(InsertOutcome::Inserted(id)) => {
            debug!(path = %path.display(), hash = %hash_hex, "imported");
            ImportOutcome::Imported(id)
        }
        Ok(InsertOutcome::Existing(id)) => {
            debug!(path = %path.display(), hash = %hash_hex, "duplicate");
            ImportOutcome::Duplicate(id)
        }
        Err(e) => ImportOutcome::Failed(Error::Db(e)),
    }
}

pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub skipped: u32,
    pub failed: Vec<(PathBuf, Error)>,
}

pub async fn import_dir(dir: &Path, repo: &PgAssetsRepo, config: &Paths) -> Result<ImportSummary> {
    if !dir.is_dir() {
        return Err(Error::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "path does not exist or is not a directory",
            ),
        });
    }

    info!(dir = %dir.display(), "starting import");

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

        match import_file(entry.path(), repo, config).await {
            ImportOutcome::Imported(_) => summary.imported += 1,
            ImportOutcome::Duplicate(_) => summary.duplicates += 1,
            ImportOutcome::Skipped => summary.skipped += 1,
            ImportOutcome::Failed(e) => {
                summary.failed.push((entry.path().to_path_buf(), e));
            }
        }
    }

    info!(
        imported = summary.imported,
        duplicates = summary.duplicates,
        skipped = summary.skipped,
        failed = summary.failed.len(),
        "import complete"
    );

    Ok(summary)
}
