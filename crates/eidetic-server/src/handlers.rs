use crate::{AppState, ServerError};
use axum::extract::{Path, Query, State};
use axum::response::Html;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[allow(dead_code)]
pub(crate) struct SearchQuery {
    pub q: Option<String>,
    pub limit: Option<u32>,
}

pub(crate) async fn index(State(_state): State<AppState>) -> Result<Html<String>, ServerError> {
    Ok(Html(
        "<!doctype html><p>eidetic-server: index (TODO)</p>".to_string(),
    ))
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
    State(_state): State<AppState>,
    Path(_id): Path<Uuid>,
) -> Result<Html<String>, ServerError> {
    Ok(Html(
        "<!doctype html><p>eidetic-server: asset detail (TODO)</p>".to_string(),
    ))
}

pub(crate) async fn asset_raw(
    State(_state): State<AppState>,
    Path(_id): Path<Uuid>,
) -> Result<axum::response::Response, ServerError> {
    use axum::response::IntoResponse;
    Ok((axum::http::StatusCode::NOT_IMPLEMENTED, "TODO").into_response())
}

pub(crate) async fn thumb(
    State(_state): State<AppState>,
    Path((_size, _hash)): Path<(String, String)>,
) -> Result<axum::response::Response, ServerError> {
    use axum::response::IntoResponse;
    Ok((axum::http::StatusCode::NOT_IMPLEMENTED, "TODO").into_response())
}
