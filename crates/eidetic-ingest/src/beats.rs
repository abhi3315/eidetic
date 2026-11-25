//! Beat tracking for music-synced reels (goals-v0.7.md, phase 1).
//!
//! The classic pipeline, hand-rolled in pure Rust: spectral-flux onset
//! envelope → autocorrelation tempo estimate → Ellis dynamic-programming
//! beat placement. This is the same algorithm as `librosa.beat.beat_track`
//! (Ellis 2007, "Beat Tracking by Dynamic Programming"), chosen over the
//! alternatives deliberately: aubio's Rust bindings are unmaintained, the
//! young pure-Rust beat crates are unproven, and this is ~300 lines against
//! `rustfft` with no native dependency.
//!
//! Accuracy target is *cut-on-beat video editing*, not musicology: a
//! consistent grid matters more than winning MIREX. Constant-tempo pop /
//! film-score / EDM — i.e. reel music — is the supported case; tracks with
//! no stable tempo return `None` and the caller falls back to fixed
//! intervals.

use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

/// Input sample rate — the 16 kHz mono PCM `video::extract_audio_pcm`
/// produces. Onsets only need content below ~8 kHz, so this is plenty.
pub const SAMPLE_RATE: f64 = 16_000.0;

const FFT_SIZE: usize = 1024;
const HOP: usize = 256;
/// Seconds per onset-envelope frame.
const FRAME_SECS: f64 = HOP as f64 / SAMPLE_RATE;
/// Tempo search range. Below 60 BPM cuts drag; above 180 the "beat" for
/// editing purposes is really the half-tempo pulse.
const BPM_MIN: f64 = 60.0;
const BPM_MAX: f64 = 180.0;
/// librosa's default DP tightness: how strongly intervals are pulled toward
/// the estimated period.
const TIGHTNESS: f64 = 100.0;

/// A track's rhythm, as the reel planner consumes it. Serializable because
/// reel project files persist it (goals-v0.8.md phase 0).
///
/// Every way in — tracking, [`BeatGrid::from_inferred`], deserialization —
/// goes through [`BeatGrid::sanitize`], so a grid never holds a non-finite,
/// negative or unsorted beat, nor a downbeat index past the end of `beats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawGrid")]
pub struct BeatGrid {
    pub bpm: f64,
    /// Beat times in seconds, ascending.
    pub beats: Vec<f64>,
    /// Indices into `beats` that start a bar. What they mean depends on
    /// `tracker`: model-inferred bar starts for [`Tracker::BeatThis`], but
    /// for [`Tracker::Ellis`] only every 4th beat anchored on the strongest
    /// onset — a guess, not a detected bar. [`BeatGrid::downbeats_inferred`]
    /// is the one question planners should ask.
    pub downbeats: Vec<usize>,
    /// High-energy spans (seconds): where cuts should get denser.
    pub high_energy: Vec<(f64, f64)>,
    /// Which tracker produced this grid. Project files written before
    /// v0.9 lack the field and load as Ellis, which is what made them.
    pub tracker: Tracker,
    /// The model behind an inferred grid.
    pub model: Option<ModelProvenance>,
    /// Why a Beat This run was attempted and this Ellis grid used instead.
    pub fallback: Option<String>,
}

/// The beat tracker behind a [`BeatGrid`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tracker {
    /// The in-crate spectral-flux + dynamic-programming tracker. Its bars
    /// are heuristic.
    #[default]
    Ellis,
    /// Beat This! (ISMIR 2024) model inference: beats and downbeats.
    BeatThis,
}

/// Identity of the model files an inferred grid came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProvenance {
    pub name: String,
    /// Immutable upstream revision the files were fetched from.
    pub revision: String,
    /// SHA-256 of the beat model the digest check accepted.
    pub sha256: String,
}

/// The wire shape of [`BeatGrid`]: identical fields, optional where older
/// project files omit them. Converted through `sanitize` on the way in.
#[derive(Deserialize)]
struct RawGrid {
    bpm: f64,
    beats: Vec<f64>,
    downbeats: Vec<usize>,
    high_energy: Vec<(f64, f64)>,
    #[serde(default)]
    tracker: Tracker,
    #[serde(default)]
    model: Option<ModelProvenance>,
    #[serde(default)]
    fallback: Option<String>,
}

impl From<RawGrid> for BeatGrid {
    fn from(r: RawGrid) -> Self {
        BeatGrid {
            bpm: r.bpm,
            beats: r.beats,
            downbeats: r.downbeats,
            high_energy: r.high_energy,
            tracker: r.tracker,
            model: r.model,
            fallback: r.fallback,
        }
        .sanitize()
    }
}

impl BeatGrid {
    /// Is `t` inside a high-energy section?
    pub fn is_high_energy(&self, t: f64) -> bool {
        self.high_energy.iter().any(|(a, b)| t >= *a && t < *b)
    }

    /// Are `downbeats` real, model-detected bar starts? False for Ellis
    /// grids, whose bars are every 4th beat by assumption.
    pub fn downbeats_inferred(&self) -> bool {
        self.tracker == Tracker::BeatThis && self.model.is_some()
    }

    /// A grid from model output: beat and downbeat *times* (seconds), as
    /// Beat This reports them. Each downbeat maps to the nearest beat
    /// within 70 ms; one that matches no beat is dropped rather than
    /// invented. `pcm` is the 16 kHz track, used for the energy sections.
    /// `None` when fewer than four usable beats remain — no tempo.
    pub fn from_inferred(
        beats: &[f64],
        downbeat_times: &[f64],
        pcm: &[f32],
        model: ModelProvenance,
    ) -> Option<Self> {
        const SNAP_SECS: f64 = 0.07;
        let mut beats: Vec<f64> = beats
            .iter()
            .copied()
            .filter(|t| t.is_finite() && *t >= 0.0)
            .collect();
        beats.sort_by(f64::total_cmp);
        beats.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        if beats.len() < 4 {
            return None;
        }
        let mut downbeats: Vec<usize> = downbeat_times
            .iter()
            .filter(|t| t.is_finite())
            .filter_map(|&t| {
                let i = beats.partition_point(|b| *b < t);
                [i.checked_sub(1), Some(i)]
                    .into_iter()
                    .flatten()
                    .filter(|&j| j < beats.len())
                    .min_by(|&a, &b| (beats[a] - t).abs().total_cmp(&(beats[b] - t).abs()))
                    .filter(|&j| (beats[j] - t).abs() <= SNAP_SECS)
            })
            .collect();
        downbeats.sort_unstable();
        downbeats.dedup();
        Some(
            BeatGrid {
                bpm: median_bpm(&beats),
                beats,
                downbeats,
                high_energy: high_energy_spans(pcm),
                tracker: Tracker::BeatThis,
                model: Some(model),
                fallback: None,
            }
            .sanitize(),
        )
    }

    /// Enforce the grid invariants: beats finite, non-negative, strictly
    /// ascending; downbeats strictly ascending indices that still point at
    /// the same beat *times* they did before cleaning; energy spans finite
    /// and non-empty; bpm finite and positive (re-derived when not).
    pub fn sanitize(mut self) -> Self {
        let downbeat_times: Vec<f64> = self
            .downbeats
            .iter()
            .filter_map(|&i| self.beats.get(i).copied())
            .collect();
        self.beats.retain(|t| t.is_finite() && *t >= 0.0);
        self.beats.sort_by(f64::total_cmp);
        self.beats.dedup();
        self.downbeats = downbeat_times
            .iter()
            .filter_map(|t| self.beats.binary_search_by(|b| b.total_cmp(t)).ok())
            .collect();
        self.downbeats.sort_unstable();
        self.downbeats.dedup();
        self.high_energy
            .retain(|(a, b)| a.is_finite() && b.is_finite() && a < b);
        if !(self.bpm.is_finite() && self.bpm > 0.0) {
            self.bpm = median_bpm(&self.beats);
        }
        self
    }
}

/// Tempo from the median inter-beat interval; 0 when it cannot be told.
fn median_bpm(beats: &[f64]) -> f64 {
    let mut intervals: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.sort_by(f64::total_cmp);
    let mid = intervals[intervals.len() / 2];
    if mid > 0.0 { 60.0 / mid } else { 0.0 }
}

/// Track beats in 16 kHz mono PCM. `None` when no stable tempo exists
/// (speech, ambience, silence) — the caller falls back to fixed intervals.
pub fn track_beats(pcm: &[f32]) -> Option<BeatGrid> {
    if pcm.len() < FFT_SIZE * 8 {
        return None;
    }
    let envelope = onset_envelope(pcm);
    let period = estimate_period(&envelope)?;
    let beat_frames = dp_beats(&envelope, period);
    if beat_frames.len() < 4 {
        return None;
    }

    let beats: Vec<f64> = beat_frames.iter().map(|&f| f as f64 * FRAME_SECS).collect();
    let bpm = 60.0 / (period * FRAME_SECS);

    // Downbeat anchor: the beat with the strongest onset energy.
    let anchor = beat_frames
        .iter()
        .enumerate()
        .max_by(|a, b| envelope[*a.1].total_cmp(&envelope[*b.1]))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let downbeats: Vec<usize> = (0..beats.len()).filter(|i| i % 4 == anchor % 4).collect();

    Some(
        BeatGrid {
            bpm,
            beats,
            downbeats,
            high_energy: high_energy_spans(pcm),
            tracker: Tracker::Ellis,
            model: None,
            fallback: None,
        }
        .sanitize(),
    )
}

/// Log-compressed spectral flux, locally mean-subtracted and rectified.
fn onset_envelope(pcm: &[f32]) -> Vec<f64> {
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window: Vec<f64> = (0..FFT_SIZE)
        .map(|i| {
            let x = std::f64::consts::PI * i as f64 / FFT_SIZE as f64;
            x.sin() * x.sin() // Hann
        })
        .collect();

    let frames = (pcm.len() - FFT_SIZE) / HOP + 1;
    let mut prev_mag = vec![0.0f64; FFT_SIZE / 2];
    let mut flux = Vec::with_capacity(frames);
    let mut buf = vec![Complex::new(0.0, 0.0); FFT_SIZE];

    for f in 0..frames {
        let start = f * HOP;
        for i in 0..FFT_SIZE {
            buf[i] = Complex::new(pcm[start + i] as f64 * window[i], 0.0);
        }
        fft.process(&mut buf);

        let mut sum = 0.0;
        for (k, prev) in prev_mag.iter_mut().enumerate() {
            // Log compression flattens loudness differences so quiet
            // sections still contribute onsets.
            let mag = (1.0 + 100.0 * buf[k].norm()).ln();
            sum += (mag - *prev).max(0.0);
            *prev = mag;
        }
        flux.push(sum);
    }

    // Subtract a ~0.5 s moving average and rectify: keeps transients,
    // drops the slow loudness contour.
    let half = (0.25 / FRAME_SECS) as usize;
    let mut env = vec![0.0f64; flux.len()];
    for i in 0..flux.len() {
        let a = i.saturating_sub(half);
        let b = (i + half + 1).min(flux.len());
        let mean = flux[a..b].iter().sum::<f64>() / (b - a) as f64;
        env[i] = (flux[i] - mean).max(0.0);
    }
    // Normalise to unit std so DP tightness behaves consistently.
    let std = (env.iter().map(|x| x * x).sum::<f64>() / env.len() as f64).sqrt();
    if std > 1e-12 {
        env.iter_mut().for_each(|x| *x /= std);
    }
    env
}

/// Autocorrelation tempo estimate with a log-Gaussian prior around 120 BPM.
/// Returns the beat period in envelope frames, or `None` when the best peak
/// barely beats the autocorrelation floor (no rhythm to lock onto).
fn estimate_period(env: &[f64]) -> Option<f64> {
    let min_lag = (60.0 / BPM_MAX / FRAME_SECS).round() as usize;
    let max_lag = (60.0 / BPM_MIN / FRAME_SECS).round() as usize;
    if env.len() < max_lag * 2 {
        return None;
    }

    // Centered, normalised autocorrelation: rhythmic material shows a real
    // peak (r/r0 well above zero at the period); noise and speech hover
    // near zero at every lag.
    let mean = env.iter().sum::<f64>() / env.len() as f64;
    let centered: Vec<f64> = env.iter().map(|x| x - mean).collect();
    let r0: f64 = centered.iter().map(|x| x * x).sum::<f64>() / centered.len() as f64;
    if r0 <= 1e-12 {
        return None;
    }

    let mut best: Option<(usize, f64, f64)> = None; // (lag, prior*r_norm, r_norm)
    for lag in min_lag..=max_lag {
        let mut r = 0.0;
        for t in 0..centered.len() - lag {
            r += centered[t] * centered[t + lag];
        }
        let r_norm = r / (centered.len() - lag) as f64 / r0;
        let period_secs = lag as f64 * FRAME_SECS;
        let octaves = (period_secs / 0.5).log2(); // 0.5 s = 120 BPM
        let prior = (-0.5 * (octaves / 1.0).powi(2)).exp();
        let score = r_norm * prior;
        if best.is_none_or(|(_, b, _)| score > b) {
            best = Some((lag, score, r_norm));
        }
    }
    let (lag, _, r_norm) = best?;
    // The gate: an actual pulse correlates strongly at its period. 0.25 is
    // comfortably cleared by any track with a beat and comfortably missed
    // by noise, ambience and most speech.
    if r_norm < 0.25 {
        return None;
    }
    Some(lag as f64)
}

/// Ellis DP: place beats so onset strength is high AND intervals stay close
/// to the period. `score[t] = env[t] + max over τ (score[τ] - T·log²((t-τ)/P))`.
fn dp_beats(env: &[f64], period: f64) -> Vec<usize> {
    let n = env.len();
    let p = period;
    let lo = (p * 0.5).round() as usize;
    let hi = (p * 2.0).round() as usize;

    let mut score = vec![0.0f64; n];
    let mut back = vec![usize::MAX; n];
    for t in 0..n {
        score[t] = env[t];
        if t < lo {
            continue;
        }
        let from = t.saturating_sub(hi);
        let to = t - lo;
        let mut best = f64::MIN;
        let mut best_tau = usize::MAX;
        for (tau, prev_score) in score.iter().enumerate().take(to + 1).skip(from) {
            let interval = (t - tau) as f64;
            let penalty = TIGHTNESS * (interval / p).ln().powi(2);
            let s = prev_score - penalty;
            if s > best {
                best = s;
                best_tau = tau;
            }
        }
        if best_tau != usize::MAX && best > 0.0 {
            score[t] += best;
            back[t] = best_tau;
        }
    }

    // Best endpoint in the final period, then backtrack.
    let tail_start = n.saturating_sub(hi.max(1));
    let Some(mut t) = (tail_start..n).max_by(|a, b| score[*a].total_cmp(&score[*b])) else {
        return Vec::new();
    };
    let mut beats = vec![t];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t);
    }
    beats.reverse();
    beats
}

/// Spans where short-term RMS runs well above the track's quiet sections —
/// the chorus/drop sections where cuts should get denser.
///
/// The rule is contrast-based, not floor-based. The first version used
/// "1.3 x the 40th-percentile RMS", which worked on synthetic clicks but
/// went blind on real masters: an Instagram-style track spends ~80% of its
/// runtime in the loud section, so *any* mid percentile lands inside the
/// chorus and nothing clears 1.3x it (found dogfooding v0.8 on real trip
/// media). Instead: take the 10th percentile as the quiet reference and
/// the 80th as the loud one; if they differ by less than 1.3x the track is
/// genuinely flat (club-style constant bangers — correctly no sections);
/// otherwise split the two levels at their geometric mean.
fn high_energy_spans(pcm: &[f32]) -> Vec<(f64, f64)> {
    let win = SAMPLE_RATE as usize; // 1 s windows
    if pcm.len() < win * 4 {
        return Vec::new();
    }
    let rms: Vec<f64> = pcm
        .chunks(win)
        .map(|c| (c.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(f64::total_cmp);
    let quiet = sorted[sorted.len() / 10];
    let loud = sorted[sorted.len() * 4 / 5];
    if quiet <= 1e-9 || loud < quiet * 1.3 {
        return Vec::new();
    }
    let threshold = (quiet * loud).sqrt();

    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, r) in rms.iter().enumerate() {
        let hot = *r > threshold;
        match (hot, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s as f64, i as f64));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s as f64, rms.len() as f64));
    }
    // Merge blips: keep spans of at least 3 s.
    spans.retain(|(a, b)| b - a >= 3.0);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic click track: decaying noise bursts at a fixed BPM over a
    /// quiet noise floor.
    fn click_track(bpm: f64, secs: f64) -> Vec<f32> {
        let n = (secs * SAMPLE_RATE) as usize;
        let period = (60.0 / bpm * SAMPLE_RATE) as usize;
        let mut pcm = vec![0.0f32; n];
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as f64 / u64::MAX as f64) as f32 - 0.5
        };
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = rng() * 0.01; // floor
            let since = i % period;
            if since < 800 {
                // 50 ms decaying burst on each beat
                *s += rng() * (1.0 - since as f32 / 800.0);
            }
        }
        pcm
    }

    #[test]
    fn recovers_tempo_and_phase_of_a_click_track() {
        for bpm in [90.0, 120.0, 150.0] {
            let grid = track_beats(&click_track(bpm, 20.0)).expect("stable tempo");
            assert!(
                (grid.bpm - bpm).abs() < bpm * 0.04,
                "expected ~{bpm} BPM, got {:.1}",
                grid.bpm
            );
            // Beats should land near multiples of the true period.
            let period = 60.0 / bpm;
            let aligned = grid
                .beats
                .iter()
                .filter(|b| {
                    let phase = (*b % period).min(period - *b % period);
                    // STFT smearing gives this algorithm class a known
                    // 20-60ms late bias (librosa#1052); 80ms is under 2.5
                    // frames at 30fps and invisible in a cut.
                    phase < 0.08
                })
                .count();
            assert!(
                aligned * 10 >= grid.beats.len() * 7,
                "{bpm} BPM: only {aligned}/{} beats within 80ms of the grid",
                grid.beats.len()
            );
        }
    }

    #[test]
    fn beats_are_ascending_and_reasonably_spaced() {
        let grid = track_beats(&click_track(120.0, 15.0)).expect("grid");
        for w in grid.beats.windows(2) {
            let dt = w[1] - w[0];
            assert!(dt > 0.2 && dt < 1.2, "odd interval {dt}");
        }
        assert!(!grid.downbeats.is_empty());
        for w in grid.downbeats.windows(2) {
            assert_eq!(w[1] - w[0], 4, "downbeats every 4 beats");
        }
    }

    #[test]
    fn silence_and_noise_return_none() {
        assert!(track_beats(&vec![0.0f32; 160_000]).is_none(), "silence");
        let mut seed = 42u64;
        let noise: Vec<f32> = (0..160_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as f64 / u64::MAX as f64) as f32 - 0.5
            })
            .collect();
        assert!(track_beats(&noise).is_none(), "white noise has no tempo");
    }

    #[test]
    fn high_energy_flags_the_loud_half() {
        // 10 s quiet, 10 s loud.
        let mut pcm = vec![0.005f32; 160_000];
        pcm.extend(vec![0.3f32; 160_000]);
        let spans = high_energy_spans(&pcm);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(spans[0].0 >= 9.0 && spans[0].0 <= 11.0, "{spans:?}");
    }

    /// The real-master regression (v0.8 dogfooding): a short quiet intro
    /// and a drop that runs ~80% of the track. Any mid-percentile floor
    /// sits inside the loud section and flags nothing; the contrast rule
    /// must still find the drop.
    #[test]
    fn high_energy_finds_a_drop_that_dominates_the_track() {
        // 12 s intro at ~0.16 RMS, 48 s drop at ~0.5 — the shape of the
        // actual Instagram-style track that exposed the bug.
        let mut pcm = vec![0.16f32; 12 * 16_000];
        pcm.extend(vec![0.5f32; 48 * 16_000]);
        let spans = high_energy_spans(&pcm);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(spans[0].0 >= 11.0 && spans[0].0 <= 13.0, "{spans:?}");
        assert!(
            (spans[0].1 - 60.0).abs() <= 1.0,
            "runs to the end: {spans:?}"
        );
    }

    #[test]
    fn ellis_grids_never_claim_inferred_downbeats() {
        let grid = track_beats(&click_track(120.0, 15.0)).expect("grid");
        assert_eq!(grid.tracker, Tracker::Ellis);
        assert!(!grid.downbeats_inferred());
        assert!(grid.model.is_none() && grid.fallback.is_none());
    }

    /// A v0.8 project file's grid: no tracker/model/fallback fields.
    #[test]
    fn pre_v09_grids_load_as_heuristic_ellis() {
        let json = r#"{"bpm":120.0,"beats":[0.0,0.5,1.0,1.5,2.0],
                       "downbeats":[0,4],"high_energy":[]}"#;
        let grid: BeatGrid = serde_json::from_str(json).expect("old grid parses");
        assert_eq!(grid.tracker, Tracker::Ellis);
        assert!(!grid.downbeats_inferred());
        assert_eq!(grid.downbeats, vec![0, 4]);
    }

    /// A hand-edited or damaged grid: out-of-order and negative beats,
    /// indices past the end, a zero-length energy span, a bogus bpm.
    /// Loading cleans it; downbeats keep pointing at the same *times*.
    #[test]
    fn deserialization_sanitizes_hostile_grids() {
        let json = r#"{"bpm":-3.0,"beats":[1.0,-2.0,0.5,1.5,1.0,2.0],
                       "downbeats":[0,1,3,99],"high_energy":[[4.0,4.0],[1.0,3.0]],
                       "tracker":"beat-this",
                       "model":{"name":"m","revision":"r","sha256":"s"}}"#;
        let grid: BeatGrid = serde_json::from_str(json).expect("parses");
        assert_eq!(grid.beats, vec![0.5, 1.0, 1.5, 2.0]);
        // Old index 0 was t=1.0 (now index 1); index 1 was t=-2.0 (gone);
        // index 3 was t=1.5 (now 2); 99 never existed.
        assert_eq!(grid.downbeats, vec![1, 2]);
        assert_eq!(grid.high_energy, vec![(1.0, 3.0)]);
        assert!((grid.bpm - 120.0).abs() < 1e-9);
        assert!(grid.downbeats_inferred());
    }

    fn provenance() -> ModelProvenance {
        ModelProvenance {
            name: "beat_this_small".into(),
            revision: "rev".into(),
            sha256: "abc".into(),
        }
    }

    #[test]
    fn inferred_grids_snap_downbeats_and_drop_non_finite_times() {
        let beats = [0.0, f64::NAN, 0.5, 1.0, f64::INFINITY, 1.5, 2.0, -1.0, 2.5];
        // 0.02 snaps to beat 0; 1.48 snaps to 1.5; 2.25 is between beats
        // and matches nothing within 70 ms, so it is dropped, not invented.
        let downbeats = [0.02, 1.48, 2.25, f64::NAN];
        let grid = BeatGrid::from_inferred(&beats, &downbeats, &[], provenance()).expect("grid");
        assert_eq!(grid.beats, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
        assert_eq!(grid.downbeats, vec![0, 3]);
        assert!((grid.bpm - 120.0).abs() < 1e-9);
        assert!(grid.downbeats_inferred());
        assert!(grid.fallback.is_none());
    }

    #[test]
    fn inferred_output_with_too_few_beats_is_no_tempo() {
        assert!(
            BeatGrid::from_inferred(&[0.0, 0.5, f64::NAN], &[0.0], &[], provenance()).is_none()
        );
    }

    /// A loudness-flat track (constant club banger) has no sections at all
    /// — dense cuts throughout would be the wrong call to force.
    #[test]
    fn high_energy_stays_silent_on_flat_tracks() {
        let pcm = vec![0.4f32; 40 * 16_000];
        assert!(high_energy_spans(&pcm).is_empty());
    }
}
