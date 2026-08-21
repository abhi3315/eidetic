use crate::{
    Error, Result, hash_file,
    store::{commit_staged, stage_file},
};
use eidetic_core::{AssetId, Paths, geocoder::Geocoder};
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
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

pub async fn import_file(
    path: &Path,
    repo: &AssetsRepo,
    config: &Paths,
    geocoder: Option<&Geocoder>,
) -> ImportOutcome {
    match try_import_file(path, repo, config, geocoder).await {
        Ok(outcome) => outcome,
        Err(e) => ImportOutcome::Failed(e),
    }
}

async fn try_import_file(
    path: &Path,
    repo: &AssetsRepo,
    config: &Paths,
    geocoder: Option<&Geocoder>,
) -> Result<ImportOutcome> {
    // MIME detection reads only 512 bytes, acceptable before staging.
    let Some(mime_type) = crate::meta::detect_mime(path)? else {
        return Ok(ImportOutcome::Skipped);
    };
    if !mime_type.starts_with("image/") && !mime_type.starts_with("video/") {
        return Ok(ImportOutcome::Skipped);
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
    let hash = hash_file(path)?;
    let hash_hex = hash.to_string();

    if let Some(existing_id) = repo.find_by_hash(&hash_hex).await? {
        return Ok(ImportOutcome::Duplicate(existing_id));
    }

    // Past the dedup gate. Stage a stable copy so EXIF and the CAS commit
    // both read the same bytes that will end up canonical at the CAS path.
    let stage = stage_file(path, &config.library_dir)?;

    let file_size = std::fs::metadata(stage.path())
        .map_err(|source| Error::Io {
            path: stage.path().to_path_buf(),
            source,
        })?
        .len();

    let exif = if mime_type.starts_with("image/") {
        crate::meta::extract_exif(stage.path())
    } else {
        crate::meta::ExifData::default()
    };

    // Videos get their metadata from ffprobe instead of EXIF (ADR-0011). A
    // probe failure downgrades to "imported without video metadata": the
    // bytes still land, and the thumbnail/embed backfills re-probe later.
    let video = if mime_type.starts_with("video/") {
        match crate::video::probe(stage.path()) {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                warn_ffmpeg_missing_once();
                None
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "ffprobe failed; importing video without metadata"
                );
                None
            }
        }
    } else {
        None
    };

    let storage_path = commit_staged(stage, &hash, ext.as_deref(), &config.library_dir)?;

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
    } else if mime_type.starts_with("video/") && crate::video::ffmpeg().is_some() {
        let src = storage_path.clone();
        let hash_clone = hash.clone();
        let lib = config.library_dir.clone();
        let duration = video.as_ref().and_then(|v| v.duration_secs);
        let result = tokio::task::spawn_blocking(move || {
            crate::thumbnail::generate_video_thumbnails(&src, &hash_clone, &lib, duration)
        })
        .await
        .expect("thumbnail thread panicked");
        match result {
            Ok(()) => true,
            Err(e) => {
                tracing::info!(
                    path = %path.display(),
                    error = %e,
                    "video thumbnail failed; asset stored without thumbnails (eidetic thumbnail will retry)"
                );
                false
            }
        }
    } else {
        if mime_type.starts_with("video/") {
            warn_ffmpeg_missing_once();
        }
        false
    };

    // Photos carry GPS in EXIF, iPhone videos in the QuickTime location tag;
    // either way the coordinates geocode identically.
    let date_taken = exif
        .date_taken
        .or_else(|| video.as_ref().and_then(|v| v.date_taken));
    let latitude = exif
        .latitude
        .or_else(|| video.as_ref().and_then(|v| v.latitude));
    let longitude = exif
        .longitude
        .or_else(|| video.as_ref().and_then(|v| v.longitude));

    let place = match (geocoder, latitude, longitude) {
        (Some(g), Some(lat), Some(lon)) => g.lookup(lat, lon),
        _ => None,
    };

    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path: storage_path.clone(),
        file_size,
        mime_type: Some(mime_type),
        date_taken,
        latitude,
        longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
        lens_make: exif.lens_make,
        lens_model: exif.lens_model,
        focal_length: exif.focal_length,
        focal_length_35mm: exif.focal_length_35mm,
        aperture: exif.aperture,
        shutter: exif.shutter,
        iso: exif.iso,
        orientation: exif.orientation,
        altitude: exif.altitude,
        gps_direction: exif.gps_direction,
        exif_raw: exif.raw,
        country_code: place.as_ref().map(|p| p.country_code.clone()),
        country_name: place.as_ref().map(|p| p.country_name.clone()),
        admin1: place.as_ref().map(|p| p.admin1.clone()),
        place: place.as_ref().map(|p| p.place.clone()),
        place_distance_m: place.as_ref().map(|p| p.distance_m),
        thumbnails_generated,
        duration_secs: video.as_ref().and_then(|v| v.duration_secs),
        video_codec: video.as_ref().and_then(|v| v.video_codec.clone()),
        pixel_width: video.as_ref().and_then(|v| v.width),
        pixel_height: video.as_ref().and_then(|v| v.height),
    };

    match repo.insert_asset(new_asset).await? {
        InsertOutcome::Inserted(id) => {
            debug!(path = %path.display(), hash = %hash_hex, "imported");
            Ok(ImportOutcome::Imported(id))
        }
        InsertOutcome::Existing(id) => {
            debug!(path = %path.display(), hash = %hash_hex, "duplicate");
            Ok(ImportOutcome::Duplicate(id))
        }
    }
}

/// Missing ffmpeg is one situation, not one situation per file: warn once
/// per process, then stay quiet.
fn warn_ffmpeg_missing_once() {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            "ffmpeg/ffprobe not found; videos import without metadata or thumbnails. \
             Install ffmpeg (or set EIDETIC_FFMPEG_PATH / EIDETIC_FFPROBE_PATH) and run \
             `eidetic thumbnail` + `eidetic embed` to backfill."
        );
    });
}

pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub skipped: u32,
    pub failed: Vec<(PathBuf, Error)>,
}

pub async fn import_dir(
    dir: &Path,
    repo: &AssetsRepo,
    config: &Paths,
    geocoder: Option<&Geocoder>,
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

        match import_file(entry.path(), repo, config, geocoder).await {
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
