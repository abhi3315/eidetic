//! Speech-to-text over video audio via whisper.cpp (goals-v0.5.md #4).
//!
//! Only compiled with the `speech` feature: whisper-rs builds C through
//! cmake, which default builds must not require (ADR-0009). Transcripts are
//! recall fuel for search, never display text — casual home-video audio
//! (noise, distance, code-switched speech) transcribes too roughly to show.

use crate::{Error, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// One spoken span.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechSegment {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// Model files from the whisper.cpp GGML conversions on HuggingFace.
/// `small` is the multilingual sweet spot for CPU; turbo is the quality
/// option once GPU decode lands.
fn model_file(raw: Option<&str>) -> Result<&'static str> {
    match raw {
        Some("base") => Ok("ggml-base.bin"),
        None | Some("small") => Ok("ggml-small.bin"),
        Some("large-v3-turbo") => Ok("ggml-large-v3-turbo-q5_0.bin"),
        Some(other) => Err(Error::ModelLoad(format!(
            "Unknown EIDETIC_SPEECH_MODEL={other:?}; valid values: base, small, large-v3-turbo"
        ))),
    }
}

/// Owns a loaded whisper model. Synchronous like the other model wrappers:
/// drive it from a blocking worker.
pub struct SpeechTranscriber {
    ctx: WhisperContext,
}

impl SpeechTranscriber {
    /// Load the model selected by `EIDETIC_SPEECH_MODEL` (default `small`,
    /// ~466 MB), downloading on first use like the SigLIP models.
    pub fn load(models_dir: &Path) -> Result<Self> {
        let file = model_file(std::env::var("EIDETIC_SPEECH_MODEL").ok().as_deref())?;
        let path = crate::siglip::download(models_dir, "ggerganov/whisper.cpp", file)?;
        let ctx = WhisperContext::new_with_params(
            path.to_str()
                .ok_or_else(|| Error::ModelLoad("model path is not valid UTF-8".to_string()))?,
            WhisperContextParameters::default(),
        )
        .map_err(|e| Error::ModelLoad(format!("load whisper model: {e}")))?;
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 PCM into timestamped segments.
    ///
    /// Language is auto-detected per clip (home libraries mix languages).
    /// Empty and whitespace-only segments are dropped.
    pub fn transcribe(&self, pcm: &[f32]) -> Result<Vec<SpeechSegment>> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| Error::Inference(format!("whisper state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("auto"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, pcm)
            .map_err(|e| Error::Inference(format!("whisper inference: {e}")))?;

        let n = state.full_n_segments();
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map_err(|e| Error::Inference(e.to_string()))?
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            // Segment timestamps are in centiseconds.
            out.push(SpeechSegment {
                start_secs: segment.start_timestamp() as f64 / 100.0,
                end_secs: segment.end_timestamp() as f64 / 100.0,
                text,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_file_resolves_and_rejects() {
        assert_eq!(model_file(None).unwrap(), "ggml-small.bin");
        assert_eq!(model_file(Some("base")).unwrap(), "ggml-base.bin");
        assert!(model_file(Some("tiny-typo")).is_err());
    }

    /// Real-model round trip on the classic whisper.cpp sample. Set
    /// EIDETIC_MODELS_CACHE and EIDETIC_SPEECH_TEST_WAV (a 16 kHz mono
    /// 16-bit PCM WAV) to run.
    #[test]
    #[ignore = "requires whisper model + a wav; set EIDETIC_MODELS_CACHE and EIDETIC_SPEECH_TEST_WAV"]
    fn transcribes_known_speech() {
        let models_dir = std::env::var("EIDETIC_MODELS_CACHE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_MODELS_CACHE");
        let wav_path = std::env::var("EIDETIC_SPEECH_TEST_WAV")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_SPEECH_TEST_WAV");

        // Minimal WAV parse: 44-byte canonical header, 16-bit little-endian.
        let bytes = std::fs::read(&wav_path).expect("read wav");
        let pcm: Vec<f32> = bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();

        let transcriber = SpeechTranscriber::load(&models_dir).expect("load model");
        let segments = transcriber.transcribe(&pcm).expect("transcribe");
        let all: String = segments
            .iter()
            .map(|s| s.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        println!("segments: {segments:?}");
        assert!(!segments.is_empty());
        assert!(
            all.contains("country"),
            "JFK sample should mention 'country': {all}"
        );
        assert!(segments[0].end_secs > segments[0].start_secs);
    }
}
