# Ingestion pipeline — Phase 1 core

**Date:** 2026-05-07
**Status:** Approved

## What this covers

The end-to-end import pipeline: hash a file, deduplicate against the DB, copy it to content-addressable storage, insert a DB row. Exposed as `eidetic import <PATH>`.

Explicitly out of scope: MIME detection, EXIF extraction, thumbnail generation, file watcher. Those land in a follow-up.

---

## Dependency graph change

`eidetic-db` gains a dependency on `eidetic-ingest`. The ingest crate defines the interface it needs (ports-and-adapters); the DB crate provides the implementation.

```
eidetic-core ←── eidetic-ingest ←── eidetic-db
                       ↑                ↑
                  eidetic-cli ──────────┘
```

AGENTS.md workspace map updated to reflect this.

---

## eidetic-ingest

### src/repo.rs — interface ingestion needs from storage

```rust
pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> Result<AssetId>;
}

pub struct NewAsset {
    pub hash: String,            // 64-char lowercase hex SHA-256
    pub original_filename: String,
    pub storage_path: PathBuf,   // absolute path in CAS layout
    pub file_size: u64,
    pub mime_type: Option<String>, // always None until EXIF phase
}
```

The trait is defined in `eidetic-ingest` (near the consumer), not in `eidetic-db`. This is the ports-and-adapters pattern: ingestion defines what it needs; the DB adapter implements it.

### src/store.rs — content-addressable file copy

CAS path layout:
```
{library_dir}/{hash[0..2]}/{hash[2..4]}/{hash}.{ext}
```

Example: hash `2cf24dba…`, extension `.jpg` → `library/2c/f2/2cf24dba….jpg`

- Extension is taken from the source filename (lowercased). If no extension, the dot is omitted.
- Creates all intermediate directories.
- Idempotent: if the target path already exists, the copy is skipped. The caller sees a normal path return and decides it's a duplicate at the DB layer.

Signature:
```rust
pub fn store_file(src: &Path, hash: &str, library_dir: &Path) -> Result<PathBuf>
```

Sync (file copy is not async-bound; tokio::fs would add complexity for no real benefit here).

### src/import.rs — per-file and batch orchestration

```rust
pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Failed(Error),
}

pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub failed: Vec<(PathBuf, Error)>,
}

pub async fn import_file(
    path: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> ImportOutcome

pub async fn import_dir(
    dir: &Path,
    index: &impl AssetIndex,
    config: &Paths,
) -> Result<ImportSummary>
```

Per-file pipeline in `import_file`:
1. `hash_file(path)` — get SHA-256
2. `index.find_by_hash(&hash)` — if Some, return `Duplicate(id)`
3. `store_file(path, &hash, &config.library_dir)` — copy to CAS
4. `index.insert_asset(NewAsset { … })` — write DB row, return `Imported(id)`
5. Any error at any step → `Failed(err)`

`import_dir` walks recursively using the `walkdir` crate (add to workspace deps). Calls `import_file` per file entry. Skips directory entries. Collects `Failed` outcomes; never aborts early. Returns `Err` only if the directory walk itself fails (e.g. path doesn't exist).

### Cargo.toml changes

Add:
```toml
tokio = { workspace = true }
walkdir = { workspace = true }
```

`walkdir` is also added to `[workspace.dependencies]` in the root `Cargo.toml`.

---

## eidetic-db

### src/assets.rs — PgAssetsRepo

```rust
pub struct PgAssetsRepo {
    pool: PgPool,
}

impl PgAssetsRepo {
    pub fn new(pool: PgPool) -> Self
}
```

Implements `eidetic_ingest::AssetIndex`:

- `find_by_hash`: `SELECT id FROM assets WHERE hash = $1` → `Option<AssetId>`
- `insert_asset`: `INSERT INTO assets (id, hash, original_filename, storage_path, file_size, mime_type) VALUES (…) RETURNING id`

### Cargo.toml changes

Add:
```toml
eidetic-ingest = { path = "../eidetic-ingest" }
```

---

## eidetic-cli

### src/commands/import.rs — import subcommand

```
eidetic import <PATH>
```

`<PATH>` can be a file or a directory. The command:
1. Loads config from env (`Config::from_env()`)
2. Connects to DB (`eidetic_db::connect(&config).await`)
3. Wraps pool: `PgAssetsRepo::new(pool)`
4. Calls `import_file` or `import_dir` depending on whether PATH is a file or dir
5. Prints summary:

```
Imported  42 files
Skipped    3 duplicates
Failed     1 files
  /path/to/bad.jpg — permission denied (os error 13)
```

No new dependencies needed in `eidetic-cli`.

---

## Testing

### eidetic-ingest

Unit tests only — no DB required:

- `store_file`: tempdir, verify CAS path created, content matches, idempotent re-call
- `import_file` with `MockAssetIndex` (in-memory HashMap):
  - new file → `Imported`
  - same hash again → `Duplicate`
  - IO error on bad path → `Failed`
- `import_dir`: tempdir with a mix of files; verify `ImportSummary` counts

`MockAssetIndex` is a test helper inside `#[cfg(test)]` in `src/repo.rs`.

### eidetic-db

Integration test using testcontainers (as per AGENTS.md — no SQL mocks):

- `PgAssetsRepo::insert_asset` then `find_by_hash` round-trip
- Duplicate hash insert returns an error (unique constraint)

---

## Deferred

| Item | When |
|------|------|
| MIME type detection | EXIF phase |
| EXIF extraction (`kamadak-exif`) | Phase 1 follow-up |
| Thumbnail generation (`image` + `fast_image_resize`) | Phase 1 follow-up |
| File watcher (`notify-debouncer-full`) | Phase 1 follow-up |
| Filtering non-image files | After MIME detection exists |
| Sparse hashing for large files | When streaming hasher is observably slow |
