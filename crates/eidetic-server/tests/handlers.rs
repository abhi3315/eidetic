//! Handler-level tests: build the Axum router via `test_router`,
//! exercise routes via `tower::ServiceExt::oneshot`, assert status
//! codes and content snippets.
//!
//! Uses a real testcontainer Postgres because PR #15 deleted the
//! AssetIndex trait; there is no in-memory mock to substitute.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use eidetic_core::{Config, Paths};
use eidetic_db::{NewAsset, PgAssetsRepo};
use http_body_util::BodyExt;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};
use tower::ServiceExt;

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

/// Build an AppState wired to a real repo + a dead-end embedder channel.
async fn fixture() -> (
    eidetic_server::TestRouter,
    PgAssetsRepo,
    tempfile::TempDir,
    testcontainers::ContainerAsync<GenericImage>,
) {
    let (container, url) = start_db().await;
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_url: url.clone(),
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    // Build a second repo for the test to use directly (the router takes
    // one by value). Same pool URL, fresh connection.
    let config2 = Config {
        database_url: url,
        ..config.clone()
    };
    let pool2 = eidetic_db::connect(&config2).await.expect("connect2");
    let repo_clone = PgAssetsRepo::new(pool2);

    let router = eidetic_server::test_router(repo, config.paths.library_dir.clone());
    (router, repo_clone, tmp, container)
}

#[tokio::test]
async fn index_with_empty_db_renders_empty_grid() {
    let (router, _repo, _tmp, _container) = fixture().await;

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
    let (router, _repo, _tmp, _container) = fixture().await;

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
    let (router, _repo, _tmp, _container) = fixture().await;

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
    let (router, _repo, _tmp, _container) = fixture().await;

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
    let (router, _repo, _tmp, _container) = fixture().await;

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
    let (router, _repo, tmp, _container) = fixture().await;

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
    let (router, repo, tmp, _container) = fixture().await;

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
        thumbnails_generated: false,
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
