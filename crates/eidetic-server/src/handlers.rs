use crate::{AppState, ServerError};
use axum::extract::{Path, Query, State};
use axum::response::Html;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    pub q: Option<String>,
    pub limit: Option<u32>,
}

pub(crate) async fn index(State(state): State<AppState>) -> Result<Html<String>, ServerError> {
    use crate::views::{GridTile, asset_grid, layout, search_form};
    use maud::html;

    let recent = state
        .repo
        .fetch_recent(24)
        .await
        .map_err(ServerError::DbFailed)?;

    let tiles: Vec<GridTile> = recent
        .into_iter()
        .map(|r| GridTile {
            id: r.id,
            hash: r.hash,
            alt: r.original_filename,
            score: None,
            mime_type: r.mime_type,
            thumbnails_generated: r.thumbnails_generated,
            file_size: r.file_size,
        })
        .collect();

    let body = html! {
        (search_form(""))
        h2 { "Recent imports" }
        (asset_grid(&tiles))
    };

    Ok(Html(layout("Home", body).into_string()))
}

pub(crate) async fn search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<axum::response::Response, ServerError> {
    use crate::views::{GridTile, asset_grid, layout, search_form};
    use axum::http::StatusCode;
    use axum::response::{Html, IntoResponse, Redirect};
    use maud::html;
    use tokio::sync::oneshot;

    let query_text = match q.q.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Ok(Redirect::to("/").into_response()),
    };
    let limit = q.limit.unwrap_or(24).min(60);

    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .embed_tx
        .send((query_text.clone(), reply_tx))
        .await
        .map_err(|_| {
            ServerError::EmbedFailed(eidetic_ml::Error::Inference(
                "embedder worker dropped".into(),
            ))
        })?;

    let query_vec = reply_rx
        .await
        .map_err(|_| {
            ServerError::EmbedFailed(eidetic_ml::Error::Inference(
                "embedder reply dropped".into(),
            ))
        })?
        .map_err(ServerError::EmbedFailed)?;

    let results = state
        .repo
        .search_similar(&query_vec, limit)
        .await
        .map_err(ServerError::DbFailed)?;

    // SearchResult carries storage_path (CAS-shaped) but no hash. The CAS
    // filename's stem IS the hash hex; derive it from there. If the path
    // shape is somehow weird, fall back to a zero hash and the tile will
    // show a broken-image icon (404 from /thumbs/m/<bogus>).
    let tiles: Vec<GridTile> = results
        .into_iter()
        .map(|r| {
            let hash = hash_from_path(&r.storage_path)
                .and_then(|hex| eidetic_core::Sha256::from_hex(&hex))
                .unwrap_or_else(|| eidetic_core::Sha256::from_bytes([0u8; 32]));
            // Search results are always embedded, which means mime is image/*.
            // Use empty string as a safe default if it's somehow None; the
            // grid will fall through to the placeholder branch.
            let mime_type = r.mime_type.unwrap_or_default();
            GridTile {
                id: r.id,
                hash,
                alt: query_text.clone(),
                score: Some(r.score),
                mime_type,
                thumbnails_generated: r.thumbnails_generated,
                file_size: r.file_size,
            }
        })
        .collect();

    let body = html! {
        (search_form(&query_text))
        h2 { "Results for " (query_text) " (" (tiles.len()) ")" }
        (asset_grid(&tiles))
    };

    Ok((
        StatusCode::OK,
        Html(layout(&query_text, body).into_string()),
    )
        .into_response())
}

fn hash_from_path(p: &std::path::Path) -> Option<String> {
    p.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

pub(crate) async fn asset_detail(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, ServerError> {
    use crate::views::{DetailView, detail_page, layout};
    use eidetic_core::AssetId;
    use maud::html;

    let asset_id = AssetId::from(id);
    let detail = state
        .repo
        .fetch_by_id(asset_id)
        .await
        .map_err(ServerError::DbFailed)?
        .ok_or_else(|| ServerError::NotFound(format!("asset {id}")))?;

    let view = DetailView {
        id: detail.id,
        hash: detail.hash,
        original_filename: detail.original_filename.clone(),
        mime_type: detail.mime_type,
        file_size: detail.file_size,
        imported_at: detail.imported_at,
        date_taken: detail.date_taken,
        latitude: detail.latitude,
        longitude: detail.longitude,
        camera_make: detail.camera_make,
        camera_model: detail.camera_model,
        lens_model: detail.lens_model,
        focal_length: detail.focal_length,
        focal_length_35mm: detail.focal_length_35mm,
        aperture: detail.aperture,
        shutter: detail.shutter,
        iso: detail.iso,
        altitude: detail.altitude,
        country_name: detail.country_name,
        admin1: detail.admin1,
        place: detail.place,
        place_distance_m: detail.place_distance_m,
        thumbnails_generated: detail.thumbnails_generated,
    };

    let people: Vec<crate::views::PersonChip> = state
        .faces
        .fetch_faces_for_asset(asset_id)
        .await
        .map_err(ServerError::DbFailed)?
        .into_iter()
        .map(|f| crate::views::PersonChip {
            face_id: f.face_id,
            person_id: f.person_id,
            name: f.person_name,
        })
        .collect();

    let title = detail.original_filename;
    let body = html! { (detail_page(&view, &people)) };
    Ok(Html(layout(&title, body).into_string()))
}

pub(crate) async fn asset_raw(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    req: axum::extract::Request,
) -> Result<axum::response::Response, ServerError> {
    use axum::http::header;
    use eidetic_core::AssetId;
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    let asset_id = AssetId::from(id);
    let detail = state
        .repo
        .fetch_by_id(asset_id)
        .await
        .map_err(ServerError::DbFailed)?
        .ok_or_else(|| ServerError::NotFound(format!("asset {id}")))?;

    // ServeFile handles Range requests (206/416, Content-Range,
    // Accept-Ranges) — required for <video> seeking; Safari refuses to play
    // without it. The stored mime beats extension guessing, so pass it
    // explicitly.
    let mime: mime::Mime = detail
        .mime_type
        .as_deref()
        .and_then(|m| m.parse().ok())
        .unwrap_or(mime::APPLICATION_OCTET_STREAM);

    let response = match ServeFile::new_with_mime(&detail.storage_path, &mime)
        .oneshot(req)
        .await
    {
        Ok(response) => response,
        // ServeFile's error type is Infallible: I/O problems come back as
        // error *responses* (404 for a missing file), not as Err.
        Err(infallible) => match infallible {},
    };
    let mut response = response.map(axum::body::Body::new);

    // Backslash-escape any quotes in the filename for the legacy
    // Content-Disposition form. Personal use; the cleaner RFC 6266
    // filename* with UTF-8 encoding can come later.
    let safe_filename = detail.original_filename.replace('"', "\\\"");
    let disposition = format!("inline; filename=\"{safe_filename}\"");
    if let Ok(value) = disposition.parse() {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }

    Ok(response)
}

pub(crate) async fn persons_index(
    State(state): State<AppState>,
) -> Result<Html<String>, ServerError> {
    use crate::views::{PersonTile, layout, persons_page};

    let persons = state
        .faces
        .list_persons()
        .await
        .map_err(ServerError::DbFailed)?;

    let tiles: Vec<PersonTile> = persons
        .into_iter()
        // list_persons only returns people with faces, so cover_face is
        // always present; a raced-away person is simply skipped.
        .filter_map(|p| {
            p.cover_face.map(|cover| PersonTile {
                id: p.id,
                cover_face: cover,
                name: p.name,
                face_count: p.face_count,
            })
        })
        .collect();

    Ok(Html(layout("People", persons_page(&tiles)).into_string()))
}

pub(crate) async fn person_detail(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, ServerError> {
    use crate::views::{FaceCrop, layout, person_page};
    use eidetic_core::PersonId;

    let person_id = PersonId::from(id);
    let person = state
        .faces
        .fetch_person(person_id)
        .await
        .map_err(ServerError::DbFailed)?
        .ok_or_else(|| ServerError::NotFound(format!("person {id}")))?;

    let crops: Vec<FaceCrop> = state
        .faces
        .fetch_faces_for_person(person_id)
        .await
        .map_err(ServerError::DbFailed)?
        .into_iter()
        .map(|f| FaceCrop {
            face_id: f.face_id,
            asset_id: f.asset_id,
        })
        .collect();

    let title = person.name.clone().unwrap_or_else(|| "(unnamed)".into());
    Ok(Html(
        layout(&title, person_page(&person, &crops)).into_string(),
    ))
}

pub(crate) async fn face_crop(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ServerError> {
    use eidetic_core::FaceId;

    let face_id = FaceId::from(id);
    let cache_dir = state.library_dir.join(".faces");
    let cache_path = cache_dir.join(format!("{face_id}.jpg"));

    // Serve the cached crop if a previous request already rendered it.
    // Crops are 112x112 JPEGs, small enough to read whole.
    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return Ok(jpeg_crop_response(bytes));
    }

    let face = state
        .faces
        .fetch_face(face_id)
        .await
        .map_err(ServerError::DbFailed)?
        .ok_or_else(|| ServerError::NotFound(format!("face {id}")))?;

    // Decode + align is CPU-bound (the original can be a 50 MP photo), so it
    // must not run on the async worker threads.
    let bytes = tokio::task::spawn_blocking(move || render_face_crop(&face))
        .await
        .map_err(|e| ServerError::CropFailed(format!("crop thread panicked: {e}")))??;

    // Cache on disk: write to a temp name, then rename, so a concurrent
    // request can never read a half-written file.
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .map_err(ServerError::Io)?;
    let tmp_path = cache_dir.join(format!("{face_id}.jpg.tmp-{}", std::process::id()));
    if tokio::fs::write(&tmp_path, &bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp_path, &cache_path).await;
    }

    Ok(jpeg_crop_response(bytes))
}

fn jpeg_crop_response(bytes: Vec<u8>) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(axum::body::Body::from(bytes))
        .expect("static headers should build")
}

/// Rebuild [`eidetic_ml::Landmarks`] from the DB's template-order array.
///
/// The write site stores `Landmarks::as_template_order()`, which is
/// `[right_eye, left_eye, nose, right_mouth, left_mouth]` — the template is
/// named in image space, so the columns called `lm_left_eye_*` hold the
/// image-left point, i.e. the subject's anatomical RIGHT eye. This is the
/// exact inverse of `as_template_order`; the round-trip test below pins it.
pub(crate) fn landmarks_from_template_order(lm: [(f32, f32); 5]) -> eidetic_ml::Landmarks {
    eidetic_ml::Landmarks {
        right_eye: lm[0],
        left_eye: lm[1],
        nose: lm[2],
        right_mouth: lm[3],
        left_mouth: lm[4],
    }
}

/// Decode the original image, align the face to 112x112, JPEG-encode it.
///
/// Runs on a blocking thread. When the stored landmarks are degenerate
/// (`align_face` returns `None`), falls back to a plain bbox crop resized to
/// the same 112x112 so the tile still shows *something*.
fn render_face_crop(face: &eidetic_db::PersonFace) -> Result<Vec<u8>, ServerError> {
    let img = eidetic_ml::image_io::load_oriented_image(&face.storage_path)
        .map_err(|e| ServerError::CropFailed(format!("{}: {e}", face.storage_path.display())))?;

    let landmarks = landmarks_from_template_order(face.landmarks);
    let crop = eidetic_ml::face::align_face(&img, &landmarks)
        .unwrap_or_else(|| bbox_fallback_crop(&img, face.bbox));

    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut std::io::Cursor::new(&mut bytes), 85)
        .encode_image(&crop)
        .map_err(|e| ServerError::CropFailed(format!("jpeg encode: {e}")))?;
    Ok(bytes)
}

/// Crop the detector's bbox (clamped to image bounds) and resize to 112x112.
/// Only used when the landmarks can't drive a similarity transform.
fn bbox_fallback_crop(
    img: &image::DynamicImage,
    (x, y, w, h): (f32, f32, f32, f32),
) -> image::RgbImage {
    let (iw, ih) = (img.width(), img.height());
    let x0 = (x.max(0.0) as u32).min(iw.saturating_sub(1));
    let y0 = (y.max(0.0) as u32).min(ih.saturating_sub(1));
    let cw = (w.max(1.0) as u32).min(iw - x0).max(1);
    let ch = (h.max(1.0) as u32).min(ih - y0).max(1);
    let cropped = img.crop_imm(x0, y0, cw, ch).to_rgb8();
    image::imageops::resize(
        &cropped,
        eidetic_ml::face::ALIGNED_SIZE,
        eidetic_ml::face::ALIGNED_SIZE,
        image::imageops::FilterType::Triangle,
    )
}

pub(crate) async fn thumb(
    State(state): State<AppState>,
    Path((size, hash)): Path<(String, String)>,
) -> Result<axum::response::Response, ServerError> {
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::response::Response;
    use tokio_util::io::ReaderStream;

    // Validate size letter.
    if size != "s" && size != "m" {
        return Err(ServerError::NotFound(format!("thumbnail size {size:?}")));
    }
    // Validate hash shape: 64 lowercase hex chars only.
    if hash.len() != 64
        || !hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(ServerError::NotFound(format!("hash {hash:?}")));
    }

    let path = state
        .library_dir
        .as_ref()
        .join(".thumbs")
        .join(&size)
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(format!("{hash}.jpg"));

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ServerError::NotFound(format!("thumbnail {size}/{hash}")));
        }
        Err(e) => return Err(ServerError::Io(e)),
    };

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(body)
        .expect("static headers should build"))
}

#[cfg(test)]
mod tests {
    use super::{bbox_fallback_crop, landmarks_from_template_order};
    use eidetic_ml::Landmarks;

    #[test]
    fn landmarks_reconstruction_inverts_template_order() {
        let original = Landmarks {
            right_eye: (1.0, 2.0),
            left_eye: (3.0, 4.0),
            nose: (5.0, 6.0),
            right_mouth: (7.0, 8.0),
            left_mouth: (9.0, 10.0),
        };
        let rebuilt = landmarks_from_template_order(original.as_template_order());
        assert_eq!(rebuilt, original);
    }

    /// The full write path: `as_template_order()` goes into the DB columns,
    /// `fetch_face` reads them back, and the reconstruction must land on the
    /// original anatomical fields. Guards against a silent eye/mouth swap,
    /// which a similarity transform cannot represent — crops would come out
    /// scale-collapsed with no error anywhere.
    #[tokio::test]
    async fn landmarks_round_trip_through_the_database() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let config = eidetic_core::Config {
            database_path: tmp.path().join("eidetic.db"),
            ..Default::default()
        };
        let pool = eidetic_db::connect(&config).await.expect("connect");
        let assets = eidetic_db::AssetsRepo::new(pool.clone());
        let faces = eidetic_db::FacesRepo::new(pool);

        let asset_id = match assets
            .insert_asset(eidetic_db::NewAsset {
                hash: "ab".repeat(32),
                original_filename: "img.jpg".into(),
                storage_path: std::path::PathBuf::from("/lib/img.jpg"),
                file_size: 4096,
                mime_type: Some("image/jpeg".into()),
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
            .expect("insert")
        {
            eidetic_db::InsertOutcome::Inserted(id) | eidetic_db::InsertOutcome::Existing(id) => id,
        };

        // Asymmetric values so any slot swap is caught.
        let original = Landmarks {
            right_eye: (101.0, 51.0),
            left_eye: (149.5, 52.5),
            nose: (125.0, 80.25),
            right_mouth: (106.75, 110.0),
            left_mouth: (144.0, 111.5),
        };

        let ids = faces
            .record_detection(
                asset_id,
                &[eidetic_db::NewFace {
                    asset_id,
                    bbox: (90.0, 30.0, 80.0, 100.0),
                    // Exactly what the CLI face-scan write site stores.
                    landmarks: original.as_template_order(),
                    score: 0.9,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("record");

        let stored = faces
            .fetch_face(ids[0])
            .await
            .expect("fetch")
            .expect("exists");
        let rebuilt = landmarks_from_template_order(stored.landmarks);
        assert_eq!(
            rebuilt, original,
            "Landmarks -> DB -> Landmarks must be the identity"
        );
    }

    #[test]
    fn bbox_fallback_clamps_out_of_bounds_boxes() {
        let img = image::DynamicImage::new_rgb8(100, 80);
        // Box hangs off every edge; must still produce a 112x112 crop.
        let crop = bbox_fallback_crop(&img, (-20.0, -10.0, 500.0, 400.0));
        assert_eq!(crop.width(), 112);
        assert_eq!(crop.height(), 112);

        // Degenerate box collapses to at least one pixel.
        let crop = bbox_fallback_crop(&img, (99.5, 79.5, 0.0, 0.0));
        assert_eq!(crop.width(), 112);
        assert_eq!(crop.height(), 112);
    }
}
