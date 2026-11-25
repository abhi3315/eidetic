//! A library written by the v0.8.0 release must open, migrate and read back
//! unchanged.
//!
//! `fixtures/eidetic-v0.8.0.db` holds the schema v0.8.0 shipped (migrations
//! 001-008) with every value encoded by sqlx 0.8.6, the version v0.8.0
//! locked. It was generated at 008fa5a by
//! `cargo test -p eidetic-db --test legacy_db -- --ignored write_v08_fixture`.
//! The generator refuses to overwrite the file: regenerating it under a newer
//! sqlx would encode the rows with that version and stop proving anything
//! about libraries already on disk.

use chrono::{DateTime, TimeZone, Utc};
use eidetic_core::{AssetId, Config, FaceId, PersonId};
use eidetic_db::{AssetsRepo, FacesRepo, InsertOutcome, MomentKey, NewAsset, SearchFilters};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::{Path, PathBuf};

const IMAGE: &str = "11111111-1111-4111-8111-111111111111";
const VIDEO: &str = "22222222-2222-4222-8222-222222222222";
const FACE: &str = "33333333-3333-4333-8333-333333333333";
const PERSON: &str = "44444444-4444-4444-8444-444444444444";

/// What migration 009 backfills onto 768-dim image and frame embeddings.
const SIGLIP_BACKFILL: &str = "siglip2-base-patch16-256";
/// What migration 009 backfills onto face embeddings.
const FACE_BACKFILL: &str = "auraface-v1";

/// The last migration the v0.8.0 release shipped.
const V080_LAST_MIGRATION: i64 = 8;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eidetic-v0.8.0.db")
}

fn date_taken() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 3, 15, 10, 30, 45).unwrap() + chrono::Duration::milliseconds(123)
}

fn exif_raw() -> serde_json::Value {
    serde_json::json!({
        "Make": "Google",
        "ExposureTime": "1/120",
        "ISOSpeedRatings": 64,
        "nested": {"tags": ["a", "b"], "ratio": 1.5}
    })
}

fn one_hot(dim: usize, hot: usize) -> Vec<f32> {
    let mut v = vec![0.0; dim];
    v[hot] = 1.0;
    v
}

fn asset_id(raw: &str) -> AssetId {
    AssetId::from(uuid::Uuid::parse_str(raw).unwrap())
}

/// Write the v0.8.0 fixture: the 001-008 schema through sqlx's own migrator,
/// then rows bound the way v0.8.0's repos bound them (chrono, JSON and bool
/// values go through sqlx's encoders, exactly as `insert_asset` did).
#[tokio::test]
#[ignore = "regenerates the committed fixture; run by hand only"]
async fn write_v08_fixture() {
    let out = fixture_path();
    assert!(
        !out.exists(),
        "{} exists; delete it deliberately before regenerating",
        out.display()
    );
    let tmp = tempfile::tempdir().unwrap();
    // The same pool options v0.8.0's `connect` used.
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(tmp.path().join("eidetic.db"))
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut v080 = sqlx::migrate!("../../migrations");
    v080.migrations = std::borrow::Cow::Owned(
        v080.migrations
            .iter()
            .filter(|m| m.version <= V080_LAST_MIGRATION)
            .cloned()
            .collect(),
    );
    v080.run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO assets (id, hash, original_filename, storage_path, file_size, mime_type, \
           imported_at, date_taken, latitude, longitude, camera_make, camera_model, \
           lens_make, lens_model, focal_length, focal_length_35mm, aperture, shutter, iso, \
           orientation, altitude, gps_direction, exif_raw, country_code, country_name, admin1, \
           place, place_distance_m, thumbnails_generated) \
         VALUES (?, ?, 'IMG_0001.jpg', '/library/ab/IMG_0001.jpg', 2048000, 'image/jpeg', \
           '2024-06-01T08:00:00.000Z', ?, 32.5387, 75.9710, 'Google', 'Pixel 8', \
           'Google', 'Pixel 8 back camera', 6.9, 25.0, 1.68, '1/120', 64, \
           6, 1970.5, 182.25, ?, 'IN', 'India', 'Himachal Pradesh', 'Dalhousie', 812.5, ?)",
    )
    .bind(IMAGE)
    .bind("a1".repeat(32))
    .bind(date_taken())
    .bind(exif_raw())
    .bind(true)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assets (id, hash, original_filename, storage_path, file_size, mime_type, \
           imported_at, exif_raw, thumbnails_generated, duration_secs, video_codec, \
           pixel_width, pixel_height, playback_path) \
         VALUES (?, ?, 'VID_0002.mov', '/library/b2/VID_0002.mov', 52428800, 'video/quicktime', \
           '2023-01-10T12:00:00.000Z', ?, ?, 12.5, 'hevc', 1920, 1080, \
           '/library/.playback/b2/VID_0002.mp4')",
    )
    .bind(VIDEO)
    .bind("b2".repeat(32))
    .bind(serde_json::json!({}))
    .bind(true)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO embeddings (asset_id, dim, vector) VALUES (?, 768, ?)")
        .bind(IMAGE)
        .bind(eidetic_db::vector::encode(&one_hot(768, 3)))
        .execute(&pool)
        .await
        .unwrap();
    for (ts, hot) in [(2.0, 10), (8.0, 20)] {
        sqlx::query(
            "INSERT INTO frame_embeddings (asset_id, ts_secs, dim, vector) VALUES (?, ?, 768, ?)",
        )
        .bind(VIDEO)
        .bind(ts)
        .bind(eidetic_db::vector::encode(&one_hot(768, hot)))
        .execute(&pool)
        .await
        .unwrap();
    }
    for ts in [4.0, 9.5, -1.0] {
        sqlx::query("INSERT INTO video_scenes (asset_id, ts_secs) VALUES (?, ?)")
            .bind(VIDEO)
            .bind(ts)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (start, end, text) in [
        (0.5, 2.5, "happy birthday to you"),
        (3.0, 5.0, "blow out the candles"),
    ] {
        sqlx::query(
            "INSERT INTO transcripts (asset_id, ts_secs, end_secs, text) VALUES (?, ?, ?, ?)",
        )
        .bind(VIDEO)
        .bind(start)
        .bind(end)
        .bind(text)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO transcript_runs (asset_id, segment_count) VALUES (?, 2)")
        .bind(VIDEO)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO persons (id, name) VALUES (?, 'Asha')")
        .bind(PERSON)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO faces (id, asset_id, bbox_x, bbox_y, bbox_w, bbox_h, \
           lm_left_eye_x, lm_left_eye_y, lm_right_eye_x, lm_right_eye_y, lm_nose_x, lm_nose_y, \
           lm_left_mouth_x, lm_left_mouth_y, lm_right_mouth_x, lm_right_mouth_y, \
           score, embedding, dim, person_id, assignment_source) \
         VALUES (?, ?, 100.0, 120.0, 80.0, 96.0, 120.0, 150.0, 160.0, 150.0, 140.0, 170.0, \
           125.0, 190.0, 155.0, 190.0, 0.96875, ?, 512, ?, 'user')",
    )
    .bind(FACE)
    .bind(IMAGE)
    .bind(eidetic_db::vector::encode(&one_hot(512, 7)))
    .bind(PERSON)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO face_detection_runs (asset_id, face_count) VALUES (?, 1)")
        .bind(IMAGE)
        .execute(&pool)
        .await
        .unwrap();

    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    // VACUUM INTO writes one compacted file with the WAL already folded in,
    // so no -wal/-shm side files exist to commit or forget.
    sqlx::query("VACUUM INTO ?")
        .bind(out.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

/// Copy the fixture into a fresh TempDir; the committed file is never opened
/// in place, so a test run cannot modify it.
fn legacy_copy() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().expect("tempdir");
    let database_path = dir.path().join("eidetic.db");
    std::fs::copy(fixture_path(), &database_path).expect("copy the v0.8.0 fixture");
    let config = Config {
        database_path,
        paths: eidetic_core::Paths {
            library_dir: dir.path().join("library"),
            models_cache: dir.path().join("models"),
        },
    };
    (dir, config)
}

#[tokio::test]
async fn v08_database_opens_and_migrates() {
    let (_tmp, config) = legacy_copy();
    let pool = eidetic_db::connect(&config)
        .await
        .expect("a v0.8.0 library opens and migrates");

    // Every migration is recorded once, successfully, with the checksum this
    // build embeds — the old rows from v0.8.0 and the ones applied just now.
    let recorded: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    let embedded = sqlx::migrate!("../../migrations");
    assert!(
        embedded
            .migrations
            .iter()
            .any(|m| m.version > V080_LAST_MIGRATION),
        "the upgrade path applies at least one migration on top of v0.8.0"
    );
    assert_eq!(recorded.len(), embedded.migrations.len());
    for ((version, success, checksum), m) in recorded.iter().zip(embedded.migrations.iter()) {
        assert_eq!(*version, m.version);
        assert!(success, "migration {version} recorded as failed");
        assert_eq!(
            checksum.as_slice(),
            m.checksum.as_ref(),
            "migration {version}"
        );
    }

    let assets = AssetsRepo::new(pool.clone());
    let faces = FacesRepo::new(pool.clone());
    let image = asset_id(IMAGE);
    let video = asset_id(VIDEO);

    let d = assets.fetch_by_id(image).await.unwrap().expect("image row");
    assert_eq!(d.hash.to_string(), "a1".repeat(32));
    assert_eq!(d.original_filename, "IMG_0001.jpg");
    assert_eq!(d.storage_path, PathBuf::from("/library/ab/IMG_0001.jpg"));
    assert_eq!(d.file_size, 2_048_000);
    assert_eq!(d.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(
        d.imported_at,
        Utc.with_ymd_and_hms(2024, 6, 1, 8, 0, 0).unwrap()
    );
    assert_eq!(d.date_taken, Some(date_taken()));
    assert_eq!(d.latitude, Some(32.5387));
    assert_eq!(d.longitude, Some(75.9710));
    assert_eq!(d.camera_make.as_deref(), Some("Google"));
    assert_eq!(d.camera_model.as_deref(), Some("Pixel 8"));
    assert_eq!(d.lens_make.as_deref(), Some("Google"));
    assert_eq!(d.lens_model.as_deref(), Some("Pixel 8 back camera"));
    assert_eq!(d.focal_length, Some(6.9));
    assert_eq!(d.focal_length_35mm, Some(25.0));
    assert_eq!(d.aperture, Some(1.68));
    assert_eq!(d.shutter.as_deref(), Some("1/120"));
    assert_eq!(d.iso, Some(64));
    assert_eq!(d.orientation, Some(6));
    assert_eq!(d.altitude, Some(1970.5));
    assert_eq!(d.gps_direction, Some(182.25));
    assert_eq!(d.exif_raw, Some(exif_raw()));
    assert_eq!(d.country_code.as_deref(), Some("IN"));
    assert_eq!(d.country_name.as_deref(), Some("India"));
    assert_eq!(d.admin1.as_deref(), Some("Himachal Pradesh"));
    assert_eq!(d.place.as_deref(), Some("Dalhousie"));
    assert_eq!(d.place_distance_m, Some(812.5));
    assert!(d.thumbnails_generated);
    assert_eq!(d.duration_secs, None);
    assert_eq!(d.playback_path, None);

    let v = assets.fetch_by_id(video).await.unwrap().expect("video row");
    assert_eq!(
        v.imported_at,
        Utc.with_ymd_and_hms(2023, 1, 10, 12, 0, 0).unwrap()
    );
    assert_eq!(v.date_taken, None);
    assert_eq!(v.exif_raw, Some(serde_json::json!({})));
    assert_eq!(v.duration_secs, Some(12.5));
    assert_eq!(v.video_codec.as_deref(), Some("hevc"));
    assert_eq!((v.pixel_width, v.pixel_height), (Some(1920), Some(1080)));
    assert_eq!(
        v.playback_path,
        Some(PathBuf::from("/library/.playback/b2/VID_0002.mp4"))
    );

    assert_eq!(
        assets.fetch_video_scenes(video).await.unwrap(),
        Some(vec![4.0, 9.5])
    );
    assert_eq!(
        assets.fetch_transcript(video).await.unwrap(),
        vec![
            (0.5, 2.5, "happy birthday to you".to_string()),
            (3.0, 5.0, "blow out the candles".to_string()),
        ]
    );
    // FTS5 shadow tables written by the old SQLite still answer queries.
    assert_eq!(
        assets.search_transcripts("candles", 5, None).await.unwrap(),
        vec![(video, 3.0, "blow out the candles".to_string())]
    );
    assert!(
        assets
            .fetch_videos_untranscribed("whisper-small")
            .await
            .unwrap()
            .is_empty(),
        "migration 009 attributes the old transcript run to whisper-small"
    );

    // Migration 009 attributes the old vectors to their models; 011 leaves
    // every old frame unmeasured rather than still.
    let signals = assets
        .fetch_moment_signals(
            SIGLIP_BACKFILL,
            &[
                MomentKey {
                    asset: image,
                    at: None,
                },
                MomentKey {
                    asset: video,
                    at: Some(7.0),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(signals[0].embedding, Some(one_hot(768, 3)));
    assert_eq!(signals[0].motion, None);
    assert_eq!(signals[1].ts_secs, Some(8.0));
    assert_eq!(signals[1].embedding, Some(one_hot(768, 20)));
    assert_eq!(signals[1].motion, None);
    let backlog = assets.fetch_motion_backlog().await.unwrap();
    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0].id, video);
    assert_eq!(backlog[0].timestamps, vec![2.0, 8.0]);

    let persons = faces.list_persons().await.unwrap();
    assert_eq!(persons.len(), 1);
    let person_id = PersonId::from(uuid::Uuid::parse_str(PERSON).unwrap());
    let face_id = FaceId::from(uuid::Uuid::parse_str(FACE).unwrap());
    assert_eq!(persons[0].id, person_id);
    assert_eq!(persons[0].name.as_deref(), Some("Asha"));
    assert_eq!(persons[0].face_count, 1);
    assert_eq!(persons[0].cover_face, Some(face_id));
    let embeddings = faces.fetch_embeddings(false, FACE_BACKFILL).await.unwrap();
    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].id, face_id);
    assert_eq!(embeddings[0].asset_id, image);
    assert_eq!(embeddings[0].person_id, Some(person_id));
    assert_eq!(embeddings[0].score, 0.96875);
    assert_eq!(embeddings[0].embedding, one_hot(512, 7));
    assert!(embeddings[0].pinned);
    assert_eq!(embeddings[0].ts_secs, None);
    let face = faces.fetch_face(face_id).await.unwrap().expect("face row");
    assert_eq!(face.bbox, (100.0, 120.0, 80.0, 96.0));
    assert_eq!(face.landmarks[4], (155.0, 190.0));
    // cluster_state arrived with migration 010: nothing was ever clustered.
    assert_eq!(faces.fetch_cluster_fingerprint().await.unwrap(), None);

    // A row written now must store the same instant as the same TEXT, since
    // the date filters compare these columns as strings.
    let InsertOutcome::Inserted(new) = assets
        .insert_asset(NewAsset {
            hash: "c3".repeat(32),
            original_filename: "IMG_0003.jpg".into(),
            storage_path: PathBuf::from("/library/c3/IMG_0003.jpg"),
            file_size: 1,
            mime_type: Some("image/jpeg".into()),
            date_taken: Some(date_taken()),
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
            exif_raw: Some(exif_raw()),
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
        .unwrap()
    else {
        panic!("fresh hash must insert");
    };
    let new_text = new.as_uuid().to_string();
    let stored: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, date_taken, exif_raw FROM assets WHERE id IN (?, ?) ORDER BY id",
    )
    .bind(IMAGE)
    .bind(&new_text)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    let old = stored.iter().find(|r| r.0 == IMAGE).unwrap();
    let fresh = stored.iter().find(|r| r.0 == new_text).unwrap();
    assert_eq!(fresh.1, old.1, "date_taken TEXT encoding changed");
    assert_eq!(fresh.2, old.2, "exif_raw TEXT encoding changed");

    let window = SearchFilters {
        after: Some(date_taken() - chrono::Duration::seconds(1)),
        before: Some(date_taken() + chrono::Duration::seconds(1)),
        ..Default::default()
    };
    let hits = assets.fetch_filtered_ids(&window).await.unwrap().unwrap();
    assert_eq!(hits, [image, new].into_iter().collect());
    let exact = SearchFilters {
        after: Some(date_taken()),
        ..Default::default()
    };
    let hits = assets.fetch_filtered_ids(&exact).await.unwrap().unwrap();
    assert_eq!(
        hits,
        [image, new].into_iter().collect(),
        "an inclusive bound at the stored instant matches old and new rows alike"
    );
}
