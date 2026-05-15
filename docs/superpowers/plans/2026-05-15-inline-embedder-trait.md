# Inline `Embedder` Trait Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `Embedder` trait and `Embedding(Vec<f32>)` newtype from `eidetic-ml`. `SiglipEmbedder` gains inherent `&mut self` methods that return `Vec<f32>` directly, and the two `Mutex<Session>` wrappers disappear. The CLI's `Embed` command switches from `Arc<SiglipEmbedder>` + per-job `spawn_blocking` to a single `tokio::sync::mpsc` worker thread that owns the embedder for the duration of the command.

**Architecture:**
- `eidetic-ml`: trait gone, newtype gone, `MockEmbedder` test double gone (it only tested itself). `SiglipEmbedder` becomes a concrete struct with `embed(&mut self, &Path) -> Result<Vec<f32>>` and `embed_text(&mut self, &str) -> Result<Vec<f32>>` inherent methods. `vision_session` and `text_session` become plain `Session` fields, not `Mutex<Session>`.
- `eidetic-cli`:
  - `Search` command: collapse the two `spawn_blocking`s (load, then call) into one closure that loads and immediately calls — the embedder is owned-mutable inside the closure and discarded after the single text embed.
  - `Embed` command: introduce a worker thread that takes `(PathBuf, oneshot::Sender<Result<Vec<f32>>>)` jobs over an mpsc channel. The async loop sends a job per asset, awaits its oneshot reply, then awaits the DB write. When the loop ends, `job_tx` is dropped → worker's `blocking_recv()` returns `None` → worker exits cleanly. This pattern is the idiomatic Rust bridge between async (Tokio) and a `&mut self` resource living on a blocking thread, and aligns with `AGENTS.md`'s "tokio::sync::mpsc for async pipelines" rule.
  - `eval.rs`: trivial — `let mut embedder = SiglipEmbedder::load(...)?;` and call methods on the local mutable binding.
- `AGENTS.md`: workspace map drops the "Embedder trait" mention.

**Tech Stack:** Rust 2024 edition workspace, ort 2, tokio (mpsc + oneshot + spawn_blocking), thiserror. No new dependencies.

**Out of scope:**
- Any batching of embed calls (the audit lists batched SigLIP inference + parallel `import_dir` as separate work).
- Touching face-detection / SCRFD / ArcFace plumbing.
- Touching `eidetic-search` (doesn't exist yet — search lives inline in the CLI).
- Anything in PR #15 (`refactor/delete-asset-index-trait`); this branch must rebase or land independently. Plan assumes branching from `main` and that `main` and the work here remain conflict-free (touched files don't overlap with PR #15).

---

## File map

**Modify:**
- `crates/eidetic-ml/src/lib.rs` — drop `mod embedder` and the `Embedder`/`Embedding` re-exports.
- `crates/eidetic-ml/src/siglip.rs` — strip `impl Embedder`; convert methods to inherent `&mut self`; drop `Mutex` wrapping; drop the `use crate::{Embedder, Embedding}` import; drop `use std::sync::Mutex`; return `Vec<f32>` directly.
- `crates/eidetic-cli/src/main.rs` — refactor `Search` and `Embed` command bodies (see Tasks 1.6 and 1.7).
- `crates/eidetic-cli/src/eval.rs` — make `embedder` mutable; the `.as_slice()` call on what was `Embedding` becomes a borrow on the returned `Vec<f32>`.
- `AGENTS.md` — workspace map row for `eidetic-ml`.

**Delete:**
- `crates/eidetic-ml/src/embedder.rs` (the file: trait + `Embedding` newtype + `MockEmbedder` + 4 tests).

---

## Task 1: Atomic refactor — drop trait/newtype, rewire CLI

All seven sub-steps land in one commit. The workspace pre-commit hook runs `cargo clippy --workspace --all-targets -- -D warnings`; an intermediate commit that leaves `eidetic-cli` broken would be rejected, so we keep everything together. Do **not** use `--no-verify`.

**Files (full list, same as the "Modify" + "Delete" sections above).**

- [ ] **Step 1: Create a working branch off main**

```bash
git checkout main
git pull --ff-only
git checkout -b refactor/inline-embedder-trait
```

If `main` is currently behind `origin/main` after PR #14 or PR #15 merges, that's fine — pull will fast-forward.

- [ ] **Step 2: Delete `crates/eidetic-ml/src/embedder.rs`**

```bash
rm crates/eidetic-ml/src/embedder.rs
```

The file holds the `Embedder` trait, the `Embedding` newtype, `MockEmbedder`, and four tests that exist only to exercise the mock. Nothing in production calls `MockEmbedder`. No other source file in the workspace defines an `impl Embedder`.

- [ ] **Step 3: Update `crates/eidetic-ml/src/lib.rs`**

Current contents:

```rust
//! ML inference for Eidetic.

mod embedder;
mod error;
mod siglip;

pub use embedder::{Embedder, Embedding};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

Replace with:

```rust
//! ML inference for Eidetic.

mod error;
mod siglip;

pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

- [ ] **Step 4: Rewrite `crates/eidetic-ml/src/siglip.rs`**

Three surgical changes:

**4a. Imports at the top.** Replace lines 1–6:

```rust
use crate::{Embedder, Embedding, Error, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::Tokenizer;
```

with:

```rust
use crate::{Error, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use tokenizers::Tokenizer;
```

(`Mutex` import is dropped; `Embedder`/`Embedding` imports are dropped.)

**4b. Struct fields.** Replace the `SiglipEmbedder` struct definition (currently lines 89–96):

```rust
pub struct SiglipEmbedder {
    // Session::run takes &mut self, so we use Mutex for interior mutability
    // to satisfy the &self required by the Embedder trait.
    vision_session: Mutex<Session>,
    text_session: Mutex<Session>,
    tokenizer: Tokenizer,
    variant: &'static ModelVariant,
}
```

with:

```rust
pub struct SiglipEmbedder {
    vision_session: Session,
    text_session: Session,
    tokenizer: Tokenizer,
    variant: &'static ModelVariant,
}
```

(Both `Mutex` wrappers gone; the explanatory comment is no longer relevant.)

**4c. Constructor body.** Inside `SiglipEmbedder::load`, replace:

```rust
        Ok(Self {
            vision_session: Mutex::new(vision_session),
            text_session: Mutex::new(text_session),
            tokenizer,
            variant,
        })
```

with:

```rust
        Ok(Self {
            vision_session,
            text_session,
            tokenizer,
            variant,
        })
```

**4d. Replace the `impl Embedder for SiglipEmbedder` block.** Currently lines 136–190:

```rust
impl Embedder for SiglipEmbedder {
    fn dim(&self) -> usize {
        self.variant.embed_dim
    }

    fn embed(&self, path: &Path) -> Result<Embedding> {
        let image_size = self.variant.image_size;
        let pixels = preprocess_image(path, image_size)?;
        let shape = [1usize, 3, image_size as usize, image_size as usize];
        let tensor = Tensor::<f32>::from_array((shape, pixels))
            .map_err(|e| Error::Inference(format!("create tensor: {e}")))?;

        let mut session = self
            .vision_session
            .lock()
            .map_err(|e| Error::Inference(format!("lock vision session: {e}")))?;

        let outputs = session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }

    fn embed_text(&self, text: &str) -> Result<Embedding> {
        let ids = tokenize(&self.tokenizer, text)?;

        let seq_shape = [1usize, SEQ_LEN];
        let ids_tensor = Tensor::<i64>::from_array((seq_shape, ids))
            .map_err(|e| Error::Inference(format!("create ids tensor: {e}")))?;

        let mut session = self
            .text_session
            .lock()
            .map_err(|e| Error::Inference(format!("lock text session: {e}")))?;

        let outputs = session
            .run(ort::inputs!["input_ids" => ids_tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }
}
```

with inherent methods on `SiglipEmbedder`. These go *inside* the existing `impl SiglipEmbedder { ... }` block (immediately after `load`, before the closing `}`):

```rust
    /// Output dimension. Stable for the lifetime of an embedder instance.
    pub fn dim(&self) -> usize {
        self.variant.embed_dim
    }

    /// Compute an L2-normalised embedding for the image at `path`.
    ///
    /// Synchronous; inference is CPU/GPU-bound. Callers running inside a
    /// Tokio runtime should invoke this on a blocking thread (see the
    /// `embed` command in `eidetic-cli` for the mpsc-worker pattern).
    pub fn embed(&mut self, path: &Path) -> Result<Vec<f32>> {
        let image_size = self.variant.image_size;
        let pixels = preprocess_image(path, image_size)?;
        let shape = [1usize, 3, image_size as usize, image_size as usize];
        let tensor = Tensor::<f32>::from_array((shape, pixels))
            .map_err(|e| Error::Inference(format!("create tensor: {e}")))?;

        let outputs = self
            .vision_session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(vec)
    }

    /// Compute an L2-normalised embedding for a text string.
    pub fn embed_text(&mut self, text: &str) -> Result<Vec<f32>> {
        let ids = tokenize(&self.tokenizer, text)?;

        let seq_shape = [1usize, SEQ_LEN];
        let ids_tensor = Tensor::<i64>::from_array((seq_shape, ids))
            .map_err(|e| Error::Inference(format!("create ids tensor: {e}")))?;

        let outputs = self
            .text_session
            .run(ort::inputs!["input_ids" => ids_tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(vec)
    }
```

Behavior changes from the deleted impl:
1. `&self` → `&mut self`.
2. Returns `Vec<f32>` instead of `Embedding(Vec<f32>)`.
3. No `.lock()` / `Mutex` poisoning paths. The session field is accessed directly through `&mut self`.

The doc comments for `embed` and `embed_text` are new; they replace the trait-level docs that disappeared with the trait.

- [ ] **Step 5: Update `crates/eidetic-cli/src/eval.rs`**

Three lines change. Line 10 currently imports both trait and struct:

```rust
use eidetic_ml::{Embedder, SiglipEmbedder};
```

Change to:

```rust
use eidetic_ml::SiglipEmbedder;
```

Line 35 declares `embedder` immutable:

```rust
    let embedder = SiglipEmbedder::load(&config.paths.models_cache)
        .context("failed to load SigLIP 2 model")?;
```

Change to:

```rust
    let mut embedder = SiglipEmbedder::load(&config.paths.models_cache)
        .context("failed to load SigLIP 2 model")?;
```

Line 46 wraps the return value in `.as_slice().to_vec()` because the trait returned `Embedding`:

```rust
        img_embeddings.push(emb.as_slice().to_vec());
```

Change to:

```rust
        img_embeddings.push(emb);
```

`emb` is now `Vec<f32>` directly, which is exactly what `img_embeddings: Vec<Vec<f32>>` expects.

Line 62 likewise unwraps via `.as_slice()`:

```rust
            let q = q_emb.as_slice();
```

Change to:

```rust
            let q = q_emb.as_slice();
```

Wait — that still works. `Vec<f32>::as_slice()` returns `&[f32]`. So this line **does not change**. (Stating the non-change explicitly so the implementer doesn't second-guess and "fix" it.) The same applies to any other `.as_slice()` call on what used to be `Embedding` — they're now calling the same method on `Vec<f32>`, which has identical behavior.

- [ ] **Step 6: Refactor the CLI `Search` command body**

In `crates/eidetic-cli/src/main.rs`, find the `Command::Search` arm (starts around line 298). The current body for the embedder-loading section (around lines 316–329) is:

```rust
            let config = Config::from_env();

            let models_dir = config.paths.models_cache.clone();
            let embedder =
                tokio::task::spawn_blocking(move || eidetic_ml::SiglipEmbedder::load(&models_dir))
                    .await
                    .context("embedder thread panicked")?
                    .context("failed to load SigLIP 2 model — check your internet connection")?;

            let query_clone = query.clone();
            let query_emb = tokio::task::spawn_blocking(move || embedder.embed_text(&query_clone))
                .await
                .context("embedder thread panicked")?
                .context("text embedding failed")?;
```

Replace those 14 lines with one consolidated blocking call:

```rust
            let config = Config::from_env();

            let models_dir = config.paths.models_cache.clone();
            let query_clone = query.clone();
            let query_emb = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<f32>> {
                let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                embedder.embed_text(&query_clone)
            })
            .await
            .context("embedder thread panicked")?
            .context("text embedding failed — check your internet connection and that the SigLIP 2 model is downloaded")?;
```

The two spawn_blocking hops collapse into one, the `move ||` closure now owns the embedder mutably, and the error message folds together the previously-separated load + embed failures. The `query_emb` binding's type is now `Vec<f32>` (was `Embedding`). The next line in the existing body uses it as `query_emb.as_slice()` to pass to `repo.search_similar` — that line is unchanged because `Vec<f32>::as_slice()` works identically.

- [ ] **Step 7: Refactor the CLI `Embed` command body**

This is the biggest behavioral change. The current `Embed` command body (around lines 222–296) loads the embedder once into `Arc<SiglipEmbedder>` and dispatches a `spawn_blocking` per asset. With `&mut self` methods, `Arc` no longer works — we replace it with a mpsc worker.

**7a. Add imports near the top of `crates/eidetic-cli/src/main.rs`.** Whatever import block currently exists, ensure these two are present (add if missing):

```rust
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
```

(Verify via grep before adding; `std::path::PathBuf` may already be imported, in which case leave it. `tokio::sync::{mpsc, oneshot}` is unlikely to be there.)

**7b. Replace the entire `Command::Embed` arm body.** Find it; the arm starts with `Command::Embed => {` and runs ~75 lines. Replace its contents (everything between the outer `{ ... }` of the arm) with:

```rust
            let config = Config::from_env();

            // Load the embedder on a blocking thread. We hand it to a worker
            // task immediately afterwards, so the binding is local to that scope.
            let models_dir = config.paths.models_cache.clone();
            println!("Loading model (downloads ~1.4 GiB on first run)…");

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

            // Channel of (path, reply) jobs sent to the worker thread. The
            // worker owns the embedder; the async loop sends paths and awaits
            // results via per-job oneshot replies. Channel capacity 1 keeps
            // the worker tightly coupled to the async loop — no large queue
            // of pending embeds builds up if the DB write side stalls.
            type EmbedJob = (PathBuf, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);
            let (job_tx, mut job_rx) = mpsc::channel::<EmbedJob>(1);

            let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                while let Some((path, reply)) = job_rx.blocking_recv() {
                    let result = embedder.embed(&path);
                    // If the receiver was dropped (e.g. caller gave up), keep
                    // serving the next job rather than aborting the worker.
                    let _ = reply.send(result);
                }
                Ok(())
            });

            let total = unembedded.len();
            println!("Found {total} images to embed");

            let mut embedded = 0u32;
            let mut skipped = 0u32;
            let mut failed = 0u32;

            for (i, (id, path)) in unembedded.into_iter().enumerate() {
                let (reply_tx, reply_rx) = oneshot::channel();
                // If `send` fails, the worker died — surface its error below.
                if job_tx.send((path.clone(), reply_tx)).await.is_err() {
                    break;
                }
                let result = reply_rx
                    .await
                    .context("embed worker dropped reply channel")?;

                match result {
                    Ok(emb) => match repo.store_embedding(id, &emb).await {
                        Ok(()) => {
                            println!("[{}/{}] {}", i + 1, total, path.display());
                            embedded += 1;
                        }
                        Err(e) => {
                            eprintln!("  failed to store {}: {e}", path.display());
                            failed += 1;
                        }
                    },
                    Err(e) => {
                        eprintln!("  skipped {}: {e}", path.display());
                        skipped += 1;
                    }
                }
            }

            // Drop the sender so the worker's `blocking_recv` returns `None`
            // and the worker exits. Then await the worker's result so a load
            // or panic during embed surfaces here instead of being lost.
            drop(job_tx);
            worker
                .await
                .context("embed worker thread panicked")?
                .context("failed to load SigLIP 2 model — check your internet connection")?;

            if failed > 0 {
                println!(
                    "Done. Embedded {embedded}, skipped {skipped}, failed {failed}. \
                     Re-run `eidetic embed` to retry the failed rows."
                );
                std::process::exit(1);
            } else {
                println!("Done. Embedded {embedded}, skipped {skipped}.");
            }
```

Behavior preserved (verify after the change):
- Loading prompt printed before the work starts.
- `Nothing to do.` short-circuit when there's nothing unembedded.
- Per-asset progress line `[i/total] path`.
- Skip vs. fail accounting (embed error → `skipped`; store error → `failed`).
- Non-zero exit when `failed > 0`.
- Successful exit otherwise prints `Done. Embedded N, skipped M.`.

Behavior changed (intentional):
- The model now loads inside the worker thread (after the "Loading model…" print). If load fails, the error surfaces at the `worker.await` join point, which happens at the very end of the command. That's later than the old code (which surfaced load failure before any DB work). To avoid that regression, the body above prints "Loading model…", then immediately starts the worker; if there are no unembedded rows, the `return Ok(());` path drops `job_tx` before the worker ever pulls a job, so a load that would have failed is harmlessly cancelled — same outcome as the old code which never loaded in that case either.
- One small ordering shift: DB connect now happens before the worker spawns, so an invalid `EIDETIC_DATABASE_URL` is caught before the model download even begins. That's a net positive for personal use; keep the change.

- [ ] **Step 8: Verify the workspace is green**

Run: `cargo fmt --all`
Expected: clean or minor reformatting.

Run: `cargo build --workspace --all-targets`
Expected: success.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. If clippy flags `let _ = reply.send(result);` as "unused result," allow it inline — but it's the documented pattern for "fire and forget if the receiver dropped," and clippy generally accepts `let _ =` for `oneshot::Sender::send` because the `Sender`'s `Result` payload is a sentinel.

Run: `cargo test --workspace`
Expected: all tests pass. The 4 `MockEmbedder` tests are gone, but the 8 `siglip.rs` tests (l2_normalize × 4, preprocess_image, parse_accelerator × 4) all stay green — none depended on the trait or newtype. Note: the `#[ignore]`d integration test `image_embedding_is_768_dim_and_normalized` previously called `embedder.embed(&path)` which returned `Embedding`. After the change it returns `Vec<f32>`. Update it as part of this step:

In `crates/eidetic-ml/src/siglip.rs`, the test currently reads (around lines 466–484):

```rust
        let embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.dim(), 768);
        let dot: f32 = emb.as_slice().iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "not normalized: dot={dot}");
```

Change `let embedder` to `let mut embedder` and adjust the post-embed assertions to use `Vec<f32>` directly:

```rust
        let mut embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.len(), 768);
        let dot: f32 = emb.iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "not normalized: dot={dot}");
```

(`.dim()` → `.len()`, `.as_slice().iter()` → `.iter()`. Both are direct `Vec<f32>` API equivalents.)

The test is `#[ignore]`d so it won't run in normal CI, but it must still compile.

- [ ] **Step 9: Commit**

```bash
git add crates/eidetic-ml/src/lib.rs \
        crates/eidetic-ml/src/siglip.rs \
        crates/eidetic-cli/src/eval.rs \
        crates/eidetic-cli/src/main.rs
git rm crates/eidetic-ml/src/embedder.rs
git commit -m "refactor: inline Embedder trait, drop Mutex<Session> ceremony

One prod impl, one test mock. The trait forced &self on inherently
&mut Session methods, which forced two Mutex<Session> wrappers on
SiglipEmbedder. Now methods take &mut self and the sessions are bare
fields. The Embedding(Vec<f32>) newtype goes too — every caller was
already only using as_slice().

The CLI's Embed command can no longer share an Arc<SiglipEmbedder>
across many spawn_blocking calls, since &mut self isn't compatible
with Arc-style sharing. Replaced with the canonical mpsc-worker
pattern: a single spawn_blocking owns the embedder for the duration
of the command and serves jobs over a tokio::sync::mpsc channel,
with per-job oneshot replies. This matches AGENTS.md's
'tokio::sync::mpsc for async pipelines' rule and is the natural
shape for any future ONNX session (face detection, etc.) that wants
the same single-owner, async-callers shape.

The Search command's two-hop spawn_blocking collapses to one since
we no longer need to hold the embedder between load and call."
```

Pre-commit hook will run fmt + clippy. Fix anything it asks for and re-commit; do not use `--no-verify`.

---

## Task 2: Update `AGENTS.md` workspace map

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update the `eidetic-ml` row**

In the workspace-map table in `AGENTS.md`, the current row reads:

```
| `eidetic-ml` | `Embedder` trait + impls (SigLIP 2 via `ort`). Mock impl for testing. | `eidetic-core` |
```

Replace with:

```
| `eidetic-ml` | `SiglipEmbedder` (concrete, no trait) loads ONNX models via `ort` and produces L2-normalised image/text embeddings as `Vec<f32>`. | `eidetic-core` |
```

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): update workspace map after Embedder trait deletion"
```

---

## Task 3: Verify and open PR

- [ ] **Step 1: Final fmt / clippy / test**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If `cargo test --workspace` requires Docker (testcontainers in `eidetic-db/tests/`), and Docker isn't available locally, run `cargo test --workspace --exclude eidetic-db` instead and note in the PR body that CI runs the Docker tests.

- [ ] **Step 2: Push and open PR**

```bash
git push -u origin refactor/inline-embedder-trait
gh pr create --title "refactor: inline Embedder trait, drop Mutex<Session> ceremony" --body "$(cat <<'EOF'
## Summary

- Delete the \`Embedder\` trait and the \`Embedding(Vec<f32>)\` newtype from \`eidetic-ml\`. One prod impl, one test mock — the trait earned nothing but a pair of \`Mutex<Session>\` wrappers forced by its \`&self\` shape.
- \`SiglipEmbedder\` now exposes inherent \`pub fn embed(&mut self, &Path) -> Result<Vec<f32>>\` and \`pub fn embed_text(&mut self, &str) -> Result<Vec<f32>>\` directly. \`vision_session\` and \`text_session\` become bare \`Session\` fields.
- CLI \`Embed\` switches from \`Arc<SiglipEmbedder>\` + per-asset \`spawn_blocking\` to a \`tokio::sync::mpsc\` worker pattern: a single blocking thread owns the embedder and serves \`(PathBuf, oneshot::Sender<...>)\` jobs sent by the async loop. This matches \`AGENTS.md\`'s "mpsc for async pipelines" rule and is the natural shape for future ONNX-backed pipelines (face detection, etc.).
- CLI \`Search\` collapses two \`spawn_blocking\`s into one — the embedder is owned mutably inside the load-and-call closure and discarded after.
- \`eval.rs\` becomes a tiny \`mut embedder\` change; \`emb.as_slice().to_vec()\` → \`emb\` (already \`Vec<f32>\`).

Per the 2026-05-09 audit's [eidetic-ml/embedder.rs] finding. Plan at \`docs/superpowers/plans/2026-05-15-inline-embedder-trait.md\`.

## Behavioural changes worth flagging

1. **Model load timing in \`Embed\`.** Before: load happened on a \`spawn_blocking\` *before* DB connect, so load failures surfaced first. After: DB connect runs first; if the library is empty (\`fetch_unembedded\` returns nothing), the worker is never told to load — small efficiency win for the no-op case. If the library has unembedded rows, load happens inside the worker; failures surface at the worker-join point at the end of the command.
2. **Error message for load failure** now mentions both the network and the model cache as suspects (the old message only mentioned the network).
3. \`Search\` command's combined error message for load+embed failure now reads: "text embedding failed — check your internet connection and that the SigLIP 2 model is downloaded." Previously the load and embed errors had separate strings.

No public CLI surface change. No DB schema change. No new dependencies.

## Test plan
- [x] \`cargo fmt --all -- --check\`
- [x] \`cargo clippy --workspace --all-targets -- -D warnings\`
- [x] \`cargo test --workspace --exclude eidetic-db\` (Docker tests gated on CI)
- [ ] Manual smoke: \`eidetic embed\` on a small library (CI doesn't cover this; verify locally before merge)
- [ ] Manual smoke: \`eidetic search "dog"\` (CI doesn't cover this; verify locally before merge)
EOF
)"
```

Return the PR URL.

---

## Self-Review

**Spec coverage (against the audit and the goal stated above):**

| Audit item / brief item | Task |
|---|---|
| Inline trait methods onto `SiglipEmbedder` | Task 1 Steps 4, 5, 6, 7 |
| Drop both `Mutex<Session>` wrappers | Task 1 Step 4 (struct + constructor) |
| Replace `Embedding(Vec<f32>)` with `Vec<f32>` | Task 1 Steps 2, 4d, 5 |
| Test mock as `#[cfg(test)]` struct without `impl Embedder` | Mock is deleted entirely (it only tested itself). If the implementer notices a real test gap, that gap predates this change. |
| `AGENTS.md` reflects new shape | Task 2 |
| Conventional commits | Task 1 Step 9, Task 2 Step 2 |
| No `Co-Authored-By` lines | None in the commit messages |
| Pre-PR: fmt + clippy + test | Task 3 Step 1 |
| Single atomic commit covering all workspace-breaking changes | Task 1 (one commit) |

**Placeholder scan:** No TBDs. Every code step shows the actual code. Behavioural diffs in the `Embed` command are spelled out, not handwaved.

**Type consistency:**
- `Vec<f32>` is the return type throughout: `SiglipEmbedder::embed`, `SiglipEmbedder::embed_text`, the EmbedJob reply oneshot, and `repo.store_embedding(id, &emb)` (the existing signature takes `&[f32]` which `&Vec<f32>` coerces to).
- `&mut self` on `embed` and `embed_text` is consistent with the bare `Session` field type and with the mpsc-worker ownership pattern.
- `EmbedJob` is a type alias defined inline in the Embed arm; the implementer should keep it as a `type` alias (not a struct) — the channel-of-tuples pattern doesn't need a struct for two fields, and a fresh struct would be premature abstraction.

**Risks documented:**
- **Channel capacity of 1.** Picked deliberately so the worker can't outrun the DB-write side. A larger channel would just buffer jobs the async loop hasn't asked for, with no throughput gain (embed is single-threaded). If profiling later shows DB writes are stalling embedding work, revisit — but until then, depth-1 is the simplest correct setting.
- **Reply-channel drop tolerance.** The worker uses `let _ = reply.send(result);` so a dropped receiver doesn't kill the worker. This is intentional: we'd rather process the remaining jobs than abort mid-library on a slow caller. The async loop, in turn, breaks out of the for loop if `job_tx.send` fails (worker died), then awaits the worker's join handle, which surfaces the actual error.
- **`Send` requirement.** `SiglipEmbedder` no longer needs to be `Sync` (was forced by the trait). It still needs to be `Send` because the worker thread takes ownership via `spawn_blocking`. `Session: Send` per ort's docs; `Tokenizer: Send`; `&'static ModelVariant: Send`. So `SiglipEmbedder: Send` continues to hold without any explicit annotation.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-15-inline-embedder-trait.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, two-stage review between tasks. Same flow used for the AssetIndex deletion last session; the workspace-clippy constraint is the same.

**2. Inline Execution** — execute tasks in this session via `superpowers:executing-plans`.

Which approach?
