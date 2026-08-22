//! Browser-playback policy: which videos need a transcoded copy, and where
//! that copy lives (goals-v0.5.md #1).
//!
//! This is the single source of truth for the codec/container allowlist —
//! the CLI's `transcode` backfill and the server's player both consult it,
//! so the two can never drift apart.

use std::path::{Path, PathBuf};

/// Video codecs every current browser decodes (Firefox is the floor: it has
/// no HEVC support at all, which is what makes iPhone video the problem
/// case). Names as ffprobe's `codec_name` reports them.
const SAFE_CODECS: [&str; 4] = ["h264", "vp8", "vp9", "av1"];

/// Does this video play in a browser as-is?
///
/// The check is a codec×container *compatibility matrix*, not two
/// independent allowlists: `infer` cannot tell MKV from WebM (identical EBML
/// magic, both arrive as `video/webm`), and real WebM only ever carries
/// VP8/VP9/AV1 — so "h264 in video/webm" is a mislabeled Matroska file that
/// Firefox refuses, and must remux. Matroska and AVI are deliberately absent
/// from the matrix for the same reason.
///
/// An unknown codec (`None` — imported without ffprobe) is treated as unsafe:
/// the transcode pass re-probes and decides for real, and a false "needs
/// work" only costs a probe, while a false "safe" is an unplayable video.
pub fn is_browser_playable(video_codec: Option<&str>, mime_type: &str) -> bool {
    let Some(codec) = video_codec else {
        return false;
    };
    match mime_type {
        "video/mp4" | "video/quicktime" => ["h264", "vp9", "av1"].contains(&codec),
        "video/webm" => ["vp8", "vp9", "av1"].contains(&codec),
        _ => false,
    }
}

/// Whether the codec itself is fine (so a copy can be a remux, not a
/// re-encode).
pub fn is_safe_codec(video_codec: Option<&str>) -> bool {
    video_codec.is_some_and(|c| SAFE_CODECS.contains(&c))
}

/// Where an asset's playback copy lives: hash-sharded under `.playback/`,
/// mirroring the `.thumbs/` layout, always MP4.
pub fn playback_path(library_dir: &Path, hash: &crate::Sha256) -> PathBuf {
    let hex = hash.to_string();
    library_dir
        .join(".playback")
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(format!("{hex}.mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_in_mp4_is_not_playable() {
        assert!(!is_browser_playable(Some("hevc"), "video/mp4"));
    }

    #[test]
    fn h264_in_mp4_and_quicktime_is_playable() {
        assert!(is_browser_playable(Some("h264"), "video/mp4"));
        assert!(is_browser_playable(Some("h264"), "video/quicktime"));
    }

    #[test]
    fn h264_in_matroska_needs_a_copy_but_only_a_remux() {
        assert!(!is_browser_playable(Some("h264"), "video/x-matroska"));
        assert!(is_safe_codec(Some("h264")));
    }

    #[test]
    fn h264_in_webm_is_a_mislabeled_mkv_and_not_playable() {
        // `infer` reports Matroska as video/webm (same EBML magic); real
        // WebM never contains H.264, so this combination must remux.
        assert!(!is_browser_playable(Some("h264"), "video/webm"));
        assert!(is_browser_playable(Some("vp9"), "video/webm"));
    }

    #[test]
    fn unknown_codec_is_conservatively_unplayable() {
        assert!(!is_browser_playable(None, "video/mp4"));
        assert!(!is_safe_codec(None));
    }

    #[test]
    fn playback_path_is_hash_sharded_mp4() {
        let hash = crate::Sha256::from_hex(&"ab".repeat(32)).unwrap();
        let p = playback_path(Path::new("/lib"), &hash);
        let s = p.to_str().unwrap();
        assert!(s.starts_with("/lib/.playback/ab/ab/"));
        assert!(s.ends_with(".mp4"));
    }
}
