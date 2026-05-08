use crate::Result;
use std::path::{Path, PathBuf};

/// Copy `src` into the content-addressable library at
/// `{library_dir}/{hash[0..2]}/{hash[2..4]}/{hash}.{ext}`.
///
/// Creates intermediate directories. Idempotent: if the target already
/// exists the copy is skipped and the existing path is returned. Dedup
/// at the DB layer happens before this call, but the idempotency covers
/// partial-failure recovery.
pub fn store_file(src: &Path, hash: &str, library_dir: &Path) -> Result<PathBuf> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()));

    let filename = match &ext {
        Some(e) => format!("{hash}{e}"),
        None => hash.to_string(),
    };

    let dest = library_dir
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&filename);

    if dest.exists() {
        return Ok(dest);
    }

    let parent = dest.parent().expect("dest always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| crate::Error::StoreIo {
        path: parent.to_path_buf(),
        source,
    })?;

    let tmp = parent.join(format!("{filename}.tmp"));

    std::fs::copy(src, &tmp).map_err(|source| crate::Error::StoreIo {
        path: tmp.clone(),
        source,
    })?;

    std::fs::rename(&tmp, &dest).map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        crate::Error::StoreIo {
            path: dest.clone(),
            source,
        }
    })?;

    Ok(dest)
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
