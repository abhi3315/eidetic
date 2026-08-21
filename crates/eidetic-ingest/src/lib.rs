mod error;
mod hasher;
mod import;
mod meta;
mod store;
pub mod thumbnail;
pub mod video;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::{ExifData, extract_exif};
pub use store::{commit_staged, stage_file};

/// Register libheif as a decoder hook on the `image` crate.
///
/// `register_all_decoding_hooks` is itself idempotent (image::hooks uses
/// HashMap::entry, occupied returns false), so the Once is just an
/// optimisation to keep the hot path cheap. Duplicated in eidetic-ml,
/// because centralising in eidetic-core would drag a heavy C-binding dep
/// into the foundation crate.
///
/// Without the `heic` feature this is a no-op: HEIC files then fail to
/// decode like any other unsupported format, and every other format keeps
/// working through the pure-Rust decoders in the `image` crate (ADR-0008).
pub fn ensure_heic_registered() {
    #[cfg(feature = "heic")]
    {
        use std::sync::Once;
        static HEIC_INIT: Once = Once::new();
        HEIC_INIT.call_once(|| {
            libheif_rs::integration::image::register_all_decoding_hooks();
        });
    }
}
