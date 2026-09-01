use eidetic_core::Config;
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
use std::path::PathBuf;

/// A throwaway SQLite database in its own temp dir.
///
/// The returned `TempDir` guard must outlive the test body — dropping it
/// deletes the directory and the database file along with it.
fn temp_db() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config {
        database_path: dir.path().join("eidetic.db"),
        ..Default::default()
    };
    (dir, config)
}

#[tokio::test]
async fn insert_asset_then_find_by_hash() {
    let (_tmp, config) = temp_db();

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

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
    let (_tmp, config) = temp_db();

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let found = repo
        .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("find");
    assert!(found.is_none());
}

#[tokio::test]
async fn insert_duplicate_returns_existing() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

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
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool.clone());

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
    let outcome_a = repo.insert_asset(asset_a).await.expect("insert a");
    repo.insert_asset(asset_b).await.expect("insert b");

    let id_a = match outcome_a {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    // Embed asset_a. Now only asset_b should be in the unembedded list.
    repo.store_embedding(id_a, &[0.1f32; 768])
        .await
        .expect("store embedding");

    let unembedded = repo.fetch_unembedded().await.expect("fetch unembedded");
    assert_eq!(unembedded.len(), 1);
    let (_, path) = &unembedded[0];
    assert!(path.to_str().unwrap().contains("b.jpg"));
}

#[tokio::test]
async fn search_similar_orders_by_cosine_similarity() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool.clone());

    // Asset A: embedding aligned with e1 = [1, 0, 0, ..., 0]
    let mut emb_a = vec![0.0f32; 768];
    emb_a[0] = 1.0;

    // Asset B: embedding aligned with e2 = [0, 1, 0, ..., 0]
    let mut emb_b = vec![0.0f32; 768];
    emb_b[1] = 1.0;

    let asset_a = NewAsset {
        hash: "dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000".to_string(),
        original_filename: "d.jpg".to_string(),
        storage_path: PathBuf::from("/lib/dd/dd/d.jpg"),
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
    };
    let asset_b = NewAsset {
        hash: "eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000".to_string(),
        original_filename: "e.jpg".to_string(),
        storage_path: PathBuf::from("/lib/ee/ee/e.jpg"),
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
    };

    let id_a = match repo.insert_asset(asset_a).await.expect("insert a") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!(),
    };
    let id_b = match repo.insert_asset(asset_b).await.expect("insert b") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!(),
    };

    repo.store_embedding(id_a, &emb_a).await.expect("embed a");
    repo.store_embedding(id_b, &emb_b).await.expect("embed b");

    // Query with e1: asset A should come first (score ~1.0), B second (score ~0.0).
    let query = emb_a.clone();
    let results = repo.search_similar(&query, 10, None).await.expect("search");

    assert_eq!(results.len(), 2);
    assert!(
        results[0].score > 0.99,
        "first result score should be ~1.0, got {}",
        results[0].score
    );
    assert!(
        results[1].score < 0.01,
        "second result score should be ~0.0, got {}",
        results[1].score
    );
    assert!(results[0].storage_path.to_str().unwrap().contains("d.jpg"));
}

#[tokio::test]
async fn fetch_unthumbnailed_returns_images_with_flag_false() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let image_pending = NewAsset {
        hash: "11110000111100001111000011110000111100001111000011110000ffffffff".to_string(),
        original_filename: "pending.jpg".to_string(),
        storage_path: PathBuf::from("/lib/11/11/pending.jpg"),
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
    };
    let image_done = NewAsset {
        hash: "22220000222200002222000022220000222200002222000022220000ffffffff".to_string(),
        original_filename: "done.jpg".to_string(),
        storage_path: PathBuf::from("/lib/22/22/done.jpg"),
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
        thumbnails_generated: true,
        duration_secs: None,
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    };
    let video_pending = NewAsset {
        hash: "33330000333300003333000033330000333300003333000033330000ffffffff".to_string(),
        original_filename: "video.mp4".to_string(),
        storage_path: PathBuf::from("/lib/33/33/video.mp4"),
        file_size: 1,
        mime_type: Some("video/mp4".to_string()),
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

    repo.insert_asset(image_pending)
        .await
        .expect("insert pending image");
    repo.insert_asset(image_done)
        .await
        .expect("insert done image");
    repo.insert_asset(video_pending)
        .await
        .expect("insert video");

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(
        pending.len(),
        2,
        "the unthumbnailed image AND the video should come back (ADR-0011); got {pending:?}"
    );
    let paths: Vec<&str> = pending
        .iter()
        .map(|a| a.storage_path.to_str().unwrap())
        .collect();
    assert!(paths.iter().any(|p| p.contains("pending.jpg")));
    assert!(paths.iter().any(|p| p.contains("video.mp4")));
    let video = pending
        .iter()
        .find(|a| a.mime_type.starts_with("video/"))
        .unwrap();
    assert_eq!(
        video.duration_secs, None,
        "stub row carries its NULL duration"
    );
}

#[tokio::test]
async fn mark_thumbnailed_flips_flag_to_true() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let asset = NewAsset {
        hash: "44440000444400004444000044440000444400004444000044440000ffffffff".to_string(),
        original_filename: "mark_me.jpg".to_string(),
        storage_path: PathBuf::from("/lib/44/44/mark.jpg"),
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
    };
    let outcome = repo.insert_asset(asset).await.expect("insert");
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    let before = repo.fetch_unthumbnailed().await.expect("fetch before");
    assert_eq!(before.len(), 1);

    repo.mark_thumbnailed(id).await.expect("mark");

    let after = repo.fetch_unthumbnailed().await.expect("fetch after");
    assert_eq!(after.len(), 0, "row should no longer be pending after mark");
}

#[tokio::test]
async fn fetch_recent_orders_by_imported_at_desc() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    // Insert 3 image assets. The `imported_at` DEFAULT is millisecond-
    // precision strftime('now'), so they get ascending timestamps and the
    // most-recent comes last by insertion order. fetch_recent reverses that.
    for (i, hash) in [
        "aaaa000000000000000000000000000000000000000000000000000000000001",
        "aaaa000000000000000000000000000000000000000000000000000000000002",
        "aaaa000000000000000000000000000000000000000000000000000000000003",
    ]
    .iter()
    .enumerate()
    {
        repo.insert_asset(NewAsset {
            hash: hash.to_string(),
            original_filename: format!("img{i}.jpg"),
            storage_path: PathBuf::from(format!("/lib/{}.jpg", hash)),
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
        .expect("insert");
        // Tiny sleep so imported_at differs row-to-row.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let recent = repo.fetch_recent(10, 0).await.expect("fetch_recent");
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].original_filename, "img2.jpg");
    assert_eq!(recent[1].original_filename, "img1.jpg");
    assert_eq!(recent[2].original_filename, "img0.jpg");
}

#[tokio::test]
async fn fetch_by_id_returns_full_row_or_none() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let asset = NewAsset {
        hash: "bbbb000000000000000000000000000000000000000000000000000000000001".to_string(),
        original_filename: "detail.jpg".to_string(),
        storage_path: PathBuf::from("/lib/detail.jpg"),
        file_size: 4096,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: chrono::DateTime::parse_from_rfc3339("2024-06-15T10:30:00Z")
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc)),
        latitude: Some(37.7749),
        longitude: Some(-122.4194),
        camera_make: Some("Canon".to_string()),
        camera_model: Some("EOS R5".to_string()),
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
        thumbnails_generated: true,
        duration_secs: None,
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    };
    let id = match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    let detail = repo.fetch_by_id(id).await.expect("fetch").expect("present");
    assert_eq!(detail.original_filename, "detail.jpg");
    assert_eq!(detail.file_size, 4096);
    assert_eq!(detail.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(detail.camera_make.as_deref(), Some("Canon"));
    assert_eq!(detail.camera_model.as_deref(), Some("EOS R5"));
    assert!((detail.latitude.unwrap() - 37.7749).abs() < 1e-6);
    assert!(detail.thumbnails_generated);

    let missing = repo
        .fetch_by_id(eidetic_core::AssetId::new())
        .await
        .expect("fetch");
    assert!(missing.is_none());
}

#[tokio::test]
async fn transcript_round_trips_in_order() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let asset = NewAsset {
        hash: "1111aaaa2222bbbb3333cccc4444dddd5555eeee6666ffff7777aaaa8888bbbb".to_string(),
        original_filename: "talk.mp4".to_string(),
        storage_path: PathBuf::from("/library/11/11/1111aaaa.mp4"),
        file_size: 1,
        mime_type: Some("video/mp4".to_string()),
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
        duration_secs: Some(12.0),
        video_codec: Some("h264".to_string()),
        pixel_width: None,
        pixel_height: None,
    };
    let id = match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    assert!(repo.fetch_transcript(id).await.expect("empty").is_empty());

    // Stored out of order; fetch returns playback order.
    let segments = vec![
        (5.0, 8.0, "second thing said".to_string()),
        (0.5, 4.0, "first thing said".to_string()),
    ];
    repo.store_transcript(id, &segments).await.expect("store");
    let back = repo.fetch_transcript(id).await.expect("fetch");
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].2, "first thing said");
    assert!((back[0].0 - 0.5).abs() < 1e-9);
    assert_eq!(back[1].2, "second thing said");
}
