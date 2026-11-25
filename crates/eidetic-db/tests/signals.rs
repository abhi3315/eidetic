//! Quality signals (goals-v0.9.md phase 2) against a real temporary SQLite
//! database: per-moment motion persistence, the legacy/unknown distinction,
//! the migration over an existing library, and batched signal lookup with
//! embedding-identity checks. Vectors are hand-written; no model runs.

use eidetic_core::{AssetId, Config};
use eidetic_db::{AssetsRepo, InsertOutcome, MomentKey, NewAsset, NewFrame};
use std::path::PathBuf;

/// Pinned identity, as the embed pass stores it (ADR-0013).
const MODEL: &str = "siglip2-base-patch16-256@d1114256522a37ffa257a0a58017348ab0058db2";
/// The same variant before revisions were pinned: a different identity.
const LEGACY: &str = "siglip2-base-patch16-256";

fn temp_db() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config {
        database_path: dir.path().join("eidetic.db"),
        paths: eidetic_core::Paths {
            library_dir: dir.path().join("library"),
            models_cache: dir.path().join("models"),
        },
    };
    (dir, config)
}

fn asset(n: u8, mime: &str) -> NewAsset {
    NewAsset {
        hash: format!("{n:02x}").repeat(32),
        original_filename: format!("f{n}"),
        storage_path: PathBuf::from(format!("/library/f{n}")),
        file_size: 1,
        mime_type: Some(mime.to_string()),
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
        duration_secs: mime.starts_with("video/").then_some(10.0),
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    }
}

async fn insert(repo: &AssetsRepo, n: u8, mime: &str) -> AssetId {
    match repo.insert_asset(asset(n, mime)).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("fresh database"),
    }
}

fn frame(ts: f64, v: [f32; 2], motion: Option<f32>) -> NewFrame {
    NewFrame {
        ts_secs: ts,
        vector: v.to_vec(),
        motion,
    }
}

#[tokio::test]
async fn measured_zero_unmeasured_and_photos_stay_distinct() {
    let (_tmp, config) = temp_db();
    let repo = AssetsRepo::new(eidetic_db::connect(&config).await.expect("connect"));
    let photo = insert(&repo, 1, "image/jpeg").await;
    let video = insert(&repo, 2, "video/mp4").await;
    repo.store_embedding(photo, &[1.0, 0.0], MODEL)
        .await
        .unwrap();
    repo.store_frame_embeddings(
        video,
        &[
            frame(1.0, [1.0, 0.0], Some(0.0)),
            frame(5.0, [0.0, 1.0], Some(0.4)),
            frame(9.0, [0.6, 0.8], None),
        ],
        MODEL,
    )
    .await
    .unwrap();

    let got = repo
        .fetch_moment_signals(
            MODEL,
            &[
                MomentKey {
                    asset: photo,
                    at: None,
                },
                MomentKey {
                    asset: video,
                    at: Some(1.2),
                },
                MomentKey {
                    asset: video,
                    at: Some(4.0),
                },
                MomentKey {
                    asset: video,
                    at: Some(8.5),
                },
                MomentKey {
                    asset: AssetId::new(),
                    at: None,
                },
            ],
        )
        .await
        .unwrap();

    assert_eq!(got[0].motion, None, "a photo is never a measured video");
    assert_eq!(got[0].ts_secs, None);
    assert_eq!(got[0].embedding.as_deref(), Some(&[1.0, 0.0][..]));
    assert_eq!(
        got[1].motion,
        Some(0.0),
        "measured still shot is 0, not unknown"
    );
    assert_eq!(got[1].ts_secs, Some(1.0), "nearest sampled frame");
    assert_eq!(got[2].motion, Some(0.4));
    assert_eq!(got[3].motion, None, "unmeasured stays unknown");
    assert_eq!(got[3].embedding.as_deref(), Some(&[0.6, 0.8][..]));
    assert!(got[4].embedding.is_none() && got[4].other_model.is_none());
}

#[tokio::test]
async fn other_models_are_never_compared_and_bad_signals_are_refused() {
    let (_tmp, config) = temp_db();
    let repo = AssetsRepo::new(eidetic_db::connect(&config).await.expect("connect"));
    let photo = insert(&repo, 1, "image/jpeg").await;
    let video = insert(&repo, 2, "video/mp4").await;
    repo.store_embedding(photo, &[1.0, 0.0], "siglip2-large-patch16-384")
        .await
        .unwrap();
    repo.store_embedding(video, &[f32::NAN, 0.0], MODEL)
        .await
        .unwrap();
    let legacy = insert(&repo, 3, "image/jpeg").await;
    repo.store_embedding(legacy, &[1.0, 0.0], LEGACY)
        .await
        .unwrap();

    let got = repo
        .fetch_moment_signals(
            MODEL,
            &[
                MomentKey {
                    asset: photo,
                    at: None,
                },
                MomentKey {
                    asset: video,
                    at: None,
                },
                MomentKey {
                    asset: legacy,
                    at: None,
                },
            ],
        )
        .await
        .unwrap();
    assert!(
        got[2].embedding.is_none(),
        "an unpinned legacy row is not the pinned model"
    );
    assert_eq!(got[2].other_model.as_deref(), Some(LEGACY));
    assert!(got[0].embedding.is_none());
    assert_eq!(
        got[0].other_model.as_deref(),
        Some("siglip2-large-patch16-384")
    );
    assert!(
        got[1].embedding.is_none(),
        "non-finite vectors are unusable"
    );

    for bad in [f32::NAN, f32::INFINITY, -0.1, 1.5] {
        let err = repo
            .store_frame_embeddings(video, &[frame(1.0, [1.0, 0.0], Some(bad))], MODEL)
            .await
            .unwrap_err();
        assert!(
            matches!(err, eidetic_db::Error::InvalidSignal { .. }),
            "{bad}: {err}"
        );
        let err = repo.store_frame_motion(video, &[(1.0, bad)]).await;
        assert!(err.is_err(), "{bad} accepted by the backfill path");
    }
    assert!(
        repo.fetch_motion_backlog().await.unwrap().is_empty(),
        "a refused store writes nothing"
    );
}

#[tokio::test]
async fn backfill_lists_only_unmeasured_moments_and_fills_them_in_place() {
    let (_tmp, config) = temp_db();
    let repo = AssetsRepo::new(eidetic_db::connect(&config).await.expect("connect"));
    let video = insert(&repo, 2, "video/mp4").await;
    repo.store_frame_embeddings(
        video,
        &[
            frame(1.0, [1.0, 0.0], Some(0.2)),
            frame(5.0, [0.0, 1.0], None),
        ],
        MODEL,
    )
    .await
    .unwrap();

    let backlog = repo.fetch_motion_backlog().await.unwrap();
    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0].id, video);
    assert_eq!(backlog[0].timestamps, vec![5.0]);
    assert_eq!(backlog[0].duration_secs, Some(10.0));

    repo.store_frame_motion(video, &[(5.0, 0.0)]).await.unwrap();
    assert!(repo.fetch_motion_backlog().await.unwrap().is_empty());
    let got = repo
        .fetch_moment_signals(
            MODEL,
            &[MomentKey {
                asset: video,
                at: Some(5.0),
            }],
        )
        .await
        .unwrap();
    assert_eq!(got[0].motion, Some(0.0));
    assert_eq!(
        got[0].embedding.as_deref(),
        Some(&[0.0, 1.0][..]),
        "backfill leaves the embedding alone"
    );
}

/// A library created before migration 011 keeps every frame, and each one
/// reads back as "not measured" — never as a still shot.
#[tokio::test]
async fn migration_keeps_existing_frames_as_unmeasured() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let (_tmp, config) = temp_db();
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&config.database_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut before = sqlx::migrate!("../../migrations");
    before.migrations = std::borrow::Cow::Owned(
        before
            .migrations
            .iter()
            .filter(|m| m.version <= 10)
            .cloned()
            .collect(),
    );
    before.run(&pool).await.expect("migrations 001-010");
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO assets (id, hash, original_filename, storage_path, file_size, mime_type) \
         VALUES (?, ?, 'old.mp4', '/library/old.mp4', 1, 'video/mp4')",
    )
    .bind(&id)
    .bind("ab".repeat(32))
    .execute(&pool)
    .await
    .unwrap();
    let blob: Vec<u8> = [1.0f32, 0.0].iter().flat_map(|x| x.to_le_bytes()).collect();
    sqlx::query(
        "INSERT INTO frame_embeddings (asset_id, ts_secs, dim, vector, model_id) \
         VALUES (?, 2.0, 2, ?, ?)",
    )
    .bind(&id)
    .bind(blob)
    .bind(MODEL)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let repo = AssetsRepo::new(eidetic_db::connect(&config).await.expect("migrate to 011"));
    let asset = AssetId::from(uuid::Uuid::parse_str(&id).unwrap());
    let backlog = repo.fetch_motion_backlog().await.unwrap();
    assert_eq!(backlog.len(), 1, "the legacy moment is listed for backfill");
    assert_eq!(backlog[0].timestamps, vec![2.0]);
    let got = repo
        .fetch_moment_signals(
            MODEL,
            &[MomentKey {
                asset,
                at: Some(2.0),
            }],
        )
        .await
        .unwrap();
    assert_eq!(got[0].motion, None);
    assert_eq!(got[0].embedding.as_deref(), Some(&[1.0, 0.0][..]));
}
