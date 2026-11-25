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

    let asset = new_asset(filename, mime, src_path, bytes.len() as u64, hash_byte);
    match repo.insert_asset(asset).await.expect("insert") {
        eidetic_db::InsertOutcome::Inserted(id) | eidetic_db::InsertOutcome::Existing(id) => id,
    }
}

fn new_asset(
    filename: &str,
    mime: &str,
    storage_path: std::path::PathBuf,
    file_size: u64,
    hash_byte: u8,
) -> NewAsset {
    NewAsset {
        hash: format!("{hash_byte:02x}").repeat(32),
        original_filename: filename.to_string(),
        storage_path,
        file_size,
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
        duration_secs: None,
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    }
}

/// Insert one asset taken at `taken`, with bytes at a path that does not
/// depend on the visible filename. `hash_byte` must be unique per test.
async fn seed_dav(
    repo: &AssetsRepo,
    tmp: &tempfile::TempDir,
    filename: &str,
    bytes: &[u8],
    hash_byte: u8,
    taken: chrono::DateTime<chrono::Utc>,
) -> eidetic_core::AssetId {
    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let path = library.join(format!("stored-{hash_byte:02x}.bin"));
    std::fs::write(&path, bytes).unwrap();
    let mut asset = new_asset(
        filename,
        "application/octet-stream",
        path,
        bytes.len() as u64,
        hash_byte,
    );
    asset.date_taken = Some(taken);
    match repo.insert_asset(asset).await.expect("insert") {
        eidetic_db::InsertOutcome::Inserted(id) => id,
        eidetic_db::InsertOutcome::Existing(_) => panic!("duplicate hash byte {hash_byte:#x}"),
    }
}

async fn dav_request(
    router: &eidetic_server::TestRouter,
    method: &str,
    uri: &str,
) -> (StatusCode, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Depth", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

/// File hrefs of a month listing (the collection's own href is dropped).
async fn month_files(router: &eidetic_server::TestRouter, month: &str) -> (Vec<String>, Vec<u8>) {
    let (status, body) = dav_request(router, "PROPFIND", month).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let text = String::from_utf8(body.clone()).unwrap();
    let hrefs = text
        .split("<D:href>")
        .skip(1)
        .filter_map(|s| s.split("</D:href>").next())
        .filter(|h| *h != month)
        .map(str::to_string)
        .collect();
    (hrefs, body)
}

#[tokio::test]
async fn dav_lists_every_colliding_asset_under_a_unique_name() {
    use chrono::TimeZone;
    let (router, repo, _faces, tmp) = fixture().await;
    let base = chrono::Utc.with_ymd_and_hms(2024, 3, 15, 12, 0, 0).unwrap();
    let at = |s: i64| base + chrono::Duration::seconds(s);

    // C is inserted first only to learn its id prefix; it is listed last of
    // the "a" group and has to retry past three taken names.
    let c = seed_dav(&repo, &tmp, "a.jpg", b"bytes-c", 1, at(3)).await;
    let c8 = c.as_uuid().simple().to_string()[..8].to_string();
    seed_dav(&repo, &tmp, "a.jpg", b"bytes-a", 2, at(0)).await;
    seed_dav(&repo, &tmp, &format!("a-{c8}.jpg"), b"bytes-b", 3, at(1)).await;
    seed_dav(&repo, &tmp, &format!("a-{c8}-2.jpg"), b"bytes-d", 4, at(2)).await;
    // Extensionless: G's natural name equals the name F would be given, yet
    // G is listed after F.
    seed_dav(&repo, &tmp, "notes", b"bytes-e", 5, at(4)).await;
    let f = seed_dav(&repo, &tmp, "notes", b"bytes-f", 6, at(5)).await;
    let f8 = f.as_uuid().simple().to_string()[..8].to_string();
    seed_dav(&repo, &tmp, &format!("notes-{f8}"), b"bytes-g", 7, at(6)).await;
    // Same name, another month.
    let april = chrono::Utc.with_ymd_and_hms(2024, 4, 15, 12, 0, 0).unwrap();
    seed_dav(&repo, &tmp, "a.jpg", b"bytes-h", 8, april).await;

    let expected: Vec<(String, &[u8])> = vec![
        ("a.jpg".into(), b"bytes-a"),
        (format!("a-{c8}.jpg"), b"bytes-b"),
        (format!("a-{c8}-2.jpg"), b"bytes-d"),
        (format!("a-{c8}-3.jpg"), b"bytes-c"),
        ("notes".into(), b"bytes-e"),
        (format!("notes-{f8}"), b"bytes-g"),
        (format!("notes-{f8}-2"), b"bytes-f"),
    ];

    let (hrefs, first_body) = month_files(&router, "/dav/2024/03/").await;
    let mut want_hrefs: Vec<String> = expected
        .iter()
        .map(|(n, _)| format!("/dav/2024/03/{n}"))
        .collect();
    want_hrefs.sort();
    let mut got = hrefs.clone();
    got.sort();
    assert_eq!(got, want_hrefs);

    let (_, second_body) = month_files(&router, "/dav/2024/03/").await;
    assert_eq!(first_body, second_body, "listing is stable");

    for (name, bytes) in &expected {
        let (status, body) = dav_request(&router, "GET", &format!("/dav/2024/03/{name}")).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(body, *bytes, "{name}");
    }

    let (hrefs, _) = month_files(&router, "/dav/2024/04/").await;
    assert_eq!(hrefs, vec!["/dav/2024/04/a.jpg".to_string()]);
    let (status, body) = dav_request(&router, "GET", "/dav/2024/04/a.jpg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"bytes-h");
}

#[tokio::test]
async fn dav_propfind_walks_the_virtual_tree_and_get_serves_bytes() {
    let (router, repo, _faces, _tmp) = fixture().await;
    let id = seed_asset(&repo, &_tmp, "clip.jpg", "image/jpeg", b"JPEGBYTES", 0x31).await;
    let _ = id;

    // Root listing: 207 with at least one year collection.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PROPFIND")
                .uri("/dav/")
                .header("Depth", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MULTI_STATUS);
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .into_owned();
    assert!(body.contains("<D:collection/>"), "{body}");
    let year = body
        .split("/dav/")
        .nth(2)
        .and_then(|s| s.split('/').next())
        .expect("a year href")
        .to_string();

    // Walk to the month, then list files.
    let month_list = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PROPFIND")
                .uri(format!("/dav/{year}/"))
                .header("Depth", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(month_list.status(), StatusCode::MULTI_STATUS);
    let body = String::from_utf8_lossy(&month_list.into_body().collect().await.unwrap().to_bytes())
        .into_owned();
    let month = body
        .split(&format!("/dav/{year}/"))
        .nth(2)
        .and_then(|s| s.split('/').next())
        .expect("a month href")
        .to_string();

    let files = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PROPFIND")
                .uri(format!("/dav/{year}/{month}/"))
                .header("Depth", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&files.into_body().collect().await.unwrap().to_bytes())
        .into_owned();
    assert!(body.contains("clip.jpg"), "{body}");
    assert!(
        body.contains("<D:getcontentlength>9</D:getcontentlength>"),
        "{body}"
    );

    // GET the file through DAV.
    let got = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/dav/{year}/{month}/clip.jpg"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(got.status(), StatusCode::OK);
    let bytes = got.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"JPEGBYTES");

    // Read-only: PUT is refused.
    let put = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/dav/{year}/{month}/new.jpg"))
                .body(Body::from("x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn play_without_transcode_copy_returns_404() {
    let (router, repo, _faces, _tmp) = fixture().await;
    let id = seed_asset(&repo, &_tmp, "clip.mp4", "video/mp4", b"stub", 0x77).await;
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}/play", id.as_uuid()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
    assert!(body_str.contains("Library (0 assets)"), "{body_str}");
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

/// The invalid-input error bodies quote the request back at the client.
/// Whatever they quote, the response must be inert text: a browser that
/// followed a link to this URL must not end up executing the segment.
#[tokio::test]
async fn error_bodies_are_served_as_inert_text() {
    let (router, _repo, _faces, _tmp) = fixture().await;

    // Percent-encoded `<script>alert(1)</script>` as the thumbnail size, so
    // axum's path decoding hands the handler raw markup.
    let payload = "%3Cscript%3Ealert(1)%3C%2Fscript%3E";
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/thumbs/{payload}/not-a-hash"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let content_types: Vec<&str> = response
        .headers()
        .get_all(axum::http::header::CONTENT_TYPE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(
        content_types,
        vec!["text/plain; charset=utf-8"],
        "error responses must be typed as text, exactly once"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    // The echo is real — this test only means something because the payload
    // reaches the body — but it arrives without any HTML wrapper around it.
    assert!(body.contains("<script>alert(1)</script>"), "{body}");
    assert!(
        !body.to_ascii_lowercase().contains("<!doctype"),
        "error body is being rendered as a document: {body}"
    );
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
        ts_secs: None,
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
        .record_detection(
            asset,
            &[test_face(asset, 0.9)],
            "yunet-2023mar",
            "auraface-v1",
        )
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
        .record_detection(
            asset,
            &[test_face(asset, 0.9)],
            "yunet-2023mar",
            "auraface-v1",
        )
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
        .record_detection(
            asset,
            &[test_face(asset, 0.9)],
            "yunet-2023mar",
            "auraface-v1",
        )
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
        .record_detection(
            asset,
            &[test_face(asset, 0.9)],
            "yunet-2023mar",
            "auraface-v1",
        )
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
