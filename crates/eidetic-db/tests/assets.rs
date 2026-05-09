use chrono::Utc;
use eidetic_core::Config;
#[allow(unused_imports)]
use eidetic_db::{PgAssetsRepo, SearchResult};
use eidetic_ingest::{AssetIndex, InsertOutcome, NewAsset};
use std::path::PathBuf;
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

#[tokio::test]
async fn insert_asset_then_find_by_hash() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let hash = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222";
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "test.jpg".to_string(),
        storage_path: PathBuf::from("/library/aa/aa/aaaa1111.jpg"),
        file_size: 2048,
        mime_type: None,
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
    };

    let outcome = repo.insert_asset(asset).await.expect("insert");
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted, got Existing"),
    };
    let found = repo.find_by_hash(hash).await.expect("find");
    assert_eq!(found, Some(id));
}

#[tokio::test]
async fn find_by_hash_returns_none_for_unknown() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let found = repo
        .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("find");
    assert!(found.is_none());
}

#[tokio::test]
async fn insert_asset_stores_exif_fields() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    let hash = "bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222cccc3333";
    let date_taken = chrono::DateTime::parse_from_rfc3339("2023-06-15T10:30:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "vacation.jpg".to_string(),
        storage_path: PathBuf::from("/library/bb/bb/bbbb2222.jpg"),
        file_size: 4096,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: Some(date_taken),
        latitude: Some(37.7749),
        longitude: Some(-122.4194),
        camera_make: Some("Canon".to_string()),
        camera_model: Some("EOS R5".to_string()),
    };

    repo.insert_asset(asset).await.expect("insert");

    type ExifRow = (
        Option<chrono::DateTime<Utc>>,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<String>,
    );
    let row: ExifRow = sqlx::query_as(
        "SELECT date_taken, latitude, longitude, camera_make, camera_model \
         FROM assets WHERE hash = $1",
    )
    .bind(hash)
    .fetch_one(&pool)
    .await
    .expect("select");

    let (db_date_taken, db_lat, db_lon, db_make, db_model) = row;
    assert_eq!(db_date_taken, Some(date_taken));
    assert!((db_lat.unwrap() - 37.7749).abs() < 1e-6);
    assert!((db_lon.unwrap() - (-122.4194)).abs() < 1e-6);
    assert_eq!(db_make.as_deref(), Some("Canon"));
    assert_eq!(db_model.as_deref(), Some("EOS R5"));
}

#[tokio::test]
async fn insert_duplicate_returns_existing() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let hash = "cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222cccc3333dddd4444";
    let make_asset = || NewAsset {
        hash: hash.to_string(),
        original_filename: "dup.jpg".to_string(),
        storage_path: PathBuf::from("/library/cc/cc/cccc3333.jpg"),
        file_size: 1024,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
    };

    let first = repo.insert_asset(make_asset()).await.expect("first insert");
    let first_id = match first {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    let second = repo
        .insert_asset(make_asset())
        .await
        .expect("second insert");
    match second {
        InsertOutcome::Existing(id) => assert_eq!(id, first_id),
        InsertOutcome::Inserted(_) => panic!("expected Existing on duplicate"),
    }
}

#[tokio::test]
async fn fetch_unembedded_returns_only_null_embedding_images() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    // Insert two image assets
    let asset_a = NewAsset {
        hash: "aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000".to_string(),
        original_filename: "a.jpg".to_string(),
        storage_path: PathBuf::from("/lib/aa/aa/a.jpg"),
        file_size: 1024,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
    };
    let asset_b = NewAsset {
        hash: "bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000".to_string(),
        original_filename: "b.jpg".to_string(),
        storage_path: PathBuf::from("/lib/bb/bb/b.jpg"),
        file_size: 2048,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
    };
    let outcome_a = repo.insert_asset(asset_a).await.expect("insert a");
    repo.insert_asset(asset_b).await.expect("insert b");

    let id_a = match outcome_a {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    // Embed asset_a — now only asset_b should be in the unembedded list
    repo.store_embedding(id_a, &[0.1f32; 768])
        .await
        .expect("store embedding");

    let unembedded = repo.fetch_unembedded().await.expect("fetch unembedded");
    assert_eq!(unembedded.len(), 1);
    let (_, path) = &unembedded[0];
    assert!(path.to_str().unwrap().contains("b.jpg"));
}

#[tokio::test]
async fn store_embedding_persists_float_values() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    let asset = NewAsset {
        hash: "cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000".to_string(),
        original_filename: "c.jpg".to_string(),
        storage_path: PathBuf::from("/lib/cc/cc/c.jpg"),
        file_size: 512,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
    };
    let outcome = repo.insert_asset(asset).await.expect("insert");
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    let embedding: Vec<f32> = (0..768).map(|i| i as f32 / 768.0).collect();
    repo.store_embedding(id, &embedding).await.expect("store");

    // Verify via raw SQL that the embedding column is non-null
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM assets WHERE id = $1 AND embedding IS NOT NULL")
            .bind(id.as_uuid())
            .fetch_one(&pool)
            .await
            .expect("count query");
    assert_eq!(count, 1);
}
