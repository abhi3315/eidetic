//! File ingestion for Eidetic.

mod error;
mod hasher;
mod import;
mod meta;
mod store;
pub mod thumbnail;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use store::{commit_staged, stage_file};

/// Register libheif as a decoder hook on the `image` crate.
///
/// Call from every decode site. The `Once` keeps the hot path cheap;
/// the underlying `register_all_decoding_hooks` is itself idempotent
/// (per spec V2 — `image::hooks` uses HashMap::entry, occupied returns
/// false), so calling twice never corrupts the hook table.
///
/// Duplicated in eidetic-ml. Centralising in eidetic-core would force
/// the lightweight foundation crate to carry a heavy C-binding dep —
/// AGENTS.md flags eidetic-core as "the lightweight foundation".
pub fn ensure_heic_registered() {
    use std::sync::Once;
    static HEIC_INIT: Once = Once::new();
    HEIC_INIT.call_once(|| {
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}
