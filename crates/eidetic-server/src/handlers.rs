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

    let title = detail.original_filename;
    let body = html! { (detail_page(&view)) };
    Ok(Html(layout(&title, body).into_string()))
}

pub(crate) async fn asset_raw(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ServerError> {
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::response::Response;
    use eidetic_core::AssetId;
    use tokio_util::io::ReaderStream;

    let asset_id = AssetId::from(id);
    let detail = state
        .repo
        .fetch_by_id(asset_id)
        .await
        .map_err(ServerError::DbFailed)?
        .ok_or_else(|| ServerError::NotFound(format!("asset {id}")))?;

    let file = tokio::fs::File::open(&detail.storage_path)
        .await
        .map_err(ServerError::Io)?;

    let mime = detail
        .mime_type
        .as_deref()
        .unwrap_or("application/octet-stream");

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    // Backslash-escape any quotes in the filename for the legacy
    // Content-Disposition form. Personal use; the cleaner RFC 6266
    // filename* with UTF-8 encoding can come later.
    let safe_filename = detail.original_filename.replace('"', "\\\"");
    let disposition = format!("inline; filename=\"{safe_filename}\"");

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .expect("disposition is ASCII-safe by construction"))
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
