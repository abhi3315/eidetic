# Embedding + Semantic Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `eidetic embed` to generate SigLIP 2 image embeddings and `eidetic search <query>` to find photos by natural-language description.

**Architecture:** `SiglipEmbedder` in `eidetic-ml` handles model download (via `hf-hub`), image preprocessing, and ONNX inference (via `ort`). Three new DB methods on `PgAssetsRepo` handle embedding storage and vector search. Two new CLI subcommands wire it all together in `eidetic-cli`. The embedding column (`vector(768)`) and VectorChord index already exist in `migrations/002_embeddings.sql` — no new migration needed.

**Tech Stack:** `ort 2` (ONNX Runtime), `hf-hub 0.4` (model download), `image 0.25` (preprocessing), `ndarray 0.16` (tensors), `tokenizers 0.21` (text tokenization), `pgvector 0.4` (sqlx vector type), `serde_json 1` (JSON output).

---

## File map

**Create:**
- `crates/eidetic-ml/src/siglip.rs` — `SiglipEmbedder`: load, image embed, text embed

**Modify:**
- `Cargo.toml` (root) — add 6 new workspace deps
- `crates/eidetic-ml/Cargo.toml` — pull in ort, hf-hub, image, ndarray, tokenizers
- `crates/eidetic-ml/src/error.rs` — add ModelLoad, ModelDownload, Inference, Tokenize
- `crates/eidetic-ml/src/embedder.rs` — add `embed_text` to trait + MockEmbedder
- `crates/eidetic-ml/src/lib.rs` — export SiglipEmbedder
- `crates/eidetic-db/Cargo.toml` — add pgvector
- `crates/eidetic-db/src/error.rs` — add Query variant
- `crates/eidetic-db/src/assets.rs` — SearchResult struct + 3 new methods
- `crates/eidetic-db/src/lib.rs` — export SearchResult + new method names
- `crates/eidetic-db/tests/assets.rs` — 3 new integration tests
- `crates/eidetic-cli/Cargo.toml` — add eidetic-ml, serde_json
- `crates/eidetic-cli/src/main.rs` — Embed + Search subcommands

---

## Task 1: Workspace dependencies

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/eidetic-ml/Cargo.toml`
- Modify: `crates/eidetic-db/Cargo.toml`
- Modify: `crates/eidetic-cli/Cargo.toml`

- [ ] **Step 1: Add new workspace deps to root `Cargo.toml`**

Add these entries to the `[workspace.dependencies]` section (after the existing `tempfile` entry):

```toml
# ONNX Runtime (ML inference, downloads prebuilt native library at build time)
ort = { version = "2", features = ["download-binaries", "ndarray"] }

# HuggingFace Hub (model file download)
hf-hub = "0.4"

# Image loading and resizing
image = "0.25"

# N-dimensional arrays for tensor construction
ndarray = "0.16"

# Text tokenization (HuggingFace tokenizers port)
tokenizers = { version = "0.21", default-features = false, features = ["onig"] }

# pgvector sqlx type integration
pgvector = { version = "0.4", features = ["sqlx"] }

# JSON serialization for search output
serde_json = "1"
```

- [ ] **Step 2: Add deps to `crates/eidetic-ml/Cargo.toml`**

Replace the current `[dependencies]` block with:

```toml
[dependencies]
eidetic-core = { path = "../eidetic-core" }
thiserror = { workspace = true }
tracing = { workspace = true }
ort = { workspace = true }
hf-hub = { workspace = true }
image = { workspace = true }
ndarray = { workspace = true }
tokenizers = { workspace = true }
```

- [ ] **Step 3: Add pgvector to `crates/eidetic-db/Cargo.toml`**

Add to `[dependencies]`:

```toml
pgvector = { workspace = true }
```

- [ ] **Step 4: Add eidetic-ml and serde_json to `crates/eidetic-cli/Cargo.toml`**

Add to `[dependencies]`:

```toml
eidetic-ml = { path = "../eidetic-ml" }
serde_json = { workspace = true }
```

- [ ] **Step 5: Verify the workspace compiles**

```bash
cargo build --workspace
```

Expected: compiles without errors. The `ort` `download-binaries` feature will fetch the ONNX Runtime native library on first build (~7 MB download).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/eidetic-ml/Cargo.toml crates/eidetic-db/Cargo.toml crates/eidetic-cli/Cargo.toml
git commit -m "chore: add ort, hf-hub, image, ndarray, tokenizers, pgvector, serde_json deps"
```

---

## Task 2: `eidetic-ml` errors + `Embedder` trait extension

**Files:**
- Modify: `crates/eidetic-ml/src/error.rs`
- Modify: `crates/eidetic-ml/src/embedder.rs`

- [ ] **Step 1: Write tests for the new trait method on MockEmbedder**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/eidetic-ml/src/embedder.rs`:

```rust
    #[test]
    fn mock_embed_text_returns_correct_dim() {
        let embedder = MockEmbedder::new(768);
        let emb = embedder.embed_text("dog on beach").unwrap();
        assert_eq!(emb.dim(), 768);
    }

    #[test]
    fn mock_embed_text_returns_zeros() {
        let embedder = MockEmbedder::new(4);
        let emb = embedder.embed_text("test").unwrap();
        assert_eq!(emb.as_slice(), &[0.0f32; 4]);
    }
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p eidetic-ml
```

Expected: compile error — `embed_text` does not exist on `MockEmbedder`.

- [ ] **Step 3: Update `crates/eidetic-ml/src/error.rs`**

Replace the file contents with:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("model load failed: {0}")]
    ModelLoad(String),

    #[error("model download failed: {0}")]
    ModelDownload(String),

    #[error("inference failed: {0}")]
    Inference(String),

    #[error("tokenizer error: {0}")]
    Tokenize(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 4: Add `embed_text` to the `Embedder` trait and `MockEmbedder`**

In `crates/eidetic-ml/src/embedder.rs`, update the trait and `MockEmbedder` impl. The full updated file:

```rust
use crate::Result;
use std::path::Path;

/// A vector embedding produced by an [`Embedder`].
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(Vec<f32>);

impl Embedding {
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<f32> {
        self.0
    }
}

/// Produces vector embeddings from images and text.
///
/// Synchronous on purpose — inference is CPU/GPU-bound, not I/O-bound.
/// Callers that want to keep their tokio runtime threads free should wrap
/// calls in `tokio::task::spawn_blocking`.
pub trait Embedder: Send + Sync {
    /// Output dimension. Stable for the lifetime of an embedder instance.
    fn dim(&self) -> usize;

    /// Compute an embedding for the image at `path`.
    fn embed(&self, path: &Path) -> Result<Embedding>;

    /// Compute an embedding for a text string.
    fn embed_text(&self, text: &str) -> Result<Embedding>;
}

/// Deterministic mock embedder for tests. Returns zeros for all inputs.
pub struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Embedder for MockEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, _path: &Path) -> Result<Embedding> {
        Ok(Embedding(vec![0.0; self.dim]))
    }

    fn embed_text(&self, _text: &str) -> Result<Embedding> {
        Ok(Embedding(vec![0.0; self.dim]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn mock_returns_correct_dim() {
        let embedder = MockEmbedder::new(768);
        assert_eq!(embedder.dim(), 768);
    }

    #[test]
    fn mock_returns_correct_size_vector() {
        let embedder = MockEmbedder::new(768);
        let path = PathBuf::from("/nonexistent");
        let emb = embedder.embed(&path).unwrap();
        assert_eq!(emb.dim(), 768);
        assert_eq!(emb.as_slice(), &[0.0; 768]);
    }

    #[test]
    fn embedding_round_trip() {
        let values = vec![1.0, 2.0, 3.0];
        let emb = Embedding::new(values.clone());
        assert_eq!(emb.dim(), 3);
        assert_eq!(emb.into_vec(), values);
    }

    #[test]
    fn mock_embed_text_returns_correct_dim() {
        let embedder = MockEmbedder::new(768);
        let emb = embedder.embed_text("dog on beach").unwrap();
        assert_eq!(emb.dim(), 768);
    }

    #[test]
    fn mock_embed_text_returns_zeros() {
        let embedder = MockEmbedder::new(4);
        let emb = embedder.embed_text("test").unwrap();
        assert_eq!(emb.as_slice(), &[0.0f32; 4]);
    }
}
```

- [ ] **Step 5: Run tests and confirm they pass**

```bash
cargo test -p eidetic-ml
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/eidetic-ml/src/error.rs crates/eidetic-ml/src/embedder.rs
git commit -m "feat(ml): extend Embedder trait with embed_text, update error variants"
```

---

## Task 3: `SiglipEmbedder` — model loading + image embedding

**Files:**
- Create: `crates/eidetic-ml/src/siglip.rs`
- Modify: `crates/eidetic-ml/src/lib.rs`

- [ ] **Step 1: Write a `#[ignore]` integration test first**

Create `crates/eidetic-ml/src/siglip.rs` with just the test:

```rust
use crate::{Embedder, Result};
use std::path::Path;

pub struct SiglipEmbedder {
    vision_session: ort::Session,
    text_session: ort::Session,
    tokenizer: tokenizers::Tokenizer,
}

impl SiglipEmbedder {
    pub fn load(_models_dir: &Path) -> Result<Self> {
        todo!()
    }
}

impl Embedder for SiglipEmbedder {
    fn dim(&self) -> usize {
        768
    }

    fn embed(&self, _path: &Path) -> Result<crate::Embedding> {
        todo!()
    }

    fn embed_text(&self, _text: &str) -> Result<crate::Embedding> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires SigLIP 2 ONNX model on disk"]
    fn image_embedding_is_768_dim_and_normalized() {
        let models_dir = std::env::var("EIDETIC_MODELS_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap()
                    .join(".cache/eidetic/models")
            });

        let embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        // Use any real image file on disk — the test checks shape + normalization only.
        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg to run this test");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.dim(), 768, "output must be 768-dimensional");

        let dot: f32 = emb.as_slice().iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "embedding must be L2-normalized, dot={dot}");
    }
}
```

- [ ] **Step 2: Add `siglip` module to `crates/eidetic-ml/src/lib.rs`**

Add `pub mod siglip;` and `pub use siglip::SiglipEmbedder;` to the lib.rs exports:

```rust
//! ML inference for Eidetic.

pub mod embedder;
pub mod error;
pub mod siglip;

pub use embedder::{Embedder, Embedding, MockEmbedder};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

- [ ] **Step 3: Implement `SiglipEmbedder::load`**

Replace the `todo!()` in `load` with the full implementation. The `dirs` crate is not available — we don't need it since `models_dir` is always passed in. Remove the `dirs` reference from the test too (use env var or a fixed path).

Full `crates/eidetic-ml/src/siglip.rs`:

```rust
use crate::{Embedder, Embedding, Error, Result};
use ndarray::Array;
use std::path::Path;
use tokenizers::Tokenizer;

const MODEL_REPO: &str = "google/siglip2-base-patch16-256";
const VISION_MODEL_FILE: &str = "onnx/vision_model.onnx";
const TEXT_MODEL_FILE: &str = "onnx/text_model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const IMAGE_SIZE: u32 = 256;
const SEQ_LEN: usize = 64;
const EMBED_DIM: usize = 768;
const PAD_TOKEN_ID: i64 = 1;

pub struct SiglipEmbedder {
    vision_session: ort::Session,
    text_session: ort::Session,
    tokenizer: Tokenizer,
}

impl SiglipEmbedder {
    /// Load the SigLIP 2 model from `models_dir`.
    ///
    /// Downloads `onnx/vision_model.onnx`, `onnx/text_model.onnx`, and
    /// `tokenizer.json` from HuggingFace on first call. Subsequent calls
    /// load from the local cache.
    pub fn load(models_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(models_dir)
            .map_err(|e| Error::ModelLoad(format!("cannot create models dir: {e}")))?;

        let vision_path = download(models_dir, VISION_MODEL_FILE)?;
        let text_path = download(models_dir, TEXT_MODEL_FILE)?;
        let tokenizer_path = download(models_dir, TOKENIZER_FILE)?;

        let vision_session = ort::Session::builder()
            .map_err(|e| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&vision_path)
            .map_err(|e| Error::ModelLoad(format!("vision model: {e}")))?;

        let text_session = ort::Session::builder()
            .map_err(|e| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&text_path)
            .map_err(|e| Error::ModelLoad(format!("text model: {e}")))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| Error::Tokenize(format!("load tokenizer: {e}")))?;

        Ok(Self {
            vision_session,
            text_session,
            tokenizer,
        })
    }
}

impl Embedder for SiglipEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    fn embed(&self, path: &Path) -> Result<Embedding> {
        let pixels = preprocess_image(path)?;
        let input = Array::from_shape_vec((1usize, 3, IMAGE_SIZE as usize, IMAGE_SIZE as usize), pixels)
            .map_err(|e| Error::Inference(format!("tensor shape: {e}")))?;

        let outputs = self
            .vision_session
            .run(ort::inputs!["pixel_values" => input.view()]
                .map_err(|e| Error::Inference(e.to_string()))?)
            .map_err(|e| Error::Inference(e.to_string()))?;

        let tensor = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = tensor.view().iter().copied().collect();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }

    fn embed_text(&self, text: &str) -> Result<Embedding> {
        let (ids, mask) = tokenize(&self.tokenizer, text)?;

        let ids_array = Array::from_shape_vec((1usize, SEQ_LEN), ids)
            .map_err(|e| Error::Tokenize(e.to_string()))?;
        let mask_array = Array::from_shape_vec((1usize, SEQ_LEN), mask)
            .map_err(|e| Error::Tokenize(e.to_string()))?;

        let outputs = self
            .text_session
            .run(
                ort::inputs![
                    "input_ids" => ids_array.view(),
                    "attention_mask" => mask_array.view()
                ]
                .map_err(|e| Error::Inference(e.to_string()))?,
            )
            .map_err(|e| Error::Inference(e.to_string()))?;

        let tensor = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = tensor.view().iter().copied().collect();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn download(models_dir: &Path, filename: &str) -> Result<std::path::PathBuf> {
    use hf_hub::api::sync::ApiBuilder;

    // Check local cache first (flat layout: models_dir/<basename>)
    let basename = std::path::Path::new(filename)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let local = models_dir.join(basename);
    if local.exists() {
        return Ok(local);
    }

    tracing::info!("Downloading {filename} from {MODEL_REPO}…");

    let api = ApiBuilder::new()
        .with_cache_dir(models_dir.to_path_buf())
        .build()
        .map_err(|e| Error::ModelDownload(e.to_string()))?;

    let path = api
        .model(MODEL_REPO.to_string())
        .get(filename)
        .map_err(|e| Error::ModelDownload(format!("{filename}: {e}")))?;

    Ok(path)
}

fn preprocess_image(path: &Path) -> Result<Vec<f32>> {
    let img = image::open(path)
        .map_err(|e| Error::Inference(format!("cannot open image: {e}")))?;

    let rgb = img
        .resize_exact(IMAGE_SIZE, IMAGE_SIZE, image::imageops::FilterType::Lanczos3)
        .into_rgb8();

    // Convert HWC → CHW layout, normalize pixel values to [-1.0, 1.0].
    // SigLIP 2 expects: (pixel / 255.0 - 0.5) / 0.5 per channel.
    let size = IMAGE_SIZE as usize;
    let mut chw = vec![0.0f32; 3 * size * size];
    for y in 0..size {
        for x in 0..size {
            let pixel = rgb.get_pixel(x as u32, y as u32);
            for c in 0..3usize {
                chw[c * size * size + y * size + x] =
                    (pixel.0[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    Ok(chw)
}

fn tokenize(tokenizer: &Tokenizer, text: &str) -> Result<(Vec<i64>, Vec<i64>)> {
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| Error::Tokenize(e.to_string()))?;

    let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
    let mut mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&x| x as i64)
        .collect();

    // Truncate then pad to exactly SEQ_LEN tokens.
    ids.truncate(SEQ_LEN);
    mask.truncate(SEQ_LEN);
    ids.resize(SEQ_LEN, PAD_TOKEN_ID);
    mask.resize(SEQ_LEN, 0);

    Ok((ids, mask))
}

fn l2_normalize(v: &mut Vec<f32>) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_unchanged() {
        let mut v = vec![1.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
        assert!(v[1].abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_scales_to_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_unchanged() {
        let mut v = vec![0.0f32; 768];
        l2_normalize(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn tokenize_pads_to_seq_len() {
        // Use a tokenizer stub: we can't load the real tokenizer without the model.
        // Test the padding logic directly with a simulated short sequence.
        let mut ids = vec![1i64, 2, 3];
        let mut mask = vec![1i64, 1, 1];
        ids.truncate(SEQ_LEN);
        mask.truncate(SEQ_LEN);
        ids.resize(SEQ_LEN, PAD_TOKEN_ID);
        mask.resize(SEQ_LEN, 0);
        assert_eq!(ids.len(), SEQ_LEN);
        assert_eq!(mask.len(), SEQ_LEN);
        assert_eq!(ids[63], PAD_TOKEN_ID);
        assert_eq!(mask[3], 0);
        assert_eq!(mask[0], 1);
    }

    #[test]
    fn tokenize_truncates_long_sequence() {
        let mut ids: Vec<i64> = (0..100).collect();
        let mut mask: Vec<i64> = vec![1; 100];
        ids.truncate(SEQ_LEN);
        mask.truncate(SEQ_LEN);
        ids.resize(SEQ_LEN, PAD_TOKEN_ID);
        mask.resize(SEQ_LEN, 0);
        assert_eq!(ids.len(), SEQ_LEN);
        assert_eq!(ids[63], 63); // truncated at index 63
    }

    #[test]
    #[ignore = "requires SigLIP 2 ONNX model — set EIDETIC_MODELS_CACHE and EIDETIC_TEST_IMAGE"]
    fn image_embedding_is_768_dim_and_normalized() {
        let models_dir = std::env::var("EIDETIC_MODELS_CACHE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap())
                    .join(".cache/eidetic/models")
            });

        let embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.dim(), 768);
        let dot: f32 = emb.as_slice().iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "not normalized: dot={dot}");
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p eidetic-ml
```

Expected: all non-ignored tests pass. The `image_embedding_is_768_dim_and_normalized` test is skipped (requires model file).

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ml/src/siglip.rs crates/eidetic-ml/src/lib.rs
git commit -m "feat(ml): add SiglipEmbedder with model download, image preprocessing, text encoding"
```

---

## Task 4: `eidetic-db` — `SearchResult` + `fetch_unembedded` + `store_embedding`

**Files:**
- Modify: `crates/eidetic-db/src/error.rs`
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-db/src/lib.rs`
- Modify: `crates/eidetic-db/tests/assets.rs`

Note: `migrations/002_embeddings.sql` already adds the `embedding vector(768)` column and VectorChord index. No new migration needed.

- [ ] **Step 1: Write failing integration tests**

Add these two tests to `crates/eidetic-db/tests/assets.rs`. The `start_db` helper and imports are already in that file.

```rust
use eidetic_db::{PgAssetsRepo, SearchResult};

#[tokio::test]
async fn fetch_unembedded_returns_only_null_embedding_images() {
    let (_container, url) = start_db().await;
    let config = Config { database_url: url, ..Default::default() };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    // Insert two image assets
    let asset_a = NewAsset {
        hash: "aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000".to_string(),
        original_filename: "a.jpg".to_string(),
        storage_path: PathBuf::from("/lib/aa/aa/a.jpg"),
        file_size: 1024,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None, latitude: None, longitude: None,
        camera_make: None, camera_model: None,
    };
    let asset_b = NewAsset {
        hash: "bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000".to_string(),
        original_filename: "b.jpg".to_string(),
        storage_path: PathBuf::from("/lib/bb/bb/b.jpg"),
        file_size: 2048,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None, latitude: None, longitude: None,
        camera_make: None, camera_model: None,
    };
    let outcome_a = repo.insert_asset(asset_a).await.expect("insert a");
    repo.insert_asset(asset_b).await.expect("insert b");

    let id_a = match outcome_a {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    // Embed asset_a — now only asset_b should be in the unembedded list
    repo.store_embedding(id_a, &[0.1f32; 768]).await.expect("store embedding");

    let unembedded = repo.fetch_unembedded().await.expect("fetch unembedded");
    assert_eq!(unembedded.len(), 1);
    let (_, path) = &unembedded[0];
    assert!(path.to_str().unwrap().contains("b.jpg"));
}

#[tokio::test]
async fn store_embedding_persists_float_values() {
    let (_container, url) = start_db().await;
    let config = Config { database_url: url, ..Default::default() };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    let asset = NewAsset {
        hash: "cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000".to_string(),
        original_filename: "c.jpg".to_string(),
        storage_path: PathBuf::from("/lib/cc/cc/c.jpg"),
        file_size: 512,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None, latitude: None, longitude: None,
        camera_make: None, camera_model: None,
    };
    let outcome = repo.insert_asset(asset).await.expect("insert");
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("expected Inserted"),
    };

    let embedding: Vec<f32> = (0..768).map(|i| i as f32 / 768.0).collect();
    repo.store_embedding(id, &embedding).await.expect("store");

    // Verify via raw SQL that the embedding column is non-null
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM assets WHERE id = $1 AND embedding IS NOT NULL"
    )
    .bind(id.as_uuid())
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 1);
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p eidetic-db -- fetch_unembedded store_embedding 2>&1 | head -20
```

Expected: compile errors — `fetch_unembedded`, `store_embedding`, `SearchResult` don't exist yet.

- [ ] **Step 3: Add `Query` error variant to `crates/eidetic-db/src/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to connect to database: {0}")]
    Connect(#[source] sqlx::Error),

    #[error("failed to run migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),

    #[error("database query failed: {0}")]
    Query(#[source] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 4: Add `SearchResult` struct and two new methods to `crates/eidetic-db/src/assets.rs`**

Add after the existing `use` block and before the `PgAssetsRepo` struct:

```rust
use chrono::{DateTime, Utc};
use crate::Error;
use std::path::PathBuf;

/// One row returned by a vector similarity search.
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

Then add a new `impl PgAssetsRepo` block (separate from the `AssetIndex` impl) with the two methods:

```rust
impl PgAssetsRepo {
    /// Return `(id, storage_path)` for every image asset with no embedding yet.
    /// Videos (`mime_type NOT LIKE 'image/%'`) are excluded — SigLIP 2 is image-only.
    pub async fn fetch_unembedded(&self) -> crate::Result<Vec<(AssetId, PathBuf)>> {
        let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT id, storage_path FROM assets \
             WHERE embedding IS NULL AND mime_type LIKE 'image/%'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(uuid, path)| (AssetId::from(uuid), PathBuf::from(path)))
            .collect())
    }

    /// Persist a 768-dim float32 embedding for an asset.
    pub async fn store_embedding(&self, id: AssetId, embedding: &[f32]) -> crate::Result<()> {
        let vec = pgvector::Vector::from(embedding.to_vec());
        sqlx::query("UPDATE assets SET embedding = $1 WHERE id = $2")
            .bind(vec)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(Error::Query)?;
        Ok(())
    }
}
```

Add `use pgvector;` to the top of `assets.rs`.

- [ ] **Step 5: Export from `crates/eidetic-db/src/lib.rs`**

Add to lib.rs:

```rust
pub use assets::{PgAssetsRepo, SearchResult};
```

(Replace the existing `pub use assets::PgAssetsRepo;` line.)

- [ ] **Step 6: Run failing tests and confirm they pass**

```bash
cargo test -p eidetic-db -- fetch_unembedded store_embedding
```

Expected: both tests pass (requires Docker for the Postgres container).

- [ ] **Step 7: Commit**

```bash
git add crates/eidetic-db/src/error.rs crates/eidetic-db/src/assets.rs \
        crates/eidetic-db/src/lib.rs crates/eidetic-db/tests/assets.rs
git commit -m "feat(db): add SearchResult, fetch_unembedded, store_embedding"
```

---

## Task 5: `eidetic-db` — `search_similar`

**Files:**
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-db/tests/assets.rs`

- [ ] **Step 1: Write the failing integration test**

Add to `crates/eidetic-db/tests/assets.rs`:

```rust
#[tokio::test]
async fn search_similar_orders_by_cosine_similarity() {
    let (_container, url) = start_db().await;
    let config = Config { database_url: url, ..Default::default() };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool.clone());

    // Asset A: embedding aligned with e1 = [1, 0, 0, ..., 0]
    let mut emb_a = vec![0.0f32; 768];
    emb_a[0] = 1.0;

    // Asset B: embedding aligned with e2 = [0, 1, 0, ..., 0]
    let mut emb_b = vec![0.0f32; 768];
    emb_b[1] = 1.0;

    let asset_a = NewAsset {
        hash: "dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000".to_string(),
        original_filename: "d.jpg".to_string(),
        storage_path: PathBuf::from("/lib/dd/dd/d.jpg"),
        file_size: 1, mime_type: Some("image/jpeg".to_string()),
        date_taken: None, latitude: None, longitude: None,
        camera_make: None, camera_model: None,
    };
    let asset_b = NewAsset {
        hash: "eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000".to_string(),
        original_filename: "e.jpg".to_string(),
        storage_path: PathBuf::from("/lib/ee/ee/e.jpg"),
        file_size: 1, mime_type: Some("image/jpeg".to_string()),
        date_taken: None, latitude: None, longitude: None,
        camera_make: None, camera_model: None,
    };

    let id_a = match repo.insert_asset(asset_a).await.expect("insert a") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!(),
    };
    let id_b = match repo.insert_asset(asset_b).await.expect("insert b") {
        InsertOutcome::Inserted(id) => id,
        _ => panic!(),
    };

    repo.store_embedding(id_a, &emb_a).await.expect("embed a");
    repo.store_embedding(id_b, &emb_b).await.expect("embed b");

    // Query with e1 — asset A should come first (score ≈ 1.0), B second (score ≈ 0.0)
    let query = emb_a.clone();
    let results = repo.search_similar(&query, 10).await.expect("search");

    assert_eq!(results.len(), 2);
    assert!(results[0].score > 0.99, "first result score should be ~1.0, got {}", results[0].score);
    assert!(results[1].score < 0.01, "second result score should be ~0.0, got {}", results[1].score);
    assert!(results[0].storage_path.to_str().unwrap().contains("d.jpg"));
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test -p eidetic-db -- search_similar 2>&1 | head -20
```

Expected: compile error — `search_similar` doesn't exist yet.

- [ ] **Step 3: Implement `search_similar`**

Add to the `impl PgAssetsRepo` block in `assets.rs`:

```rust
    /// Return the top `limit` assets ordered by cosine similarity to `query_vec`.
    /// Assets with no embedding are excluded. Score is in [0.0, 1.0] — higher = better match.
    pub async fn search_similar(
        &self,
        query_vec: &[f32],
        limit: u32,
    ) -> crate::Result<Vec<SearchResult>> {
        #[derive(sqlx::FromRow)]
        struct SearchRow {
            id: uuid::Uuid,
            storage_path: String,
            score: f32,
            mime_type: Option<String>,
            date_taken: Option<DateTime<Utc>>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<SearchRow> = sqlx::query_as(
            "SELECT id, storage_path, mime_type, date_taken, \
                    camera_make, camera_model, latitude, longitude, \
                    (1.0 - (embedding <=> $1))::real AS score \
             FROM assets \
             WHERE embedding IS NOT NULL \
             ORDER BY embedding <=> $1 \
             LIMIT $2",
        )
        .bind(vec)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|r| SearchResult {
                id: AssetId::from(r.id),
                storage_path: PathBuf::from(r.storage_path),
                score: r.score,
                mime_type: r.mime_type,
                date_taken: r.date_taken,
                camera_make: r.camera_make,
                camera_model: r.camera_model,
                latitude: r.latitude,
                longitude: r.longitude,
            })
            .collect())
    }
```

- [ ] **Step 4: Run the test and confirm it passes**

```bash
cargo test -p eidetic-db -- search_similar
```

Expected: PASS.

- [ ] **Step 5: Run all DB tests**

```bash
cargo test -p eidetic-db
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/eidetic-db/src/assets.rs crates/eidetic-db/tests/assets.rs
git commit -m "feat(db): add search_similar — cosine ANN via VectorChord"
```

---

## Task 6: `eidetic embed` command

**Files:**
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Add the `Embed` variant and its handler to `main.rs`**

Add `Embed` to the `Command` enum:

```rust
/// Generate embeddings for all imported images that don't have one yet.
Embed,
```

Add the handler in the `match cli.command` block. The full embed handler:

```rust
Command::Embed => {
    let config = Config::from_env().context("failed to load config")?;

    let models_dir = config.paths.models_cache.clone();
    println!("Loading model (downloads ~350 MB on first run)…");
    let embedder = std::sync::Arc::new(
        tokio::task::spawn_blocking(move || eidetic_ml::SiglipEmbedder::load(&models_dir))
            .await?
            .context("failed to load SigLIP 2 model — check your internet connection")?,
    );

    let pool = eidetic_db::connect(&config)
        .await
        .context("failed to connect to database")?;
    let repo = eidetic_db::PgAssetsRepo::new(pool);

    let unembedded = repo
        .fetch_unembedded()
        .await
        .context("failed to fetch unembedded assets")?;

    if unembedded.is_empty() {
        println!("Nothing to do.");
        return Ok(());
    }

    let total = unembedded.len();
    println!("Found {total} images to embed");

    let mut embedded = 0u32;
    let mut skipped = 0u32;

    for (i, (id, path)) in unembedded.into_iter().enumerate() {
        let embedder = std::sync::Arc::clone(&embedder);
        let path_clone = path.clone();

        let result =
            tokio::task::spawn_blocking(move || embedder.embed(&path_clone)).await?;

        match result {
            Ok(emb) => {
                repo.store_embedding(id, emb.as_slice())
                    .await
                    .context("failed to store embedding")?;
                println!("[{}/{}] {}", i + 1, total, path.display());
                embedded += 1;
            }
            Err(e) => {
                eprintln!("  skipped {}: {e}", path.display());
                skipped += 1;
            }
        }
    }

    println!("Done. Embedded {embedded}, skipped {skipped}.");
}
```

Add `use std::sync::Arc;` if not already present, and add `use eidetic_ml` import.

- [ ] **Step 2: Build to verify it compiles**

```bash
cargo build -p eidetic-cli
```

Expected: compiles without errors.

- [ ] **Step 3: Smoke-test against a real library (optional, requires running Postgres)**

```bash
cargo run -p eidetic-cli -- embed
```

Expected: if no images are imported yet, prints `"Nothing to do."`. If images exist, starts embedding.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-cli/src/main.rs
git commit -m "feat(cli): add 'eidetic embed' command"
```

---

## Task 7: `eidetic search` command

**Files:**
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Add the `Search` variant to the `Command` enum**

```rust
/// Search the library by natural-language description.
Search {
    /// The text query, e.g. "dog on beach".
    query: String,

    /// Maximum number of results to return.
    #[arg(long, default_value = "10")]
    limit: u32,

    /// Comma-separated fields to include in output.
    /// Valid: path, score, date, make, model, lat, lon, mime.
    /// Defaults to path-only when omitted.
    #[arg(long, value_delimiter = ',')]
    fields: Option<Vec<String>>,

    /// Output results as a JSON array with all fields.
    #[arg(long)]
    json: bool,
},
```

- [ ] **Step 2: Add the field formatter helper**

Add this function outside `main`, before or after the existing helpers:

```rust
const VALID_FIELDS: &[&str] = &["path", "score", "date", "make", "model", "lat", "lon", "mime"];

fn format_result(r: &eidetic_db::SearchResult, fields: &[String]) -> anyhow::Result<String> {
    let mut parts = Vec::new();
    for field in fields {
        let value = match field.as_str() {
            "path" => r.storage_path.display().to_string(),
            "score" => format!("{:.3}", r.score),
            "date" => r
                .date_taken
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            "make" => r.camera_make.clone().unwrap_or_default(),
            "model" => r.camera_model.clone().unwrap_or_default(),
            "lat" => r.latitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "lon" => r.longitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "mime" => r.mime_type.clone().unwrap_or_default(),
            other => anyhow::bail!(
                "Unknown field: '{other}'. Valid fields: {}",
                VALID_FIELDS.join(", ")
            ),
        };
        parts.push(value);
    }
    Ok(parts.join("\t"))
}
```

- [ ] **Step 3: Add the `Search` handler in `main`**

```rust
Command::Search { query, limit, fields, json } => {
    // Validate fields early so we don't download the model for a bad flag.
    if let Some(ref f) = fields {
        for field in f {
            if !VALID_FIELDS.contains(&field.as_str()) {
                anyhow::bail!(
                    "Unknown field: '{field}'. Valid fields: {}",
                    VALID_FIELDS.join(", ")
                );
            }
        }
    }

    let config = Config::from_env().context("failed to load config")?;

    let models_dir = config.paths.models_cache.clone();
    let embedder =
        tokio::task::spawn_blocking(move || eidetic_ml::SiglipEmbedder::load(&models_dir))
            .await?
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to load model: {e}. Run 'eidetic embed' first to download it."
                )
            })?;

    let query_clone = query.clone();
    let query_emb = tokio::task::spawn_blocking(move || embedder.embed_text(&query_clone))
        .await?
        .context("text embedding failed")?;

    let pool = eidetic_db::connect(&config)
        .await
        .context("failed to connect to database")?;
    let repo = eidetic_db::PgAssetsRepo::new(pool);

    let results = repo
        .search_similar(query_emb.as_slice(), limit)
        .await
        .context("search failed")?;

    if results.is_empty() {
        println!("No results.");
        return Ok(());
    }

    if json {
        let arr: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.storage_path.to_string_lossy(),
                    "score": r.score,
                    "date": r.date_taken.map(|d| d.to_rfc3339()),
                    "make": r.camera_make,
                    "model": r.camera_model,
                    "lat": r.latitude,
                    "lon": r.longitude,
                    "mime": r.mime_type,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if let Some(ref field_list) = fields {
        for r in &results {
            println!("{}", format_result(r, field_list)?);
        }
    } else {
        for r in &results {
            println!("{}", r.storage_path.display());
        }
    }
}
```

Add `use serde_json` at the top of the file.

- [ ] **Step 4: Build to verify it compiles**

```bash
cargo build -p eidetic-cli
```

Expected: compiles without errors.

- [ ] **Step 5: Run the full test suite**

```bash
cargo test --workspace
```

Expected: all tests pass (DB integration tests require Docker).

- [ ] **Step 6: Commit**

```bash
git add crates/eidetic-cli/src/main.rs
git commit -m "feat(cli): add 'eidetic search' command with --limit, --fields, --json"
```
