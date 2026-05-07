# Ingestion Pipeline Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the Phase 1 import pipeline — hash → deduplicate → CAS copy → DB insert — exposed as `eidetic import <PATH>`.

**Architecture:** `eidetic-ingest` defines the `AssetIndex` trait (ports-and-adapters); `eidetic-db` provides `PgAssetsRepo` implementing it. `eidetic-cli` wires both together into the `import` subcommand. The orchestration loop lives in `eidetic-ingest::import`, not in the CLI binary.

**Tech Stack:** Rust 1.92, tokio, sqlx 0.8, walkdir 2, testcontainers 0.23, clap 4 derive.

---

## File map

**New files:**
- `crates/eidetic-ingest/src/repo.rs` — `AssetIndex` trait, `NewAsset` struct, `MockAssetIndex` test helper
- `crates/eidetic-ingest/src/store.rs` — `store_file()` CAS copy
- `crates/eidetic-ingest/src/import.rs` — `import_file()`, `import_dir()`, `ImportOutcome`, `ImportSummary`
- `crates/eidetic-db/src/assets.rs` — `PgAssetsRepo` implementing `AssetIndex`
- `crates/eidetic-db/tests/assets.rs` — integration tests with testcontainers

**Modified files:**
- `Cargo.toml` (root) — add `walkdir`, `testcontainers`, `tempfile` to `[workspace.dependencies]`
- `crates/eidetic-core/src/lib.rs` — re-export `Paths`
- `crates/eidetic-ingest/src/error.rs` — add `StoreIo` and `Index` variants
- `crates/eidetic-ingest/src/lib.rs` — export new modules
- `crates/eidetic-ingest/Cargo.toml` — add `tokio`, `walkdir`
- `crates/eidetic-db/src/lib.rs` — export `PgAssetsRepo`
- `crates/eidetic-db/Cargo.toml` — add `eidetic-ingest` dep, `testcontainers` dev-dep
- `crates/eidetic-cli/src/main.rs` — add `Import` command, make `main` async
- `crates/eidetic-cli/Cargo.toml` — add `eidetic-db`, `tokio`
- `AGENTS.md` — update `eidetic-db` dep list

---

## Task 1: Workspace prep

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/eidetic-core/src/lib.rs`
- Modify: `AGENTS.md`

- [ ] **Step 1: Add new workspace deps to root Cargo.toml**

In `Cargo.toml`, add to `[workspace.dependencies]`:
```toml
# Directory walking
walkdir = "2"

# Testing
testcontainers = { version = "0.23", features = ["tokio"] }
tempfile = "3"
```

- [ ] **Step 2: Re-export Paths from eidetic-core**

In `crates/eidetic-core/src/lib.rs`, change the existing re-exports to:
```rust
pub use config::{Config, Paths};
pub use error::{Error, Result};
pub use ids::AssetId;
```

- [ ] **Step 3: Update AGENTS.md workspace map**

Change the `eidetic-db` row from:
```
| `eidetic-db` | sqlx pool, migration runner, `*Repo` traits and Postgres impls. | `eidetic-core` |
```
to:
```
| `eidetic-db` | sqlx pool, migration runner, `*Repo` traits and Postgres impls. | `eidetic-core`, `eidetic-ingest` |
```

- [ ] **Step 4: Verify**

```bash
cargo check --workspace
```
Expected: `Finished dev profile` with no errors.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/eidetic-core/src/lib.rs AGENTS.md
git commit -m "chore: add walkdir/testcontainers/tempfile deps; re-export Paths; update AGENTS.md"
```

---

## Task 2: AssetIndex trait + NewAsset + error variants

**Files:**
- Create: `crates/eidetic-ingest/src/repo.rs`
- Modify: `crates/eidetic-ingest/src/error.rs`
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ingest/Cargo.toml`

- [ ] **Step 1: Add tokio to eidetic-ingest Cargo.toml**

```toml
[dependencies]
eidetic-core = { path = "../eidetic-core" }
sha2 = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 2: Write failing tests — create repo.rs with test block only first**

Create `crates/eidetic-ingest/src/repo.rs`:

```rust
use crate::Result;
use eidetic_core::AssetId;
use std::path::PathBuf;

pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> Result<AssetId>;
}

pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(crate) struct MockAssetIndex {
        records: Mutex<HashMap<String, AssetId>>,
    }

    impl MockAssetIndex {
        pub(crate) fn new() -> Self {
            Self { records: Mutex::new(HashMap::new()) }
        }

        pub(crate) fn seed(&self, hash: &str, id: AssetId) {
            self.records.lock().unwrap().insert(hash.to_string(), id);
        }
    }

    impl AssetIndex for MockAssetIndex {
        async fn find_by_hash(&self, hash: &str) -> crate::Result<Option<AssetId>> {
            Ok(self.records.lock().unwrap().get(hash).copied())
        }

        async fn insert_asset(&self, asset: NewAsset) -> crate::Result<AssetId> {
            let id = AssetId::new();
            self.records.lock().unwrap().insert(asset.hash.clone(), id);
            Ok(id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::MockAssetIndex;

    #[tokio::test]
    async fn mock_insert_then_find() {
        let index = MockAssetIndex::new();
        let asset = NewAsset {
            hash: "abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd".to_string(),
            original_filename: "photo.jpg".to_string(),
            storage_path: PathBuf::from("/library/ab/c1/abc123.jpg"),
            file_size: 1024,
            mime_type: None,
        };
        let id = index.insert_asset(asset).await.unwrap();
        let found = index.find_by_hash("abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd").await.unwrap();
        assert_eq!(found, Some(id));
    }

    #[tokio::test]
    async fn mock_unknown_hash_returns_none() {
        let index = MockAssetIndex::new();
        let found = index.find_by_hash("0000000000000000000000000000000000000000000000000000000000000000").await.unwrap();
        assert!(found.is_none());
    }
}
```

- [ ] **Step 3: Run — expect compile error (repo module not wired yet)**

```bash
cargo test -p eidetic-ingest 2>&1 | head -20
```
Expected: compile error about unknown module `repo`.

- [ ] **Step 4: Add StoreIo + Index variants to error.rs**

Replace `crates/eidetic-ingest/src/error.rs` entirely:

```rust
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to store {path}: {source}")]
    StoreIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("asset index: {0}")]
    Index(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 5: Wire repo module in lib.rs**

Replace `crates/eidetic-ingest/src/lib.rs` entirely:

```rust
//! File ingestion for Eidetic.

pub mod error;
pub mod hasher;
pub mod repo;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use repo::{AssetIndex, NewAsset};
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p eidetic-ingest
```
Expected: all prior tests pass plus 2 new `mock_*` tests — 6 total.

- [ ] **Step 7: Commit**

```bash
git add crates/eidetic-ingest/
git commit -m "feat(ingest): add AssetIndex trait, NewAsset, error variants"
```

---

## Task 3: store_file()

**Files:**
- Create: `crates/eidetic-ingest/src/store.rs`
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ingest/Cargo.toml`

- [ ] **Step 1: Add tempfile dev dep**

In `crates/eidetic-ingest/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: Write failing tests — create store.rs with todo!()**

Create `crates/eidetic-ingest/src/store.rs`:

```rust
use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Copy `src` into the content-addressable library at
/// `{library_dir}/{hash[0..2]}/{hash[2..4]}/{hash}.{ext}`.
///
/// Creates intermediate directories. Idempotent: if the target already
/// exists the copy is skipped and the existing path is returned. Dedup
/// at the DB layer happens before this call, but the idempotency covers
/// partial-failure recovery.
pub fn store_file(src: &Path, hash: &str, library_dir: &Path) -> Result<PathBuf> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_tmp(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[test]
    fn stores_at_cas_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "photo.jpg", b"fake image data");
        let library = tmp.path().join("library");
        let hash = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

        let dest = store_file(&src, hash, &library).unwrap();

        assert_eq!(
            dest,
            library.join("2c").join("f2").join(format!("{hash}.jpg"))
        );
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"fake image data");
    }

    #[test]
    fn creates_intermediate_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "img.png", b"data");
        let library = tmp.path().join("nested").join("library");
        let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let dest = store_file(&src, hash, &library).unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn idempotent_second_call_returns_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "dup.jpg", b"content");
        let library = tmp.path().join("library");
        let hash = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222";

        let dest1 = store_file(&src, hash, &library).unwrap();
        let dest2 = store_file(&src, hash, &library).unwrap();
        assert_eq!(dest1, dest2);
    }

    #[test]
    fn file_without_extension_stored_without_dot() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "noext", b"data");
        let library = tmp.path().join("library");
        let hash = "1111222233334444555566667777888899990000aaaabbbbccccddddeeeeffff";

        let dest = store_file(&src, hash, &library).unwrap();
        assert_eq!(dest.file_name().unwrap().to_str().unwrap(), hash);
    }
}
```

- [ ] **Step 3: Run — verify tests fail**

```bash
cargo test -p eidetic-ingest store
```
Expected: 4 test failures (panics at `todo!()`).

- [ ] **Step 4: Implement store_file**

Replace the `todo!()` body:

```rust
pub fn store_file(src: &Path, hash: &str, library_dir: &Path) -> Result<PathBuf> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()));

    let filename = match &ext {
        Some(e) => format!("{hash}{e}"),
        None => hash.to_string(),
    };

    let dest = library_dir.join(&hash[..2]).join(&hash[2..4]).join(&filename);

    if dest.exists() {
        return Ok(dest);
    }

    let parent = dest.parent().expect("dest always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| Error::StoreIo {
        path: parent.to_path_buf(),
        source,
    })?;

    std::fs::copy(src, &dest).map_err(|source| Error::StoreIo {
        path: dest.clone(),
        source,
    })?;

    Ok(dest)
}
```

- [ ] **Step 5: Export from lib.rs**

```rust
pub mod error;
pub mod hasher;
pub mod repo;
pub mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use repo::{AssetIndex, NewAsset};
pub use store::store_file;
```

- [ ] **Step 6: Run all ingest tests**

```bash
cargo test -p eidetic-ingest
```
Expected: all 10 tests pass (6 prior + 4 new store tests).

- [ ] **Step 7: Commit**

```bash
git add crates/eidetic-ingest/ Cargo.toml Cargo.lock
git commit -m "feat(ingest): add store_file() content-addressable copy"
```

---

## Task 4: import_file() + ImportOutcome

**Files:**
- Create: `crates/eidetic-ingest/src/import.rs`
- Modify: `crates/eidetic-ingest/src/lib.rs`

- [ ] **Step 1: Write failing tests — create import.rs with todo!()**

Create `crates/eidetic-ingest/src/import.rs`:

```rust
use crate::{
    hash_file, store_file,
    repo::{AssetIndex, NewAsset},
    Error, Result,
};
use eidetic_core::{AssetId, Paths};
use std::path::Path;

pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Failed(Error),
}

pub async fn import_file(
    path: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> ImportOutcome {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::test_support::MockAssetIndex;
    use eidetic_core::{AssetId, Paths};
    use std::io::Write;
    use std::path::Path;

    fn make_paths(tmp: &tempfile::TempDir) -> Paths {
        Paths {
            import_dir: tmp.path().join("import"),
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        }
    }

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[tokio::test]
    async fn new_file_is_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_file(tmp.path(), "photo.jpg", b"fake image");
        let index = MockAssetIndex::new();

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Imported(_)));
    }

    #[tokio::test]
    async fn known_hash_returns_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_file(tmp.path(), "photo.jpg", b"fake image");
        let index = MockAssetIndex::new();
        let hash = crate::hash_file(&src).unwrap();
        let existing_id = AssetId::new();
        index.seed(&hash, existing_id);

        let outcome = import_file(&src, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Duplicate(id) if id == existing_id));
    }

    #[tokio::test]
    async fn missing_file_returns_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let index = MockAssetIndex::new();
        let nonexistent = tmp.path().join("nope.jpg");

        let outcome = import_file(&nonexistent, &index, &paths).await;
        assert!(matches!(outcome, ImportOutcome::Failed(_)));
    }
}
```

- [ ] **Step 2: Wire import module in lib.rs**

```rust
pub mod error;
pub mod hasher;
pub mod import;
pub mod repo;
pub mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{import_file, ImportOutcome};
pub use repo::{AssetIndex, NewAsset};
pub use store::store_file;
```

- [ ] **Step 3: Run — verify 3 test failures**

```bash
cargo test -p eidetic-ingest import_file
```
Expected: 3 failures (panics at `todo!()`).

- [ ] **Step 4: Implement import_file**

Replace the `todo!()` body:

```rust
pub async fn import_file(
    path: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> ImportOutcome {
    let file_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(source) => {
            return ImportOutcome::Failed(Error::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    let hash = match hash_file(path) {
        Ok(h) => h,
        Err(e) => return ImportOutcome::Failed(e),
    };

    match index.find_by_hash(&hash).await {
        Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
        Ok(None) => {}
        Err(e) => return ImportOutcome::Failed(e),
    }

    let storage_path = match store_file(path, &hash, &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let original_filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let new_asset = NewAsset {
        hash,
        original_filename,
        storage_path,
        file_size,
        mime_type: None,
    };

    match index.insert_asset(new_asset).await {
        Ok(id) => ImportOutcome::Imported(id),
        Err(e) => ImportOutcome::Failed(e),
    }
}
```

- [ ] **Step 5: Run all ingest tests**

```bash
cargo test -p eidetic-ingest
```
Expected: all 13 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/eidetic-ingest/
git commit -m "feat(ingest): add import_file() with ImportOutcome"
```

---

## Task 5: import_dir() + ImportSummary

**Files:**
- Modify: `crates/eidetic-ingest/src/import.rs`
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ingest/Cargo.toml`

- [ ] **Step 1: Add walkdir to eidetic-ingest Cargo.toml**

```toml
[dependencies]
eidetic-core = { path = "../eidetic-core" }
sha2 = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }
walkdir = { workspace = true }
```

- [ ] **Step 2: Add ImportSummary + import_dir stub to import.rs**

Append to the imports at the top of `import.rs`:
```rust
use walkdir::WalkDir;
```

Add after the `import_file` function (before `#[cfg(test)]`):

```rust
pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub failed: Vec<(std::path::PathBuf, Error)>,
}

pub async fn import_dir(
    dir: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> Result<ImportSummary> {
    todo!()
}
```

- [ ] **Step 3: Add tests to the existing #[cfg(test)] block in import.rs**

Add to the `tests` mod inside `#[cfg(test)]`:

```rust
    #[tokio::test]
    async fn dir_imports_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        write_file(&src_dir, "a.jpg", b"image a");
        write_file(&src_dir, "b.jpg", b"image b");
        write_file(&src_dir, "c.jpg", b"image c");
        let index = MockAssetIndex::new();

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 3);
        assert_eq!(summary.duplicates, 0);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn dir_counts_duplicates_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        let file = write_file(&src_dir, "photo.jpg", b"image content");
        let index = MockAssetIndex::new();
        let hash = crate::hash_file(&file).unwrap();
        index.seed(&hash, AssetId::new());

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.duplicates, 1);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn dir_not_found_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let index = MockAssetIndex::new();
        let missing = tmp.path().join("does_not_exist");

        let result = import_dir(&missing, &index, &paths).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dir_skips_subdirectory_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src_dir = tmp.path().join("photos");
        write_file(&src_dir, "good.jpg", b"good image");
        std::fs::create_dir(src_dir.join("subdir")).unwrap();
        let index = MockAssetIndex::new();

        let summary = import_dir(&src_dir, &index, &paths).await.unwrap();
        assert_eq!(summary.imported, 1);
        assert!(summary.failed.is_empty());
    }
```

- [ ] **Step 4: Run — verify 4 new test failures**

```bash
cargo test -p eidetic-ingest import_dir
```
Expected: 4 failures (panics at `todo!()`).

- [ ] **Step 5: Implement import_dir**

Replace `todo!()` with:

```rust
pub async fn import_dir(
    dir: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> Result<ImportSummary> {
    if !dir.is_dir() {
        return Err(Error::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "path does not exist or is not a directory",
            ),
        });
    }

    let mut summary = ImportSummary {
        imported: 0,
        duplicates: 0,
        failed: Vec::new(),
    };

    for entry in WalkDir::new(dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let path = e.path().unwrap_or(dir).to_path_buf();
                let io_err = e.into_io_error().unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::Other, "directory walk error")
                });
                summary.failed.push((path.clone(), Error::Io { path, source: io_err }));
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        match import_file(entry.path(), index, config).await {
            ImportOutcome::Imported(_) => summary.imported += 1,
            ImportOutcome::Duplicate(_) => summary.duplicates += 1,
            ImportOutcome::Failed(e) => {
                summary.failed.push((entry.path().to_path_buf(), e));
            }
        }
    }

    Ok(summary)
}
```

- [ ] **Step 6: Export ImportSummary + import_dir from lib.rs**

```rust
pub use import::{import_dir, import_file, ImportOutcome, ImportSummary};
```

- [ ] **Step 7: Run all ingest tests**

```bash
cargo test -p eidetic-ingest
```
Expected: all 17 tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/eidetic-ingest/ Cargo.toml Cargo.lock
git commit -m "feat(ingest): add import_dir() with ImportSummary"
```

---

## Task 6: PgAssetsRepo in eidetic-db

**Files:**
- Modify: `crates/eidetic-db/Cargo.toml`
- Create: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-db/src/lib.rs`
- Create: `crates/eidetic-db/tests/assets.rs`

- [ ] **Step 1: Update eidetic-db Cargo.toml**

Replace `crates/eidetic-db/Cargo.toml` entirely:

```toml
[package]
name = "eidetic-db"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
description = "Database access layer for Eidetic."

[dependencies]
eidetic-core = { path = "../eidetic-core" }
eidetic-ingest = { path = "../eidetic-ingest" }
sqlx = { workspace = true }
tokio = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
testcontainers = { workspace = true }
tokio = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Write failing integration tests**

Create `crates/eidetic-db/tests/assets.rs`:

```rust
use eidetic_core::{AssetId, Config};
use eidetic_db::PgAssetsRepo;
use eidetic_ingest::{AssetIndex, NewAsset};
use std::path::PathBuf;
use testcontainers::{
    core::WaitFor,
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

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

#[tokio::test]
async fn insert_asset_then_find_by_hash() {
    let (_container, url) = start_db().await;
    let mut config = Config::default();
    config.database_url = url;

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let hash = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222";
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "test.jpg".to_string(),
        storage_path: PathBuf::from("/library/aa/aa/aaaa1111.jpg"),
        file_size: 2048,
        mime_type: None,
    };

    let id = repo.insert_asset(asset).await.expect("insert");
    let found = repo.find_by_hash(hash).await.expect("find");
    assert_eq!(found, Some(id));
}

#[tokio::test]
async fn find_by_hash_returns_none_for_unknown() {
    let (_container, url) = start_db().await;
    let mut config = Config::default();
    config.database_url = url;

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let found = repo
        .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("find");
    assert!(found.is_none());
}
```

- [ ] **Step 3: Run — expect compile error (PgAssetsRepo doesn't exist yet)**

```bash
cargo test -p eidetic-db 2>&1 | head -10
```
Expected: `error[E0433]: failed to resolve: use of undeclared crate or module`.

- [ ] **Step 4: Create PgAssetsRepo**

Create `crates/eidetic-db/src/assets.rs`:

```rust
use eidetic_core::AssetId;
use eidetic_ingest::{AssetIndex, NewAsset};
use sqlx::PgPool;

pub struct PgAssetsRepo {
    pool: PgPool,
}

impl PgAssetsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AssetIndex for PgAssetsRepo {
    async fn find_by_hash(&self, hash: &str) -> eidetic_ingest::Result<Option<AssetId>> {
        let row: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
                .bind(hash)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| eidetic_ingest::Error::Index(Box::new(e)))?;

        Ok(row.map(|(uuid,)| AssetId::from(uuid)))
    }

    async fn insert_asset(&self, asset: NewAsset) -> eidetic_ingest::Result<AssetId> {
        let id = AssetId::new();
        sqlx::query(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id.as_uuid())
        .bind(&asset.hash)
        .bind(&asset.original_filename)
        .bind(asset.storage_path.to_string_lossy().as_ref())
        .bind(asset.file_size as i64)
        .bind(asset.mime_type.as_deref())
        .execute(&self.pool)
        .await
        .map_err(|e| eidetic_ingest::Error::Index(Box::new(e)))?;

        Ok(id)
    }
}
```

- [ ] **Step 5: Export PgAssetsRepo from lib.rs**

Add to `crates/eidetic-db/src/lib.rs` (keep all existing content, add at the top):

```rust
pub mod assets;
pub use assets::PgAssetsRepo;
```

- [ ] **Step 6: Run integration tests (requires Docker)**

```bash
cargo test -p eidetic-db
```

Docker pulls `tensorchord/vchord-postgres:pg17-v0.4.3` on first run (~500 MB). Subsequent runs use the cached image.

Expected output after ~20-30 seconds:
```
test insert_asset_then_find_by_hash ... ok
test find_by_hash_returns_none_for_unknown ... ok

test result: ok. 2 passed
```

Also includes existing `sanitize_url_*` unit tests: 4 total.

- [ ] **Step 7: Commit**

```bash
git add crates/eidetic-db/ Cargo.toml Cargo.lock
git commit -m "feat(db): add PgAssetsRepo implementing AssetIndex"
```

---

## Task 7: import CLI subcommand

**Files:**
- Modify: `crates/eidetic-cli/Cargo.toml`
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Update eidetic-cli Cargo.toml**

Replace `crates/eidetic-cli/Cargo.toml` entirely:

```toml
[package]
name = "eidetic-cli"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
description = "Eidetic command-line interface."

[[bin]]
name = "eidetic"
path = "src/main.rs"

[dependencies]
eidetic-core = { path = "../eidetic-core" }
eidetic-ingest = { path = "../eidetic-ingest" }
eidetic-db = { path = "../eidetic-db" }
clap = { workspace = true }
anyhow = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 2: Rewrite main.rs**

Replace `crates/eidetic-cli/src/main.rs` entirely:

```rust
//! `eidetic` — personal media intelligence engine, command-line interface.
//!
//! This binary contains no business logic. Each subcommand wires up the
//! library crates and dispatches to them. Logic lives in the libraries.

use anyhow::Context;
use clap::{Parser, Subcommand};
use eidetic_core::Config;
use eidetic_ingest::ImportOutcome;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "eidetic", version, about = "Personal media intelligence engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compute the SHA-256 hash of a file.
    Hash {
        /// Path to the file.
        path: PathBuf,
    },
    /// Import a file or directory into the library.
    Import {
        /// File or directory to import.
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_env("EIDETIC_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    match cli.command {
        Command::Hash { path } => {
            let hash = eidetic_ingest::hash_file(&path)
                .with_context(|| format!("failed to hash {}", path.display()))?;
            println!("{hash}  {}", path.display());
        }

        Command::Import { path } => {
            let config = Config::from_env().context("failed to load config")?;
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::PgAssetsRepo::new(pool);

            if path.is_file() {
                match eidetic_ingest::import_file(&path, &repo, &config.paths).await {
                    ImportOutcome::Imported(id) => {
                        println!("Imported  {} ({})", path.display(), id);
                    }
                    ImportOutcome::Duplicate(id) => {
                        println!(
                            "Duplicate {} (already in library as {})",
                            path.display(),
                            id
                        );
                    }
                    ImportOutcome::Failed(e) => {
                        eprintln!("Failed    {} — {e}", path.display());
                        std::process::exit(1);
                    }
                }
            } else {
                let summary = eidetic_ingest::import_dir(&path, &repo, &config.paths)
                    .await
                    .with_context(|| format!("cannot import {}", path.display()))?;

                println!("Imported   {:>6} files", summary.imported);
                println!("Duplicates {:>6} files", summary.duplicates);
                println!("Failed     {:>6} files", summary.failed.len());
                for (p, e) in &summary.failed {
                    println!("  {} — {e}", p.display());
                }

                if !summary.failed.is_empty() {
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 3: Build**

```bash
cargo build -p eidetic-cli
```
Expected: `Finished dev profile` with no errors.

- [ ] **Step 4: Run clippy on the whole workspace**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: no warnings.

- [ ] **Step 5: Smoke test — hash command**

```bash
printf "hello" > /tmp/eidetic-smoke.txt
cargo run -p eidetic-cli -- hash /tmp/eidetic-smoke.txt
```
Expected:
```
2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  /tmp/eidetic-smoke.txt
```

- [ ] **Step 6: Smoke test — import (requires Postgres at default URL)**

```bash
# Start postgres if not running:
# docker run -d -e POSTGRES_USER=eidetic -e POSTGRES_PASSWORD=eidetic \
#   -e POSTGRES_DB=eidetic -p 5432:5432 tensorchord/vchord-postgres:pg17-v0.4.3

printf "test photo data" > /tmp/smoke-photo.jpg
cargo run -p eidetic-cli -- import /tmp/smoke-photo.jpg
```
Expected: `Imported  /tmp/smoke-photo.jpg (<uuid>)`

Run again:
```bash
cargo run -p eidetic-cli -- import /tmp/smoke-photo.jpg
```
Expected: `Duplicate /tmp/smoke-photo.jpg (already in library as <uuid>)`

- [ ] **Step 7: Commit**

```bash
git add crates/eidetic-cli/ Cargo.lock
git commit -m "feat(cli): add import subcommand"
```

---

## Deferred (out of scope for this plan)

- EXIF extraction (`kamadak-exif`) — `mime_type` field is always `None` until this lands
- Thumbnail generation (`image` + `fast_image_resize`)
- File watcher (`notify-debouncer-full`) for watching `import_dir` for new files
- Filtering non-image files (needs MIME detection first)
