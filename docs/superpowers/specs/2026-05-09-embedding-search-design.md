# Embedding + Semantic Search Design

**Goal:** Add `eidetic embed` to generate image embeddings and `eidetic search <query>` to find photos by natural-language description.

**Architecture:** Two new CLI commands wired directly in `eidetic-cli`, composing `eidetic-ml` (SigLIP 2 inference via ONNX Runtime) and `eidetic-db` (vector storage + similarity search via pgvector/VectorChord). No new crate — `eidetic-search` is deferred until `eidetic-server` exists as a second caller.

**Tech stack additions:** `ort` (ONNX Runtime), `hf-hub` (model download), `image` (preprocessing), `ndarray` (tensor construction), `tokenizers` (text tokenization), `pgvector` (sqlx vector type integration).

---

## 1. `SiglipEmbedder` in `eidetic-ml`

### Struct

```rust
pub struct SiglipEmbedder {
    vision_session: ort::Session,
    text_session: ort::Session,
    tokenizer: tokenizers::Tokenizer,
}
```

### Loading

`SiglipEmbedder::load(models_dir: &Path) -> Result<Self>`

Uses `hf-hub` to download three files from `google/siglip2-base-patch16-256` into `models_dir` if not already cached:
- `onnx/vision_model.onnx`
- `onnx/text_model.onnx`
- `tokenizer.json`

Prints `"Downloading SigLIP 2 model (~350 MB)…"` on first run. Subsequent runs load from disk. Total download ~350 MB. Network failure propagates as `ModelDownload` error; the CLI prints `"Failed to download model: check your internet connection."` and exits non-zero.

### `Embedder` trait extension

```rust
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, path: &Path) -> Result<Embedding>;       // image
    fn embed_text(&self, text: &str) -> Result<Embedding>;  // text query
}
```

`MockEmbedder` implements `embed_text` by returning `vec![0.0; dim]`.

### Image preprocessing (inside `embed`)

1. Open with `image` crate → convert to RGB8
2. Resize to 256×256 (Lanczos3)
3. Normalize: `pixel / 255.0`, then `(val - 0.5) / 0.5` per channel → range `[-1.0, 1.0]`
4. Build CHW float32 ndarray shape `[1, 3, 256, 256]`
5. Run vision ONNX session → extract output `[1, 768]` → L2-normalize → return `Embedding`

### Text preprocessing (inside `embed_text`)

1. Tokenize with `tokenizer.json` (HuggingFace `tokenizers` crate), pad/truncate to 64 tokens
2. Build `input_ids` tensor `[1, 64]` (i64) and `attention_mask` tensor `[1, 64]` (i64)
3. Run text ONNX session → extract `[1, 768]` → L2-normalize → return `Embedding`

### Errors

New variants in `eidetic_ml::Error`:
- `ModelLoad { source: ort::Error }` — session failed to open
- `ModelDownload { source: hf_hub::Error }` — network failure
- `Inference { source: ort::Error }` — inference failure
- `Tokenize(String)` — tokenizer error

---

## 2. DB migration + new queries

### Migration `004_embeddings.sql`

```sql
ALTER TABLE assets ADD COLUMN embedding vector(768);

CREATE INDEX assets_embedding_idx
    ON assets USING vchordrq (embedding vector_cosine_ops);
```

`NULL` embeddings (assets not yet embedded) are excluded from the index automatically.

### New functions on `PgAssetsRepo`

**`fetch_unembedded() -> Result<Vec<(AssetId, PathBuf)>>`**

Returns `(id, storage_path)` for every asset where `embedding IS NULL AND mime_type LIKE 'image/%'`. Videos skipped in v0.

**`store_embedding(id: AssetId, embedding: &[f32]) -> Result<()>`**

```sql
UPDATE assets SET embedding = $1 WHERE id = $2
```

The `pgvector` crate provides sqlx type integration for `vector` columns — accepts `Vec<f32>` directly.

**`search_similar(query_vec: &[f32], limit: u32) -> Result<Vec<SearchResult>>`**

```sql
SELECT id, storage_path, mime_type, date_taken,
       camera_make, camera_model, latitude, longitude,
       1 - (embedding <=> $1) AS score
FROM assets
WHERE embedding IS NOT NULL
ORDER BY embedding <=> $1
LIMIT $2
```

`<=>` is cosine distance. `1 - distance` gives cosine similarity as score (0–1, higher = better match).

### `SearchResult` struct (in `eidetic-db`)

```rust
pub struct SearchResult {
    pub id: AssetId,
    pub storage_path: PathBuf,
    pub score: f32,
    pub mime_type: Option<String>,
    pub date_taken: Option<DateTime<Utc>>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}
```

---

## 3. `eidetic embed` command

**Invocation:** `eidetic embed`

No arguments. Uses `EIDETIC_DATABASE_URL` and `EIDETIC_LIBRARY_DIR` from env. `models_cache` from `Paths` config.

**Flow:**

1. Load `SiglipEmbedder::load(&config.models_cache)` — downloads on first run
2. Call `fetch_unembedded()` — get list of assets needing embeddings
3. If empty: print `"Nothing to do."` and exit
4. Print `"Found N images to embed"`
5. For each asset:
   - `tokio::task::spawn_blocking(|| embedder.embed(storage_path))`
   - On success: `store_embedding(id, &embedding)`, print `"[42/500] filename.jpg"`
   - On failure: print `"  skipped: <error>"`, continue — one bad file doesn't abort the run
6. Print summary: `"Done. Embedded 498, skipped 2."`

**Re-run behaviour:** idempotent. Already-embedded assets are excluded by `fetch_unembedded`. Running twice is safe and fast.

**Concurrency:** sequential in v0 — one image at a time via `spawn_blocking`. Parallel workers deferred until sequential proves too slow.

---

## 4. `eidetic search` command

**Invocation:**

```
eidetic search <query> [--limit 10] [--fields path,score,date] [--json]
```

**Available fields:** `path` (default), `score`, `date`, `make`, `model`, `lat`, `lon`, `mime`

**Flow:**

1. Load `SiglipEmbedder::load(&config.models_cache)` — if models not present, fail with: `"Run 'eidetic embed' first to download the model and generate embeddings."`
2. `embedder.embed_text(query)` → query vector (via `spawn_blocking`)
3. `search_similar(query_vec, limit)` → `Vec<SearchResult>`
4. If empty: print `"No results."` and exit
5. Format and print

**Output formats:**

Default (path only, pipe-friendly):
```
/home/user/.cache/eidetic/library/2c/f2/2cf24d...jpg
```

`--fields score,date,path` (tab-separated):
```
0.92    2024-03-15    /home/user/.cache/eidetic/library/2c/f2/...jpg
0.87    2023-08-01    /home/user/.cache/eidetic/library/ab/cd/...jpg
```

`--json` (overrides `--fields`, dumps all fields):
```json
[{"path":"...","score":0.92,"date":"2024-03-15T10:23:00Z","make":"Apple",...}]
```

**Edge cases:**
- Assets not yet embedded are silently excluded (`WHERE embedding IS NOT NULL`)
- Unknown field in `--fields`: fail early with `"Unknown field: 'foo'. Valid fields: path, score, date, make, model, lat, lon, mime"`

---

## 5. Testing

**`eidetic-ml`:** `MockEmbedder` covers the trait contract for all other crate tests. `SiglipEmbedder` gets one `#[ignore]` integration test: load model from disk, embed a real image, assert dim is 768 and the vector is L2-normalized (dot product with itself ≈ 1.0). Run explicitly with `cargo test -- --ignored`.

**`eidetic-db`:** Three testcontainers integration tests (same pattern as existing `assets.rs`):
- `fetch_unembedded_excludes_already_embedded` — insert two assets, embed one, assert only one returned
- `store_embedding_persists_and_updates` — store an embedding, verify round-trip via raw SQL
- `search_similar_orders_by_cosine_similarity` — insert assets with known embeddings, verify result ordering

**`eidetic-cli`:** Unit tests using `MockEmbedder` — no model or DB needed:
- `embed_command_calls_store_for_each_unembedded_asset`
- `search_command_default_output_is_path_per_line`
- `search_command_fields_flag_adds_columns`
- `search_command_json_flag_serializes_all_fields`

---

## 6. Deferred

- `eidetic-search` crate extraction — deferred until `eidetic-server` needs the same search logic
- Parallel embedding workers — deferred until sequential proves too slow
- Video embeddings — SigLIP 2 is image-only; video support requires frame extraction
- SigLIP 2 So400m upgrade (1152-dim) — migration path documented in ADR-0004
