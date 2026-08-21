use axum::body::Body;
use axum::http::{Request, StatusCode};
use eidetic_core::{Config, Paths};
use eidetic_db::{AssetsRepo, FacesRepo, NewAsset, NewFace};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// One temp dir holds both the SQLite file and the library tree. The returned
/// `TempDir` guard must outlive the test body, or the database file is
/// deleted mid-test.
async fn fixture() -> (
    eidetic_server::TestRouter,
    AssetsRepo,
    FacesRepo,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_path: tmp.path().join("eidetic.db"),
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");

    // The router takes the repos by value, so the test gets a second pair
    // over the same pool to seed rows with.
    let repo = AssetsRepo::new(pool.clone());
    let faces = FacesRepo::new(pool.clone());
    let repo_for_test = AssetsRepo::new(pool.clone());
    let faces_for_test = FacesRepo::new(pool);

    let router = eidetic_server::test_router(repo, faces, config.paths.library_dir.clone());
    (router, repo_for_test, faces_for_test, tmp)
}

/// Insert one asset whose bytes live at `library/<filename>`.
async fn seed_asset(
    repo: &AssetsRepo,
    tmp: &tempfile::TempDir,
    filename: &str,
    mime: &str,
    bytes: &[u8],
    hash_byte: u8,
) -> eidetic_core::AssetId {
    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let src_path = library.join(filename);
    std::fs::write(&src_path, bytes).unwrap();

    let asset = NewAsset {
        hash: format!("{hash_byte:02x}").repeat(32),
        original_filename: filename.to_string(),
        storage_path: src_path,
        file_size: bytes.len() as u64,
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
    };
    match repo.insert_asset(asset).await.expect("insert") {
        eidetic_db::InsertOutcome::Inserted(id) | eidetic_db::InsertOutcome::Existing(id) => id,
    }
}

#[tokio::test]
async fn index_with_empty_db_renders_empty_grid() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("Recent imports"));
    assert!(body_str.contains("No results."));
}

#[tokio::test]
async fn search_with_empty_q_redirects_to_index() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/search?q=")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Redirect::to() in Axum produces SEE_OTHER (303) by default.
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap();
    assert_eq!(location.to_str().unwrap(), "/");
}

#[tokio::test]
async fn assets_id_unknown_returns_404() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}", uuid::Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumbs_invalid_size_returns_404() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/thumbs/x/0000000000000000000000000000000000000000000000000000000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumbs_invalid_hash_returns_404() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/thumbs/s/not-a-hash")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumb_serves_jpeg_bytes_when_file_exists() {
    let (router, _repo, _faces, tmp) = fixture().await;

    let library = tmp.path().join("library");
    let hash = "abcd000000000000000000000000000000000000000000000000000000000001";
    let dir = library
        .join(".thumbs")
        .join("s")
        .join(&hash[..2])
        .join(&hash[2..4]);
    std::fs::create_dir_all(&dir).unwrap();
    let thumb_path = dir.join(format!("{hash}.jpg"));
    // Real JPEG magic + tiny body.
    std::fs::write(&thumb_path, [0xFF, 0xD8, 0xFF, 0xE0, b'X']).unwrap();

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/thumbs/s/{hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..2], &[0xFF, 0xD8]);
}

#[tokio::test]
async fn raw_streams_original_with_mime_and_disposition() {
    let (router, repo, _faces, tmp) = fixture().await;

    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let src_path = library.join("photo.jpg");
    std::fs::write(&src_path, b"FAKE-JPEG-BODY").unwrap();

    let hash = "cafe000000000000000000000000000000000000000000000000000000000001";
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "user-pic.jpg".to_string(),
        storage_path: src_path.clone(),
        file_size: 14,
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
        eidetic_db::InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}/raw", id.as_uuid()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "image/jpeg"
    );
    assert!(
        response
            .headers()
            .get(axum::http::header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("user-pic.jpg")
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"FAKE-JPEG-BODY");
}

#[tokio::test]
async fn raw_serves_a_byte_range_with_206() {
    let (router, repo, _faces, tmp) = fixture().await;
    let id = seed_asset(&repo, &tmp, "clip.mp4", "video/mp4", b"hello world", 0x21).await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}/raw", id.as_uuid()))
                .header(axum::http::header::RANGE, "bytes=0-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_RANGE)
            .expect("Content-Range header")
            .to_str()
            .unwrap(),
        "bytes 0-1/11"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::ACCEPT_RANGES)
            .expect("Accept-Ranges header")
            .to_str()
            .unwrap(),
        "bytes"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "video/mp4"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"he", "exactly the two requested bytes");
}

#[tokio::test]
async fn raw_rejects_unsatisfiable_range_with_416() {
    let (router, repo, _faces, tmp) = fixture().await;
    let id = seed_asset(&repo, &tmp, "clip.mp4", "video/mp4", b"hello world", 0x22).await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}/raw", id.as_uuid()))
                .header(axum::http::header::RANGE, "bytes=999-1000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn persons_index_renders_empty_state() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/persons")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("People"));
    assert!(body_str.contains("No people yet"));
}

/// A plausible face inside a small test photo. Landmarks are the ArcFace
/// template shifted by +40, i.e. what a real frontal face at that spot would
/// produce, stored in template order exactly like the CLI write site does.
fn test_face(asset_id: eidetic_core::AssetId, score: f32) -> NewFace {
    let template = [
        (38.2946_f32, 51.6963_f32),
        (73.5318, 51.5014),
        (56.0252, 71.7366),
        (41.5493, 92.3655),
        (70.7299, 92.2041),
    ];
    NewFace {
        asset_id,
        bbox: (40.0, 40.0, 80.0, 100.0),
        landmarks: template.map(|(x, y)| (x + 40.0, y + 40.0)),
        score,
        embedding: vec![1.0, 0.0],
    }
}

/// Encode a 200x200 gradient JPEG so face-crop rendering has real pixels.
fn tiny_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(200, 200, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut std::io::Cursor::new(&mut bytes))
        .encode_image(&img)
        .expect("encode");
    bytes
}

#[tokio::test]
async fn persons_index_lists_people_with_cover_crops() {
    let (router, repo, faces, tmp) = fixture().await;
    let jpeg = tiny_jpeg();
    let asset = seed_asset(&repo, &tmp, "group.jpg", "image/jpeg", &jpeg, 0x23).await;

    let ids = faces
        .record_detection(asset, &[test_face(asset, 0.9)])
        .await
        .expect("record");
    let person = faces.create_person().await.expect("person");
    faces
        .assign_face(ids[0], person, false)
        .await
        .expect("assign");
    faces.name_person(person, "Asha").await.expect("name");

    let response = router
        .oneshot(
            Request::builder()
                .uri("/persons")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("Asha"));
    assert!(body_str.contains(&format!("/persons/{person}")));
    assert!(body_str.contains(&format!("/faces/{}/crop", ids[0])));
}

#[tokio::test]
async fn person_page_unknown_returns_404() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/persons/{}", uuid::Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn person_page_links_faces_to_their_assets() {
    let (router, repo, faces, tmp) = fixture().await;
    let jpeg = tiny_jpeg();
    let asset = seed_asset(&repo, &tmp, "trip.jpg", "image/jpeg", &jpeg, 0x24).await;

    let ids = faces
        .record_detection(asset, &[test_face(asset, 0.9)])
        .await
        .expect("record");
    let person = faces.create_person().await.expect("person");
    faces
        .assign_face(ids[0], person, false)
        .await
        .expect("assign");

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/persons/{person}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("(unnamed)"));
    assert!(body_str.contains(&format!("/faces/{}/crop", ids[0])));
    assert!(body_str.contains(&format!("/assets/{asset}")));
}

#[tokio::test]
async fn face_crop_unknown_returns_404() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/faces/{}/crop", uuid::Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn face_crop_renders_112px_jpeg_and_caches_it() {
    let (router, repo, faces, tmp) = fixture().await;
    let jpeg = tiny_jpeg();
    let asset = seed_asset(&repo, &tmp, "portrait.jpg", "image/jpeg", &jpeg, 0x25).await;

    let ids = faces
        .record_detection(asset, &[test_face(asset, 0.9)])
        .await
        .expect("record");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/faces/{}/crop", ids[0]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "image/jpeg"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap(),
        "public, max-age=31536000, immutable"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let decoded = image::load_from_memory(&body).expect("valid JPEG");
    assert_eq!(decoded.width(), 112);
    assert_eq!(decoded.height(), 112);

    // The crop must have been cached on disk...
    let cache_path = tmp
        .path()
        .join("library")
        .join(".faces")
        .join(format!("{}.jpg", ids[0]));
    assert!(cache_path.exists(), "crop cache file missing");

    // ...and a second request serves those exact bytes.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/faces/{}/crop", ids[0]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cached = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&cached[..], &body[..], "cached crop must be byte-identical");
}

#[tokio::test]
async fn asset_detail_lists_people_in_the_photo() {
    let (router, repo, faces, tmp) = fixture().await;
    let jpeg = tiny_jpeg();
    let asset = seed_asset(&repo, &tmp, "friends.jpg", "image/jpeg", &jpeg, 0x26).await;

    let ids = faces
        .record_detection(asset, &[test_face(asset, 0.9)])
        .await
        .expect("record");
    let person = faces.create_person().await.expect("person");
    faces
        .assign_face(ids[0], person, false)
        .await
        .expect("assign");
    faces.name_person(person, "Asha").await.expect("name");

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{asset}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("Asha"));
    assert!(body_str.contains(&format!("/persons/{person}")));
    assert!(body_str.contains(&format!("/faces/{}/crop", ids[0])));
}
