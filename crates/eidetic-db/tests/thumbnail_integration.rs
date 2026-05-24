use eidetic_core::{Config, Paths};
use eidetic_db::{InsertOutcome, NewAsset, PgAssetsRepo};
use eidetic_ingest::{ImportOutcome, hash_file, import_file, thumbnail};
use image::{ImageBuffer, ImageFormat, Rgb};
use std::path::{Path, PathBuf};
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

async fn start_db() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("tensorchord/vchord-postgres", "pg17-v0.4.3")
        .with_wait_for(WaitFor::message_on_stderr("ready to accept connections"))
        .with_env_var("POSTGRES_USER", "eidetic")
        .with_env_var("POSTGRES_PASSWORD", "eidetic")
        .with_env_var("POSTGRES_DB", "eidetic")
        .start()
        .await
        .expect("failed to start postgres container");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://eidetic:eidetic@127.0.0.1:{port}/eidetic");
    (container, url)
}

async fn fixture() -> (
    PgAssetsRepo,
    Paths,
    tempfile::TempDir,
    testcontainers::ContainerAsync<GenericImage>,
) {
    let (container, url) = start_db().await;
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_url: url,
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);
    (repo, config.paths, tmp, container)
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
    let (repo, paths, tmp, _container) = fixture().await;
    let src = write_real_jpeg(tmp.path(), "photo.jpg", 800, 600);

    let outcome = import_file(&src, &repo, &paths).await;
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
async fn imported_video_stays_thumbnails_generated_false() {
    let (repo, paths, tmp, _container) = fixture().await;

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

    let outcome = import_file(&src, &repo, &paths).await;
    let _id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported (video should still ingest), got {other:?}"),
    };

    // Video should NOT appear in the unthumbnailed list (filter is
    // `mime_type LIKE 'image/%'`).
    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert!(
        pending.is_empty(),
        "video should not appear in unthumbnailed list; got {pending:?}"
    );
}

#[tokio::test]
async fn corrupt_image_lands_but_thumbnails_stay_pending() {
    let (repo, paths, tmp, _container) = fixture().await;

    // 4-byte JPEG-magic stub: passes `infer` (which only checks the
    // first few bytes) but `image::decode` rejects it. The asset row
    // should land; the row should appear in fetch_unthumbnailed for retry.
    let src = tmp.path().join("corrupt.jpg");
    std::fs::write(&src, [0xFF, 0xD8, 0xFF, 0xE0]).expect("write stub");

    let outcome = import_file(&src, &repo, &paths).await;
    let id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported even on corrupt image, got {other:?}"),
    };

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(pending.len(), 1, "corrupt image should be retry-pending");
    let (pending_id, _hash, _path) = &pending[0];
    assert_eq!(
        *pending_id, id,
        "the pending row should be the one we imported"
    );
}

#[tokio::test]
async fn backfill_flow_generates_and_marks_pending_image() {
    let (repo, paths, tmp, _container) = fixture().await;

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
        thumbnails_generated: false,
    };
    let id = match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    // Simulate the backfill: fetch, generate, mark.
    let pending = repo.fetch_unthumbnailed().await.expect("fetch before");
    assert_eq!(pending.len(), 1);
    let (pending_id, pending_hash, pending_path) = pending.into_iter().next().unwrap();
    assert_eq!(pending_id, id);
    assert_eq!(pending_hash.to_string(), hash_hex);

    thumbnail::generate_thumbnails(&pending_path, &pending_hash, &paths.library_dir)
        .expect("generate");
    repo.mark_thumbnailed(id).await.expect("mark");

    // After: row no longer pending, files on disk.
    let after = repo.fetch_unthumbnailed().await.expect("fetch after");
    assert!(after.is_empty());

    let small = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending_hash,
        thumbnail::ThumbSize::Small,
    );
    let medium = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending_hash,
        thumbnail::ThumbSize::Medium,
    );
    assert!(small.exists());
    assert!(medium.exists());
}
