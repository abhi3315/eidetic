use crate::{AppState, ServerError};
use axum::extract::{Path, Query, State};
use axum::response::Html;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    #[allow(dead_code)]
    pub q: Option<String>,
    #[allow(dead_code)]
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
    State(_state): State<AppState>,
    Query(_q): Query<SearchQuery>,
) -> Result<Html<String>, ServerError> {
    Ok(Html(
        "<!doctype html><p>eidetic-server: search (TODO)</p>".to_string(),
    ))
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
