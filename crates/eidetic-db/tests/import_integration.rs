use eidetic_core::{Config, Paths};
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
use eidetic_ingest::{ImportOutcome, hash_file, import_dir, import_file};
use sqlx::SqlitePool;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Waker};
use walkdir::WalkDir;

/// One temp dir holds both the SQLite file and the library tree. The returned
/// `TempDir` guard must outlive the test body, or the database file is
/// deleted mid-test.
async fn fixture() -> (AssetsRepo, SqlitePool, Paths, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_path: tmp.path().join("eidetic.db"),
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool.clone());
    (repo, pool, config.paths, tmp)
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

async fn asset_count(pool: &SqlitePool) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM assets")
        .fetch_one(pool)
        .await
        .expect("count assets");
    n
}

/// Hold every connection the pool can hand out, so the next query blocks
/// until these guards are dropped.
async fn hold_every_connection(pool: &SqlitePool) -> Vec<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let mut held = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        held.push(pool.acquire().await.expect("acquire"));
    }
    held
}

/// Poll `import` once and require that it parked.
///
/// This is the whole suspension seam. `import_file` sniffs and hashes the
/// source synchronously and then awaits its duplicate lookup, so against a
/// drained pool a future observed as `Pending` has finished reading the
/// source and has not yet decided anything about it — exactly the window a
/// file being rewritten underneath the importer occupies. No sleeps, no
/// public test hooks, no dependence on thread scheduling: the poll either
/// parked or the assertion fails.
fn park_after_reading_the_source<F: Future>(import: &mut Pin<Box<F>>) {
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        import.as_mut().poll(&mut cx).is_pending(),
        "import did not park at its duplicate lookup, so the mutation below \
         would not land in the window under test"
    );
}

/// The importer reads the source twice — once to hash for the dedup gate,
/// once to copy into staging — and nothing locks the file in between. What
/// gets stored must be described by the second read: if the commit keeps
/// using the pre-mutation digest, the library ends up with bytes filed under
/// the hash of content they never had.
#[tokio::test]
async fn source_rewritten_at_the_dedup_gate_is_stored_as_the_new_bytes() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "moving.jpg", b"original");
    let stale = hash_file(&src).unwrap().to_string();

    let held = hold_every_connection(&pool).await;
    let mut import = Box::pin(import_file(&src, &repo, &paths, None));
    park_after_reading_the_source(&mut import);

    // Whatever was writing this file had not finished when the walk reached
    // it — a sync client, or an editor saving in place.
    write_jpeg(
        tmp.path(),
        "moving.jpg",
        b"rewritten, at a different length",
    );
    drop(held);

    let id = match import.await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (hash, storage_path): (String, String) =
        sqlx::query_as("SELECT hash, storage_path FROM assets WHERE id = ?")
            .bind(id.as_uuid().to_string())
            .fetch_one(&pool)
            .await
            .expect("select hash");

    assert_ne!(hash, stale, "the row kept the pre-mutation digest");
    let stored = PathBuf::from(&storage_path);
    assert_eq!(
        std::fs::read(&stored).unwrap(),
        std::fs::read(&src).unwrap(),
        "stored bytes are not the post-mutation content",
    );
    assert_eq!(
        hash_file(&stored).unwrap().to_string(),
        hash,
        "assets.hash does not describe the file at storage_path",
    );
    assert_eq!(
        stored,
        paths
            .library_dir
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(format!("{hash}.jpg")),
        "storage_path is not the CAS path for the recorded hash",
    );
}

/// Same window, but the source turns into content the library already has,
/// stored under a different extension. The staged bytes have to be checked
/// against the database before the commit: otherwise `<hash>.jpg` lands
/// beside the existing `<hash>.jpeg`, the insert reports the row that was
/// already there, and the file nothing points at stays forever.
#[tokio::test]
async fn source_rewritten_into_an_existing_asset_leaves_no_orphan() {
    let (repo, pool, paths, tmp) = fixture().await;
    let already = write_jpeg(tmp.path(), "already.jpeg", b"the same bytes");
    let existing_id = match import_file(&already, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let src = write_jpeg(tmp.path(), "incoming.jpg", b"something else entirely");
    let before = snapshot_dir(&paths.library_dir);

    let held = hold_every_connection(&pool).await;
    let mut import = Box::pin(import_file(&src, &repo, &paths, None));
    park_after_reading_the_source(&mut import);
    write_jpeg(tmp.path(), "incoming.jpg", b"the same bytes");
    drop(held);

    match import.await {
        ImportOutcome::Duplicate(id) => assert_eq!(id, existing_id),
        other => panic!("expected Duplicate, got {other:?}"),
    }

    assert_eq!(asset_count(&pool).await, 1, "a second asset row appeared");
    let after = snapshot_dir(&paths.library_dir);
    assert_eq!(
        before, after,
        "duplicate left an orphan or a temp file; before={before:?} after={after:?}",
    );
}

/// The type has to come from the staged bytes as well. A source that stops
/// being media inside the same window is a clean skip: no row, no CAS file,
/// no staging leftovers — and certainly not a text file recorded as
/// `image/jpeg`.
#[tokio::test]
async fn source_rewritten_into_non_media_is_skipped() {
    let (repo, pool, paths, tmp) = fixture().await;
    // One real import first, so library_dir already exists when the
    // snapshot is taken and the comparison is about the skipped file only.
    let anchor = write_jpeg(tmp.path(), "anchor.jpg", b"anchor");
    match import_file(&anchor, &repo, &paths, None).await {
        ImportOutcome::Imported(_) => {}
        other => panic!("expected Imported, got {other:?}"),
    }

    let src = write_jpeg(tmp.path(), "incoming.jpg", b"still a photo");
    let before = snapshot_dir(&paths.library_dir);

    let held = hold_every_connection(&pool).await;
    let mut import = Box::pin(import_file(&src, &repo, &paths, None));
    park_after_reading_the_source(&mut import);
    std::fs::write(&src, b"this is not a media file at all").unwrap();
    drop(held);

    let outcome = import.await;
    assert!(
        matches!(outcome, ImportOutcome::Skipped),
        "expected Skipped, got {outcome:?}",
    );
    assert_eq!(asset_count(&pool).await, 1, "the skipped file got a row");
    let after = snapshot_dir(&paths.library_dir);
    assert_eq!(
        before, after,
        "skipped file left something behind; before={before:?} after={after:?}",
    );
}

#[tokio::test]
async fn new_file_is_imported() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");

    let outcome = import_file(&src, &repo, &paths, None).await;
    assert!(
        matches!(outcome, ImportOutcome::Imported(_)),
        "expected Imported, got {outcome:?}",
    );
}

#[tokio::test]
async fn known_hash_returns_duplicate() {
    let (repo, _pool, paths, tmp) = fixture().await;
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
        })
        .await
        .expect("seed insert")
    {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("seed insert returned Existing"),
    };

    let outcome = import_file(&src, &repo, &paths, None).await;
    match outcome {
        ImportOutcome::Duplicate(id) => assert_eq!(id, existing_id),
        other => panic!("expected Duplicate, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_file_returns_failed() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let nonexistent = tmp.path().join("nope.jpg");

    let outcome = import_file(&nonexistent, &repo, &paths, None).await;
    assert!(matches!(outcome, ImportOutcome::Failed(_)));
}

#[tokio::test]
async fn non_media_file_returns_skipped() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let src = write_non_media(tmp.path(), "document.txt");

    let outcome = import_file(&src, &repo, &paths, None).await;
    assert!(matches!(outcome, ImportOutcome::Skipped));
}

#[tokio::test]
async fn imported_file_has_jpeg_mime_type() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"a");

    let id = match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (mime,): (Option<String>,) = sqlx::query_as("SELECT mime_type FROM assets WHERE id = ?")
        .bind(id.as_uuid().to_string())
        .fetch_one(&pool)
        .await
        .expect("select mime_type");
    assert_eq!(mime.as_deref(), Some("image/jpeg"));
}

#[tokio::test]
async fn dir_imports_all_files() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let src_dir = tmp.path().join("photos");
    write_jpeg(&src_dir, "a.jpg", b"a");
    write_jpeg(&src_dir, "b.jpg", b"b");
    write_jpeg(&src_dir, "c.jpg", b"c");

    let summary = import_dir(&src_dir, &repo, &paths, None).await.unwrap();
    assert_eq!(summary.imported, 3);
    assert_eq!(summary.duplicates, 0);
    assert_eq!(summary.skipped, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_counts_duplicates_separately() {
    let (repo, _pool, paths, tmp) = fixture().await;
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
    })
    .await
    .expect("seed");

    let summary = import_dir(&src_dir, &repo, &paths, None).await.unwrap();
    assert_eq!(summary.imported, 0);
    assert_eq!(summary.duplicates, 1);
    assert_eq!(summary.skipped, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_counts_non_media_as_skipped() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let src_dir = tmp.path().join("mixed");
    write_jpeg(&src_dir, "photo.jpg", b"a");
    write_non_media(&src_dir, "notes.txt");
    write_non_media(&src_dir, "archive.zip");

    let summary = import_dir(&src_dir, &repo, &paths, None).await.unwrap();
    assert_eq!(summary.imported, 1);
    assert_eq!(summary.skipped, 2);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn dir_not_found_returns_err() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let missing = tmp.path().join("does_not_exist");

    let result = import_dir(&missing, &repo, &paths, None).await;
    assert!(matches!(result, Err(eidetic_ingest::Error::Io { .. })));
}

#[tokio::test]
async fn non_ascii_filename_preserved_not_unknown() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "héllo.jpg", b"a");

    let id = match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (original_filename,): (String,) =
        sqlx::query_as("SELECT original_filename FROM assets WHERE id = ?")
            .bind(id.as_uuid().to_string())
            .fetch_one(&pool)
            .await
            .expect("select original_filename");
    assert_eq!(original_filename, "héllo.jpg");
    assert_ne!(original_filename, "unknown");
}

#[tokio::test]
async fn dir_recurses_into_subdirectories() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let src_dir = tmp.path().join("photos");
    write_jpeg(&src_dir, "good.jpg", b"a");
    let subdir = src_dir.join("subdir");
    write_jpeg(&subdir, "nested.jpg", b"b");

    let summary = import_dir(&src_dir, &repo, &paths, None).await.unwrap();
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.duplicates, 0);
    assert!(summary.failed.is_empty());
}

#[tokio::test]
async fn duplicate_import_does_not_touch_library_dir() {
    let (repo, _pool, paths, tmp) = fixture().await;
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
    })
    .await
    .expect("seed");

    assert!(!paths.library_dir.exists());
    let before = snapshot_dir(&paths.library_dir);

    let outcome = import_file(&src, &repo, &paths, None).await;
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

/// The recorded hash has to be the digest of the bytes that actually landed
/// in the CAS, because everything downstream — dedup, thumbnail paths, the
/// `/thumbs/{hash}` route — treats it as the identity of that file.
#[tokio::test]
async fn recorded_hash_is_the_digest_of_the_stored_bytes() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"identity");

    let id = match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let (hash, storage_path): (String, String) =
        sqlx::query_as("SELECT hash, storage_path FROM assets WHERE id = ?")
            .bind(id.as_uuid().to_string())
            .fetch_one(&pool)
            .await
            .expect("select hash");

    let stored = PathBuf::from(&storage_path);
    assert_eq!(
        hash_file(&stored).unwrap().to_string(),
        hash,
        "assets.hash does not describe the file at storage_path",
    );
    assert_eq!(
        stored,
        paths
            .library_dir
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(format!("{hash}.jpg")),
        "storage_path is not the CAS path for the recorded hash",
    );
}

/// Same bytes, different extension: the hash gate catches it before any
/// copying, so the library keeps exactly one file for that content. Worth
/// pinning — a duplicate check that ran after the copy instead would leave
/// a second CAS file (`<hash>.jpeg`) that no asset row points at.
#[tokio::test]
async fn same_bytes_with_another_extension_adds_no_second_cas_file() {
    let (repo, _pool, paths, tmp) = fixture().await;
    let first = write_jpeg(tmp.path(), "photo.jpg", b"identity");

    match import_file(&first, &repo, &paths, None).await {
        ImportOutcome::Imported(_) => {}
        other => panic!("expected Imported, got {other:?}"),
    }
    let hash = hash_file(&first).unwrap().to_string();
    let cas_dir = paths.library_dir.join(&hash[..2]).join(&hash[2..4]);
    assert_eq!(snapshot_dir(&cas_dir).len(), 2, "expected the dir + 1 file");

    let second = tmp.path().join("photo.jpeg");
    std::fs::copy(&first, &second).unwrap();
    let outcome = import_file(&second, &repo, &paths, None).await;
    assert!(
        matches!(outcome, ImportOutcome::Duplicate(_)),
        "expected Duplicate, got {outcome:?}",
    );

    assert_eq!(
        snapshot_dir(&cas_dir).len(),
        2,
        "second import left an extra file in {}: {:?}",
        cas_dir.display(),
        snapshot_dir(&cas_dir),
    );
}

fn cas_path(paths: &Paths, hash: &str, ext: &str) -> PathBuf {
    paths
        .library_dir
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(format!("{hash}.{ext}"))
}

/// Plain files directly under `library_dir` — staging temp files, which
/// every import path has to clean up.
fn staging_leftovers(paths: &Paths) -> Vec<PathBuf> {
    std::fs::read_dir(&paths.library_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect()
}

/// Two imports of the same bytes, one as `.jpg` and one as `.jpeg`, both
/// past the dedup gate before either records a row: each commits its own
/// CAS file, one insert wins, the other gets `Existing`. The loser's file is
/// then referenced by nothing, and has to go — without ever touching the
/// file the winning row points at.
///
/// The interleaving is forced, not hoped for. A reserved write lock held on
/// a side connection stalls both inserts in SQLite's busy wait while reads
/// still go through (WAL), so neither gate can see the other's row. Both
/// CAS files existing is proof both imports got past their gates and their
/// commits; only then is the lock released and the inserts left to race.
/// Whichever wins, the assertions below are the same.
#[tokio::test]
async fn racing_imports_under_different_extensions_leave_one_consistent_file() {
    let (repo, pool, paths, tmp) = fixture().await;
    let jpg = write_jpeg(&tmp.path().join("a"), "photo.jpg", b"one photo, twice");
    let jpeg = write_jpeg(&tmp.path().join("b"), "photo.jpeg", b"one photo, twice");
    let hash = hash_file(&jpg).unwrap().to_string();
    let as_jpg = cas_path(&paths, &hash, "jpg");
    let as_jpeg = cas_path(&paths, &hash, "jpeg");

    let mut lock = pool.acquire().await.expect("acquire");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .expect("take the write lock");

    let release = async {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while !(as_jpg.exists() && as_jpeg.exists()) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "both imports never reached their CAS commit"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        sqlx::query("COMMIT")
            .execute(&mut *lock)
            .await
            .expect("release the write lock");
    };
    let (a, b, ()) = tokio::join!(
        import_file(&jpg, &repo, &paths, None),
        import_file(&jpeg, &repo, &paths, None),
        release,
    );

    let id = match (a, b) {
        (ImportOutcome::Imported(x), ImportOutcome::Duplicate(y))
        | (ImportOutcome::Duplicate(y), ImportOutcome::Imported(x)) => {
            assert_eq!(x, y, "the loser reported a different asset");
            x
        }
        other => panic!("expected one Imported and one Duplicate, got {other:?}"),
    };

    assert_eq!(asset_count(&pool).await, 1);
    let detail = repo.fetch_by_id(id).await.unwrap().expect("winner row");
    assert_eq!(detail.hash.to_string(), hash);
    assert_eq!(
        hash_file(&detail.storage_path).unwrap().to_string(),
        hash,
        "the winner's object is gone or wrong",
    );
    let loser = if detail.storage_path == as_jpg {
        &as_jpeg
    } else {
        assert_eq!(detail.storage_path, as_jpeg, "unexpected storage_path");
        &as_jpg
    };
    assert!(
        !loser.exists(),
        "the losing import left its CAS file behind: {}",
        loser.display()
    );
    assert!(staging_leftovers(&paths).is_empty(), "staging temp leaked");
}

/// A file at the CAS path with the right length and the wrong bytes — a
/// torn write, bit rot, something copied in by hand. Equal size used to be
/// taken as proof of equal content, and the import reported success against
/// a file that was not the photo. It has to fail instead, say where, record
/// nothing and leave the file for the user to inspect; once it is moved
/// aside the same import goes through.
#[tokio::test]
async fn same_size_wrong_bytes_at_the_cas_path_fail_the_import() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"the real photo");
    let hash = hash_file(&src).unwrap().to_string();
    let dest = cas_path(&paths, &hash, "jpg");
    let name = dest.file_name().unwrap().to_str().unwrap().to_owned();
    write_jpeg(dest.parent().unwrap(), &name, b"a fake  photo!");
    let corrupt = std::fs::read(&dest).unwrap();
    assert_eq!(
        corrupt.len() as u64,
        std::fs::metadata(&src).unwrap().len(),
        "the impostor must match on length for this test to mean anything",
    );

    match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Failed(eidetic_ingest::Error::Io { path, source }) => {
            assert_eq!(path, dest);
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("expected an InvalidData failure, got {other:?}"),
    }
    assert_eq!(asset_count(&pool).await, 0, "a row points at corrupt bytes");
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        corrupt,
        "the file was altered"
    );
    assert!(staging_leftovers(&paths).is_empty(), "staging temp leaked");

    // The recovery the error message asks for.
    std::fs::rename(&dest, tmp.path().join("set-aside.jpg")).unwrap();
    let id = match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported after moving it aside, got {other:?}"),
    };
    let detail = repo.fetch_by_id(id).await.unwrap().expect("row");
    assert_eq!(detail.storage_path, dest);
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
}

/// The other side of the same check: a file that really does hold the bytes
/// its name claims — left by a crash between commit and insert — is reused
/// as-is instead of failing the import.
#[tokio::test]
async fn valid_leftover_at_the_cas_path_is_reused() {
    let (repo, pool, paths, tmp) = fixture().await;
    let src = write_jpeg(tmp.path(), "photo.jpg", b"crash survivor");
    let hash = hash_file(&src).unwrap().to_string();
    let dest = cas_path(&paths, &hash, "jpg");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dest).unwrap();

    let id = match import_file(&src, &repo, &paths, None).await {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };
    let detail = repo.fetch_by_id(id).await.unwrap().expect("row");
    assert_eq!(detail.storage_path, dest);
    assert_eq!(asset_count(&pool).await, 1);
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
    assert!(staging_leftovers(&paths).is_empty(), "staging temp leaked");
}
