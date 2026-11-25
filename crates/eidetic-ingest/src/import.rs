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
    // A cheap gate: 512 bytes off the source rejects documents and archives
    // before anything gets copied. It is not the answer we keep — these
    // bytes can still change under us — so the staged copy gets sniffed
    // again below.
    match crate::meta::detect_mime(path)? {
        Some(mime) if is_media(&mime) => {}
        _ => return Ok(ImportOutcome::Skipped),
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
    // this avoids gigabytes of pointless I/O per duplicate. This digest is
    // good for that gate and nothing else: it describes the source as it was
    // a moment ago, which is not necessarily what we end up storing.
    let source_hash = hash_file(path)?;

    if let Some(existing_id) = repo.find_by_hash(&source_hash.to_string()).await? {
        return Ok(ImportOutcome::Duplicate(existing_id));
    }

    // Past the dedup gate. Stage a stable copy so EXIF, the CAS commit and
    // the `hash` column all describe the same bytes — `hash` here is the
    // digest of the copy itself, computed as it was written.
    let (stage, hash) = stage_file(path, &config.library_dir)?;
    let hash_hex = hash.to_string();

    // Sniff the staged copy, and let it decide. Once we accept that the
    // source can change between two reads, the type has to come from the
    // same bytes as the digest: `mime_type` picks EXIF over ffprobe, the
    // thumbnail strategy, and what every reader later believes this asset
    // is. If the source became unsupported media, drop the staged copy
    // without inserting an asset or committing a CAS file.
    let Some(mime_type) = crate::meta::detect_mime(stage.path())? else {
        return Ok(ImportOutcome::Skipped);
    };
    if !is_media(&mime_type) {
        return Ok(ImportOutcome::Skipped);
    }

    // The source changed between the gate and the copy: an editor saving in
    // place, or a sync client that was still writing when the walk reached
    // the file. The staged bytes are what would be stored, so they get their
    // own duplicate check — without it the commit lands a CAS file that the
    // insert then rejects as a duplicate, and nothing ever removes it.
    if hash != source_hash
        && let Some(existing_id) = repo.find_by_hash(&hash_hex).await?
    {
        return Ok(ImportOutcome::Duplicate(existing_id));
    }

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

    let (storage_path, created) = commit_staged(stage, &hash, ext.as_deref(), &config.library_dir)?;

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

    // An insert error leaves a file we created in place on purpose: a
    // concurrent import of the same bytes and extension may have found it
    // at commit and be about to record it, and deleting it would leave that
    // row pointing at nothing. It is verified content under its own hash, so
    // the next import of it reuses it rather than tripping over it.
    match repo.insert_asset(new_asset).await? {
        InsertOutcome::Inserted(id) => {
            debug!(path = %path.display(), hash = %hash_hex, "imported");
            Ok(ImportOutcome::Imported(id))
        }
        InsertOutcome::Existing(id) => {
            // A concurrent import of the same bytes committed its row first.
            // If it stored them under another extension, the file this
            // import created is referenced by nothing and goes; a file this
            // import merely found, or the one the winning row points at,
            // stays. The winner's row is already committed and owns the
            // hash, so no later insert can come to reference ours.
            let winner = repo.fetch_by_id(id).await?.map(|a| a.storage_path);
            if created && winner.as_deref() != Some(storage_path.as_path()) {
                std::fs::remove_file(&storage_path).map_err(|source| Error::Io {
                    path: storage_path.clone(),
                    source,
                })?;
            }
            debug!(path = %path.display(), hash = %hash_hex, "duplicate");
            Ok(ImportOutcome::Duplicate(id))
        }
    }
}

/// Photos and videos only — `infer` recognises a great deal more than that,
/// and everything else is somebody else's file.
fn is_media(mime: &str) -> bool {
    mime.starts_with("image/") || mime.starts_with("video/")
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
