use eidetic_core::{Config, Paths};
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
use eidetic_ingest::{ImportOutcome, hash_file, import_file, thumbnail};
use image::{ImageBuffer, ImageFormat, Rgb};
use std::path::{Path, PathBuf};

/// One temp dir holds both the SQLite file and the library tree. The returned
/// `TempDir` guard must outlive the test body, or the database file is
/// deleted mid-test.
async fn fixture() -> (AssetsRepo, Paths, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_path: tmp.path().join("eidetic.db"),
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);
    (repo, config.paths, tmp)
}

/// Write a real JPEG (not a 4-byte stub) so `image::decode` succeeds.
fn write_real_jpeg(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    img.save_with_format(&path, ImageFormat::Jpeg).unwrap();
    path
}

#[tokio::test]
async fn imported_image_has_thumbnails_generated_true() {
    let (repo, paths, tmp) = fixture().await;
    let src = write_real_jpeg(tmp.path(), "photo.jpg", 800, 600);

    let outcome = import_file(&src, &repo, &paths, None).await;
    let _id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert!(
        pending.is_empty(),
        "newly imported image should have thumbnails_generated = true; pending={pending:?}"
    );

    let hash = hash_file(&src).unwrap();
    let small = thumbnail::thumbnail_path(&paths.library_dir, &hash, thumbnail::ThumbSize::Small);
    let medium = thumbnail::thumbnail_path(&paths.library_dir, &hash, thumbnail::ThumbSize::Medium);
    assert!(small.exists(), "small thumb missing at {small:?}");
    assert!(medium.exists(), "medium thumb missing at {medium:?}");
}

#[tokio::test]
async fn corrupt_video_lands_and_stays_thumbnail_pending() {
    let (repo, paths, tmp) = fixture().await;

    // Stub video: 12 bytes of an MP4-ish header that `infer` accepts.
    // The byte pattern 00 00 00 20 66 74 79 70 69 73 6F 6D = "....ftypisom"
    // is the canonical mp4 brand identifier, so `infer` returns video/mp4.
    let src = tmp.path().join("clip.mp4");
    std::fs::write(
        &src,
        [
            0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6F, 0x6D,
        ],
    )
    .expect("write stub mp4");

    let outcome = import_file(&src, &repo, &paths, None).await;
    let _id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported (video should still ingest), got {other:?}"),
    };

    // Since ADR-0011 videos ARE thumbnail candidates: the stub is corrupt,
    // so generation fails at import, and the row stays pending for the
    // `eidetic thumbnail` backfill to retry.
    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(
        pending.len(),
        1,
        "video should be retry-pending in the unthumbnailed list; got {pending:?}"
    );
    assert!(pending[0].mime_type.starts_with("video/"));
}

#[tokio::test]
async fn corrupt_image_lands_but_thumbnails_stay_pending() {
    let (repo, paths, tmp) = fixture().await;

    // 4-byte JPEG-magic stub: passes `infer` (which only checks the
    // first few bytes) but `image::decode` rejects it. The asset row
    // should land; the row should appear in fetch_unthumbnailed for retry.
    let src = tmp.path().join("corrupt.jpg");
    std::fs::write(&src, [0xFF, 0xD8, 0xFF, 0xE0]).expect("write stub");

    let outcome = import_file(&src, &repo, &paths, None).await;
    let id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported even on corrupt image, got {other:?}"),
    };

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(pending.len(), 1, "corrupt image should be retry-pending");
    assert_eq!(
        pending[0].id, id,
        "the pending row should be the one we imported"
    );
}

#[tokio::test]
async fn backfill_flow_generates_and_marks_pending_image() {
    let (repo, paths, tmp) = fixture().await;

    // Seed the DB with a row whose thumbnails_generated = FALSE,
    // simulating an asset imported before this feature existed.
    let src = write_real_jpeg(tmp.path(), "src.jpg", 400, 300);
    let hash = hash_file(&src).unwrap();
    let hash_hex = hash.to_string();
    let asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename: "src.jpg".to_string(),
        storage_path: src.clone(),
        file_size: std::fs::metadata(&src).unwrap().len(),
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        lens_make: None,
        lens_model: None,
        focal_length: None,
        focal_length_35mm: None,
        aperture: None,
        shutter: None,
        iso: None,
        orientation: None,
        altitude: None,
        gps_direction: None,
        exif_raw: None,
        country_code: None,
        country_name: None,
        admin1: None,
        place: None,
        place_distance_m: None,
        thumbnails_generated: false,
        duration_secs: None,
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    };
    let id = match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    // Simulate the backfill: fetch, generate, mark.
    let pending = repo.fetch_unthumbnailed().await.expect("fetch before");
    assert_eq!(pending.len(), 1);
    let pending = pending.into_iter().next().unwrap();
    assert_eq!(pending.id, id);
    assert_eq!(pending.hash.to_string(), hash_hex);

    thumbnail::generate_thumbnails(&pending.storage_path, &pending.hash, &paths.library_dir)
        .expect("generate");
    repo.mark_thumbnailed(id).await.expect("mark");

    // After: row no longer pending, files on disk.
    let after = repo.fetch_unthumbnailed().await.expect("fetch after");
    assert!(after.is_empty());

    let small = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending.hash,
        thumbnail::ThumbSize::Small,
    );
    let medium = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending.hash,
        thumbnail::ThumbSize::Medium,
    );
    assert!(small.exists());
    assert!(medium.exists());
}
