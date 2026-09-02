//! Localhost-bound, no authentication. See
//! `docs/superpowers/specs/2026-05-19-http-server-design.md` for scope.

use axum::Router;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use eidetic_db::{AssetsRepo, FacesRepo};
use eidetic_ml::SiglipEmbedder;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tower_http::trace::TraceLayer;
use tracing::info;

mod dav;
mod handlers;
mod views;

pub struct ServerDeps {
    pub repo: AssetsRepo,
    pub faces: FacesRepo,
    pub library_dir: PathBuf,
    pub models_cache: PathBuf,
}

pub(crate) type EmbedJob = (String, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) repo: Arc<AssetsRepo>,
    pub(crate) faces: Arc<FacesRepo>,
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
    #[error("face crop failed: {0}")]
    CropFailed(String),
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
            ServerError::CropFailed(_) => {
                tracing::error!(error = %self, "face crop failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to render face crop.".to_string(),
                )
            }
        };
        (status, Html(format!("<!doctype html><pre>{body}</pre>"))).into_response()
    }
}

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
        faces: Arc::new(deps.faces),
        library_dir: Arc::new(deps.library_dir),
        embed_tx,
    };

    let mut app = build_router(state);
    // DNS-rebinding guard for the default loopback listener: a page on
    // attacker.example can rebind its hostname to 127.0.0.1 and read this
    // server with the victim's own browser, and same-origin policy will not
    // stop it — the Host header is the one thing the attacker cannot forge.
    // Only for loopback binds: an exposed bind (--expose) is reachable
    // directly, so a Host check there adds friction without protection.
    if addr.ip().is_loopback() {
        app = app.layer(axum::middleware::from_fn(require_local_host));
    }

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!("listening on http://{}", listener.local_addr()?);
    println!("Listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

/// Reject requests whose Host header does not name this machine's loopback.
///
/// Values a legitimate local client can send: `localhost`, `127.0.0.1`, or
/// `[::1]`, each with or without a port. Anything else — an external
/// hostname, a LAN IP, a missing header — is a request the browser was
/// tricked into sending here and gets a 403.
pub(crate) async fn require_local_host(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    // Strip the port: "[::1]:8080" splits after "]", others at the last ':'.
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    if matches!(name, "localhost" | "127.0.0.1" | "::1") {
        next.run(req).await
    } else {
        (
            axum::http::StatusCode::FORBIDDEN,
            "Host header does not name localhost; refusing (DNS-rebinding guard)",
        )
            .into_response()
    }
}

pub(crate) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::index))
        .route("/search", get(handlers::search))
        .route("/assets/{id}", get(handlers::asset_detail))
        .route("/assets/{id}/raw", get(handlers::asset_raw))
        .route("/assets/{id}/play", get(handlers::asset_play))
        .route("/thumbs/{size}/{hash}", get(handlers::thumb))
        .route("/persons", get(handlers::persons_index))
        .route("/persons/{id}", get(handlers::person_detail))
        .route("/faces/{id}/crop", get(handlers::face_crop))
        .route("/dav", axum::routing::any(dav::dav))
        .route("/dav/", axum::routing::any(dav::dav))
        .route("/dav/{*path}", axum::routing::any(dav::dav))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub type TestRouter = Router;

/// Build a router wired to `repo`, `faces` and `library_dir`, with a dead-end
/// embedder channel. For tests only; searches with a non-empty `q` will fail
/// because no embedder worker reads the channel. The empty-redirect path
/// still works.
pub fn test_router(repo: AssetsRepo, faces: FacesRepo, library_dir: PathBuf) -> Router {
    let (embed_tx, _embed_rx) = mpsc::channel::<EmbedJob>(1);
    // _embed_rx drops at end of scope; embed_tx.send() in tests will fail.
    // That's fine: handler tests for /search use the empty-q redirect path.
    let state = AppState {
        repo: Arc::new(repo),
        faces: Arc::new(faces),
        library_dir: Arc::new(library_dir),
        embed_tx,
    };
    build_router(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn guarded_router() -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(require_local_host))
    }

    #[tokio::test]
    async fn local_host_names_pass_the_rebinding_guard() {
        for host in [
            "localhost",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]",
            "[::1]:8080",
        ] {
            let res = guarded_router()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("Host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "Host: {host}");
        }
    }

    #[tokio::test]
    async fn foreign_hosts_are_refused() {
        // The DNS-rebinding shape: the victim's browser resolves an attacker
        // domain to 127.0.0.1 and sends the request here — with the
        // attacker's hostname in Host, the one part it cannot forge.
        for host in [
            "attacker.example",
            "attacker.example:8080",
            "192.168.1.5:8080",
            "localhost.attacker.example",
            "",
        ] {
            let res = guarded_router()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("Host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "Host: {host:?}");
        }
    }
}
