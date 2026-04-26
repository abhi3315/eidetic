//! Streaming SHA-256 hashing.
//!
//! Files are read in fixed-size chunks and fed into the hasher
//! incrementally. Memory usage is bounded regardless of file size,
//! so this works for multi-gigabyte video files without trouble.
//!
//! Sparse hashing (hash of three sampled regions for very large
//! files) is intentionally not in v0. It's an optimization that
//! waits for evidence the streaming hasher is observably slow on
//! the user's library.

use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// 64 KiB read buffer. Large enough to amortize syscall overhead,
/// small enough that running many hashers in parallel doesn't blow
/// up RAM.
const READ_BUF_SIZE: usize = 64 * 1024;

/// Compute the SHA-256 hash of a file. Returns a lowercase hex string.
pub fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::with_capacity(READ_BUF_SIZE, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; READ_BUF_SIZE];

    loop {
        let n = reader.read(&mut buf).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
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
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hashes_empty_file() {
        let path = temp_path("empty");
        File::create(&path).unwrap();

        let hash = hash_file(&path).unwrap();
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
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
            // 200 KiB of repeating bytes — crosses 3 buffer boundaries.
            let chunk = vec![0xABu8; 1024];
            for _ in 0..200 {
                f.write_all(&chunk).unwrap();
            }
        }

        let streamed = hash_file(&path).unwrap();
        let oneshot = {
            let bytes = std::fs::read(&path).unwrap();
            let mut h = Sha256::new();
            h.update(&bytes);
            format!("{:x}", h.finalize())
        };
        assert_eq!(streamed, oneshot);

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
