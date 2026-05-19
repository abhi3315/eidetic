# HTTP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A minimal Axum server (`eidetic-server` library + `eidetic serve` subcommand) exposing the existing library through a browser: landing page + recent grid + semantic search + asset detail + raw download + thumbnail serving. Localhost-bound, no auth.

**Architecture:** New library crate `eidetic-server`. `serve(addr, deps)` builds an Axum `Router` over `AppState { repo, library_dir, embed_tx }`, spawns a long-lived `spawn_blocking` worker that owns a `SiglipEmbedder` and answers text-embed jobs sent via `tokio::sync::mpsc`. Five routes: `/`, `/search`, `/assets/:id`, `/assets/:id/raw`, `/thumbs/:size/:hash`. HTML via `maud`. Two new `PgAssetsRepo` methods: `fetch_recent` and `fetch_by_id`.

**Tech Stack:** Rust 2024 workspace, Axum 0.8, tower-http (trace), maud 0.27, ONNX Runtime via `ort` (already in workspace), `sqlx`, `tokio`. **Four new workspace deps:** `axum`, `tower`, `tower-http`, `maud`.

**Spec reference:** `docs/superpowers/specs/2026-05-19-http-server-design.md`.

---

## File map

**Create:**
- `crates/eidetic-server/Cargo.toml`
- `crates/eidetic-server/src/lib.rs` — `ServerDeps`, `AppState`, `ServerError`, `serve(addr, deps)`, `build_router(state)`.
- `crates/eidetic-server/src/handlers.rs` — `index`, `search`, `asset_detail`, `asset_raw`, `thumb`.
- `crates/eidetic-server/src/views.rs` — `layout`, `asset_grid`, `detail_page`, `INLINE_CSS`.
- `crates/eidetic-server/tests/handlers.rs` — handler-level tests via `tower::ServiceExt::oneshot`.
- `crates/eidetic-db/tests/server_integration.rs` — end-to-end testcontainer tests of the server hitting a real DB.

**Modify:**
- `Cargo.toml` (workspace root) — add `axum`, `tower`, `tower-http`, `maud` to `[workspace.dependencies]`.
- `crates/eidetic-db/src/assets.rs` — add `RecentAsset`, `AssetDetail`, `fetch_recent`, `fetch_by_id`. Re-export from `lib.rs`.
- `crates/eidetic-db/src/lib.rs` — add `pub use assets::{AssetDetail, RecentAsset};` to existing re-export line.
- `crates/eidetic-db/tests/assets.rs` — two new testcontainer tests for the new repo methods.
- `crates/eidetic-cli/Cargo.toml` — add `eidetic-server = { path = "../eidetic-server" }`.
- `crates/eidetic-cli/src/main.rs` — `Command::Serve { bind: Option<String> }` + arm body.
- `AGENTS.md` — add `eidetic-server` row to the workspace map.

---

## Task 1: Workspace deps + scaffold `eidetic-server`

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/eidetic-server/Cargo.toml`
- Create: `crates/eidetic-server/src/lib.rs`
- Create: `crates/eidetic-server/src/handlers.rs`
- Create: `crates/eidetic-server/src/views.rs`

After this task, the new crate compiles but has no caller. Workspace stays green. The CLI integration is Task 7.

- [ ] **Step 1: Create branch (only if not already created from the spec commit)**

```bash
git checkout main
git pull --ff-only
git checkout -b feat/http-server  # skip if already on this branch
```

- [ ] **Step 2: Add four new workspace deps to root `Cargo.toml`**

Find the `[workspace.dependencies]` block. Add these entries (alphabetical position):

```toml
axum = "0.8"
maud = "0.27"
tower = "0.5"
tower-http = { version = "0.6", features = ["trace"] }
```

- [ ] **Step 3: Update workspace `members`**

In the same `Cargo.toml`, find `[workspace] members = [...]` and add `"crates/eidetic-server"` (alphabetical position, after `eidetic-ml`):

```toml
[workspace]
members = [
    "crates/eidetic-cli",
    "crates/eidetic-core",
    "crates/eidetic-db",
    "crates/eidetic-ingest",
    "crates/eidetic-ml",
    "crates/eidetic-server",
]
```

Verify the existing members order by reading the file first — keep alphabetical if that's the existing convention.

- [ ] **Step 4: Create `crates/eidetic-server/Cargo.toml`**

```toml
[package]
name = "eidetic-server"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
description = "Axum HTTP server for Eidetic."

[dependencies]
eidetic-core = { path = "../eidetic-core" }
eidetic-db = { path = "../eidetic-db" }
eidetic-ml = { path = "../eidetic-ml" }
anyhow = { workspace = true }
axum = { workspace = true }
chrono = { workspace = true }
maud = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tower = { workspace = true }
tower-http = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
http-body-util = "0.1"
tower = { workspace = true }
```

`http-body-util` is needed by handler tests (Task 8) to read response bodies. The `tower` dev-dep duplication is intentional: `oneshot` is in `tower::ServiceExt` and dev tests need it.

- [ ] **Step 5: Create `crates/eidetic-server/src/lib.rs`**

```rust
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
    pub(crate) embed_tx: mpsc::Sender<EmbedJob>,
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("embed failed: {0}")]
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
            ServerError::NotFound(what) => {
                (StatusCode::NOT_FOUND, format!("Not found: {what}"))
            }
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
```

- [ ] **Step 6: Create `crates/eidetic-server/src/handlers.rs`**

Initial stub. Real handlers land in Tasks 4–6.

```rust
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

pub(crate) async fn index(State(_state): State<AppState>) -> Result<Html<String>, ServerError> {
    Ok(Html("<!doctype html><p>eidetic-server: index (TODO)</p>".to_string()))
}

pub(crate) async fn search(
    State(_state): State<AppState>,
    Query(_q): Query<SearchQuery>,
) -> Result<Html<String>, ServerError> {
    Ok(Html("<!doctype html><p>eidetic-server: search (TODO)</p>".to_string()))
}

pub(crate) async fn asset_detail(
    State(_state): State<AppState>,
    Path(_id): Path<Uuid>,
) -> Result<Html<String>, ServerError> {
    Ok(Html("<!doctype html><p>eidetic-server: asset detail (TODO)</p>".to_string()))
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
```

This stub structure is intentional: subsequent tasks replace bodies but keep signatures stable, so handler-level tests don't churn.

- [ ] **Step 7: Create `crates/eidetic-server/src/views.rs`**

Empty module for now. Task 3 fills it in.

```rust
//! Maud HTML helpers for the server. Filled in by Task 3.
#![allow(dead_code)]
```

- [ ] **Step 8: Build the workspace**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. The new crate compiles. No tests fail.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock \
        crates/eidetic-server/
git commit -m "feat(server): scaffold eidetic-server crate with stub handlers

New library crate. Builds the Axum router shape (5 routes, all
returning placeholder HTML), AppState/ServerDeps/ServerError types,
and the embedder-worker bootstrap in serve(). All handlers return
'TODO' placeholders; subsequent tasks wire them to real data."
```

Pre-commit hook runs fmt + clippy. Do not use `--no-verify`.

---

## Task 2: `fetch_recent` + `fetch_by_id` on `PgAssetsRepo`

**Files:**
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-db/src/lib.rs`
- Modify: `crates/eidetic-db/tests/assets.rs`

- [ ] **Step 1: Add two new structs + two new methods**

In `crates/eidetic-db/src/assets.rs`, add these public types at the top level (after the existing `SearchResult` struct):

```rust
/// One row from `fetch_recent`. Just what the landing-page grid needs.
pub struct RecentAsset {
    pub id: AssetId,
    pub hash: eidetic_core::Sha256,
    pub original_filename: String,
    pub imported_at: chrono::DateTime<chrono::Utc>,
}

/// Full asset row for the detail page.
pub struct AssetDetail {
    pub id: AssetId,
    pub hash: eidetic_core::Sha256,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: i64,
    pub mime_type: Option<String>,
    pub imported_at: chrono::DateTime<chrono::Utc>,
    pub date_taken: Option<chrono::DateTime<chrono::Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub thumbnails_generated: bool,
}
```

Then, inside the existing `impl PgAssetsRepo { ... }`, add (placement: after `mark_thumbnailed`):

```rust
    /// Most recently imported image assets, descending. Caps at `limit`.
    pub async fn fetch_recent(&self, limit: u32) -> crate::Result<Vec<RecentAsset>> {
        let rows: Vec<(uuid::Uuid, String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT id, hash, original_filename, imported_at \
             FROM assets \
             WHERE mime_type LIKE 'image/%' \
             ORDER BY imported_at DESC \
             LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(uuid, hash_hex, original_filename, imported_at)| {
                let hash = eidetic_core::Sha256::from_hex(&hash_hex)
                    .expect("hash column is CHAR(64) of lowercase hex");
                RecentAsset {
                    id: AssetId::from(uuid),
                    hash,
                    original_filename,
                    imported_at,
                }
            })
            .collect())
    }

    /// Single asset by id, or `None` if no row.
    pub async fn fetch_by_id(&self, id: AssetId) -> crate::Result<Option<AssetDetail>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: uuid::Uuid,
            hash: String,
            original_filename: String,
            storage_path: String,
            file_size: i64,
            mime_type: Option<String>,
            imported_at: chrono::DateTime<chrono::Utc>,
            date_taken: Option<chrono::DateTime<chrono::Utc>>,
            latitude: Option<f64>,
            longitude: Option<f64>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            thumbnails_generated: bool,
        }

        let row: Option<Row> = sqlx::query_as(
            "SELECT id, hash, original_filename, storage_path, file_size, mime_type, \
                    imported_at, date_taken, latitude, longitude, camera_make, camera_model, \
                    thumbnails_generated \
             FROM assets WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(row.map(|r| AssetDetail {
            id: AssetId::from(r.id),
            hash: eidetic_core::Sha256::from_hex(&r.hash)
                .expect("hash column is CHAR(64) of lowercase hex"),
            original_filename: r.original_filename,
            storage_path: PathBuf::from(r.storage_path),
            file_size: r.file_size,
            mime_type: r.mime_type,
            imported_at: r.imported_at,
            date_taken: r.date_taken,
            latitude: r.latitude,
            longitude: r.longitude,
            camera_make: r.camera_make,
            camera_model: r.camera_model,
            thumbnails_generated: r.thumbnails_generated,
        }))
    }
```

- [ ] **Step 2: Re-export the new structs from `crates/eidetic-db/src/lib.rs`**

Current line 20:

```rust
pub use assets::{InsertOutcome, LibraryStats, NewAsset, PgAssetsRepo, SearchResult};
```

Change to:

```rust
pub use assets::{
    AssetDetail, InsertOutcome, LibraryStats, NewAsset, PgAssetsRepo, RecentAsset, SearchResult,
};
```

- [ ] **Step 3: Add two integration tests to `crates/eidetic-db/tests/assets.rs`**

Append at the end of the file. They follow the existing `start_db` helper pattern (already present at the top of the file):

```rust
#[tokio::test]
async fn fetch_recent_orders_by_imported_at_desc() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    // Insert 3 image assets. Postgres' DEFAULT NOW() will give them
    // ascending imported_at, so the most-recent comes last by insertion
    // order. fetch_recent should reverse that.
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
            thumbnails_generated: false,
        })
        .await
        .expect("insert");
        // Tiny sleep so imported_at differs row-to-row.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let recent = repo.fetch_recent(10).await.expect("fetch_recent");
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].original_filename, "img2.jpg");
    assert_eq!(recent[1].original_filename, "img1.jpg");
    assert_eq!(recent[2].original_filename, "img0.jpg");
}

#[tokio::test]
async fn fetch_by_id_returns_full_row_or_none() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

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
        thumbnails_generated: true,
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
```

- [ ] **Step 4: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green. (Tests require Docker; CI runs them.)

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-db/src/assets.rs \
        crates/eidetic-db/src/lib.rs \
        crates/eidetic-db/tests/assets.rs
git commit -m "feat(db): fetch_recent + fetch_by_id

Two new PgAssetsRepo methods feeding the HTTP server. fetch_recent
returns ORDER BY imported_at DESC LIMIT N for image assets;
fetch_by_id is a single-row lookup returning the full AssetDetail
for the detail page. Two testcontainer-backed tests cover ordering
and the present-vs-missing cases."
```

---

## Task 3: maud views (`layout`, `asset_grid`, `detail_page`)

**Files:**
- Modify: `crates/eidetic-server/src/views.rs`

- [ ] **Step 1: Replace `views.rs` with the real implementations**

Replace the file contents with:

```rust
//! Maud HTML helpers for the server.

use chrono::{DateTime, Utc};
use eidetic_core::{AssetId, Sha256};
use maud::{DOCTYPE, Markup, html};

/// One tile in a thumbnail grid. Used by `/` and `/search`.
pub(crate) struct GridTile {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) alt: String,
    /// Optional cosine-similarity score [0, 1]. Some(score) means render a badge.
    pub(crate) score: Option<f32>,
}

/// Detail-page input. Mirrors `eidetic_db::AssetDetail`'s fields the page
/// actually displays.
pub(crate) struct DetailView {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) original_filename: String,
    pub(crate) mime_type: Option<String>,
    pub(crate) file_size: i64,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) date_taken: Option<DateTime<Utc>>,
    pub(crate) latitude: Option<f64>,
    pub(crate) longitude: Option<f64>,
    pub(crate) camera_make: Option<String>,
    pub(crate) camera_model: Option<String>,
    pub(crate) thumbnails_generated: bool,
}

const INLINE_CSS: &str = r#"
* { box-sizing: border-box; }
body { font-family: -apple-system, system-ui, sans-serif; margin: 0; background: #fafafa; color: #111; }
header { padding: 1rem 1.5rem; background: white; border-bottom: 1px solid #e5e5e5; }
header a { color: #111; text-decoration: none; font-weight: 600; }
main { padding: 1.5rem; max-width: 1400px; margin: 0 auto; }
form.search { display: flex; gap: 0.5rem; margin-bottom: 1.5rem; }
form.search input[type=text] { flex: 1; padding: 0.6rem 0.8rem; border: 1px solid #ccc; border-radius: 4px; font-size: 1rem; }
form.search button { padding: 0.6rem 1rem; background: #111; color: white; border: 0; border-radius: 4px; cursor: pointer; }
h2 { margin: 1.5rem 0 1rem; font-weight: 600; font-size: 1.1rem; color: #555; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 0.6rem; }
.tile { position: relative; aspect-ratio: 1 / 1; overflow: hidden; border-radius: 4px; background: #eee; }
.tile img { width: 100%; height: 100%; object-fit: cover; display: block; }
.tile .score { position: absolute; bottom: 0.3rem; right: 0.3rem; background: rgba(0,0,0,0.7); color: white; padding: 0.1rem 0.4rem; border-radius: 3px; font-size: 0.75rem; }
.detail { display: grid; grid-template-columns: 2fr 1fr; gap: 2rem; }
.detail img.preview { width: 100%; border-radius: 4px; background: #eee; }
.detail dl { margin: 0; }
.detail dt { font-size: 0.8rem; color: #777; margin-top: 0.8rem; }
.detail dd { margin: 0.2rem 0 0 0; }
.detail a.download { display: inline-block; margin-top: 1rem; padding: 0.6rem 1rem; background: #111; color: white; text-decoration: none; border-radius: 4px; }
.empty { color: #777; font-style: italic; }
"#;

pub(crate) fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Eidetic" }
                style { (INLINE_CSS) }
            }
            body {
                header { a href="/" { "Eidetic" } }
                main { (body) }
            }
        }
    }
}

pub(crate) fn search_form(default_q: &str) -> Markup {
    html! {
        form class="search" action="/search" method="get" {
            input type="text" name="q" placeholder="Search photos…" value=(default_q);
            button type="submit" { "Search" }
        }
    }
}

pub(crate) fn asset_grid(tiles: &[GridTile]) -> Markup {
    if tiles.is_empty() {
        return html! { p class="empty" { "No results." } };
    }
    html! {
        div class="grid" {
            @for tile in tiles {
                a href=(format!("/assets/{}", tile.id)) class="tile" {
                    img src=(format!("/thumbs/m/{}", tile.hash)) loading="lazy" alt=(tile.alt);
                    @if let Some(score) = tile.score {
                        span class="score" { (format!("{:.0}%", score * 100.0)) }
                    }
                }
            }
        }
    }
}

pub(crate) fn detail_page(view: &DetailView) -> Markup {
    html! {
        div class="detail" {
            div {
                @if view.thumbnails_generated {
                    img class="preview"
                        src=(format!("/thumbs/m/{}", view.hash))
                        alt=(view.original_filename);
                } @else {
                    p class="empty" {
                        "Thumbnails pending — run `eidetic thumbnail` to generate."
                    }
                }
                a class="download" href=(format!("/assets/{}/raw", view.id)) {
                    "Download original"
                }
            }
            div {
                dl {
                    dt { "Filename" } dd { (view.original_filename) }
                    @if let Some(mime) = &view.mime_type {
                        dt { "Type" } dd { (mime) }
                    }
                    dt { "Size" } dd { (format_bytes(view.file_size)) }
                    dt { "Imported" } dd { (view.imported_at.format("%Y-%m-%d %H:%M").to_string()) }
                    @if let Some(taken) = view.date_taken {
                        dt { "Taken" } dd { (taken.format("%Y-%m-%d %H:%M").to_string()) }
                    }
                    @if let (Some(make), Some(model)) = (&view.camera_make, &view.camera_model) {
                        dt { "Camera" } dd { (make) " " (model) }
                    } @else if let Some(make) = &view.camera_make {
                        dt { "Camera" } dd { (make) }
                    }
                    @if let (Some(lat), Some(lon)) = (view.latitude, view.longitude) {
                        dt { "GPS" } dd { (format!("{lat:.4}, {lon:.4}")) }
                    }
                }
            }
        }
    }
}

fn format_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.2} GB", n / GB)
    } else if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_hash() -> Sha256 {
        Sha256::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
            .expect("valid hex")
    }

    #[test]
    fn layout_renders_title_and_html_skeleton() {
        let m = layout("Test", html! { p { "hello" } });
        let s = m.into_string();
        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains("<title>Test · Eidetic</title>"));
        assert!(s.contains("<p>hello</p>"));
    }

    #[test]
    fn asset_grid_empty_renders_empty_message() {
        let s = asset_grid(&[]).into_string();
        assert!(s.contains("No results."));
        assert!(!s.contains("<div class=\"grid\""));
    }

    #[test]
    fn asset_grid_renders_one_tile_per_input() {
        let tiles = vec![
            GridTile {
                id: AssetId::new(),
                hash: sample_hash(),
                alt: "first.jpg".into(),
                score: None,
            },
            GridTile {
                id: AssetId::new(),
                hash: sample_hash(),
                alt: "second.jpg".into(),
                score: Some(0.87),
            },
        ];
        let s = asset_grid(&tiles).into_string();
        let tile_count = s.matches("class=\"tile\"").count();
        assert_eq!(tile_count, 2);
        assert!(s.contains("87%"));
    }

    #[test]
    fn detail_page_with_thumbnails_renders_preview() {
        let view = DetailView {
            id: AssetId::new(),
            hash: sample_hash(),
            original_filename: "foo.jpg".to_string(),
            mime_type: Some("image/jpeg".to_string()),
            file_size: 2048,
            imported_at: Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            thumbnails_generated: true,
        };
        let s = detail_page(&view).into_string();
        assert!(s.contains("class=\"preview\""));
        assert!(s.contains("Download original"));
        assert!(s.contains("2 KB"));
        assert!(!s.contains("Thumbnails pending"));
    }

    #[test]
    fn detail_page_without_thumbnails_shows_pending_notice() {
        let view = DetailView {
            id: AssetId::new(),
            hash: sample_hash(),
            original_filename: "foo.jpg".to_string(),
            mime_type: None,
            file_size: 0,
            imported_at: Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            thumbnails_generated: false,
        };
        let s = detail_page(&view).into_string();
        assert!(s.contains("Thumbnails pending"));
        assert!(!s.contains("class=\"preview\""));
    }
}
```

- [ ] **Step 2: Run the unit tests**

```bash
cargo test -p eidetic-server views
```

Expected: 5 tests pass.

- [ ] **Step 3: Workspace verify**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-server/src/views.rs
git commit -m "feat(server): maud HTML views

layout, search_form, asset_grid, detail_page + INLINE_CSS. GridTile
and DetailView are server-internal structs that handlers populate
from DB rows. Five unit tests cover the rendering paths."
```

---

## Task 4: `GET /` and `GET /thumbs/<size>/<hash>` handlers

**Files:**
- Modify: `crates/eidetic-server/src/handlers.rs`

- [ ] **Step 1: Replace the `index` handler body**

```rust
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
```

- [ ] **Step 2: Replace the `thumb` handler body**

```rust
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
    // Validate hash shape (64 lowercase hex chars).
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit() && (!c.is_ascii_uppercase())) {
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
        .header(
            header::CACHE_CONTROL,
            "public, max-age=31536000, immutable",
        )
        .body(body)
        .expect("static headers should build"))
}
```

Add `tokio-util` to `crates/eidetic-server/Cargo.toml`'s `[dependencies]` (with `io` feature for `ReaderStream`):

```toml
tokio-util = { workspace = true }
```

Then in the workspace root `Cargo.toml`, add `tokio-util` to `[workspace.dependencies]` if not already there:

```toml
tokio-util = { version = "0.7", features = ["io"] }
```

Check the workspace deps list first — `tokio-util` may already be there as a transitive dep of sqlx.

- [ ] **Step 3: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p eidetic-server
```

Green.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-server/src/handlers.rs \
        crates/eidetic-server/Cargo.toml \
        Cargo.toml Cargo.lock
git commit -m "feat(server): GET / and GET /thumbs/<size>/<hash>

Index handler renders the search form + recent-imports grid (up to
24 most recent images). Thumb handler validates size+hash, derives
the CAS-shaped path under <library>/.thumbs/, and streams the file
with immutable cache headers. 404 on bad size, bad hash, or missing
file."
```

---

## Task 5: `GET /assets/<id>` and `GET /assets/<id>/raw`

**Files:**
- Modify: `crates/eidetic-server/src/handlers.rs`

- [ ] **Step 1: Replace `asset_detail` body**

```rust
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
```

- [ ] **Step 2: Replace `asset_raw` body**

```rust
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

    // The original filename may contain non-ASCII or quote characters.
    // RFC 6266 says: use ASCII-safe filename* with UTF-8 encoding when
    // a clean ASCII fallback isn't possible. For personal use we keep
    // it simple: backslash-escape any quotes in the filename and emit
    // the legacy single-filename form.
    let safe_filename = detail.original_filename.replace('"', "\\\"");
    let disposition = format!("inline; filename=\"{safe_filename}\"");

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .expect("disposition is ASCII-safe by construction"))
}
```

- [ ] **Step 3: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-server/src/handlers.rs
git commit -m "feat(server): GET /assets/<id> and /assets/<id>/raw

Detail page renders thumbnail (or pending notice), full EXIF, and
the download link. Raw endpoint streams the CAS-stored original
with the row's mime_type and the original filename in
Content-Disposition. Both 404 on unknown UUID."
```

---

## Task 6: Embedder worker + `GET /search`

The embedder worker is already in `serve()` from Task 1. Task 6 just consumes it from the `/search` handler.

**Files:**
- Modify: `crates/eidetic-server/src/handlers.rs`

- [ ] **Step 1: Replace `search` body**

```rust
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

    // For the alt text we don't have the filename in SearchResult; use a
    // generic placeholder. The detail page (linked via the tile) shows the
    // real filename.
    let tiles: Vec<GridTile> = results
        .into_iter()
        .map(|r| GridTile {
            id: r.id,
            hash: eidetic_core::Sha256::from_hex(&hash_from_path(&r.storage_path))
                .unwrap_or_else(|| eidetic_core::Sha256::from_bytes([0u8; 32])),
            alt: query_text.clone(),
            score: Some(r.score),
        })
        .collect();

    let body = html! {
        (search_form(&query_text))
        h2 { "Results for " (query_text) " (" (tiles.len()) ")" }
        (asset_grid(&tiles))
    };

    Ok((StatusCode::OK, Html(layout(&query_text, body).into_string())).into_response())
}

/// Reverse-engineer the hash from a CAS storage path of the form
/// `<lib>/<ab>/<cd>/<full-hex>.<ext>`. The last path segment minus the
/// extension is the hash hex. Falls back to a zero hash on weird inputs.
fn hash_from_path(p: &std::path::Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}
```

**Why reverse-engineer the hash:** `SearchResult` carries `storage_path: PathBuf` but not `hash`. The thumbnail URL needs the hash. Two options:
1. Add `hash` to `SearchResult` — a 3-file change (struct, SQL, all call sites).
2. Derive hash from the CAS path's filename.

For v1, option 2 is one-liner; option 1 is the right long-term move but out of scope for this PR. The `unwrap_or_else` fallback ensures that even a weird filename can't panic the server.

If you decide to take option 1 instead, add `hash CHAR(64)` to the `SearchResult` struct, the search SQL, and the `SearchRow` deserialization. Then `hash_from_path` goes away and the tile builds `tile.hash = r.hash`. That's a cleaner change and would be a small follow-up PR worth doing. For *this* plan, keep option 2.

- [ ] **Step 2: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step 3: Commit**

```bash
git add crates/eidetic-server/src/handlers.rs
git commit -m "feat(server): GET /search via embedder worker

Search handler sends the query to the mpsc-owned embedder worker
(set up in serve() from task 1), awaits the embedding, runs
search_similar, renders a results grid with the cosine score
on each tile. Empty/whitespace-only q redirects to /. limit
capped at 60 (default 24)."
```

---

## Task 7: CLI `Serve` subcommand

**Files:**
- Modify: `crates/eidetic-cli/Cargo.toml`
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Add `eidetic-server` to `eidetic-cli/Cargo.toml`**

In `[dependencies]`, after `eidetic-ml`:

```toml
eidetic-server = { path = "../eidetic-server" }
```

- [ ] **Step 2: Add `Serve` variant to the `Command` enum**

In `crates/eidetic-cli/src/main.rs`, find the enum. Add:

```rust
    /// Start the HTTP server (localhost-bound).
    Serve {
        /// Address to bind. Defaults to 127.0.0.1:8080.
        /// Override via --bind or EIDETIC_BIND env var.
        #[arg(long)]
        bind: Option<String>,
    },
```

Place it adjacent to other side-effecting commands. Pick a position that matches the existing ordering style.

- [ ] **Step 3: Add the arm body**

In the match block, add:

```rust
        Command::Serve { bind } => {
            use std::net::SocketAddr;

            let config = Config::from_env();
            let addr_str = bind
                .or_else(|| std::env::var("EIDETIC_BIND").ok())
                .unwrap_or_else(|| "127.0.0.1:8080".to_string());
            let addr: SocketAddr = addr_str
                .parse()
                .with_context(|| format!("invalid bind address: {addr_str}"))?;

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::PgAssetsRepo::new(pool);

            let deps = eidetic_server::ServerDeps {
                repo,
                library_dir: config.paths.library_dir.clone(),
                models_cache: config.paths.models_cache.clone(),
            };

            println!("Loading model (downloads ~1.4 GiB on first run)…");
            eidetic_server::serve(addr, deps).await?;
        }
```

- [ ] **Step 4: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p eidetic-cli -- serve --help
```

The `--help` smoke confirms clap registered the subcommand. The full run requires a DB and model — don't actually start the server in CI.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-cli/Cargo.toml crates/eidetic-cli/src/main.rs Cargo.lock
git commit -m "feat(cli): eidetic serve subcommand

Wires Config + PgAssetsRepo + paths into eidetic_server::serve and
awaits. --bind flag or EIDETIC_BIND env var control the address;
default is 127.0.0.1:8080."
```

---

## Task 8: Server integration tests

**Files:**
- Create: `crates/eidetic-server/tests/handlers.rs` (Axum router-level tests)
- Create: `crates/eidetic-db/tests/server_integration.rs` (end-to-end tests through real DB)

- [ ] **Step 1: Handler-level test file**

Create `crates/eidetic-server/tests/handlers.rs`. These tests build the router with a tower-tested `Service` shape; they don't bind a real port.

```rust
//! Handler-level tests: build the Axum router, exercise routes via
//! `tower::ServiceExt::oneshot`, assert status codes and content snippets.
//!
//! These tests use a real testcontainer Postgres because PR #15 deleted
//! the AssetIndex trait; there is no in-memory mock to substitute.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use eidetic_core::{Config, Paths};
use eidetic_db::{NewAsset, PgAssetsRepo};
use http_body_util::BodyExt;
use std::path::PathBuf;
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

/// Build an AppState wired to a real repo + a no-op embedder channel.
async fn fixture() -> (
    eidetic_server::TestRouter,
    PgAssetsRepo,
    tempfile::TempDir,
    testcontainers::ContainerAsync<GenericImage>,
) {
    let (container, url) = start_db().await;
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_url: url,
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);
    let repo_clone = PgAssetsRepo::new(eidetic_db::connect(&config).await.expect("clone"));

    let router = eidetic_server::test_router(repo, config.paths.library_dir);
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
                .uri(&format!("/assets/{}", uuid::Uuid::new_v4()))
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
```

- [ ] **Step 2: Expose a test-only router builder from `eidetic-server`**

The integration tests need a router that doesn't load a real SigLIP model. Add a public function `test_router` and a type alias `TestRouter` in `crates/eidetic-server/src/lib.rs`, gated behind `#[cfg(any(test, feature = "test-support"))]` or just unconditionally `pub`:

```rust
/// Type returned by `test_router` so tests don't have to spell out the
/// generic Router parameters. Public for downstream test crates.
pub type TestRouter = Router;

/// Build a router with a dead-end embedder channel — searches will fail,
/// but every other route works. For tests only.
pub fn test_router(repo: PgAssetsRepo, library_dir: PathBuf) -> Router {
    let (embed_tx, _embed_rx) = mpsc::channel::<EmbedJob>(1);
    let state = AppState {
        repo: Arc::new(repo),
        library_dir: Arc::new(library_dir),
        embed_tx,
    };
    build_router(state)
}
```

The `_embed_rx` binding holds the receiver alive for the test's lifetime so the sender doesn't immediately disconnect. Drop the underscore and prefix to `embed_rx` and leak it — actually the underscore is fine because Rust drops it when `test_router` returns, BUT the channel will then be closed on the next `embed_tx.send().await`. For the search-empty-q test that's OK (it redirects before sending). For a search-non-empty test we'd need a real channel — that's the `#[ignore]`d integration test in Task 8 Step 3.

To keep `_embed_rx` alive across the test, return it from `test_router` too — but the existing test stubs would need to change shape. Alternative: have `test_router` return `(Router, EmbedRx)` and the test discards the rx into a `_` binding at top of function scope.

Simpler approach: don't expose the rx. The handler-level tests that don't invoke `/search` with a real query are fine. The `/search?q=foo` test belongs in `tests/server_integration.rs` where we can stand up a real model (and `#[ignore]` it).

Keep `test_router` returning just `Router`. Document that the embedder is a dead channel in test mode.

- [ ] **Step 3: End-to-end test file**

Create `crates/eidetic-db/tests/server_integration.rs`. This is the live-port test — actually `serve()` to a random port, hit it with `reqwest`-or-similar.

Actually, hitting a live port adds a `reqwest` dev-dep. Simpler: keep all the meaningful end-to-end coverage in `crates/eidetic-server/tests/handlers.rs` (router-level, no port binding). For "real flow with thumbnail file on disk," `handlers.rs` is enough.

Re-scope Task 8: **Skip the `server_integration.rs` file in `eidetic-db`.** The handler-level tests in `eidetic-server/tests/handlers.rs` are sufficient.

For the `thumbnail-serves-bytes` and `raw-streams-with-mime` paths, add them to `eidetic-server/tests/handlers.rs`:

```rust
#[tokio::test]
async fn thumb_serves_jpeg_bytes_when_file_exists() {
    let (router, _repo, tmp, _container) = fixture().await;

    // Write a fake "thumbnail" file at the expected CAS path.
    let library = tmp.path().join("library");
    let hash = "abcd000000000000000000000000000000000000000000000000000000000001";
    let dir = library.join(".thumbs").join("s").join(&hash[..2]).join(&hash[2..4]);
    std::fs::create_dir_all(&dir).unwrap();
    let thumb_path = dir.join(format!("{hash}.jpg"));
    // Real JPEG magic + tiny body.
    std::fs::write(&thumb_path, [0xFF, 0xD8, 0xFF, 0xE0, b'X']).unwrap();

    let response = router
        .oneshot(
            Request::builder()
                .uri(&format!("/thumbs/s/{hash}"))
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

    // Write an original "file" to a fake CAS path and insert its row.
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
                .uri(&format!("/assets/{}/raw", id.as_uuid()))
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
```

Total handler tests: 7 (5 from Step 1 + 2 from above).

- [ ] **Step 4: Verify**

```bash
cargo build -p eidetic-server --tests
cargo clippy --workspace --all-targets -- -D warnings
```

Green. Tests need Docker; CI runs them.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-server/src/lib.rs \
        crates/eidetic-server/tests/handlers.rs
git commit -m "test(server): handler-level tests via tower::ServiceExt

Seven tests cover index-empty, search-empty-redirect, asset-404,
thumbnail-bad-size/bad-hash 404s, thumbnail-serves-bytes, and
raw-streams-with-mime. test_router builder exposes a router with
a dead-end embedder channel so non-search routes can be tested
without loading SigLIP."
```

---

## Task 9: AGENTS.md update + final verify + PR

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update workspace map**

In `AGENTS.md`, find the workspace-map table. Add a row for `eidetic-server` (placement: between `eidetic-ml` and `eidetic-cli`):

```
| `eidetic-server` | Axum HTTP server. `serve()` owns the embedder worker and the route table (`/`, `/search`, `/assets/:id`, `/assets/:id/raw`, `/thumbs/:size/:hash`). Localhost-bound, no auth. | `eidetic-core`, `eidetic-db`, `eidetic-ml` |
```

Update the `eidetic-cli` row's `Depends on` column to include `eidetic-server`:

```
| `eidetic-cli` | Binary. Wires up dependencies and exposes subcommands. | `eidetic-core`, `eidetic-db`, `eidetic-ingest`, `eidetic-server` |
```

Also, in the "Crates added later" list below the table, remove the `eidetic-server` bullet (it's now a real crate).

- [ ] **Step 2: Final verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude eidetic-db --exclude eidetic-server
```

The exclusion of `eidetic-server` is because its tests need Docker. Local Docker is optional; CI runs everything.

If `cargo-deny` is installed:

```bash
cargo deny check
```

- [ ] **Step 3: Commit the AGENTS.md change**

```bash
git add AGENTS.md
git commit -m "docs(agents): add eidetic-server to workspace map"
```

- [ ] **Step 4: Push and open PR**

```bash
git push -u origin feat/http-server
gh pr create --title "feat: HTTP server (eidetic serve)" --body "$(cat <<'EOF'
## Summary

New \`eidetic-server\` library crate + \`eidetic serve\` subcommand. Localhost-bound Axum server with five routes:

- \`GET /\` — search form + grid of 24 most-recently-imported images.
- \`GET /search?q=…\` — semantic-search results grid with cosine-similarity badges.
- \`GET /assets/<id>\` — asset detail page (medium thumbnail, EXIF, download link).
- \`GET /assets/<id>/raw\` — streams the CAS-stored original with correct \`Content-Type\` + \`Content-Disposition\`.
- \`GET /thumbs/<size>/<hash>\` — serves a thumbnail file with \`Cache-Control: public, max-age=31536000, immutable\`.

\`SiglipEmbedder\` is owned by a long-lived \`spawn_blocking\` worker thread; \`/search\` posts \`(text, oneshot::Sender)\` jobs over an \`mpsc\` channel. Same shape as PR #16.

HTML via \`maud\` (compile-time Rust DSL, XSS-safe). Five view unit tests cover layout/grid/detail rendering; seven handler tests cover the routing/status-code/header path with a real testcontainer DB.

Plan: \`docs/superpowers/plans/2026-05-19-http-server.md\`. Spec: \`docs/superpowers/specs/2026-05-19-http-server-design.md\`.

## Behavioural notes

- Default bind: \`127.0.0.1:8080\`. Override via \`--bind\` or \`EIDETIC_BIND\` env.
- The server loads the SigLIP model on startup before binding. First run downloads ~1.4 GiB. Subsequent runs ~5–10s on cold disk.
- Bad UUID or unknown asset → 404. Empty / whitespace-only search query → 303 redirect to \`/\`.
- Thumbnail URLs are derivable from \`(hash, size)\`; the handler validates shape and serves the file without a DB hit.
- Originals route does require a DB lookup (so only assets known to the DB are exposed).

## New workspace deps

\`axum = "0.8"\`, \`tower = "0.5"\`, \`tower-http = "0.6"\`, \`maud = "0.27"\`. Plus \`http-body-util\` and \`tokio-util\` for dev-tests and ReaderStream.

## Test plan

- [x] \`cargo fmt --all -- --check\`
- [x] \`cargo clippy --workspace --all-targets -- -D warnings\`
- [x] \`cargo test --workspace --exclude eidetic-db --exclude eidetic-server\` (Docker-gated tests run on CI)
- [ ] Manual smoke after merge: \`eidetic serve\`, open \`http://127.0.0.1:8080\` in a browser. Verify recent grid, search, asset detail, download original.

## Out of scope (deferrable)

- WebDAV (separate protocol)
- Pagination / "load more"
- "Similar to this" on the detail page
- Date/camera/GPS filters
- Authentication (you SSH-tunnel for remote access)
- Adding \`hash\` to \`SearchResult\` so the search-results grid doesn't have to derive the hash from \`storage_path\` — small follow-up worth doing
EOF
)"
```

Return the PR URL.

---

## Self-Review

**Spec coverage:**

| Spec requirement | Task |
|---|---|
| New `eidetic-server` library crate | T1 |
| `serve(addr, deps)` async entry point | T1 |
| `GET /` | T4 |
| `GET /search?q=&limit=` | T6 |
| `GET /assets/:id` | T5 |
| `GET /assets/:id/raw` | T5 |
| `GET /thumbs/:size/:hash` | T4 |
| `fetch_recent` + `fetch_by_id` repo methods | T2 |
| Embedder worker (mpsc + oneshot) | T1 (worker bootstrap) + T6 (consumer) |
| `eidetic serve [--bind]` CLI subcommand | T7 |
| Maud HTML rendering | T3 |
| Cache-Control on thumbnails | T4 |
| Content-Disposition on raw | T5 |
| 404 on bad UUID, bad size, bad hash, missing file | T4, T5 |
| 303 redirect on empty search | T6 |
| Handler tests | T8 |
| AGENTS.md update | T9 |
| Zero new heavyweight deps | T1 (axum/tower stack is the only new surface) |

**Placeholder scan:**
- One mild ambiguity in Task 6 Step 1: the `hash_from_path` workaround. The plan acknowledges this is a workaround and points to the cleaner alternative (adding `hash` to `SearchResult`). Implementer picks one. Acceptable.
- Workspace `tokio-util` dep position: the plan says "check if it's already there as a transitive dep." That's a verification step, not a placeholder.
- Otherwise: no TBDs.

**Type consistency:**
- `RecentAsset`, `AssetDetail`, `GridTile`, `DetailView` are defined once each, referenced consistently across tasks.
- `EmbedJob` is `(String, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>)` in T1 and consumed in T6 with the same shape.
- `AppState` fields are stable across handler tasks.
- `ServerError` variants used in handlers match what's defined in lib.rs.

**Risks documented:**
- The `_embed_rx` lifetime issue in `test_router` is called out in T8 Step 2.
- The `hash_from_path` reverse-engineering is called out in T6 Step 1.
- Server holds ~1.5 GB resident with model loaded. Acknowledged in spec; not addressed here.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-19-http-server.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, two-stage review between tasks. Same flow used for #15, #16, #17.

**2. Inline Execution** — execute tasks in this session via `superpowers:executing-plans`.

Which approach?
