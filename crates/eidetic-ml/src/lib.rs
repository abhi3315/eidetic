mod error;
pub mod face;
pub mod image_io;
mod siglip;

pub use error::{Error, Result};
pub use face::{AnalyzedFace, BoundingBox, Detection, FaceAnalyzer, Landmarks};
pub use siglip::SiglipEmbedder;

/// Register libheif as a decoder hook on the `image` crate.
///
/// See identical helper in eidetic-ingest::ensure_heic_registered for
/// the V2-verified idempotency note. Duplicated rather than centralised
/// in eidetic-core so the lightweight-foundation crate stays free of
/// heavy C-binding deps.
/// Without the `heic` feature this is a no-op — see the note on the
/// eidetic-ingest twin (ADR-0008).
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
