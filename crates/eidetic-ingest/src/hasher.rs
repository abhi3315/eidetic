use crate::{Error, Result};
use eidetic_core::Sha256;
use sha2::{Digest, Sha256 as Sha256Hasher};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// 64 KiB read buffer. Large enough to amortize syscall overhead,
/// small enough that running many hashers in parallel doesn't blow
/// up RAM.
const READ_BUF_SIZE: usize = 64 * 1024;

pub fn hash_file(path: &Path) -> Result<Sha256> {
    let mut file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; READ_BUF_SIZE];

    loop {
        let n = file.read(&mut buf).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hasher.finalize());
    Ok(Sha256::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("eidetic-test-{}-{}", std::process::id(), name))
    }

    #[test]
    fn hashes_known_content() {
        let path = temp_path("hello");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(b"hello").unwrap();
        }

        let hash = hash_file(&path).unwrap();
        assert_eq!(
            hash.to_string(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn streaming_matches_oneshot_for_buffer_boundaries() {
        // Write a file slightly larger than the read buffer to make sure
        // multi-chunk reads produce the same hash as a single-chunk read.
        let path = temp_path("big");
        {
            let mut f = File::create(&path).unwrap();
            // 200 KiB of repeating bytes, crosses 3 buffer boundaries.
            let chunk = vec![0xABu8; 1024];
            for _ in 0..200 {
                f.write_all(&chunk).unwrap();
            }
        }

        let streamed = hash_file(&path).unwrap();
        let oneshot = {
            let bytes = std::fs::read(&path).unwrap();
            let mut h = Sha256Hasher::new();
            h.update(&bytes);
            let mut out = [0u8; 32];
            out.copy_from_slice(&h.finalize());
            Sha256::from_bytes(out).to_string()
        };
        assert_eq!(streamed.to_string(), oneshot);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn returns_error_for_missing_file() {
        let path = temp_path("does-not-exist");
        let _ = std::fs::remove_file(&path);

        let result = hash_file(&path);
        assert!(matches!(result, Err(Error::Io { .. })));
    }
}
