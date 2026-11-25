use crate::Result;
use eidetic_core::Sha256;
use std::path::{Path, PathBuf};

/// Copy `src` to a uniquely-named temp file inside `staging_dir`, which must be on the
/// same filesystem as the CAS library so that [`commit_staged`] can rename atomically.
///
/// Returns the digest of the bytes that were written, not of a separate read
/// of `src`. The staged copy is what becomes canonical, so it is the only
/// thing the CAS path and the `assets.hash` column are allowed to describe:
/// a source that changes mid-import (an editor saving in place, a sync client
/// still writing) would otherwise leave a file stored under the hash of
/// content it no longer holds.
pub fn stage_file(src: &Path, staging_dir: &Path) -> Result<(tempfile::NamedTempFile, Sha256)> {
    std::fs::create_dir_all(staging_dir).map_err(|source| crate::Error::Io {
        path: staging_dir.to_path_buf(),
        source,
    })?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(staging_dir).map_err(|source| crate::Error::Io {
            path: staging_dir.to_path_buf(),
            source,
        })?;
    let mut source = std::fs::File::open(src).map_err(|source| crate::Error::Io {
        path: src.to_path_buf(),
        source,
    })?;
    // Either end of the copy can fail; the source is the path worth naming,
    // since a staging failure is almost always a full or read-only library.
    let hash = crate::hasher::hash_copy(&mut source, tmp.as_file_mut()).map_err(|source| {
        crate::Error::Io {
            path: src.to_path_buf(),
            source,
        }
    })?;
    Ok((tmp, hash))
}

/// Atomically move a staged temp file to its CAS destination
/// `{library_dir}/{hash[0..2]}/{hash[2..4]}/{hash}[.ext]`.
///
/// Returns the destination and whether this call created it. A file already
/// at the destination (a concurrent import, or one left by an earlier crash)
/// is never replaced: it is re-hashed, reused if it really holds `hash`, and
/// otherwise reported as an error with the file left where it is. Equal
/// length is not evidence of equal content, so nothing short of the digest
/// is trusted. The `created` flag is what lets a caller that ends up not
/// referencing the file remove it without touching one somebody else owns.
pub fn commit_staged(
    stage: tempfile::NamedTempFile,
    hash: &Sha256,
    ext: Option<&str>,
    library_dir: &Path,
) -> Result<(PathBuf, bool)> {
    let hex = hash.to_string(); // always 64 chars, safe to slice
    let filename = match ext {
        Some(e) => format!("{hex}.{e}"),
        None => hex.clone(),
    };
    let dest = library_dir.join(&hex[..2]).join(&hex[2..4]).join(&filename);

    let parent = dest.parent().expect("dest always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| crate::Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    // No-clobber, so the existence check and the move are one atomic step:
    // a plain rename would silently overwrite whatever landed there first.
    let err = match stage.persist_noclobber(&dest) {
        Ok(_) => return Ok((dest, true)),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => e,
        Err(e) => {
            return Err(crate::Error::Io {
                path: dest,
                source: e.error,
            });
        }
    };
    // Dropping the handed-back temp file deletes the staged copy.
    drop(err.file);

    let existing = crate::hash_file(&dest)?;
    if existing != *hash {
        return Err(crate::Error::Io {
            path: dest,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "CAS file does not match its name (holds {existing}); it was left \
                     untouched — move it out of the library and re-import"
                ),
            ),
        });
    }
    Ok((dest, false))
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

    /// Stage and commit `src` under the digest of its staged copy, returning
    /// the digest, the CAS path and whether this call created it.
    fn store(src: &Path, library_dir: &Path) -> (Sha256, PathBuf, bool) {
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        let (stage, hash) = stage_file(src, library_dir).unwrap();
        let (dest, created) = commit_staged(stage, &hash, ext.as_deref(), library_dir).unwrap();
        (hash, dest, created)
    }

    /// Everything directly in `library` that is not a CAS shard directory —
    /// i.e. staging temp files that should have been cleaned up.
    fn staging_leftovers(library: &Path) -> Vec<PathBuf> {
        fs::read_dir(library)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .collect()
    }

    #[test]
    fn stores_at_cas_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "photo.jpg", b"fake image data");
        let library = tmp.path().join("library");

        let (hash, dest, created) = store(&src, &library);
        let hex = hash.to_string();

        assert!(created);
        assert_eq!(
            dest,
            library
                .join(&hex[..2])
                .join(&hex[2..4])
                .join(format!("{hex}.jpg"))
        );
        assert_eq!(fs::read(&dest).unwrap(), b"fake image data");
    }

    #[test]
    fn idempotent_second_call_returns_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "dup.jpg", b"content");
        let library = tmp.path().join("library");

        let (_, dest1, created1) = store(&src, &library);
        let (_, dest2, created2) = store(&src, &library);
        assert_eq!(dest1, dest2);
        assert!(created1);
        assert!(
            !created2,
            "the second call claimed a file it did not create"
        );
        assert!(staging_leftovers(&library).is_empty());
    }

    /// A file already sitting at the CAS path with the right length but the
    /// wrong bytes — a torn write from a crash, or bit rot — must not be
    /// taken as the stored copy. Length equality is exactly what the old
    /// check trusted; only the digest distinguishes the two.
    #[test]
    fn same_size_wrong_bytes_at_cas_path_is_rejected_and_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "photo.jpg", b"the real bytes");
        let library = tmp.path().join("library");

        let (stage, hash) = stage_file(&src, &library).unwrap();
        let hex = hash.to_string();
        let dest = library
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(format!("{hex}.jpg"));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let corrupt = b"the fake bytes";
        assert_eq!(corrupt.len(), b"the real bytes".len());
        fs::write(&dest, corrupt).unwrap();

        let err = commit_staged(stage, &hash, Some("jpg"), &library)
            .expect_err("a same-size impostor was accepted");
        match err {
            crate::Error::Io { path, source } => {
                assert_eq!(path, dest);
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected Io error, got {other:?}"),
        }
        assert_eq!(fs::read(&dest).unwrap(), corrupt, "existing file changed");
        assert!(staging_leftovers(&library).is_empty(), "staged copy leaked");
    }

    /// The digest must describe the staged copy, not an earlier read of the
    /// source. Mutating the source between the two — the sequential stand-in
    /// for a source that is still being written — used to leave the file
    /// stored under a hash of content it no longer had.
    #[test]
    fn staged_digest_follows_the_copy_not_an_earlier_source_read() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "moving.jpg", b"first version");
        let library = tmp.path().join("library");

        let before = crate::hash_file(&src).unwrap();
        write_tmp(
            tmp.path(),
            "moving.jpg",
            b"second version, different length",
        );

        let (stage, hash) = stage_file(&src, &library).unwrap();

        assert_ne!(hash, before, "digest is stale");
        assert_eq!(
            hash,
            crate::hash_file(stage.path()).unwrap(),
            "digest does not describe the staged bytes"
        );
        assert_eq!(
            fs::read(stage.path()).unwrap(),
            b"second version, different length"
        );
    }

    /// The copy is chunked, so a file larger than the read buffer proves
    /// every chunk reaches the staged file and the digest.
    #[test]
    fn staged_copy_is_complete_across_read_buffer_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let src = write_tmp(tmp.path(), "big.jpg", &content);
        let library = tmp.path().join("library");

        let (stage, hash) = stage_file(&src, &library).unwrap();

        assert_eq!(fs::read(stage.path()).unwrap(), content);
        assert_eq!(hash, crate::hash_file(&src).unwrap());
    }

    #[test]
    fn file_without_extension_stored_without_dot() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "noext", b"data");
        let library = tmp.path().join("library");

        let (hash, dest, _) = store(&src, &library);
        assert_eq!(
            dest.file_name().unwrap().to_str().unwrap(),
            hash.to_string()
        );
    }
}
