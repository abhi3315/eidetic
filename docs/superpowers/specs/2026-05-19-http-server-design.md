# HTTP Server Design

**Status:** Design approved 2026-05-19. Ready for implementation plan.

**Scope:** First Phase 4 milestone — a minimal Axum server that exposes the existing library through a browser. Personal use, localhost-bound, no authentication. Turns thumbnails from "files on disk" into "library you can actually browse."

---

## Goals

- One new crate `eidetic-server` (library, not binary) exposing `pub fn serve(addr, deps...) -> impl Future<Output = anyhow::Result<()>>`.
- One new CLI subcommand `eidetic serve [--bind <addr>]` that wires up dependencies and calls into the library.
- Five endpoints covering the personal browse-and-search loop end-to-end:
  - `GET /` — landing page with search form + grid of most-recent imports.
  - `GET /search?q=<text>&limit=<n>` — semantic search results as a grid.
  - `GET /assets/<id>` — single-asset detail page (medium thumbnail + EXIF + download link).
  - `GET /assets/<id>/raw` — stream the CAS-stored original.
  - `GET /thumbs/<size>/<hash>` — serve a thumbnail file (`size` is `s` or `m`).
- HTML rendered with `maud` (compile-time Rust DSL, automatic XSS escaping).
- Search powered by a `SiglipEmbedder` owned by a long-lived `tokio::task::spawn_blocking` worker thread, communicating via `tokio::sync::mpsc` jobs — same pattern PR #16 introduced.
- Default bind `127.0.0.1:8080`; overridable via `--bind` flag or `EIDETIC_BIND` env var.

## Non-goals

- WebDAV. Different protocol surface (PROPFIND, MKCOL, etc.) — separate spec when needed.
- Pagination on search results. The limit query param caps the response; "more" / infinite scroll is deferred.
- "Similar to this" on the detail page. Deferred.
- Date / camera / GPS filters. Deferred.
- Authentication. Personal use, localhost-bound. SSH-tunnel for remote access.
- A `--no-search` mode that skips model load. Boot delay (5–15s on first run) is the same as `eidetic embed` / `eidetic search`; the server's value is search, no point starting it without.
- Server-side rendering of EXIF maps (lat/long → a static map image). Deferred.
- Editing metadata or assets through the UI.
- Compression middleware. Thumbnails are JPEG, already compressed.
- CORS. Localhost-bound.

---

## Architecture overview

```
                       ┌─────────────────────────────┐
                       │  eidetic serve              │
                       │  (eidetic-cli subcommand)   │
                       └──────────────┬──────────────┘
                                      │ wires deps
                                      ▼
                       ┌─────────────────────────────┐
                       │  eidetic_server::serve(...) │
                       │  - builds Axum Router       │
                       │  - spawns embedder worker   │
                       │  - axum::serve(addr, app)   │
                       └─────────┬───────────────────┘
                                 │
              ┌──────────────────┼──────────────────────┐
              │                  │                      │
              ▼                  ▼                      ▼
       ┌────────────┐    ┌────────────┐         ┌────────────┐
       │  / handler │    │  /search   │         │  /assets/* │
       │  recent    │    │  - embed   │         │  - DB read │
       │  imports   │    │  - search  │         │  - stream  │
       └─────┬──────┘    └─────┬──────┘         └─────┬──────┘
             │                 │                      │
             ▼                 ▼                      ▼
       ┌────────────────────────────────────────────────┐
       │  AppState: { repo, embedder_tx, library_dir }  │
       └──────────────┬─────────────────────────────────┘
                      │
                      ▼
               ┌─────────────┐    mpsc::channel    ┌──────────────────┐
               │ /search arm │ ────────────────────►  embedder worker │
               │ awaits      │ ◄────────────────── │ (spawn_blocking) │
               │ oneshot     │     oneshot reply    │  owns SiglipEm…  │
               └─────────────┘                      └──────────────────┘
```

---

## Crate layout

New crate at `crates/eidetic-server/`:

```
crates/eidetic-server/
├── Cargo.toml
└── src/
    ├── lib.rs        # serve() entry point, AppState, Router, error type
    ├── handlers.rs   # one fn per route
    └── views.rs      # maud HTML helpers (layout, grid, detail page)
```

Public surface from `lib.rs`:

```rust
pub struct ServerDeps {
    pub repo: PgAssetsRepo,
    pub library_dir: PathBuf,
    pub models_cache: PathBuf,
}

pub async fn serve(addr: SocketAddr, deps: ServerDeps) -> anyhow::Result<()>;
```

The library is self-contained: it spawns the embedder worker, builds the router, calls `axum::serve`. The CLI just constructs `ServerDeps`, picks an address, and awaits `serve`.

---

## Workspace map update

`eidetic-server` becomes the sixth crate. Adding it after this PR lands satisfies the project rule "no new crate without a concrete caller" — the CLI calls it.

| Crate | Role | Depends on |
|---|---|---|
| `eidetic-server` *(new)* | Axum HTTP server. `serve()` owns the embedder worker and the route table. | `eidetic-core`, `eidetic-db`, `eidetic-ingest`, `eidetic-ml` |

The CLI gains `eidetic-server` as a dep but no other crate touches it. ML and ingest are unchanged.

---

## Routes

### `GET /`

Landing page. Layout:

```
┌────────────────────────────────────────────────┐
│  Eidetic                                       │
│  ┌────────────────────────────┐  ┌──────────┐ │
│  │ search…                    │  │  Search  │ │
│  └────────────────────────────┘  └──────────┘ │
│                                                │
│  Recent imports (24)                           │
│                                                │
│  ┌──┐  ┌──┐  ┌──┐  ┌──┐  ┌──┐  ┌──┐          │
│  │  │  │  │  │  │  │  │  │  │  │  │  ...     │
│  └──┘  └──┘  └──┘  └──┘  └──┘  └──┘          │
│                                                │
└────────────────────────────────────────────────┘
```

Backed by a new repo method `fetch_recent(limit: u32) -> Vec<RecentAsset>` returning `(AssetId, Sha256, Option<DateTime<Utc>>)` for the 24 most recently imported images.

### `GET /search?q=<text>&limit=<n>`

Query params:
- `q` (required): the natural-language query. Empty or missing → redirect to `/`.
- `limit` (optional, default 24, max 60): how many results to return.

Flow:
1. Validate `q` is non-empty (whitespace-only also rejected).
2. Send `(q, oneshot::Sender)` to the embedder worker; await reply.
3. Call `repo.search_similar(&query_vec, limit)`.
4. Render results grid.

Results page shows:
- The query echoed at top.
- Result count + the cosine-similarity score of the best match (debug aid).
- Grid of thumbnail-linked-to-detail-page tiles.

On embedder error (rare; model went bad mid-flight): 500 page with "Search temporarily unavailable" + the error logged via tracing.

### `GET /assets/<id>`

Single-asset detail page. `<id>` is a UUID; if not parseable as UUID → 404.

Page shows:
- Medium thumbnail (1024px).
- Filename, MIME type.
- "Imported" timestamp.
- "Taken" timestamp + camera + GPS (if EXIF present).
- "Download original" link → `GET /assets/<id>/raw`.

If the asset has no medium thumbnail yet (`thumbnails_generated = false`), the page renders without an image, with a small notice "Thumbnails pending — run `eidetic thumbnail` to generate."

### `GET /assets/<id>/raw`

Streams the CAS-stored original file. Steps:
1. Parse `<id>` as UUID; 404 if invalid.
2. Look up `(storage_path, mime_type, original_filename)` from DB; 404 if no row.
3. Stream the file via `tokio::fs::File` + `axum::body::Body::from_stream`.
4. `Content-Type` from DB `mime_type` column; `Content-Disposition: inline; filename="<original_filename>"`.

### `GET /thumbs/<size>/<hash>`

Static-ish file handler. Steps:
1. Validate `<size>` is `"s"` or `"m"`; 404 otherwise.
2. Validate `<hash>` is 64 lowercase hex chars; 404 otherwise.
3. Derive path: `<library_dir>/.thumbs/<size>/<hash[..2]>/<hash[2..4]>/<hash>.jpg`.
4. If file exists, stream it; 404 if not.
5. Long cache headers since thumbnails are content-addressed and never change: `Cache-Control: public, max-age=31536000, immutable`.

This handler skips the DB entirely — the path is fully derivable from URL. A 404 here means "the thumbnail file doesn't exist on disk," which is correct: it doesn't matter whether the row's `thumbnails_generated` flag is true; what matters is whether the bytes are there.

---

## DB additions

One new method on `PgAssetsRepo`:

```rust
pub struct RecentAsset {
    pub id: AssetId,
    pub hash: Sha256,
    pub original_filename: String,
    pub imported_at: DateTime<Utc>,
}

impl PgAssetsRepo {
    pub async fn fetch_recent(&self, limit: u32) -> Result<Vec<RecentAsset>>;
    pub async fn fetch_by_id(&self, id: AssetId) -> Result<Option<AssetDetail>>;
}
```

Where `AssetDetail` is the full row for the detail page:

```rust
pub struct AssetDetail {
    pub id: AssetId,
    pub hash: Sha256,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: i64,
    pub mime_type: Option<String>,
    pub imported_at: DateTime<Utc>,
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub thumbnails_generated: bool,
}
```

No migration. Existing schema has every column needed; only the structured-row getters are new.

---

## Embedder worker lifecycle

Mirrors `eidetic embed` / PR #16:

```rust
type EmbedJob = (String, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);

// in serve():
let models_dir = deps.models_cache.clone();
let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedJob>(8);
let _worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
    let mut embedder = SiglipEmbedder::load(&models_dir)?;
    while let Some((text, reply)) = embed_rx.blocking_recv() {
        let _ = reply.send(embedder.embed_text(&text));
    }
    Ok(())
});
```

The worker handle is stored in `AppState` (or held by a separate task) so a shutdown signal can drop `embed_tx` and let the worker exit cleanly. For v1, we don't implement graceful shutdown — Ctrl-C kills the process and Tokio drops things; the OS cleans up.

Channel capacity 8 (not 1 like `eidetic embed`) because the server can have several concurrent searches in flight from a multi-tab browser. Capacity 8 is "enough buffer that one slow embedding doesn't starve siblings" without inviting unbounded queue growth.

On boot, the model loads inside the worker thread. The first search has to wait for `embedder.load()` to complete (5–15s), then `embed_text` (sub-second). The `eidetic serve` command prints `Loading model… (first run downloads ~1.4 GiB)` before binding, and the bind itself prints `Listening on http://127.0.0.1:8080` once the worker is ready to serve.

To avoid binding before the worker is ready, the model load happens before `axum::serve()` runs. Simplest sequencing:

```rust
// Block on load before listening, so search works from the very first request.
let mut embedder = tokio::task::spawn_blocking(move || SiglipEmbedder::load(&models_dir))
    .await
    .context("embedder thread panicked")?
    .context("failed to load SigLIP 2 model")?;

// Move the loaded embedder into a long-lived worker that owns it.
let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedJob>(8);
tokio::task::spawn_blocking(move || {
    while let Some((text, reply)) = embed_rx.blocking_recv() {
        let _ = reply.send(embedder.embed_text(&text));
    }
});

// Now bind and serve.
```

Trade-off: load and worker startup happen on two different spawn_blocking calls. The first one returns the loaded embedder; the second one consumes it. This is fine — moving a `SiglipEmbedder` between threads is cheap (it's just a value move; no heap rearrangement).

---

## Error handling

A small `ServerError` enum implementing `axum::response::IntoResponse`:

```rust
pub enum ServerError {
    NotFound(String),                 // 404, with the missing thing's name
    BadRequest(String),               // 400, with the reason
    EmbedFailed(eidetic_ml::Error),   // 500, "search temporarily unavailable"
    DbFailed(eidetic_db::Error),      // 500, "database error"
    Io(std::io::Error),               // 500, generic
}
```

Handlers return `Result<impl IntoResponse, ServerError>`. The error type renders a minimal HTML page (status code + brief message) and `tracing::error!`-logs the source. No stack traces in the response body.

---

## HTML / styling approach

`maud` provides type-safe HTML with compile-time templates. Layout helper:

```rust
fn layout(title: &str, body: Markup) -> Markup {
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
                header { a href="/" { "Eidetic" } " · " a href="/" { "home" } }
                main { (body) }
            }
        }
    }
}
```

`INLINE_CSS` is a `const &'static str` with ~50 lines of CSS: dark-on-light, a 6-column grid, image-fit, subtle borders. No frameworks, no Tailwind, no external fonts. Goal: doesn't look broken, doesn't look corporate.

Grid helper:

```rust
fn asset_grid(assets: &[GridTile]) -> Markup {
    html! {
        div class="grid" {
            @for tile in assets {
                a href=(format!("/assets/{}", tile.id)) class="tile" {
                    img src=(format!("/thumbs/m/{}", tile.hash))
                        loading="lazy"
                        alt=(tile.alt);
                    @if let Some(score) = tile.score {
                        span class="score" { (format!("{:.0}%", score * 100.0)) }
                    }
                }
            }
        }
    }
}
```

`GridTile` is a small server-internal struct that holds `(id, hash, alt, optional_score)` so both `/` and `/search` use the same grid renderer.

---

## CLI integration

In `crates/eidetic-cli/src/main.rs`:

1. Add `Serve { bind: Option<String> }` variant to `Command`.
2. Add the matching arm body. Shape:

```rust
Command::Serve { bind } => {
    let config = Config::from_env();
    let addr: SocketAddr = bind
        .or_else(|| std::env::var("EIDETIC_BIND").ok())
        .as_deref()
        .unwrap_or("127.0.0.1:8080")
        .parse()
        .context("invalid bind address")?;

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
    Ok(())
}
```

The "Listening on …" message is printed by the server library itself (it knows when bind succeeded), not by the CLI arm.

---

## Dependencies

Workspace `Cargo.toml` gains four new entries:

```toml
axum = "0.8"
tower = "0.5"
tower-http = { version = "0.6", features = ["trace"] }
maud = "0.27"
```

`eidetic-server/Cargo.toml`:

```toml
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
```

`eidetic-cli/Cargo.toml` gains `eidetic-server = { path = "../eidetic-server" }`.

No new transitive heavyweight deps — Axum brings `hyper`/`tokio-util` which are already in the workspace via SQLx/Tokio.

---

## Testing approach

Three tiers, smallest-to-largest:

### Unit tests (in-file)

In `crates/eidetic-server/src/views.rs`:
- `layout` renders well-formed HTML with the right title.
- `asset_grid` with zero results renders an empty grid (no panic).
- `asset_grid` with multiple results includes the expected `img` and `a` tags.

These exercise the `maud` macros without touching the network or DB.

### Handler tests (in-file, using `axum::Router` + `tower::ServiceExt::oneshot`)

Each handler can be tested with a fake `AppState` (in-memory repo stub or a real testcontainer one). For v1, we use real testcontainers because the in-memory mock would be exactly the abstraction we deleted in PR #15. Three handler tests:

- `index_renders_recent_grid` — seed 3 assets via `repo.insert_asset`, GET `/`, assert response is 200 and contains the three filenames.
- `search_with_empty_q_redirects_to_index` — GET `/search?q=`, assert 303 to `/`.
- `assets_id_404_for_unknown_uuid` — GET `/assets/<random uuid>`, assert 404.

These live in `crates/eidetic-server/tests/handlers.rs`.

### Integration tests

In `crates/eidetic-db/tests/server_integration.rs` (the `eidetic-db` test crate already has the testcontainer scaffolding so reuse it rather than duplicate):

- `serve_then_curl_index_returns_html` — start the server on a random port, ingest 3 images, hit `/`, assert HTML response contains all three thumbnail URLs.
- `thumbs_endpoint_streams_jpeg_bytes` — same setup, GET `/thumbs/m/<hash>`, assert response body's first 2 bytes are `0xFF 0xD8` (JPEG magic).
- `assets_id_raw_streams_original_with_correct_mime` — GET `/assets/<id>/raw`, assert `Content-Type: image/jpeg` and the body bytes match the original.

The `/search` endpoint integration test is **deferred** because it requires the SigLIP model file to be downloaded (`eidetic-ml/src/siglip.rs:466` already has the `#[ignore]` pattern for this). A `#[tokio::test] #[ignore]` test that runs `/search?q=dog` against a real loaded model is added but excluded from CI.

---

## Manual smoke test (post-merge)

1. `eidetic import ~/Pictures/sample` (some folder with 10+ images).
2. `eidetic embed` (so search has something to find).
3. `eidetic serve` → wait for "Listening on http://127.0.0.1:8080".
4. Open the URL in a browser.
5. Expected: grid of 10+ thumbnails on the landing page.
6. Type a query, click Search. Expected: results grid.
7. Click a thumbnail. Expected: detail page with metadata + download link.
8. Click "Download original". Expected: file downloads with original filename.

---

## Implementation order (hint for plan author)

1. Add new workspace deps (`axum`, `tower`, `tower-http`, `maud`).
2. Scaffold `crates/eidetic-server/` (Cargo.toml, lib.rs, handlers.rs, views.rs) with the `serve()` entry point and the AppState struct. Empty handlers (just `Hello, world!`).
3. Add `fetch_recent` and `fetch_by_id` to `PgAssetsRepo` + their testcontainer tests.
4. Implement views: `layout`, `asset_grid`, `detail_page`. Unit tests for each.
5. Implement `GET /` and `GET /thumbs/<size>/<hash>` (the two no-search routes).
6. Implement `GET /assets/<id>` and `GET /assets/<id>/raw`.
7. Implement the embedder worker + `GET /search`.
8. Add `Command::Serve` to the CLI.
9. Integration tests in `eidetic-db/tests/server_integration.rs`.
10. AGENTS.md workspace-map update for the new crate.
11. Verify (fmt, clippy, test, cargo-deny) and open PR.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green.

---

## Spec self-review

**Placeholder scan:**
- No TBDs, no "implement later," no vague behaviors. Every endpoint's exact flow is spelled out.

**Internal consistency:**
- The architecture diagram, the route list, and the AppState fields agree.
- The embedder worker lifecycle in the dedicated section matches what the CLI Serve arm does.
- The endpoint table at the top of "Routes" matches the implementation order.

**Scope:**
- Single PR scope. Two new repo methods, one new library crate, one new CLI subcommand, eight integration test points across two test files. Plausible in one focused PR.

**Ambiguity:**
- `Cache-Control: public, max-age=31536000, immutable` for thumbnails — explicit.
- `Content-Disposition: inline; filename="..."` for raw originals — explicit.
- `q` validation rule (whitespace-only rejected, empty redirects to `/`) — explicit.
- 404 vs 400 distinction: invalid UUID is 404 (asset doesn't exist), invalid bind addr is a CLI startup error (not a HTTP error).

**Known risk:** the server holds a loaded SigLIP model in memory for as long as it runs. On a 16 GB Mac that's fine. If the server runs alongside `eidetic embed` (which also loads the model), you have two copies — wasteful but won't OOM at personal-use scale. Not addressed in v1.
