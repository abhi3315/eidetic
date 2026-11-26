use eidetic_core::Config;
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
use std::path::PathBuf;

/// A SigLIP id as migration 009 backfilled it, before revisions were pinned.
const LEGACY: &str = "siglip2-base-patch16-256";
/// The same variant under its pinned identity (ADR-0013).
const PINNED: &str = "siglip2-base-patch16-256@d1114256522a37ffa257a0a58017348ab0058db2";

/// A throwaway SQLite database in its own temp dir.
///
/// The returned `TempDir` guard must outlive the test body — dropping it
/// deletes the directory and the database file along with it.
fn temp_db() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().expect("tempdir");
    // Every path lives under the tempdir: these tests must not depend on
    // the host having a home directory, and never touch a real library.
    let config = Config {
        database_path: dir.path().join("eidetic.db"),
        paths: eidetic_core::Paths {
            library_dir: dir.path().join("library"),
            models_cache: dir.path().join("models"),
        },
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
    repo.store_embedding(id_a, &[0.1f32; 768], "siglip2-base-patch16-256")
        .await
        .expect("store embedding");

    let unembedded = repo
        .fetch_unembedded("siglip2-base-patch16-256")
        .await
        .expect("fetch unembedded");
    assert_eq!(unembedded.len(), 1);
    let (_, path) = &unembedded[0];
    assert!(path.to_str().unwrap().contains("b.jpg"));

    // Switching EIDETIC_MODEL changes the id the caller asks for, and
    // asset_a's row was produced by the old model — so it is work again.
    // Keying on row existence alone reported an empty queue here while
    // search skipped the whole library.
    let after_model_change = repo
        .fetch_unembedded("siglip2-so400m-patch14-384")
        .await
        .expect("fetch unembedded");
    assert_eq!(
        after_model_change.len(),
        2,
        "a row from another model is pending work, not a finished asset"
    );
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

    repo.store_embedding(id_a, &emb_a, "siglip2-base-patch16-256")
        .await
        .expect("embed a");
    repo.store_embedding(id_b, &emb_b, "siglip2-base-patch16-256")
        .await
        .expect("embed b");

    // Query with e1: asset A should come first (score ~1.0), B second (score ~0.0).
    let query = emb_a.clone();
    let results = repo
        .search_similar(&query, "siglip2-base-patch16-256", 10, None)
        .await
        .expect("search");

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

fn media(hash_byte: char, name: &str, mime: &str) -> NewAsset {
    NewAsset {
        hash: hash_byte.to_string().repeat(64),
        original_filename: name.to_string(),
        storage_path: PathBuf::from(format!("/lib/{name}")),
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
        duration_secs: Some(4.0),
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    }
}

#[tokio::test]
async fn legacy_rows_never_rank_under_a_pinned_model_id() {
    // Rows written before model ids carried a revision (migration 009's bare
    // names) have the same width as the pinned export's rows, so a width
    // check cannot tell them apart. Search, dupes input and the re-embed
    // queue must all go by exact id — and nothing is deleted to get there.
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);
    let insert = |a| async {
        match repo.insert_asset(a).await.expect("insert") {
            InsertOutcome::Inserted(id) => id,
            _ => panic!("expected Inserted"),
        }
    };
    let photo = insert(media('a', "a.jpg", "image/jpeg")).await;
    let video = insert(media('b', "b.mp4", "video/mp4")).await;

    let mut v = vec![0.0f32; 768];
    v[0] = 1.0;
    repo.store_embedding(photo, &v, LEGACY)
        .await
        .expect("legacy photo");
    let frame = eidetic_db::NewFrame {
        ts_secs: 1.0,
        vector: v.clone(),
        motion: None,
    };
    repo.store_frame_embeddings(video, &[frame], LEGACY)
        .await
        .expect("legacy frames");

    // Only legacy rows: a loud mismatch naming both ids, not an empty page
    // and not legacy hits.
    let Err(err) = repo.search_similar(&v, PINNED, 10, None).await else {
        panic!("legacy rows must not answer a pinned query");
    };
    let msg = err.to_string();
    assert!(msg.contains(PINNED) && msg.contains(LEGACY), "{msg}");
    assert!(msg.contains("eidetic embed"), "{msg}");

    // Both assets are queued for explicit re-embedding under the pinned id,
    // and dupes sees nothing comparable yet.
    assert_eq!(repo.fetch_unembedded(PINNED).await.expect("queue").len(), 1);
    assert_eq!(
        repo.fetch_videos_unembedded(PINNED)
            .await
            .expect("queue")
            .len(),
        1
    );
    assert!(
        repo.fetch_all_image_embeddings(PINNED)
            .await
            .expect("dupes")
            .is_empty()
    );
    assert!(
        repo.fetch_all_frame_embedding_sets(PINNED)
            .await
            .expect("dupes")
            .is_empty()
    );
    // The legacy rows are still there for the legacy id.
    assert_eq!(
        repo.fetch_all_image_embeddings(LEGACY)
            .await
            .expect("legacy")
            .len(),
        1
    );

    // Mid re-embed: the photo moves to the pinned id, the video has not.
    // Search ranks only the pinned row; the legacy frame stays out.
    repo.store_embedding(photo, &v, PINNED)
        .await
        .expect("re-embed photo");
    let hits = repo
        .search_similar(&v, PINNED, 10, None)
        .await
        .expect("search");
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![photo]);
    assert!(
        repo.fetch_unembedded(PINNED)
            .await
            .expect("queue")
            .is_empty()
    );
    assert_eq!(
        repo.fetch_all_frame_embedding_sets(LEGACY)
            .await
            .expect("legacy frames kept")
            .len(),
        1
    );
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
    repo.store_transcript(id, &segments, "whisper-small")
        .await
        .expect("store");
    let back = repo.fetch_transcript(id).await.expect("fetch");
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].2, "first thing said");
    assert!((back[0].0 - 0.5).abs() < 1e-9);
    assert_eq!(back[1].2, "second thing said");
}

// ---------------------------------------------------------------------------
// Stats readiness: the pending counts are the embed backlog, per model.
// ---------------------------------------------------------------------------

/// Fetch stats for `model` and check them against the backlog `eidetic embed`
/// would process for it, then return them for fixture-specific checks.
async fn assert_stats_match_backlog(repo: &AssetsRepo, model: &str) -> eidetic_db::LibraryStats {
    let stats = repo.fetch_stats(model).await.expect("stats");
    let images = repo.fetch_unembedded(model).await.expect("image backlog");
    let videos = repo
        .fetch_videos_unembedded(model)
        .await
        .expect("video backlog");
    assert_eq!(stats.pending_images, images.len() as i64, "{model}");
    assert_eq!(stats.pending_videos, videos.len() as i64, "{model}");
    assert_eq!(
        stats.ready(),
        stats.images + stats.videos - stats.pending_images - stats.pending_videos,
        "{model}"
    );
    stats
}

async fn insert(repo: &AssetsRepo, asset: NewAsset) -> eidetic_core::AssetId {
    match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    }
}

fn unit_vector() -> Vec<f32> {
    let mut v = vec![0.0f32; 768];
    v[0] = 1.0;
    v
}

fn frame(ts_secs: f64) -> eidetic_db::NewFrame {
    eidetic_db::NewFrame {
        ts_secs,
        vector: unit_vector(),
        motion: None,
    }
}

fn blob() -> Vec<u8> {
    unit_vector().iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A library embedded before model ids existed: migration 009 backfills the
/// bare legacy id, so none of it is ready for the pinned model.
#[tokio::test]
async fn stats_pending_follow_the_backlog_after_the_model_id_backfill() {
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
        .expect("open");
    let mut before = sqlx::migrate!("../../migrations");
    before.migrations = std::borrow::Cow::Owned(
        before
            .migrations
            .iter()
            .filter(|m| m.version <= 8)
            .cloned()
            .collect(),
    );
    before.run(&pool).await.expect("migrations 001-008");
    let legacy_rows = [
        (
            'a',
            "old.jpg",
            "image/jpeg",
            "INSERT INTO embeddings (asset_id, dim, vector) VALUES (?, 768, ?)",
        ),
        (
            'b',
            "old.mp4",
            "video/mp4",
            "INSERT INTO frame_embeddings (asset_id, ts_secs, dim, vector) VALUES (?, 1.0, 768, ?)",
        ),
    ];
    for (byte, name, mime, vector_row) in legacy_rows {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO assets (id, hash, original_filename, storage_path, file_size, mime_type) \
             VALUES (?, ?, ?, ?, 1, ?)",
        )
        .bind(&id)
        .bind(byte.to_string().repeat(64))
        .bind(name)
        .bind(format!("/lib/{name}"))
        .bind(mime)
        .execute(&pool)
        .await
        .expect("legacy asset");
        sqlx::query(vector_row)
            .bind(&id)
            .bind(blob())
            .execute(&pool)
            .await
            .expect("legacy vector");
    }
    pool.close().await;

    let repo = AssetsRepo::new(eidetic_db::connect(&config).await.expect("migrate"));

    let pinned = assert_stats_match_backlog(&repo, PINNED).await;
    assert_eq!(pinned.embedded, 2, "stored vectors are still counted");
    assert_eq!((pinned.pending_images, pinned.pending_videos), (1, 1));
    assert_eq!(pinned.ready(), 0, "nothing ranks under the pinned id");

    let legacy = assert_stats_match_backlog(&repo, LEGACY).await;
    assert_eq!((legacy.pending_images, legacy.pending_videos), (0, 0));
    assert_eq!(legacy.ready(), 2);
}

#[tokio::test]
async fn stats_pending_follow_the_backlog_on_a_mixed_model_library() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool.clone());

    let a = insert(&repo, media('a', "a.jpg", "image/jpeg")).await;
    let b = insert(&repo, media('b', "b.jpg", "image/jpeg")).await;
    insert(&repo, media('c', "c.jpg", "image/jpeg")).await;
    let v1 = insert(&repo, media('d', "v1.mp4", "video/mp4")).await;
    let v2 = insert(&repo, media('e', "v2.mp4", "video/mp4")).await;
    // Neither an image nor a video: never pending, never ready.
    insert(
        &repo,
        NewAsset {
            mime_type: None,
            ..media('f', "f.bin", "unused")
        },
    )
    .await;

    repo.store_embedding(a, &unit_vector(), PINNED)
        .await
        .expect("a");
    repo.store_embedding(b, &unit_vector(), LEGACY)
        .await
        .expect("b");
    repo.store_frame_embeddings(v1, &[frame(1.0)], PINNED)
        .await
        .expect("v1");
    repo.store_frame_embeddings(v2, &[frame(1.0)], PINNED)
        .await
        .expect("v2");
    // A leftover legacy frame beside the pinned ones; storing frames
    // replaces all of a video's rows, so it goes in directly.
    sqlx::query(
        "INSERT INTO frame_embeddings (asset_id, ts_secs, dim, vector, model_id) \
         VALUES (?, 2.0, 768, ?, ?)",
    )
    .bind(v2.as_uuid().to_string())
    .bind(blob())
    .bind(LEGACY)
    .execute(&pool)
    .await
    .expect("legacy frame");

    let pinned = assert_stats_match_backlog(&repo, PINNED).await;
    assert_eq!(pinned.total, 6);
    assert_eq!(pinned.embedded, 4);
    assert_eq!((pinned.pending_images, pinned.pending_videos), (2, 0));
    assert_eq!(pinned.ready(), 3);

    let legacy = assert_stats_match_backlog(&repo, LEGACY).await;
    assert_eq!(legacy.embedded, 4);
    assert_eq!((legacy.pending_images, legacy.pending_videos), (2, 1));
    assert_eq!(legacy.ready(), 2);
}

#[tokio::test]
async fn stats_pending_shrink_as_a_library_is_re_embedded() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

    let photo = insert(&repo, media('a', "a.jpg", "image/jpeg")).await;
    let video = insert(&repo, media('b', "b.mp4", "video/mp4")).await;
    repo.store_embedding(photo, &unit_vector(), LEGACY)
        .await
        .expect("legacy photo");
    repo.store_frame_embeddings(video, &[frame(1.0)], LEGACY)
        .await
        .expect("legacy video");

    let s = assert_stats_match_backlog(&repo, PINNED).await;
    assert_eq!(s.pending_images + s.pending_videos, 2);
    assert_eq!(s.embedded, 2);
    assert_stats_match_backlog(&repo, LEGACY).await;

    repo.store_embedding(photo, &unit_vector(), PINNED)
        .await
        .expect("re-embed photo");
    let s = assert_stats_match_backlog(&repo, PINNED).await;
    assert_eq!((s.pending_images, s.pending_videos), (0, 1));
    assert_eq!(s.embedded, 2, "re-embedding replaces, it does not add");
    assert_stats_match_backlog(&repo, LEGACY).await;

    repo.store_frame_embeddings(video, &[frame(1.0)], PINNED)
        .await
        .expect("re-embed video");
    let s = assert_stats_match_backlog(&repo, PINNED).await;
    assert_eq!((s.pending_images, s.pending_videos), (0, 0));
    assert_eq!(s.ready(), 2);
    let legacy = assert_stats_match_backlog(&repo, LEGACY).await;
    assert_eq!(legacy.ready(), 0);
}

/// A row no model produced is pending under every model, in the backlog and
/// in stats alike: search never ranks it.
#[tokio::test]
async fn embedding_without_a_model_id_is_pending_for_every_model() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool.clone());

    let photo = insert(&repo, media('a', "a.jpg", "image/jpeg")).await;
    repo.store_embedding(photo, &unit_vector(), PINNED)
        .await
        .expect("photo");
    sqlx::query("UPDATE embeddings SET model_id = NULL")
        .execute(&pool)
        .await
        .expect("clear model id");

    for model in [PINNED, LEGACY] {
        let backlog = repo.fetch_unembedded(model).await.expect("backlog");
        assert_eq!(
            backlog.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![photo],
            "{model}"
        );
        let s = assert_stats_match_backlog(&repo, model).await;
        assert_eq!((s.embedded, s.pending_images, s.ready()), (1, 1, 0));
    }
}
