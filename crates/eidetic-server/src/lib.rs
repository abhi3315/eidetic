//! Axum HTTP server for Eidetic.
//!
//! Exposes the library through a browser: search, thumbnails, raw downloads.
//! Localhost-bound, no authentication. See
//! `docs/superpowers/specs/2026-05-19-http-server-design.md` for scope.

use axum::Router;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use eidetic_db::PgAssetsRepo;
use eidetic_ml::SiglipEmbedder;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tower_http::trace::TraceLayer;
use tracing::info;

mod handlers;
mod views;

/// Dependencies the server needs to be wired up.
pub struct ServerDeps {
    pub repo: PgAssetsRepo,
    pub library_dir: PathBuf,
    pub models_cache: PathBuf,
}

/// Job sent from a handler to the embedder worker.
pub(crate) type EmbedJob = (String, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) repo: Arc<PgAssetsRepo>,
    pub(crate) library_dir: Arc<PathBuf>,
    #[allow(dead_code)]
    pub(crate) embed_tx: mpsc::Sender<EmbedJob>,
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    #[allow(dead_code)]
    BadRequest(String),
    #[error("embed failed: {0}")]
    #[allow(dead_code)]
    EmbedFailed(#[source] eidetic_ml::Error),
    #[error("database error: {0}")]
    DbFailed(#[source] eidetic_db::Error),
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, body) = match &self {
            ServerError::NotFound(what) => (StatusCode::NOT_FOUND, format!("Not found: {what}")),
            ServerError::BadRequest(why) => {
                (StatusCode::BAD_REQUEST, format!("Bad request: {why}"))
            }
            ServerError::EmbedFailed(_) => {
                tracing::error!(error = %self, "search embed failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Search temporarily unavailable.".to_string(),
                )
            }
            ServerError::DbFailed(_) => {
                tracing::error!(error = %self, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database error.".to_string(),
                )
            }
            ServerError::Io(_) => {
                tracing::error!(error = %self, "io error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error.".to_string(),
                )
            }
        };
        (status, Html(format!("<!doctype html><pre>{body}</pre>"))).into_response()
    }
}

/// Start the HTTP server. Returns on shutdown.
pub async fn serve(addr: SocketAddr, deps: ServerDeps) -> anyhow::Result<()> {
    use anyhow::Context;

    info!("loading SigLIP 2 model…");
    let models_dir = deps.models_cache.clone();
    let embedder = tokio::task::spawn_blocking(move || SiglipEmbedder::load(&models_dir))
        .await
        .context("embedder thread panicked")?
        .context("failed to load SigLIP 2 model")?;

    let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedJob>(8);
    tokio::task::spawn_blocking(move || {
        let mut embedder = embedder;
        while let Some((text, reply)) = embed_rx.blocking_recv() {
            let _ = reply.send(embedder.embed_text(&text));
        }
    });

    let state = AppState {
        repo: Arc::new(deps.repo),
        library_dir: Arc::new(deps.library_dir),
        embed_tx,
    };

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!("listening on http://{}", listener.local_addr()?);
    println!("Listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

pub(crate) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::index))
        .route("/search", get(handlers::search))
        .route("/assets/{id}", get(handlers::asset_detail))
        .route("/assets/{id}/raw", get(handlers::asset_raw))
        .route("/thumbs/{size}/{hash}", get(handlers::thumb))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
