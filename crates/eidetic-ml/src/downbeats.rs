//! Beat and downbeat inference with Beat This! (goals-v0.9.md phase 2).
//!
//! The model is Beat This! (Foscarin, Schlüter & Widmer, ISMIR 2024), run
//! through the `beat-this` crate (danigb/beat-this-rs, MIT), which ports the
//! mel front end, chunked inference and peak picking. That crate is generic
//! over a `Runtime`/`Model` pair; its own ort backend is not used because
//! enabling it forces `ort/load-dynamic` + `coreml` on the whole build.
//! [`OrtModel`] below implements the trait over the workspace ort instead,
//! so sessions come from the same builder (and `EIDETIC_ACCELERATOR`
//! handling) as SigLIP and the face models.
//!
//! Two ONNX files are needed: the mel-spectrogram front end and the small
//! beat model. Both are pinned to one immutable upstream commit and to
//! their SHA-256; a cached file is re-hashed on every load, and a download
//! is streamed to a temp file beside the destination, checked for length and
//! digest, synced, then renamed into place — an interrupted or tampered
//! fetch never leaves a file that a later load would accept.

use crate::{Error, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Human-readable identity of the beat model, recorded in grid provenance.
pub const MODEL_NAME: &str = "beat_this_small";
/// The beat-this-rs commit both artifacts are fetched from.
pub const REVISION: &str = "089b509247e6fdcec666511c0dcf0d5f39c21e73";
/// Sample rate the mel model expects. Feed PCM at exactly this rate.
pub const SAMPLE_RATE: u32 = 22_050;

struct Artifact {
    name: &'static str,
    sha256: &'static str,
    bytes: u64,
}

const MEL: Artifact = Artifact {
    name: "mel_spectrogram.onnx",
    sha256: "fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9",
    bytes: 270_742,
};

const BEAT: Artifact = Artifact {
    name: "beat_this_small.onnx",
    sha256: "a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f",
    bytes: 10_555_592,
};

/// SHA-256 of the beat model every accepted load has verified.
pub const BEAT_MODEL_SHA256: &str = BEAT.sha256;

fn url(a: &Artifact) -> String {
    format!(
        "https://raw.githubusercontent.com/danigb/beat-this-rs/{REVISION}/models/{}",
        a.name
    )
}

/// Where the model files live under the models cache. Keyed by revision so
/// a future pin never reads files verified against an older one.
pub fn cache_dir(models_dir: &Path) -> PathBuf {
    models_dir.join("beat-this").join(REVISION)
}

/// Whether [`BeatThisTracker::load`] may go to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Use verified cached files or fail. Never downloads.
    CachedOnly,
    /// Download missing or corrupt files from the pinned URLs.
    Download,
}

/// Model output, in seconds. Downbeats are a subset of beat times (the
/// upstream decoder snaps them to the nearest beat).
#[derive(Debug, Clone)]
pub struct Downbeats {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

pub struct BeatThisTracker {
    inner: beat_this::BeatThis<OrtModel>,
}

impl BeatThisTracker {
    /// Verify (and with [`Fetch::Download`], fetch) both model files, then
    /// build their ort sessions.
    pub fn load(models_dir: &Path, fetch: Fetch) -> Result<Self> {
        let dir = cache_dir(models_dir);
        let mel = ensure_artifact(&dir, &MEL, fetch)?;
        let beat = ensure_artifact(&dir, &BEAT, fetch)?;
        let mel = OrtModel(crate::siglip::session_for_current_accelerator(
            &mel, models_dir,
        )?);
        let beat = OrtModel(crate::siglip::session_for_current_accelerator(
            &beat, models_dir,
        )?);
        Ok(Self {
            inner: beat_this::BeatThis::from_models(mel, beat),
        })
    }

    /// Run the full pipeline on mono PCM at [`SAMPLE_RATE`]. Under a second
    /// of audio yields no beats rather than a degenerate model call.
    pub fn analyze(&mut self, pcm: &[f32]) -> Result<Downbeats> {
        if pcm.len() < SAMPLE_RATE as usize {
            return Ok(Downbeats {
                beats: Vec::new(),
                downbeats: Vec::new(),
            });
        }
        // The upstream decoder indexes model outputs by the shapes it
        // expects; a model that answers differently would panic inside it.
        // Surface that as an inference error the caller can fall back from.
        let analysis = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.inner.analyze_audio(pcm, SAMPLE_RATE)
        }))
        .map_err(|_| Error::Inference("Beat This pipeline panicked on model output".into()))?
        .map_err(|e| Error::Inference(format!("Beat This: {e:#}")))?;
        Ok(Downbeats {
            beats: analysis.beats.iter().map(|&t| t as f64).collect(),
            downbeats: analysis.downbeats.iter().map(|&t| t as f64).collect(),
        })
    }
}

/// Return the path of a digest-verified copy of `a` in `dir`.
fn ensure_artifact(dir: &Path, a: &Artifact, fetch: Fetch) -> Result<PathBuf> {
    let path = dir.join(a.name);
    match std::fs::File::open(&path) {
        Ok(file) => {
            let digest = sha256_hex(file)?;
            if digest == a.sha256 {
                return Ok(path);
            }
            if fetch == Fetch::CachedOnly {
                return Err(Error::ModelLoad(format!(
                    "cached {} fails its digest check (sha256 {digest}, expected {}); \
                     delete it, or set EIDETIC_BEAT_TRACKER=beat-this to re-fetch",
                    path.display(),
                    a.sha256
                )));
            }
            tracing::warn!(path = %path.display(), "cached Beat This model is corrupt; re-fetching");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if fetch == Fetch::CachedOnly {
                return Err(Error::ModelLoad(format!(
                    "Beat This model {} is not cached; set EIDETIC_BEAT_TRACKER=beat-this \
                     once to download it (~11 MB)",
                    path.display()
                )));
            }
        }
        Err(e) => return Err(e.into()),
    }

    std::fs::create_dir_all(dir)?;
    let source = url(a);
    tracing::info!(url = %source, "downloading Beat This model");
    eprintln!("Downloading {} ({} bytes) from {source}", a.name, a.bytes);
    let resp = ureq::get(&source)
        .call()
        .map_err(|e| Error::ModelDownload(format!("{source}: {e}")))?;
    publish(resp.into_reader(), dir, a)?;
    Ok(path)
}

/// Stream `reader` into `dir/<a.name>`, publishing it only if it is exactly
/// the pinned length and digest. The temp file removes itself on any early
/// return, so failures leave nothing behind.
fn publish(reader: impl Read, dir: &Path, a: &Artifact) -> Result<()> {
    use sha2::Digest;
    let mut tmp = tempfile::Builder::new()
        .prefix(".beat-this-")
        .suffix(".partial")
        .tempfile_in(dir)?;
    let mut hasher = sha2::Sha256::new();
    // One byte of slack so an oversized body is detected, not truncated
    // into a false match.
    let mut body = reader.take(a.bytes + 1);
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = body
            .read(&mut buf)
            .map_err(|e| Error::ModelDownload(format!("{}: {e}", a.name)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tmp.write_all(&buf[..n])?;
        total += n as u64;
    }
    if total != a.bytes {
        return Err(Error::ModelDownload(format!(
            "{}: got {total} bytes, expected {}",
            a.name, a.bytes
        )));
    }
    let digest = hex(&hasher.finalize());
    if digest != a.sha256 {
        return Err(Error::ModelDownload(format!(
            "{}: sha256 {digest} does not match the pinned {}",
            a.name, a.sha256
        )));
    }
    tmp.as_file().sync_all()?;
    tmp.persist(dir.join(a.name)).map_err(|e| e.error)?;
    Ok(())
}

fn sha256_hex(mut reader: impl Read) -> Result<String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `beat_this::Model` over a workspace-ort session. Adapted from the
/// upstream `runtime/ort.rs` (MIT), minus its forced CoreML registration.
struct OrtModel(ort::session::Session);

impl beat_this::Model for OrtModel {
    fn run(
        &mut self,
        inputs: &[(&str, &beat_this::Tensor)],
    ) -> anyhow::Result<HashMap<String, beat_this::Tensor>> {
        use ort::value::{DynValue, Value};
        // ort::Error is generic over a non-Send recovery type, so it is
        // flattened to its message rather than converted with `?`.
        let err = |e: ort::Error| anyhow::anyhow!(e.to_string());
        let values = inputs
            .iter()
            .map(|(name, t)| {
                let array = ndarray::ArrayD::from_shape_vec(t.shape.clone(), t.data.clone())?;
                let value: DynValue = Value::from_array(array).map_err(err)?.into_dyn();
                Ok((name.to_string(), value))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let refs: Vec<(&str, &DynValue)> = values.iter().map(|(n, v)| (n.as_str(), v)).collect();
        let outputs = self.0.run(refs).map_err(err)?;
        let mut result = HashMap::new();
        for (name, value) in outputs.iter() {
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(err)?;
            let shape = shape
                .iter()
                .map(|&d| usize::try_from(d))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            result.insert(
                name.to_string(),
                beat_this::Tensor {
                    shape,
                    data: data.to_vec(),
                },
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY: Artifact = Artifact {
        name: "tiny.onnx",
        // sha256("hello")
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        bytes: 5,
    };

    #[test]
    fn publish_accepts_exact_bytes_and_rejects_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        publish(&b"hello"[..], dir.path(), &TINY).expect("exact match publishes");
        assert_eq!(
            std::fs::read(dir.path().join("tiny.onnx")).unwrap(),
            b"hello"
        );

        let bad = tempfile::tempdir().unwrap();
        for body in [&b"hellO"[..], b"hell", b"hello!"] {
            let err = publish(body, bad.path(), &TINY).unwrap_err().to_string();
            assert!(err.contains("tiny.onnx"), "{err}");
        }
        // Nothing published, no partial files left behind.
        assert_eq!(std::fs::read_dir(bad.path()).unwrap().count(), 0);
    }

    #[test]
    fn cached_only_never_downloads_a_missing_model() {
        let dir = tempfile::tempdir().unwrap();
        let err = ensure_artifact(dir.path(), &MEL, Fetch::CachedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not cached"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cached_only_refuses_a_corrupt_model() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MEL.name), b"not an onnx file").unwrap();
        let err = ensure_artifact(dir.path(), &MEL, Fetch::CachedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("digest check"), "{err}");
    }

    #[test]
    fn urls_are_pinned_to_an_immutable_commit() {
        for a in [&MEL, &BEAT] {
            let u = url(a);
            assert!(u.starts_with("https://"), "{u}");
            assert!(u.contains(REVISION), "{u}");
            assert_eq!(a.sha256.len(), 64);
        }
    }
}
