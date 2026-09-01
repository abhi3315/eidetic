//! Prompt-driven reel generation (the goals.md stretch goal; ADR-0011).
//!
//! A reel is a consumer of data the library already has: `search_similar`
//! ranks photos and video *moments* (frame timestamps), and ffmpeg cuts the
//! winners into one mp4. Nothing here talks to a model — the prompt was
//! embedded once by the caller.
//!
//! The cut list is a `Vec<Slot>`: a serializable slot per shot, persisted
//! as a reel project file (goals-v0.8.md phase 0) so follow-up edits and
//! agents mutate slots instead of regenerating from scratch.
//!
//! Rendering strategy is deliberately boring: every selected segment is
//! normalised to the same codec/size/fps in its own ffmpeg run, then joined
//! losslessly with the concat demuxer. One subprocess per segment beats one
//! giant filter graph on debuggability, and hard cuts beat xfade on
//! robustness (a failed segment names its source file).

use anyhow::{Context, Result, bail};
use eidetic_core::AssetId;
use eidetic_db::SearchResult;
use eidetic_ingest::beats::BeatGrid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How much of a video is cut around the best-matching frame: 1.5s of lead-in
/// so the moment doesn't open the clip cold, 4s total.
const VIDEO_LEAD_SECS: f64 = 1.5;
const VIDEO_CLIP_SECS: f64 = 4.0;
/// Photos hold for 3s with a slow Ken Burns push-in.
const PHOTO_CLIP_SECS: f64 = 3.0;
/// A hit scoring below this fraction of the best hit is padding, not content.
const RELATIVE_SCORE_FLOOR: f64 = 0.35;
/// Default Laplacian-variance floor for the sharpness gate. Calibrated on
/// real media at 640px: heavy blur lands under ~10, soft-but-usable photos
/// (LFW crowd shots) around 20-70, sharp frames in the hundreds. 12 rejects
/// clear junk without punishing soft content.
pub const MIN_SHARPNESS: f64 = 12.0;

/// Is this hit's matched visual sharp enough to put in a reel? Measured on
/// what would actually render: the photo thumbnail, or the video frame at
/// the matched moment. Unmeasurable (no ffmpeg, undecodable) counts as
/// sharp — the gate exists to catch definite junk, not to veto on doubt.
pub fn sharp_enough(r: &SearchResult, library_dir: &Path, floor: f64) -> bool {
    let score = match r.frame_ts {
        Some(ts) => eidetic_ingest::sharpness::video_sharpness(&r.storage_path, ts)
            .ok()
            .flatten(),
        None => eidetic_ingest::sharpness::image_sharpness(&photo_source(r, library_dir)).ok(),
    };
    match score {
        Some(s) if s < floor => {
            eprintln!(
                "sharpness gate: dropped {} ({s:.1} < {floor:.1})",
                r.storage_path.display()
            );
            false
        }
        _ => true,
    }
}

/// One slot of the cut list — the unit `reel edit` (and later an agent)
/// inspects and mutates, so it carries identity (`asset`) and provenance
/// (`score`) alongside what ffmpeg needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slot {
    /// The library asset this slot cuts from.
    pub asset: AssetId,
    /// The file ffmpeg reads: the original for videos, the upright medium
    /// thumbnail for photos when one exists (see [`photo_source`]).
    pub source: PathBuf,
    pub kind: SlotKind,
    /// In-point in the source, seconds. Always 0 for photos.
    #[serde(default)]
    pub start: f64,
    /// Seconds this slot holds on the timeline.
    pub duration: f64,
    /// Horizontal face centre (0..1 of source width) steering the video
    /// cover-crop window. None = centred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_x: Option<f64>,
    /// The search score that put the asset here; 0 when placed by hand.
    #[serde(default)]
    pub score: f32,
    /// Pinned slots refuse `--swap` and `--drop`: this shot stays.
    #[serde(default)]
    pub pinned: bool,
    /// How this slot joins the previous one; the first slot's value is
    /// ignored.
    #[serde(default)]
    pub transition_in: Transition,
}

/// The ffmpeg treatment for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// A clip cut from a video: cover-crop (focus-steered when faces are
    /// known) or pad/blur per the frame style.
    Video,
    /// A photo held with a slow Ken Burns push-in.
    Photo,
    /// A photo fitted intact over a blurred cover of itself — panoramas
    /// and faceless landscapes.
    PhotoScenic,
}

/// The joint between a slot and its predecessor — the curated transition
/// vocabulary (goals-v0.8.md phase 0.5). Overlap transitions (everything
/// but Cut) consume media the outgoing clip supplies by extending past its
/// out-point; render clamps to what actually exists there and falls back
/// to a cut when almost none does.
///
/// The auto rules stay austere: hard cut on beats, crossfade across
/// section changes, dip-to-black never mid-reel (the render fades the tail
/// out instead). Slide and whip exist for explicit direction — a human's
/// `--transition`, or an agent with an opinion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    #[default]
    Cut,
    /// A dissolve — the section-change softener.
    Crossfade { secs: f64 },
    /// Fade to black, then in from black. Heavy punctuation.
    DipToBlack { secs: f64 },
    /// The incoming shot slides in over the outgoing one.
    Slide { secs: f64 },
    /// A fast slide the eye reads as a whip pan.
    Whip { secs: f64 },
}

impl Transition {
    /// Overlap seconds this joint needs (0 for a hard cut).
    pub fn secs(self) -> f64 {
        match self {
            Transition::Cut => 0.0,
            Transition::Crossfade { secs }
            | Transition::DipToBlack { secs }
            | Transition::Slide { secs }
            | Transition::Whip { secs } => secs,
        }
    }

    /// The ffmpeg xfade `transition` name.
    fn xfade_name(self) -> &'static str {
        match self {
            Transition::Cut => "fade", // unreachable in practice; secs() == 0
            Transition::Crossfade { .. } => "fade",
            Transition::DipToBlack { .. } => "fadeblack",
            Transition::Slide { .. } => "slideleft",
            Transition::Whip { .. } => "slideright",
        }
    }

    /// Parse an edit-op style name, e.g. `crossfade` or `crossfade:0.8`
    /// (seconds suffix optional; each style has a tasteful default).
    pub fn parse(spec: &str) -> Result<Transition> {
        let (name, secs) =
            match spec.split_once(':') {
                Some((n, s)) => (
                    n,
                    Some(s.parse::<f64>().map_err(|_| {
                        anyhow::anyhow!("bad transition duration {s:?} in {spec:?}")
                    })?),
                ),
                None => (spec, None),
            };
        let t = match name {
            "cut" => Transition::Cut,
            "crossfade" | "fade" => Transition::Crossfade {
                secs: secs.unwrap_or(0.5),
            },
            "dip" | "dip-to-black" => Transition::DipToBlack {
                secs: secs.unwrap_or(0.7),
            },
            "slide" => Transition::Slide {
                secs: secs.unwrap_or(0.4),
            },
            "whip" => Transition::Whip {
                secs: secs.unwrap_or(0.2),
            },
            other => bail!(
                "unknown transition {other:?}; valid: cut, crossfade, dip, slide, whip \
                 (optionally with :secs, e.g. crossfade:0.8)"
            ),
        };
        if t.secs() < 0.0 || t.secs() > 3.0 {
            bail!("transition duration {:.2}s is outside 0..3s", t.secs());
        }
        Ok(t)
    }
}

impl Slot {
    pub fn describe(&self) -> String {
        let mut s = match self.kind {
            SlotKind::Video => match self.focus_x {
                Some(fx) => format!(
                    "{} [{:.1}s +{:.1}s, focus x={fx:.2}]",
                    self.source.display(),
                    self.start,
                    self.duration
                ),
                None => format!(
                    "{} [{:.1}s +{:.1}s]",
                    self.source.display(),
                    self.start,
                    self.duration
                ),
            },
            SlotKind::Photo => format!("{} [photo, {:.1}s]", self.source.display(), self.duration),
            SlotKind::PhotoScenic => {
                format!(
                    "{} [photo scenic, {:.1}s]",
                    self.source.display(),
                    self.duration
                )
            }
        };
        if self.transition_in.secs() > 0.0 {
            let name = match self.transition_in {
                Transition::Cut => unreachable!("cut has no duration"),
                Transition::Crossfade { .. } => "xfade",
                Transition::DipToBlack { .. } => "dip-to-black",
                Transition::Slide { .. } => "slide",
                Transition::Whip { .. } => "whip",
            };
            s.push_str(&format!(" [{name} {:.1}s in]", self.transition_in.secs()));
        }
        if self.pinned {
            s.push_str(" [pinned]");
        }
        s
    }
}

/// Total timeline length of a cut list.
pub fn total_secs(slots: &[Slot]) -> f64 {
    slots.iter().map(|s| s.duration).sum()
}

fn video_slot(r: &SearchResult, start: f64, duration: f64, focus_x: Option<f64>) -> Slot {
    Slot {
        asset: r.id,
        source: r.storage_path.clone(),
        kind: SlotKind::Video,
        start,
        duration,
        focus_x,
        score: r.score,
        pinned: false,
        transition_in: Transition::Cut,
    }
}

fn photo_slot(r: &SearchResult, library_dir: &Path, duration: f64, scenic: bool) -> Slot {
    Slot {
        asset: r.id,
        source: photo_source(r, library_dir),
        kind: if scenic {
            SlotKind::PhotoScenic
        } else {
            SlotKind::Photo
        },
        start: 0.0,
        duration,
        focus_x: None,
        score: r.score,
        pinned: false,
        transition_in: Transition::Cut,
    }
}

/// Turn ranked search results into a cut list no longer than `target_secs`.
///
/// Results are consumed in score order. Weak tails are dropped: anything
/// scoring below zero, or below [`RELATIVE_SCORE_FLOOR`] of the top hit,
/// never enters the reel even if there is room.
///
/// `library_dir` locates photo thumbnails — see [`photo_source`]. `scenes`
/// holds per-video scene boundaries (empty entry or missing key = single
/// shot / undetected) so cuts snap to shots — see [`clip_bounds`].
pub fn plan(
    results: &[SearchResult],
    target_secs: f64,
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut total = 0.0f64;
    for r in strong_prefix(results) {
        let slot = match r.frame_ts {
            Some(ts) => {
                let empty = Vec::new();
                let bounds = scenes.get(&r.id).unwrap_or(&empty);
                let (start, len) = clip_bounds(ts, r.duration_secs, bounds);
                video_slot(r, start, len, None)
            }
            None => photo_slot(r, library_dir, PHOTO_CLIP_SECS, false),
        };
        if total + slot.duration > target_secs && !slots.is_empty() {
            break;
        }
        total += slot.duration;
        slots.push(slot);
    }
    slots
}

/// The prefix of (score-sorted) `results` that clears the weak-tail rules:
/// positive score, at least [`RELATIVE_SCORE_FLOOR`] of the top hit. This
/// is the pool every planner consumes — exposed so `--variations` can
/// rotate it without re-deriving the floor from a rotated (weaker) top.
pub fn strong_prefix(results: &[SearchResult]) -> &[SearchResult] {
    let top = results.first().map(|r| r.score as f64).unwrap_or(0.0);
    let floor = (top * RELATIVE_SCORE_FLOOR).max(0.0);
    let n = results
        .iter()
        .take_while(|r| r.score > 0.0 && (r.score as f64) >= floor)
        .count();
    &results[..n]
}

/// Re-style a variation's joints (goals-v0.8.md phase 0.5): style 0 keeps
/// the planned austere joints, 1 dissolves every joint (soft), 2 turns the
/// section crossfades into whips (punchy). Styles cycle for higher counts.
pub fn restyle(slots: &mut [Slot], style: usize) {
    match style % 3 {
        0 => {}
        1 => {
            for s in slots.iter_mut().skip(1) {
                s.transition_in = Transition::Crossfade { secs: 0.4 };
            }
        }
        _ => {
            for s in slots.iter_mut().skip(1) {
                if matches!(s.transition_in, Transition::Crossfade { .. }) {
                    s.transition_in = Transition::Whip { secs: 0.2 };
                }
            }
        }
    }
}

/// How segments are fitted to the frame.
#[derive(Clone, Copy, PartialEq)]
pub enum Fit {
    /// Fit inside, pad with black (the landscape default).
    Pad,
    /// Fill the frame, crop the overflow.
    Cover,
    /// Fit inside over a blurred, darkened cover of itself — the cinematic
    /// treatment for scenic footage in a mismatched frame: no black bars,
    /// no butchered panoramas.
    Blur,
    /// Per-clip: faces present → Cover (following them); no faces → Blur.
    Auto,
}

impl Fit {
    /// Parse a frame style (--frame, or a project file's `frame` field).
    /// `portrait` picks what auto means: portrait output follows faces,
    /// landscape keeps the classic pad.
    pub fn parse(frame: &str, portrait: bool) -> Result<Fit> {
        Ok(match frame {
            "auto" if portrait => Fit::Auto,
            // Landscape output fits landscape sources anyway; auto keeps
            // the classic pad there.
            "auto" => Fit::Pad,
            "cover" => Fit::Cover,
            "blur" => Fit::Blur,
            "pad" => Fit::Pad,
            other => {
                bail!("invalid frame style {other:?}; valid values: auto, cover, blur, pad")
            }
        })
    }

    /// Resolve Auto per segment: `focused` = this clip has face data.
    fn resolve(self, focused: bool) -> Fit {
        match self {
            Fit::Auto if focused => Fit::Cover,
            Fit::Auto => Fit::Blur,
            other => other,
        }
    }
}

/// Render the cut list to `output` at `width`x`height`, 30fps, H.264. With
/// `audio`, the track is laid under the cut, trimmed to the reel length
/// with a one-second fade-out.
pub fn render(
    slots: &[Slot],
    output: &Path,
    width: u32,
    height: u32,
    fit: Fit,
    audio: Option<&Path>,
) -> Result<()> {
    let Some(ffmpeg) = eidetic_ingest::video::ffmpeg() else {
        bail!("ffmpeg not found; install it (or set EIDETIC_FFMPEG_PATH) to render reels");
    };
    if slots.is_empty() {
        bail!("nothing matched confidently enough to cut a reel");
    }

    let workdir = tempfile::tempdir().context("create reel work dir")?;
    let mut seg_paths = Vec::with_capacity(slots.len());

    for (i, slot) in slots.iter().enumerate() {
        // An overlap transition into the NEXT slot needs this slot to run
        // `fade` secs past its out-point — the media the joint consumes.
        // Clamped to what exists there; near-zero falls back to a hard cut.
        let fade = match slots.get(i + 1).map(|n| n.transition_in.secs()) {
            Some(secs) if secs > 0.0 => extendable_secs(ffmpeg, slot, secs),
            _ => 0.0,
        };
        let seg_path = workdir.path().join(format!("seg_{i:03}.mp4"));
        let rendered = slot.duration + fade;
        let mut vf = match slot.kind {
            SlotKind::Video => match slot.focus_x {
                Some(fx) => focus_filter(width, height, fit.resolve(true), fx),
                None => normalize_filter(width, height, fit.resolve(false)),
            },
            SlotKind::Photo => ken_burns_filter(width, height, rendered),
            SlotKind::PhotoScenic => blur_fill_filter(width, height),
        };
        // Real phone footage carries non-square sample aspect ratios; the
        // concat/xfade filters refuse inputs whose SARs differ, so every
        // segment is squared here (found dogfooding on real trip media —
        // synthetic test sources are always 1:1 already).
        vf.push_str(",setsar=1");
        // Dip-to-black as an ENDING is the one place it always earns its
        // keep: with music, the video tail fades with the audio's fade.
        // The fade shrinks with a short final slot (drop sections end on
        // 2-beat cuts) rather than being skipped — an abrupt final frame
        // reads as a glitch either way.
        if i + 1 == slots.len() && audio.is_some() {
            let d = (slot.duration / 2.0).min(0.5);
            vf.push_str(&format!(
                ",fade=t=out:st={:.3}:d={d:.3}",
                (slot.duration - d).max(0.0)
            ));
        }
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-y", "-v", "error"]);
        match slot.kind {
            SlotKind::Video => {
                cmd.args(["-ss", &format!("{:.3}", slot.start)]);
            }
            SlotKind::Photo | SlotKind::PhotoScenic => {
                cmd.args(["-loop", "1"]);
            }
        }
        let status = cmd
            .arg("-i")
            .arg(&slot.source)
            .args(["-t", &format!("{rendered:.3}")])
            .args(["-vf", &vf])
            .args(["-an", "-c:v", "libx264", "-preset", "veryfast"])
            .arg(&seg_path)
            .status()
            .context("run ffmpeg for a segment")?;
        if !status.success() {
            bail!("ffmpeg failed on segment {}: {}", i + 1, slot.describe());
        }
        seg_paths.push((seg_path, fade));
    }

    let total = total_secs(slots);
    let silent = match audio {
        None => output.to_path_buf(),
        Some(_) => workdir.path().join("silent.mp4"),
    };
    if seg_paths.iter().all(|(_, fade)| *fade == 0.0) {
        concat_copy(ffmpeg, &seg_paths, workdir.path(), &silent)?;
    } else {
        xfade_chain(ffmpeg, &seg_paths, slots, &silent)?;
    }

    if let Some(track) = audio {
        let fade_start = (total - 1.0).max(0.0);
        let status = Command::new(ffmpeg)
            .args(["-y", "-v", "error"])
            .arg("-i")
            .arg(&silent)
            .arg("-i")
            .arg(track)
            .args(["-map", "0:v", "-map", "1:a", "-c:v", "copy"])
            .args(["-c:a", "aac", "-b:a", "192k"])
            .args([
                "-af",
                &format!("afade=t=out:st={fade_start:.3}:d=1"),
                "-t",
                &format!("{total:.3}"),
            ])
            .arg(output)
            .status()
            .context("run ffmpeg audio mux")?;
        if !status.success() {
            bail!("ffmpeg audio mux failed");
        }
    }
    Ok(())
}

/// Lossless join for an all-cuts reel: the concat demuxer with `-c copy`.
fn concat_copy(
    ffmpeg: &Path,
    seg_paths: &[(PathBuf, f64)],
    workdir: &Path,
    output: &Path,
) -> Result<()> {
    let mut list = String::new();
    for (p, _) in seg_paths {
        // concat-demuxer syntax; single quotes inside paths are escaped as '\''
        list.push_str(&format!(
            "file '{}'\n",
            p.display().to_string().replace('\'', "'\\''")
        ));
    }
    let list_path = workdir.join("concat.txt");
    std::fs::write(&list_path, list).context("write concat list")?;
    let status = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0"])
        .arg("-i")
        .arg(&list_path)
        .args(["-c", "copy"])
        .arg(output)
        .status()
        .context("run ffmpeg concat")?;
    if !status.success() {
        bail!("ffmpeg concat failed");
    }
    Ok(())
}

/// Join segments with per-joint transitions: hard cuts use the concat
/// filter, crossfades use xfade. One re-encode of the whole timeline.
///
/// Beat sync survives because each fading segment was rendered `fade` secs
/// LONGER than its slot: the dissolve consumes exactly that extension, so
/// every later cut still lands where the grid put it. xfade's offset is
/// therefore the sum of *nominal* durations so far, not of file lengths.
fn xfade_chain(
    ffmpeg: &Path,
    seg_paths: &[(PathBuf, f64)],
    slots: &[Slot],
    output: &Path,
) -> Result<()> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-v", "error"]);
    for (p, _) in seg_paths {
        cmd.arg("-i").arg(p);
    }
    let mut graph = String::new();
    // xfade refuses inputs with mismatched timebases (mp4 segments carry
    // 1/15360 while filter outputs carry 1/1000000), so every input is
    // normalised to AVTB first.
    for i in 0..seg_paths.len() {
        graph.push_str(&format!("[{i}:v]settb=AVTB[s{i}];"));
    }
    let mut acc = "s0".to_string();
    let mut offset = slots[0].duration;
    for i in 1..seg_paths.len() {
        let out = format!("v{i}");
        let fade = seg_paths[i - 1].1;
        if fade > 0.0 {
            let name = slots[i].transition_in.xfade_name();
            graph.push_str(&format!(
                "[{acc}][s{i}]xfade=transition={name}:duration={fade:.3}:offset={offset:.3}[{out}];"
            ));
        } else {
            graph.push_str(&format!("[{acc}][s{i}]concat=n=2:v=1:a=0[{out}];"));
        }
        offset += slots[i].duration;
        acc = out;
    }
    graph.pop(); // trailing ';'
    let status = cmd
        .args(["-filter_complex", &graph])
        .args(["-map", &format!("[{acc}]")])
        .args(["-c:v", "libx264", "-preset", "veryfast"])
        .arg(output)
        .status()
        .context("run ffmpeg transition chain")?;
    if !status.success() {
        bail!("ffmpeg transition chain failed");
    }
    Ok(())
}

/// How many seconds past its out-point `slot` can actually extend, capped
/// at `want`. Photos loop forever; videos are probed (a project file may
/// have been retimed by hand since planning). Under 0.15s a dissolve reads
/// as a glitch, so it degrades to a cut (0.0).
fn extendable_secs(ffmpeg: &Path, slot: &Slot, want: f64) -> f64 {
    let avail = match slot.kind {
        SlotKind::Photo | SlotKind::PhotoScenic => want,
        SlotKind::Video => {
            let _ = ffmpeg; // probing uses ffprobe, located independently
            match eidetic_ingest::video::probe(&slot.source) {
                Ok(Some(p)) => p
                    .duration_secs
                    .map(|d| (d - slot.start - slot.duration).max(0.0))
                    .unwrap_or(0.0)
                    .min(want),
                _ => 0.0,
            }
        }
    };
    if avail < 0.15 { 0.0 } else { avail }
}

/// Cut spans from a beat grid (goals-v0.7.md phase 1): each span starts on
/// a beat and runs 4 beats in normal sections, 2 in high-energy ones —
/// the convention auto-editors converge on — with a 0.6 s floor absorbing
/// very fast tempi. Spans stop at `total_secs`.
pub fn beat_spans(grid: &BeatGrid, total_secs: f64) -> Vec<(f64, f64)> {
    const MIN_SPAN: f64 = 0.6;
    let mut spans = Vec::new();
    let beats = &grid.beats;
    if beats.is_empty() {
        return spans;
    }
    let mut i = 0usize;
    while i < beats.len() {
        let start = beats[i];
        if start >= total_secs {
            break;
        }
        let step = if grid.is_high_energy(start) { 2 } else { 4 };
        let mut j = (i + step).min(beats.len());
        // Absorb beats until the span clears the floor (very fast tempi).
        while j < beats.len() && beats[j] - start < MIN_SPAN {
            j += 1;
        }
        let end = if j < beats.len() {
            beats[j]
        } else {
            total_secs
        };
        let end = end.min(total_secs);
        if end - start >= MIN_SPAN || spans.is_empty() {
            spans.push((start, end));
        }
        if j == i {
            break;
        }
        i = j;
    }
    // Lead-in before the first beat merges into the first span.
    if let Some(first) = spans.first_mut() {
        first.0 = 0.0;
    }
    spans
}

/// Seconds a crossfade takes at an energy-section boundary.
const SECTION_FADE_SECS: f64 = 0.5;

/// Beat-synced plan: each music span gets one shot, taken in score order
/// with the same weak-tail rules as [`plan`]. A video hit fills its span
/// exactly (sync beats scene purity: if the shot's scene is shorter than
/// the span, the clip crosses the boundary rather than breaking the grid);
/// photos hold for the span. The reel ends early if strong shots run out.
///
/// Joints where the music crosses an energy-section boundary are marked as
/// crossfades (the cut vocabulary's one softener): the eye reads the
/// dissolve as "the song changed here".
///
/// Music structure shapes the assignment (goals-v0.8.md phase 0.5): the
/// hero shots are saved for the drops. The first span of each high-energy
/// section gets the best remaining hit; every other span takes the rest in
/// score order — so the reel still opens strong, but its strongest moment
/// lands where the song peaks.
pub fn plan_synced(
    results: &[SearchResult],
    grid: &BeatGrid,
    total_secs: f64,
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
    focus: &HashMap<AssetId, f64>,
) -> Vec<Slot> {
    let spans = beat_spans(grid, total_secs);
    let strong = strong_prefix(results);

    let n = spans.len().min(strong.len());
    let hot: Vec<bool> = spans
        .iter()
        .map(|(a, b)| grid.is_high_energy((a + b) / 2.0))
        .collect();
    // Which hit fills which span: drops first (best hits), rest in order.
    let mut hit_for_span = vec![usize::MAX; n];
    let mut next = 0usize;
    for i in 0..n {
        if hot[i] && (i == 0 || !hot[i - 1]) {
            hit_for_span[i] = next;
            next += 1;
        }
    }
    for h in &mut hit_for_span {
        if *h == usize::MAX {
            *h = next;
            next += 1;
        }
    }

    let mut slots = Vec::new();
    let mut total = 0.0;
    for (i, (_, end)) in spans.iter().enumerate().take(n) {
        let span = end - total;
        let slot = fill_span(&strong[hit_for_span[i]], span, library_dir, scenes, focus);
        total += slot.duration;
        slots.push(slot);
    }
    for i in 1..slots.len() {
        if hot[i - 1] != hot[i] {
            slots[i].transition_in = Transition::Crossfade {
                secs: SECTION_FADE_SECS,
            };
        }
    }
    slots
}

/// One shot filling one span exactly: videos cut around their matched
/// moment (shot-boundary aligned when possible; sync beats scene purity),
/// photos hold — Ken Burns when faces are present, scenic blur-fill when
/// not.
pub fn fill_span(
    r: &SearchResult,
    span: f64,
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
    focus: &HashMap<AssetId, f64>,
) -> Slot {
    match r.frame_ts {
        Some(ts) => {
            let duration = r.duration_secs.unwrap_or(f64::MAX);
            let span = span.min(duration);
            let empty = Vec::new();
            let bounds = scenes.get(&r.id).unwrap_or(&empty);
            let shot_start = bounds
                .iter()
                .copied()
                .filter(|s| *s <= ts)
                .fold(0.0, f64::max);
            // Start at the shot boundary when the span fits after it;
            // otherwise center on the moment and clamp to the file.
            let start = if duration - shot_start >= span && ts - shot_start <= span {
                shot_start
            } else {
                (ts - span / 2.0).clamp(0.0, (duration - span).max(0.0))
            };
            video_slot(r, start, span, focus.get(&r.id).copied())
        }
        None => photo_slot(r, library_dir, span, !focus.contains_key(&r.id)),
    }
}

/// Voiceover plan: each pair is (span seconds, the shot chosen for what is
/// being said during that span) — see `speech_spans` and the reel command.
pub fn plan_assigned(
    pairs: &[(f64, SearchResult)],
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
    focus: &HashMap<AssetId, f64>,
) -> Vec<Slot> {
    pairs
        .iter()
        .map(|(span, r)| fill_span(r, *span, library_dir, scenes, focus))
        .collect()
}

/// Merge raw speech segments into presentation spans covering the narration
/// contiguously from t=0: each span at least `min` seconds (visuals must
/// not flash mid-sentence), split into equal slices when longer than `max`.
/// Returns (start, end, spoken text) per span; the last span stretches to
/// `total` so the tail of the narration keeps a picture.
pub fn speech_spans(
    segments: &[(f64, f64, String)],
    min: f64,
    max: f64,
    total: f64,
) -> Vec<(f64, f64, String)> {
    let mut spans: Vec<(f64, f64, String)> = Vec::new();
    let mut i = 0;
    while i < segments.len() {
        let start = spans.last().map(|s| s.1).unwrap_or(0.0);
        if start >= total {
            break;
        }
        let mut end = segments[i].1;
        let mut text = segments[i].2.clone();
        let mut j = i + 1;
        while end - start < min && j < segments.len() {
            end = segments[j].1;
            text.push(' ');
            text.push_str(&segments[j].2);
            j += 1;
        }
        let end = end.min(total).max(start + min.min(total - start));
        let len = end - start;
        let slices = (len / max).ceil().max(1.0) as usize;
        let slice = len / slices as f64;
        for k in 0..slices {
            spans.push((
                start + k as f64 * slice,
                start + (k + 1) as f64 * slice,
                text.clone(),
            ));
        }
        i = j;
    }
    if let Some(last) = spans.last_mut()
        && last.1 < total
    {
        last.1 = total;
    }
    spans
}

/// Where to cut a video segment around the matched moment `ts`.
///
/// The clip stays inside the shot containing `ts` (goals-v0.5.md #3): a cut
/// mid-shot looks like an editing accident, and crossing a boundary splices
/// in unrelated footage. Within the shot the usual window applies — up to
/// [`VIDEO_LEAD_SECS`] of lead-in, [`VIDEO_CLIP_SECS`] total — and a shot
/// shorter than the window is used whole.
fn clip_bounds(ts: f64, duration_secs: Option<f64>, scenes: &[f64]) -> (f64, f64) {
    let duration = duration_secs.unwrap_or(f64::MAX);
    let shot_start = scenes
        .iter()
        .copied()
        .filter(|s| *s <= ts)
        .fold(0.0, f64::max);
    let shot_end = scenes
        .iter()
        .copied()
        .filter(|s| *s > ts)
        .fold(duration, f64::min);

    let start = (ts - VIDEO_LEAD_SECS).max(shot_start);
    let len = (shot_end - start).clamp(0.5, VIDEO_CLIP_SECS);
    (start, len)
}

/// The file ffmpeg should read for a photo segment.
///
/// The original can be HEIC or DNG (which ffmpeg does not decode) and its
/// EXIF orientation would be ignored even for JPEG. The medium thumbnail is
/// an upright 1024px JPEG, so prefer it whenever it exists; fall back to the
/// original for the rare un-thumbnailed photo.
pub fn photo_source(r: &SearchResult, library_dir: &Path) -> PathBuf {
    if r.thumbnails_generated
        && let Some(hash) = r
            .storage_path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(eidetic_core::Sha256::from_hex)
    {
        return eidetic_ingest::thumbnail::thumbnail_path(
            library_dir,
            &hash,
            eidetic_ingest::thumbnail::ThumbSize::Medium,
        );
    }
    r.storage_path.clone()
}

/// Cover-crop centred on a horizontal focus point (0..1 of source width) —
/// the crop window slides to keep faces in frame, clamped to the image.
/// Pad mode has no crop to steer, so focus degrades to the plain filter.
fn focus_filter(w: u32, h: u32, fit: Fit, fx: f64) -> String {
    match fit {
        Fit::Pad | Fit::Blur | Fit::Auto => normalize_filter(w, h, fit),
        Fit::Cover => format!(
            "scale={w}:{h}:force_original_aspect_ratio=increase,\
             crop={w}:{h}:'clip(iw*{fx:.4}-ow/2,0,iw-ow)':'(ih-oh)/2',\
             fps=30,format=yuv420p"
        ),
    }
}

/// Fit the input to the frame per `fit`, normalise fps/pixfmt so concat's
/// `-c copy` join is legal across segments.
fn normalize_filter(w: u32, h: u32, fit: Fit) -> String {
    match fit {
        Fit::Pad => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps=30,format=yuv420p"
        ),
        Fit::Cover => format!(
            "scale={w}:{h}:force_original_aspect_ratio=increase,\
             crop={w}:{h},fps=30,format=yuv420p"
        ),
        Fit::Blur | Fit::Auto => blur_fill_filter(w, h),
    }
}

/// The cinematic mismatched-aspect treatment: the frame is a blurred,
/// darkened cover of the clip, with the clip itself fitted inside intact.
fn blur_fill_filter(w: u32, h: u32) -> String {
    format!(
        "split[bg][fg];\
         [bg]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},\
         boxblur=20:2,eq=brightness=-0.15[b];\
         [fg]scale={w}:{h}:force_original_aspect_ratio=decrease[f];\
         [b][f]overlay=(W-w)/2:(H-h)/2,fps=30,format=yuv420p"
    )
}

/// Ken Burns for stills: cover-crop to the frame, then a slow centred
/// push-in. `d` is in *output frames* (30fps x hold seconds).
fn ken_burns_filter(w: u32, h: u32, secs: f64) -> String {
    let frames = (secs * 30.0).round().max(1.0) as u32;
    // Zoom rate scales so the push-in always lands at ~1.15x regardless of
    // how long the beat span holds the photo.
    let rate = 0.15 / frames as f64;
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},\
         zoompan=z='min(zoom+{rate:.6},1.15)':\
         x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':\
         d={frames}:s={w}x{h}:fps=30,format=yuv420p"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::AssetId;

    fn lib() -> PathBuf {
        PathBuf::from("/lib")
    }

    fn hit(score: f32, frame_ts: Option<f64>, duration: Option<f64>) -> SearchResult {
        SearchResult {
            id: AssetId::new(),
            storage_path: PathBuf::from("/x"),
            score,
            frame_ts,
            duration_secs: duration,
            speech: None,
            mime_type: None,
            file_size: 0,
            thumbnails_generated: false,
            date_taken: None,
            camera_make: None,
            camera_model: None,
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn plan_respects_target_duration() {
        // Ten strong photo hits at 3s each against a 10s target: 3 fit.
        let results: Vec<_> = (0..10).map(|_| hit(0.5, None, None)).collect();
        let slots = plan(&results, 10.0, &lib(), &HashMap::new());
        assert_eq!(slots.len(), 3);
        assert!((total_secs(&slots) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn plan_drops_weak_tail_even_with_room() {
        let results = vec![hit(0.5, None, None), hit(0.1, None, None)];
        let slots = plan(&results, 60.0, &lib(), &HashMap::new());
        assert_eq!(slots.len(), 1, "0.1 < 35% of 0.5 is padding");
    }

    #[test]
    fn plan_drops_non_positive_scores() {
        let results = vec![hit(0.0, None, None), hit(-0.2, None, None)];
        assert!(plan(&results, 30.0, &lib(), &HashMap::new()).is_empty());
    }

    #[test]
    fn video_clip_leads_into_the_moment_and_respects_bounds() {
        // Moment at 9s of a 10s video: start = 7.5, len clamped to 2.5.
        let results = vec![hit(0.5, Some(9.0), Some(10.0))];
        let slots = plan(&results, 30.0, &lib(), &HashMap::new());
        assert_eq!(slots[0].kind, SlotKind::Video);
        assert!((slots[0].start - 7.5).abs() < 1e-9);
        assert!((slots[0].duration - 2.5).abs() < 1e-9);

        // Moment at 0.5s: lead-in clamps to the file start.
        let results = vec![hit(0.5, Some(0.5), Some(10.0))];
        let slots = plan(&results, 30.0, &lib(), &HashMap::new());
        assert_eq!(slots[0].start, 0.0);
        assert!((slots[0].duration - 4.0).abs() < 1e-9);
    }

    #[test]
    fn slots_carry_identity_and_score() {
        let r = hit(0.7, Some(2.0), Some(30.0));
        let slots = plan(std::slice::from_ref(&r), 30.0, &lib(), &HashMap::new());
        assert_eq!(slots[0].asset, r.id);
        assert!((slots[0].score - 0.7).abs() < 1e-6);
        assert!(!slots[0].pinned);
        assert_eq!(slots[0].transition_in, Transition::Cut);
    }

    #[test]
    fn photos_render_from_the_medium_thumbnail_when_present() {
        let hash = "ab".repeat(32);
        let mut thumbed = hit(0.5, None, None);
        thumbed.storage_path = PathBuf::from(format!("/lib/ab/ab/{hash}.heic"));
        thumbed.thumbnails_generated = true;
        let slots = plan(&[thumbed], 30.0, &lib(), &HashMap::new());
        assert_eq!(slots[0].kind, SlotKind::Photo);
        let s = slots[0].source.to_str().unwrap();
        assert!(s.contains(".thumbs/m/"), "expected thumb path, got {s}");
        assert!(s.ends_with(".jpg"));

        // No thumbnail -> the original is used as-is.
        let raw = hit(0.5, None, None);
        let slots = plan(&[raw], 30.0, &lib(), &HashMap::new());
        assert_eq!(slots[0].source, PathBuf::from("/x"));
    }

    #[test]
    fn clip_stays_inside_its_shot() {
        // Moment at 10s, shot spans [8, 11) of a 60s video: the clip may not
        // reach back past 8 nor forward past 11.
        let (start, len) = clip_bounds(10.0, Some(60.0), &[8.0, 11.0, 20.0]);
        assert_eq!(start, 8.5, "lead-in clamped to the shot start");
        assert!((start + len - 11.0).abs() < 1e-9, "ends at the boundary");

        // No scenes: the plain window applies.
        let (start, len) = clip_bounds(10.0, Some(60.0), &[]);
        assert_eq!((start, len), (8.5, 4.0));

        // Tiny shot [9.8, 10.4): used whole, floored at half a second.
        let (start, len) = clip_bounds(10.0, Some(60.0), &[9.8, 10.4]);
        assert!((start - 9.8).abs() < 1e-9);
        assert!((len - 0.6).abs() < 1e-9);
    }

    fn grid_120bpm(secs: f64) -> BeatGrid {
        let beats: Vec<f64> = (0..)
            .map(|i| i as f64 * 0.5)
            .take_while(|b| *b < secs)
            .collect();
        BeatGrid {
            bpm: 120.0,
            downbeats: (0..beats.len()).step_by(4).collect(),
            beats,
            high_energy: vec![],
        }
    }

    #[test]
    fn beat_spans_cut_every_four_beats_normally() {
        let spans = beat_spans(&grid_120bpm(20.0), 10.0);
        // 4 beats at 120 BPM = 2 s per span, 5 spans in 10 s.
        assert_eq!(spans.len(), 5, "{spans:?}");
        assert!((spans[0].1 - spans[0].0 - 2.0).abs() < 1e-9);
        assert_eq!(spans[0].0, 0.0, "lead-in merges into the first span");
    }

    #[test]
    fn beat_spans_densify_in_high_energy_sections() {
        let mut grid = grid_120bpm(20.0);
        grid.high_energy = vec![(4.0, 8.0)];
        let spans = beat_spans(&grid, 12.0);
        let in_hot: Vec<f64> = spans
            .iter()
            .filter(|(a, _)| *a >= 4.0 && *a < 8.0)
            .map(|(a, b)| b - a)
            .collect();
        assert!(!in_hot.is_empty());
        assert!(
            in_hot.iter().all(|d| (*d - 1.0).abs() < 1e-9),
            "2-beat cuts in the hot section: {in_hot:?}"
        );
    }

    #[test]
    fn plan_synced_fills_spans_and_ends_when_shots_run_out() {
        let grid = grid_120bpm(30.0);
        // Two strong hits for many spans: reel ends after two spans.
        let results = vec![hit(0.5, Some(5.0), Some(60.0)), hit(0.5, None, None)];
        let slots = plan_synced(
            &results,
            &grid,
            20.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(slots.len(), 2);
        assert!((slots[0].duration - 2.0).abs() < 1e-9, "span-sized clip");
        assert!((total_secs(&slots) - 4.0).abs() < 1e-9);
        // No face data in the test fixtures, so the photo renders
        // scenic (blur-fill) — the auto-framing default.
        assert_eq!(slots[1].kind, SlotKind::PhotoScenic);
        assert!((slots[1].duration - 2.0).abs() < 1e-9);
    }

    #[test]
    fn plan_synced_marks_crossfades_at_section_boundaries() {
        let mut grid = grid_120bpm(30.0);
        // Sections: calm [0,4), hot [4,8), calm [8,...). At 120 BPM the
        // spans are [0,2) [2,4) [4,5) [5,6) [6,7) [7,8) [8,10) ...
        grid.high_energy = vec![(4.0, 8.0)];
        let results: Vec<_> = (0..12).map(|_| hit(0.5, None, None)).collect();
        let slots = plan_synced(
            &results,
            &grid,
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let spans = beat_spans(&grid, 12.0);
        let fades: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.transition_in, Transition::Crossfade { .. }))
            .map(|(i, _)| i)
            .collect();
        // Exactly the two joints where the energy state flips.
        let expected: Vec<usize> = (1..spans.len())
            .filter(|&i| {
                grid.is_high_energy((spans[i - 1].0 + spans[i - 1].1) / 2.0)
                    != grid.is_high_energy((spans[i].0 + spans[i].1) / 2.0)
            })
            .collect();
        assert_eq!(fades, expected);
        assert_eq!(fades.len(), 2, "into the hot section and out of it");
    }

    #[test]
    fn plan_synced_saves_the_hero_shot_for_the_drop() {
        let mut grid = grid_120bpm(30.0);
        grid.high_energy = vec![(4.0, 8.0)];
        // Distinct descending scores so slots are traceable to hits.
        let results: Vec<_> = (0..12)
            .map(|i| hit(0.5 - i as f32 * 0.01, None, None))
            .collect();
        let slots = plan_synced(
            &results,
            &grid,
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
        );
        // Spans: [0,2) [2,4) | hot [4,5) [5,6) [6,7) [7,8) | [8,10) [10,12).
        // The first hot span (index 2) is the drop: it gets the best hit.
        assert!((slots[2].score - 0.5).abs() < 1e-6, "hero on the drop");
        // The reel still opens with the best remaining hit.
        assert!((slots[0].score - 0.49).abs() < 1e-6);
        // Everything else keeps score order down the timeline.
        assert!(slots[3].score > slots[4].score);
    }

    #[test]
    fn plan_always_takes_at_least_one_strong_hit() {
        // Target shorter than the first segment still yields that segment.
        let results = vec![hit(0.5, Some(5.0), Some(60.0))];
        assert_eq!(plan(&results, 1.0, &lib(), &HashMap::new()).len(), 1);
    }

    #[test]
    fn transition_parse_names_and_durations() {
        assert_eq!(Transition::parse("cut").unwrap(), Transition::Cut);
        assert_eq!(
            Transition::parse("crossfade").unwrap(),
            Transition::Crossfade { secs: 0.5 }
        );
        assert_eq!(
            Transition::parse("crossfade:0.8").unwrap(),
            Transition::Crossfade { secs: 0.8 }
        );
        assert_eq!(
            Transition::parse("dip").unwrap(),
            Transition::DipToBlack { secs: 0.7 }
        );
        assert_eq!(
            Transition::parse("whip").unwrap(),
            Transition::Whip { secs: 0.2 }
        );
        assert!(Transition::parse("zoom").is_err());
        assert!(Transition::parse("slide:9").is_err(), "outside 0..3s");
        assert!(Transition::parse("slide:x").is_err());
    }

    #[test]
    fn slot_serde_round_trips() {
        let r = hit(0.42, Some(3.0), Some(20.0));
        let mut slots = plan(std::slice::from_ref(&r), 30.0, &lib(), &HashMap::new());
        slots[0].pinned = true;
        slots[0].transition_in = Transition::Crossfade { secs: 0.5 };
        let json = serde_json::to_string(&slots).unwrap();
        let back: Vec<Slot> = serde_json::from_str(&json).unwrap();
        assert_eq!(back[0].asset, slots[0].asset);
        assert_eq!(back[0].kind, SlotKind::Video);
        assert!(back[0].pinned);
        assert_eq!(back[0].transition_in, Transition::Crossfade { secs: 0.5 });
    }
}
