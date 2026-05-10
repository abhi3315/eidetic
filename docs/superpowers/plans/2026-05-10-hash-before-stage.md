# Hash-Before-Stage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorder `import_file` so duplicate detection happens before any bytes are copied to staging — eliminating wasted I/O on re-imports of synced photo dirs.

**Architecture:** Move `hash_file` to operate on the source path directly. Run the dedup query against that hash; on a hit, return `ImportOutcome::Duplicate` without ever calling `stage_file`. Only on a miss do we stage, extract EXIF from the staged copy, and atomically commit to the CAS path. The crash-safety guarantee (atomic rename via `tempfile::persist`) and the `commit_staged` length-check both stay in place — only the *order* of operations changes. The contract — "no I/O to `library_dir` on a dedup hit" — is locked in by a snapshot test that fails on main and passes after the reorder.

**Tech Stack:** Rust 1.92 / edition 2024, tokio (async test harness), tempfile, walkdir (already a transitive dep, used here in tests for snapshotting).

---

## File Structure

This is a behavioral refactor inside one function. Two files change, no new files:

- **Modify:** `crates/eidetic-ingest/src/import.rs`
  - `import_file` body reordered (~lines 19–103). No signature changes, no public API changes.
  - One new `#[tokio::test]` added inside the existing `mod tests` block: `duplicate_import_does_not_touch_library_dir`.

- **Untouched but worth verifying still passes:**
  - `crates/eidetic-ingest/src/store.rs` — `stage_file` and `commit_staged` are unchanged.
  - `crates/eidetic-db/tests/assets.rs::import_file_round_trips_through_pg_assets_repo` — the testcontainers end-to-end check; only runs locally with Docker.

---

## Task 1: Add the failing contract test

This is the test that justifies the refactor. It snapshots `library_dir` before and after a duplicate import and asserts equality. On current `main`, `stage_file` runs unconditionally and calls `std::fs::create_dir_all(library_dir)` — so the directory comes into existence even though no file ends up in it. After the refactor, `stage_file` is never called on a dedup hit, so the directory never appears.

The snapshot uses `walkdir::WalkDir` (already a workspace dep used in `import.rs`). Comparing strict-equal `Vec<PathBuf>` catches any state change — directory creation, temp-file leftovers, anything.

**Files:**
- Modify: `crates/eidetic-ingest/src/import.rs` (add test inside `#[cfg(test)] mod tests`, near other duplicate-related tests around line 226–237)

- [ ] **Step 1: Add the snapshot helper and the failing test**

Append the test below at the end of the existing `mod tests { ... }` block in `crates/eidetic-ingest/src/import.rs` (just before the closing brace on line 366). Place the helper above the test:

```rust
    /// Sorted list of every path under `dir` (including `dir` itself), or
    /// empty if `dir` doesn't exist. Used to assert no filesystem mutation.
    fn snapshot_dir(dir: &Path) -> Vec<std::path::PathBuf> {
        if !dir.exists() {
            return Vec::new();
        }
        let mut entries: Vec<std::path::PathBuf> = WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_path_buf())
            .collect();
        entries.sort();
        entries
    }

    #[tokio::test]
    async fn duplicate_import_does_not_touch_library_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_paths(&tmp);
        let src = write_jpeg(tmp.path(), "photo.jpg", b"a");
        let index = MockAssetIndex::new();
        let hash = crate::hash_file(&src).unwrap().to_string();
        index.seed(&hash, AssetId::new());

        // Precondition: library_dir hasn't been created yet.
        assert!(!paths.library_dir.exists());
        let before = snapshot_dir(&paths.library_dir);

        let outcome = import_file(&src, &index, &paths).await;
        assert!(
            matches!(outcome, ImportOutcome::Duplicate(_)),
            "expected Duplicate, got {outcome:?}"
        );

        // Contract: a dedup hit must not write anything to library_dir,
        // not even create the directory itself. This locks in the
        // hash-before-stage refactor against future drift.
        let after = snapshot_dir(&paths.library_dir);
        assert_eq!(
            before, after,
            "duplicate import mutated library_dir; before={before:?} after={after:?}",
        );
    }
```

Note: `WalkDir` is already imported at the top of `import.rs` (line 9). `Path` is in scope inside the test module via `use std::path::Path;` (line 186). `ImportOutcome::Duplicate` already implements `Debug` (the enum has `#[derive(Debug)]` at line 11).

- [ ] **Step 2: Run the test to verify it FAILS on current main**

Run:
```bash
cargo test -p eidetic-ingest --lib duplicate_import_does_not_touch_library_dir -- --nocapture
```

Expected output: FAIL. The assertion message will look like:
```
duplicate import mutated library_dir; before=[] after=["/var/folders/.../library"]
```

(`library_dir` was created by `stage_file → create_dir_all` even though the staged temp file got auto-deleted on drop.)

If the test passes on current main, **stop** — the precondition for the refactor is wrong and we need to re-examine the call path before continuing.

- [ ] **Step 3: Commit the failing test**

```bash
git checkout -b refactor/hash-before-stage
git add crates/eidetic-ingest/src/import.rs
git commit -m "test(ingest): assert duplicate import doesn't touch library_dir

Locks in the contract for the hash-before-stage refactor: a re-import
must not create or modify anything under library_dir. Currently fails
because stage_file unconditionally create_dir_all's library_dir before
the dedup query runs."
```

(Yes, we commit a known-failing test on its own branch. The next task fixes it. This separates the contract from the implementation in history, which makes the refactor diff small and reviewable.)

---

## Task 2: Reorder `import_file` — hash before stage

Move `hash_file` to operate on the source path. Run the dedup query before staging. Stage only on a miss. Keep file_size and EXIF derived from the staged file (they're cheap once we've already paid for the copy, and EXIF specifically must read the bytes that will be canonical at the CAS path).

**Files:**
- Modify: `crates/eidetic-ingest/src/import.rs:19-103` (the `import_file` function body)

- [ ] **Step 1: Replace the `import_file` body with the reordered version**

In `crates/eidetic-ingest/src/import.rs`, replace the entire `import_file` function (currently lines 19–103) with:

```rust
pub async fn import_file(path: &Path, index: &impl AssetIndex, config: &Paths) -> ImportOutcome {
    // MIME detection reads only 512 bytes — acceptable before staging.
    let mime_type = match crate::meta::detect_mime(path) {
        Ok(Some(m)) => m,
        Ok(None) => return ImportOutcome::Skipped,
        Err(e) => return ImportOutcome::Failed(e),
    };
    if !mime_type.starts_with("image/") && !mime_type.starts_with("video/") {
        return ImportOutcome::Skipped;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    let original_filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    // Hash the source directly so we can short-circuit duplicates before
    // copying the file to staging. On a re-import of a synced photo dir
    // this avoids gigabytes of pointless I/O per duplicate.
    let hash = match hash_file(path) {
        Ok(h) => h,
        Err(e) => return ImportOutcome::Failed(e),
    };
    let hash_hex = hash.to_string();

    match index.find_by_hash(&hash_hex).await {
        Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
        Ok(None) => {}
        Err(e) => return ImportOutcome::Failed(Error::Index(e)),
    }

    // Past the dedup gate. Stage a stable copy so EXIF and the CAS commit
    // both read the same bytes that will end up canonical at the CAS path.
    let stage = match stage_file(path, &config.library_dir) {
        Ok(s) => s,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let file_size = match std::fs::metadata(stage.path()) {
        Ok(m) => m.len(),
        Err(source) => {
            return ImportOutcome::Failed(Error::Io {
                path: stage.path().to_path_buf(),
                source,
            });
        }
    };

    let exif = if mime_type.starts_with("image/") {
        crate::meta::extract_exif(stage.path())
    } else {
        crate::meta::ExifData::default()
    };

    let storage_path = match commit_staged(stage, &hash, ext.as_deref(), &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path,
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
    };

    match index.insert_asset(new_asset).await {
        Ok(InsertOutcome::Inserted(id)) => {
            debug!(path = %path.display(), hash = %hash_hex, "imported");
            ImportOutcome::Imported(id)
        }
        Ok(InsertOutcome::Existing(id)) => {
            debug!(path = %path.display(), hash = %hash_hex, "duplicate");
            ImportOutcome::Duplicate(id)
        }
        Err(e) => ImportOutcome::Failed(Error::Index(e)),
    }
}
```

What changed (and what didn't):
- `hash_file(path)` now reads the **source path**, not `stage.path()`.
- The dedup query (`index.find_by_hash`) now runs **before** `stage_file`.
- `stage_file` is only called on a dedup miss — duplicates return early.
- `file_size`, EXIF, and `commit_staged` continue to read from `stage.path()` — those bytes are canonical once we've committed to staging.
- No signature change. No new imports. The MIME detection, extension extraction, and filename extraction were already operating on `path` and stay there.

- [ ] **Step 2: Run the contract test — must now PASS**

Run:
```bash
cargo test -p eidetic-ingest --lib duplicate_import_does_not_touch_library_dir -- --nocapture
```

Expected output: PASS (`test result: ok. 1 passed`).

- [ ] **Step 3: Run the full ingest crate suite — every existing test must still pass**

Run:
```bash
cargo test -p eidetic-ingest
```

Expected output: PASS for every test, including:
- `new_file_is_imported`
- `known_hash_returns_duplicate`
- `missing_file_returns_failed` — still returns `Failed` because `hash_file` now fails first (instead of `stage_file`); either way the outcome is `Failed`.
- `non_media_file_returns_skipped`
- `imported_file_has_jpeg_mime_type`
- `dir_imports_all_files`
- `dir_counts_duplicates_separately`
- `dir_counts_non_media_as_skipped`
- `dir_not_found_returns_err`
- `non_ascii_filename_preserved_not_unknown`
- `dir_recurses_into_subdirectories`
- `duplicate_import_does_not_touch_library_dir` ← the new one
- All `store::tests::*` (untouched, sanity check)
- All `hasher::tests::*` (untouched)

If any unrelated test fails, **stop** and investigate before continuing — the refactor preserved the existing contracts and any failure suggests something subtle moved.

- [ ] **Step 4: Commit the refactor**

```bash
git add crates/eidetic-ingest/src/import.rs
git commit -m "refactor(ingest): hash before staging to short-circuit duplicates

Reorder import_file so hash + dedup check run on the source path,
before any bytes are copied to library staging. On a dedup hit we
return ImportOutcome::Duplicate immediately — no stage_file call,
no library_dir creation, no I/O.

Common case for personal use is re-importing a synced photo dir;
every duplicate previously paid the full source-to-staging copy
(~8 GB round-trip on a 4 GB video) just to be dropped. Near-full
disks could ENOSPC mid-import.

Atomic-rename crash safety (tempfile::persist in commit_staged)
and the existing CAS length-check are unchanged. EXIF still reads
from the staged file — those bytes become canonical at the CAS
path and we want EXIF measured against what we actually store.

Locked in by duplicate_import_does_not_touch_library_dir, which
fails on parent commit and passes here."
```

---

## Task 3: Workspace-wide verification

The pre-commit hook runs fmt + clippy on every commit. CI runs the full test suite. We mirror both locally before pushing — clippy on every target (including tests) catches lints that pre-commit would otherwise flag at PR time.

**Files:** none modified.

- [ ] **Step 1: Run `cargo fmt --check`**

```bash
cargo fmt --all -- --check
```

Expected: no output, exit 0. If the formatter wants changes, run `cargo fmt --all` then `git add` + `git commit --amend --no-edit` (this amends the refactor commit, which is the one whose style would be checked).

- [ ] **Step 2: Run clippy with `-D warnings` on every target**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: no warnings, exit 0. The new test code may surface lints (e.g. inefficient pattern in the snapshot helper). Fix them in-line and amend the refactor commit.

- [ ] **Step 3: Run the full workspace test suite**

```bash
cargo test --workspace
```

Expected: every test passes. Two notes:
1. Tests in `crates/eidetic-db/tests/assets.rs` use `testcontainers` and require a running Docker daemon. If Docker isn't available locally they'll be skipped or error out — that's CI's job to verify. If Docker **is** running, `import_file_round_trips_through_pg_assets_repo` is the authoritative end-to-end check for the refactor; it must pass.
2. If the testcontainers tests can't run locally, mention that in the PR description so the reviewer knows CI is the sole signal for them.

- [ ] **Step 4: (If Docker is available) Run the e2e check explicitly**

```bash
cargo test -p eidetic-db --test assets import_file_round_trips_through_pg_assets_repo -- --nocapture
```

Expected: PASS. The test imports a stub JPEG, asserts the row landed in Postgres with the right shape, then re-imports the same bytes and asserts `Duplicate`. After the refactor, the second call hits the new fast path.

---

## Task 4: Push and open the PR

**Files:** none modified — this is purely git/gh.

- [ ] **Step 1: Push the branch**

```bash
git push -u origin refactor/hash-before-stage
```

- [ ] **Step 2: Open the PR**

Use the same body shape as PR #11 (Summary + Test plan checklist). Run:

```bash
gh pr create --title "refactor(ingest): hash before staging to short-circuit duplicates" --body "$(cat <<'EOF'
## Summary

Reorder `import_file` so hash + dedup check run on the source path, before any bytes are copied to library staging. On a dedup hit we return `ImportOutcome::Duplicate` immediately — no `stage_file` call, no `library_dir` creation, no I/O.

The common case for personal use is re-importing a synced photo dir. Every duplicate previously paid the full source-to-staging copy (≈8 GB round-trip on a 4 GB video) just to be dropped. Near-full disks could ENOSPC mid-import.

The deferred "Big Lift #1" from `docs/superpowers/plans/2026-05-09-audit-fixes.md`.

### What stayed the same

- Atomic-rename crash safety (`tempfile::persist` in `commit_staged`).
- The CAS length-check on existing files (commit a3623ce).
- EXIF still reads from the staged file — those bytes become canonical at the CAS path.
- Public API (`import_file`, `ImportOutcome`, etc.) — no signature changes.

### Defense-in-depth that was deliberately *not* added

The original comment at `import.rs:39` worried about the source file being modified mid-import. For a personal photo library that's not a real threat. Re-hashing the staged file post-copy and comparing against the source-side hash would close the gap — but it's a separate decision and out of scope here.

## Test plan

- [x] New test `duplicate_import_does_not_touch_library_dir` fails on parent commit, passes here. Snapshots `library_dir` before/after a duplicate import and asserts equality — locks in the contract against future drift.
- [x] `cargo test -p eidetic-ingest` — all unit tests pass (including the existing 11 in `import.rs`).
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [x] `cargo fmt --all -- --check` clean.
- [ ] CI: `cargo test --workspace` (testcontainers needs Docker — `import_file_round_trips_through_pg_assets_repo` is the authoritative end-to-end check).
EOF
)"
```

If running locally with Docker, swap the last unchecked item to `[x]` before opening the PR.

- [ ] **Step 3: Verify the PR opened and CI started**

```bash
gh pr view --web
```

Watch CI: `cargo test --workspace`, clippy, rustfmt, cargo-deny, gitleaks should all be green.

---

## Self-Review

**Spec coverage (against the kickoff brief):**

| Brief item | Plan task |
|---|---|
| Reorder to MIME → hash(src) → dedup → stage → EXIF → commit → insert | Task 2, Step 1 |
| Atomic-rename crash safety stays | Task 2, Step 1 (no edits to `commit_staged`) |
| Length-check on existing CAS files stays | Task 2, Step 1 (no edits to `commit_staged`) |
| EXIF still on staged file | Task 2, Step 1 (`extract_exif(stage.path())` preserved) |
| All existing tests pass | Task 2, Step 3; Task 3, Step 3; Task 3, Step 4 |
| `import_file_round_trips_through_pg_assets_repo` is the e2e check | Task 3, Step 4 (explicit invocation when Docker available) |
| Defense-in-depth re-hashing NOT added without asking | Plan does not add it; PR body explains why |
| Failing-then-passing contract test | Task 1 (failing); Task 2, Step 2 (passing) |
| Snapshot library_dir state before/after duplicate import; assert equality | Task 1, Step 1 — `snapshot_dir` helper + `assert_eq!(before, after)` |
| Pre-commit: fmt + clippy | Task 3, Steps 1–2 |
| CI: full test + cargo-deny + clippy + rustfmt + gitleaks | Task 3, Step 3; Task 4, Step 3 |
| Branch off main, open PR matching `gh pr view 11` style | Task 1, Step 3 (branch); Task 4, Step 2 (PR with Summary + Test plan) |
| Conventional commits | Task 1, Step 3; Task 2, Step 4 (`test(ingest):`, `refactor(ingest):`) |
| No `Co-Authored-By` | None of the commit messages include it |

**Placeholder scan:** No TBDs. Every code step shows the actual code. Every command shows the exact invocation and expected output. Every commit message is fully written.

**Type consistency:** No new types or method signatures introduced. The only new identifier is `snapshot_dir` (test-local helper) and the test name itself. The reordered `import_file` body uses the exact same names (`hash_file`, `stage_file`, `commit_staged`, `Error::Io`, `Error::Index`, `NewAsset`, `InsertOutcome`) that already exist in scope at the top of the file.

**One known risk:** if the harness in this checkout doesn't have Docker available, the testcontainers tests in `eidetic-db` won't run locally. That's fine — CI runs them. The PR body flags this so the reviewer doesn't expect a green local check on those.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-10-hash-before-stage.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Good fit here because each task has a clear pass/fail signal (test fails → test passes → workspace clean → PR opens).

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
