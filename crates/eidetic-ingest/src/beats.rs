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

/// A track's rhythm, as the reel planner consumes it.
#[derive(Debug, Clone)]
pub struct BeatGrid {
    pub bpm: f64,
    /// Beat times in seconds, ascending.
    pub beats: Vec<f64>,
    /// Indices into `beats` that start a bar (every 4th beat, anchored on
    /// the strongest onset — the pragmatic stand-in for real downbeat ML).
    pub downbeats: Vec<usize>,
    /// High-energy spans (seconds): where cuts should get denser.
    pub high_energy: Vec<(f64, f64)>,
}

impl BeatGrid {
    /// Is `t` inside a high-energy section?
    pub fn is_high_energy(&self, t: f64) -> bool {
        self.high_energy.iter().any(|(a, b)| t >= *a && t < *b)
    }
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

    Some(BeatGrid {
        bpm,
        beats,
        downbeats,
        high_energy: high_energy_spans(pcm),
    })
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
        for tau in from..=to {
            let interval = (t - tau) as f64;
            let penalty = TIGHTNESS * (interval / p).ln().powi(2);
            let s = score[tau] - penalty;
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

/// Spans where short-term RMS runs well above the track median — the
/// chorus/drop sections where cuts should get denser.
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
    // 40th percentile, not median: choruses can be half the track, and a
    // median that already sits in the loud section would flag nothing.
    let floor = sorted[sorted.len() * 2 / 5];
    if floor <= 1e-9 {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, r) in rms.iter().enumerate() {
        let hot = *r > floor * 1.3;
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
}
