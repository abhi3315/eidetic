//! Integration tests for `eidetic_ingest::import_file` / `import_dir`
//! against a real `PgAssetsRepo` backed by a testcontainers Postgres.
//!
//! These replace the `MockAssetIndex`-driven unit tests that lived in
//! `crates/eidetic-ingest/src/import.rs` before the trait was deleted.

use eidetic_core::{Config, Paths};
use eidetic_db::{InsertOutcome, NewAsset, PgAssetsRepo};
use eidetic_ingest::{ImportOutcome, hash_file, import_dir, import_file};
use sqlx::PgPool;
use std::io::Write;
use std::path::{Path, PathBuf};
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};
use walkdir::WalkDir;

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

/// Boot a fresh DB, return everything the tests need:
/// - a `PgAssetsRepo` for the public API
/// - the raw `PgPool` for column-level assertions
/// - a `Paths` whose `library_dir` lives under a fresh tempdir
/// - the `TempDir` (kept alive so the tempdir isn't dropped)
/// - the container handle (kept alive so the DB isn't torn down)
async fn fixture() -> (
    PgAssetsRepo,
    PgPool,
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
    let repo = PgAssetsRepo::new(pool.clone());
    (repo, pool, config.paths, tmp, container)
}

/// Write stub bytes that `infer` detects as JPEG. Each call with a different
/// `tag` produces a file with a different hash.
fn write_jpeg(dir: &Path, name: &str, tag: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
    f.write_all(tag).unwrap();
    path
}

fn write_non_media(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"this is not a media file at all").unwrap();
    path
}

/// Sorted list of every path under `dir` (including `dir` itself), or
/// empty if `dir` doesn't exist. Used to assert no filesystem mutation.
fn snapshot_dir(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut entries: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .collect();
    entries.sort();
    entries
}

#[tokio::test]
async fn new_file_is_imported() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");

    let outcome = import_file(&src, &repo, &paths).await;
    assert!(
        matches!(outcome, ImportOutcome::Imported(_)),
        "expected Imported, got {outcome:?}",
    );
}

#[tokio::test]
async fn known_hash_returns_duplicate() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
    let hash = hash_file(&src).unwrap().to_string();
    let existing_id = match repo
        .insert_asset(NewAsset {
            hash: hash.clone(),
            original_filename: "seed.jpg".to_string(),
            storage_path: PathBuf::from("/library/seed.jpg"),
            file_size: 1,
            mime_type: Some("image/jpeg".to_string()),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            thumbnails_generated: false,
        })
        .await
        .expect("seed insert")
    {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("seed insert returned Existing"),
    };

    let outcome = import_file(&src, &repo, &paths).await;
    match outcome {
        ImportOutcome::Duplicate(id) => assert_eq!(id, existing_id),
        other => panic!("expected Duplicate, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_file_returns_failed() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let nonexistent = tmp.path().join("nope.jpg");

    let outcome = import_file(&nonexistent, &repo, &paths).await;
    assert!(matches!(outcome, ImportOutcome::Failed(_)));
}

#[tokio::test]
async fn non_media_file_returns_skipped() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src = write_non_media(tmp.path(), "document.txt");

    let outcome = import_file(&src, &repo, &paths).await;
    assert!(matches!(outcome, ImportOutcome::Skipped));
}

#[tokio::test]
async fn imported_file_has_jpeg_mime_type() {
    let (repo, pool, paths, tmp, _container) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");

    let id = match import_file(&src, &repo, &paths).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (mime,): (Option<String>,) = sqlx::query_as("SELECT mime_type FROM assets WHERE id = $1")
        .bind(id.as_uuid())
        .fetch_one(&pool)
        .await
        .expect("select mime_type");
    assert_eq!(mime.as_deref(), Some("image/jpeg"));
}

#[tokio::test]
async fn dir_imports_all_files() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src_dir = tmp.path().join("photos");
    write_jpeg(&src_dir, "a.jpg", b"a");
    write_jpeg(&src_dir, "b.jpg", b"b");
    write_jpeg(&src_dir, "c.jpg", b"c");

    let summary = import_dir(&src_dir, &repo, &paths).await.unwrap();
    assert_eq!(summary.imported, 3);
    assert_eq!(summary.duplicates, 0);
    assert_eq!(summary.skipped, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_counts_duplicates_separately() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src_dir = tmp.path().join("photos");
    let file = write_jpeg(&src_dir, "photo.jpg", b"a");
    let hash = hash_file(&file).unwrap().to_string();
    repo.insert_asset(NewAsset {
        hash,
        original_filename: "seed.jpg".to_string(),
        storage_path: PathBuf::from("/library/seed.jpg"),
        file_size: 1,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    })
    .await
    .expect("seed");

    let summary = import_dir(&src_dir, &repo, &paths).await.unwrap();
    assert_eq!(summary.imported, 0);
    assert_eq!(summary.duplicates, 1);
    assert_eq!(summary.skipped, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_counts_non_media_as_skipped() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src_dir = tmp.path().join("mixed");
    write_jpeg(&src_dir, "photo.jpg", b"a");
    write_non_media(&src_dir, "notes.txt");
    write_non_media(&src_dir, "archive.zip");

    let summary = import_dir(&src_dir, &repo, &paths).await.unwrap();
    assert_eq!(summary.imported, 1);
    assert_eq!(summary.skipped, 2);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_not_found_returns_err() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let missing = tmp.path().join("does_not_exist");

    let result = import_dir(&missing, &repo, &paths).await;
    assert!(matches!(result, Err(eidetic_ingest::Error::Io { .. })));
}

#[tokio::test]
async fn non_ascii_filename_preserved_not_unknown() {
    let (repo, pool, paths, tmp, _container) = fixture().await;
    let src = write_jpeg(tmp.path(), "héllo.jpg", b"a");

    let id = match import_file(&src, &repo, &paths).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (original_filename,): (String,) =
        sqlx::query_as("SELECT original_filename FROM assets WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_one(&pool)
            .await
            .expect("select original_filename");
    assert_eq!(original_filename, "héllo.jpg");
    assert_ne!(original_filename, "unknown");
}

#[tokio::test]
async fn dir_recurses_into_subdirectories() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src_dir = tmp.path().join("photos");
    write_jpeg(&src_dir, "good.jpg", b"a");
    let subdir = src_dir.join("subdir");
    write_jpeg(&subdir, "nested.jpg", b"b");

    let summary = import_dir(&src_dir, &repo, &paths).await.unwrap();
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.duplicates, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn duplicate_import_does_not_touch_library_dir() {
    let (repo, _pool, paths, tmp, _container) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
    let hash = hash_file(&src).unwrap().to_string();
    repo.insert_asset(NewAsset {
        hash,
        original_filename: "seed.jpg".to_string(),
        storage_path: PathBuf::from("/library/seed.jpg"),
        file_size: 1,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    })
    .await
    .expect("seed");

    assert!(!paths.library_dir.exists());
    let before = snapshot_dir(&paths.library_dir);

    let outcome = import_file(&src, &repo, &paths).await;
    assert!(
        matches!(outcome, ImportOutcome::Duplicate(_)),
        "expected Duplicate, got {outcome:?}",
    );

    let after = snapshot_dir(&paths.library_dir);
    assert_eq!(
        before, after,
        "duplicate import mutated library_dir; before={before:?} after={after:?}",
    );
}
