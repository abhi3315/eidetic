use crate::Result;
use eidetic_core::Sha256;
use std::path::{Path, PathBuf};

/// Copy `src` to a uniquely-named temp file inside `staging_dir` (which must be on the
/// same filesystem as the CAS library so that [`commit_staged`] can rename atomically).
///
/// The returned [`tempfile::NamedTempFile`] is automatically deleted if dropped without
/// being persisted — so callers that return early (e.g. dedup hit) get free cleanup.
pub fn stage_file(src: &Path, staging_dir: &Path) -> Result<tempfile::NamedTempFile> {
    std::fs::create_dir_all(staging_dir).map_err(|source| crate::Error::StoreIo {
        path: staging_dir.to_path_buf(),
        source,
    })?;
    let tmp =
        tempfile::NamedTempFile::new_in(staging_dir).map_err(|source| crate::Error::StoreIo {
            path: staging_dir.to_path_buf(),
            source,
        })?;
    std::fs::copy(src, tmp.path()).map_err(|source| crate::Error::StoreIo {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    Ok(tmp)
}

/// Atomically move a staged temp file to its CAS destination
/// `{library_dir}/{hash[0..2]}/{hash[2..4]}/{hash}[.ext]`.
///
/// If a concurrent import already placed the same file, the temp file is discarded and
/// the existing path is returned — safe because same hash implies identical content.
pub fn commit_staged(
    stage: tempfile::NamedTempFile,
    hash: &Sha256,
    ext: Option<&str>,
    library_dir: &Path,
) -> Result<PathBuf> {
    let hex = hash.to_string(); // always 64 chars — safe to slice
    let filename = match ext {
        Some(e) => format!("{hex}.{e}"),
        None => hex.clone(),
    };
    let dest = library_dir.join(&hex[..2]).join(&hex[2..4]).join(&filename);

    if dest.exists() {
        return Ok(dest);
    }

    let parent = dest.parent().expect("dest always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| crate::Error::StoreIo {
        path: parent.to_path_buf(),
        source,
    })?;

    match stage.persist(&dest) {
        Ok(_) => Ok(dest),
        Err(e) => {
            if dest.exists() {
                // A concurrent import completed first — same content, safe to ignore.
                Ok(dest)
            } else {
                Err(crate::Error::StoreIo {
                    path: dest,
                    source: e.error,
                })
            }
        }
    }
}

/// Stage `src` and immediately commit it to the CAS. Convenience wrapper over
/// [`stage_file`] + [`commit_staged`] for callers that don't need a stable read window.
pub fn store_file(src: &Path, hash: &Sha256, library_dir: &Path) -> Result<PathBuf> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    let stage = stage_file(src, library_dir)?;
    commit_staged(stage, hash, ext.as_deref(), library_dir)
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

    fn hex(s: &str) -> Sha256 {
        Sha256::from_hex(s).expect("test hash must be valid 64-char hex")
    }

    #[test]
    fn stores_at_cas_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "photo.jpg", b"fake image data");
        let library = tmp.path().join("library");
        let hash = hex("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");

        let dest = store_file(&src, &hash, &library).unwrap();

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
        let hash = hex("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");

        let dest = store_file(&src, &hash, &library).unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn idempotent_second_call_returns_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "dup.jpg", b"content");
        let library = tmp.path().join("library");
        let hash = hex("aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222");

        let dest1 = store_file(&src, &hash, &library).unwrap();
        let dest2 = store_file(&src, &hash, &library).unwrap();
        assert_eq!(dest1, dest2);
    }

    #[test]
    fn file_without_extension_stored_without_dot() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "noext", b"data");
        let library = tmp.path().join("library");
        let hash = hex("1111222233334444555566667777888899990000aaaabbbbccccddddeeeeffff");

        let dest = store_file(&src, &hash, &library).unwrap();
        assert_eq!(
            dest.file_name().unwrap().to_str().unwrap(),
            hash.to_string()
        );
    }

    #[test]
    fn staged_file_auto_cleaned_if_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_tmp(tmp.path(), "photo.jpg", b"data");
        let library = tmp.path().join("library");

        let staged = stage_file(&src, &library).unwrap();
        let staged_path = staged.path().to_path_buf();
        assert!(staged_path.exists());

        drop(staged); // simulates early return (e.g. dedup hit)
        assert!(!staged_path.exists());
    }
}
