//! Real-inference smoke test for the Beat This path (`--features downbeats`).
//!
//! Needs the two pinned ONNX files but never downloads them: it reads them
//! from `EIDETIC_BEAT_THIS_MODELS` (a directory holding
//! `mel_spectrogram.onnx` + `beat_this_small.onnx`), falling back to the
//! repo-local `tmp/beat-this-support/`. With neither present it skips and
//! says so. The files are digest-checked by the same `load` production
//! uses, so a wrong file fails here instead of passing vacuously.
#![cfg(feature = "downbeats")]

use eidetic_ml::downbeats::{self, BeatThisTracker, Fetch, SAMPLE_RATE};
use std::path::PathBuf;

fn supplied_models() -> Option<PathBuf> {
    let dir = std::env::var_os("EIDETIC_BEAT_THIS_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tmp/beat-this-support")
        });
    dir.join("beat_this_small.onnx").is_file().then_some(dir)
}

/// 120 BPM for 20 s: a kick-like low thump on every beat with a louder,
/// longer thump plus a noise crash on every 4th — a bar the model can hear.
fn synthetic_track() -> Vec<f32> {
    let sr = SAMPLE_RATE as f64;
    let n = (20.0 * sr) as usize;
    let beat = (0.5 * sr) as usize;
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed as f64 / u64::MAX as f64) as f32 - 0.5
    };
    (0..n)
        .map(|i| {
            let k = i / beat;
            let t = (i % beat) as f64 / sr;
            let accent = k.is_multiple_of(4);
            let decay = (-t * if accent { 12.0 } else { 25.0 }).exp() as f32;
            let freq = 60.0 + 90.0 * (-t * 30.0).exp();
            let kick = (2.0 * std::f64::consts::PI * freq * t).sin() as f32 * decay;
            let hat = if accent {
                noise() * 0.6 * decay
            } else {
                noise() * 0.15 * decay
            };
            0.7 * kick + hat + noise() * 0.003
        })
        .collect()
}

#[test]
fn beat_this_infers_beats_and_downbeats_from_synthetic_audio() {
    let Some(src) = supplied_models() else {
        eprintln!(
            "SKIPPED: no Beat This model files (set EIDETIC_BEAT_THIS_MODELS or provide \
             tmp/beat-this-support/); this test never downloads"
        );
        return;
    };
    let models = tempfile::tempdir().unwrap();
    let cache = downbeats::cache_dir(models.path());
    std::fs::create_dir_all(&cache).unwrap();
    for name in ["mel_spectrogram.onnx", "beat_this_small.onnx"] {
        std::fs::copy(src.join(name), cache.join(name)).unwrap();
    }

    let mut tracker =
        BeatThisTracker::load(models.path(), Fetch::CachedOnly).expect("supplied models verify");
    let out = tracker.analyze(&synthetic_track()).expect("inference runs");
    eprintln!("beats ({}): {:?}", out.beats.len(), out.beats);
    eprintln!("downbeats ({}): {:?}", out.downbeats.len(), out.downbeats);

    assert!(
        out.beats.len() >= 20,
        "expected ~40 beats, got {}",
        out.beats.len()
    );
    let mut intervals: Vec<f64> = out.beats.windows(2).map(|w| w[1] - w[0]).collect();
    intervals.sort_by(f64::total_cmp);
    let median = intervals[intervals.len() / 2];
    assert!((median - 0.5).abs() < 0.05, "median beat interval {median}");
    assert!(out.downbeats.len() >= 5, "downbeats: {:?}", out.downbeats);
    // Bars of four 0.5 s beats. Phase is deliberately not asserted: on this
    // synthetic pattern the pinned model places the bar line two beats off
    // the louder hit (observed 1.02, 3.02, ...), and a smoke test should not
    // encode a musical judgement about a click track.
    let mut bars: Vec<f64> = out.downbeats.windows(2).map(|w| w[1] - w[0]).collect();
    bars.sort_by(f64::total_cmp);
    let bar = bars[bars.len() / 2];
    assert!((bar - 2.0).abs() < 0.1, "median bar length {bar}");
    for d in &out.downbeats {
        assert!(
            out.beats.iter().any(|b| (b - d).abs() < 1e-6),
            "downbeat {d} is not a beat"
        );
    }
}
