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
/// The longest joint a transition may last; project validation uses it too.
pub const MAX_TRANSITION_SECS: f64 = 3.0;
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
    /// Measured pixel motion (0..1) at the moment this video slot was cut
    /// around. Always None for photos; None for a video means "not
    /// measured" (embedded before motion scoring, or a project file from
    /// before it), never "still".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<f32>,
    /// Dimensions and metadata rotation of what the render reads. None =
    /// not probed (older project file, or unreadable source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
    /// Other slots of the cut list that repeat this shot: the same footage
    /// (same photo, or the same sampled moment of the same video), or a
    /// stored embedding within [`NEAR_DUPLICATE`]. `Some(vec![])` =
    /// checked, nothing repeats; None = never checked (older project file,
    /// or no embedding from the active model and no identical footage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_duplicates: Option<Vec<SlotRef>>,
}

/// Names a slot independently of its position, so annotations survive
/// reorders and drops: the asset plus the in-point it is cut from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SlotRef {
    pub asset: AssetId,
    pub start: f64,
}

impl SlotRef {
    fn of(slot: &Slot) -> SlotRef {
        SlotRef {
            asset: slot.asset,
            start: slot.start,
        }
    }

    fn names(&self, slot: &Slot) -> bool {
        self.asset == slot.asset && (self.start - slot.start).abs() < 1e-6
    }
}

/// What container metadata says about a slot's picture. Metadata only: it
/// can warn that a rotation will NOT be applied, but a zero rotation is no
/// proof the pixels are upright (a phone with orientation lock records
/// sideways content at rotation 0). Only looking at a frame settles that.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    /// Pixel size of the file the render reads (display size for videos,
    /// i.e. after rotation; the upright thumbnail for thumbnailed photos).
    pub width: u32,
    pub height: u32,
    /// Clockwise rotation the source's metadata asks for (EXIF Orientation
    /// or the video displaymatrix): 0, 90, 180 or 270. None = none found.
    pub rotation_cw: Option<u16>,
    /// Whether the render applies that rotation: thumbnails are
    /// pre-rotated and ffmpeg autorotates video. False (a photo rendered
    /// from its original) means the shot may come out sideways.
    pub rotation_applied: bool,
}

impl Geometry {
    /// Metadata asks for a rotation the render will not perform.
    pub fn may_render_sideways(&self) -> bool {
        !self.rotation_applied && self.rotation_cw.is_some_and(|r| r % 180 != 0)
    }
}

/// Cosine at or above which two stored embeddings count as the same scene
/// for planning. Far looser than `eidetic dupes` (0.985, "same pixels"):
/// the target here is three files of one posing session, not re-exports.
/// SigLIP is not calibrated for this, so it is a heuristic threshold.
pub const NEAR_DUPLICATE: f32 = 0.92;

/// How much a hot-section span values motion relative to semantic score.
/// Scores are normalised to the pool's best hit (1.0) and motion to the
/// pool's measured range, so the most-moving measured clip gains up to
/// this much over a still of equal score.
pub const MOTION_WEIGHT: f64 = 0.3;

/// Which moment of the library a candidate or slot shows: the asset plus,
/// for videos, the time asked about (a hit's matched frame, a slot's
/// midpoint). Signals are keyed by this, never by asset alone — two slots
/// of one video can show different scenes with different motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MomentRef {
    pub asset: AssetId,
    pub at: Option<f64>,
}

impl MomentRef {
    pub fn of_hit(r: &SearchResult) -> MomentRef {
        MomentRef {
            asset: r.id,
            at: r.frame_ts,
        }
    }

    pub fn of_slot(s: &Slot) -> MomentRef {
        MomentRef {
            asset: s.asset,
            at: (s.kind == SlotKind::Video).then_some(s.start + s.duration / 2.0),
        }
    }

    fn key(self) -> (AssetId, Option<u64>) {
        (self.asset, self.at.map(f64::to_bits))
    }
}

/// One moment's stored signals, as the planner sees it.
#[derive(Debug, Clone, Default)]
pub struct Moment {
    /// The sampled frame the signals were read from (videos); None for
    /// photos, or a video with no stored frames.
    pub frame: Option<f64>,
    /// Embedding from the active model; None = nothing comparable.
    pub embedding: Option<Vec<f32>>,
    /// Measured motion; None for photos and unmeasured moments.
    pub motion: Option<f32>,
}

/// Plan-time quality signals per [`MomentRef`]. Empty = no signals, and
/// every planner then behaves exactly as it did before signals existed.
#[derive(Debug, Default)]
pub struct Signals {
    moments: HashMap<(AssetId, Option<u64>), Moment>,
}

impl Signals {
    pub fn insert(&mut self, at: MomentRef, moment: Moment) {
        self.moments.insert(at.key(), moment);
    }

    pub fn get(&self, at: MomentRef) -> Option<&Moment> {
        self.moments.get(&at.key())
    }

    /// Cosine of two moments' stored embeddings, when both are known,
    /// same-width and finite (the loader already dropped other models).
    pub fn similarity(&self, a: MomentRef, b: MomentRef) -> Option<f32> {
        let va = self.get(a)?.embedding.as_ref()?;
        let vb = self.get(b)?.embedding.as_ref()?;
        if va.len() != vb.len() || va.is_empty() {
            return None;
        }
        let dot: f32 = va.iter().zip(vb).map(|(x, y)| x * y).sum();
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let cos = dot / (norm(va) * norm(vb));
        cos.is_finite().then_some(cos)
    }

    /// The same footage: one photo twice, or one video at the same sampled
    /// frame (the query time when no frames are stored). Needs no
    /// embedding, so it holds even for unembedded or other-model assets.
    pub fn same_footage(&self, a: MomentRef, b: MomentRef) -> bool {
        if a.asset != b.asset {
            return false;
        }
        match (a.at, b.at) {
            (Some(x), Some(y)) => {
                let frame = |r: MomentRef, t: f64| self.get(r).and_then(|m| m.frame).unwrap_or(t);
                (frame(a, x) - frame(b, y)).abs() < 1e-6
            }
            _ => true,
        }
    }

    /// Would `b` read as a repeat of `a` on screen?
    pub fn repeats(&self, a: MomentRef, b: MomentRef) -> bool {
        self.same_footage(a, b) || self.similarity(a, b).is_some_and(|s| s >= NEAR_DUPLICATE)
    }

    pub fn repeats_any(&self, a: MomentRef, others: &[MomentRef]) -> bool {
        others.iter().any(|&b| self.repeats(a, b))
    }

    fn motion(&self, at: MomentRef) -> Option<f32> {
        self.get(at).and_then(|m| m.motion)
    }
}

/// Stamp stored signals onto a cut list: motion on video slots (from each
/// slot's own moment), and every slot's repeats among the others. A slot
/// with no comparable embedding and no identical footage elsewhere is
/// marked unchecked (None) rather than "no repeats".
pub fn annotate(slots: &mut [Slot], signals: &Signals) {
    let refs: Vec<MomentRef> = slots.iter().map(MomentRef::of_slot).collect();
    let names: Vec<SlotRef> = slots.iter().map(SlotRef::of).collect();
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.kind == SlotKind::Video {
            slot.motion = signals.motion(refs[i]);
        }
        let repeats: Vec<SlotRef> = (0..refs.len())
            .filter(|&j| j != i && signals.repeats(refs[i], refs[j]))
            .map(|j| names[j])
            .collect();
        let comparable = signals.get(refs[i]).is_some_and(|m| m.embedding.is_some());
        slot.near_duplicates = (comparable || !repeats.is_empty()).then_some(repeats);
    }
}

/// 1-based positions of `slots[i]`'s repeats in the current order; None
/// when the slot was never checked.
pub fn similar_to(slots: &[Slot], i: usize) -> Option<Vec<usize>> {
    let dups = slots[i].near_duplicates.as_ref()?;
    Some(
        slots
            .iter()
            .enumerate()
            .filter(|(j, s)| *j != i && dups.iter().any(|d| d.names(s)))
            .map(|(j, _)| j + 1)
            .collect(),
    )
}

/// Probe what the render will read for `slot`: video display size and
/// displaymatrix rotation via ffprobe; photo size from the source header and
/// rotation from the original's EXIF. Metadata only — see [`Geometry`].
pub fn probe_geometry(slot: &Slot, original: &Path) -> Option<Geometry> {
    match slot.kind {
        SlotKind::Video => {
            let p = eidetic_ingest::video::probe(&slot.source).ok().flatten()?;
            Some(Geometry {
                width: u32::try_from(p.width?).ok()?,
                height: u32::try_from(p.height?).ok()?,
                rotation_cw: p.rotation_cw,
                rotation_applied: true,
            })
        }
        SlotKind::Photo | SlotKind::PhotoScenic => {
            let (width, height) = image::image_dimensions(&slot.source).ok()?;
            let rotation_cw = eidetic_core::exif::read_orientation(original).map(|o| match o {
                3 | 4 => 180,
                5 | 6 => 90,
                7 | 8 => 270,
                _ => 0,
            });
            Some(Geometry {
                width,
                height,
                rotation_cw,
                // The medium thumbnail was rotated upright when generated.
                rotation_applied: slot.source != original,
            })
        }
    }
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
        if t.secs() < 0.0 || t.secs() > MAX_TRANSITION_SECS {
            bail!(
                "transition duration {:.2}s is outside 0..{MAX_TRANSITION_SECS}s",
                t.secs()
            );
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
        if self.kind == SlotKind::Video {
            match self.motion {
                Some(m) => s.push_str(&format!(" [motion {m:.3}]")),
                None => s.push_str(" [motion unmeasured]"),
            }
        }
        // Geometry is metadata: it can warn, never certify upright pixels.
        match self.geometry {
            None => s.push_str(" [size unknown]"),
            Some(g) => {
                let rotation = match g.rotation_cw {
                    Some(r) => format!("metadata rotation {r}°"),
                    None => "no rotation metadata".to_string(),
                };
                s.push_str(&format!(" [{}x{}, {rotation}]", g.width, g.height));
                if g.may_render_sideways() {
                    s.push_str(" [rotation not applied by the render: may come out sideways]");
                }
            }
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
        motion: None,
        geometry: None,
        near_duplicates: None,
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
        motion: None,
        geometry: None,
        near_duplicates: None,
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
///
/// With `signals`, near-duplicates of shots already taken are deferred to
/// the back of the queue (see [`diverse_order`]).
pub fn plan(
    results: &[SearchResult],
    target_secs: f64,
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
    signals: &Signals,
) -> Vec<Slot> {
    let strong = strong_prefix(results);
    let mut slots: Vec<Slot> = Vec::new();
    let mut total = 0.0f64;
    for r in diverse_order(strong, signals)
        .into_iter()
        .map(|i| &strong[i])
    {
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

/// The next shot from the unused part of `pool`. The diversity penalty is
/// lexicographic: a near-duplicate of an already-chosen shot is taken only
/// when nothing else is left (scarcity), so no score can buy a repeat —
/// and when scarcity forces one, [`annotate`] surfaces it as `similar_to`.
/// Within a tier the highest `value` wins; ties go to the earlier hit in
/// `pool` order, so the choice is deterministic.
fn pick(
    pool: &[SearchResult],
    used: &[bool],
    chosen: &[MomentRef],
    signals: &Signals,
    value: impl Fn(usize) -> f64,
) -> Option<usize> {
    (0..pool.len()).filter(|&j| !used[j]).min_by(|&a, &b| {
        let dup = |j: usize| signals.repeats_any(MomentRef::of_hit(&pool[j]), chosen);
        dup(a)
            .cmp(&dup(b))
            .then(value(b).total_cmp(&value(a)))
            .then(a.cmp(&b))
    })
}

/// Indices of `pool` in the order [`plan`] consumes them: `pool` order
/// (score order from search, or a `--variations` rotation of it), with
/// repeats of earlier picks deferred.
pub fn diverse_order(pool: &[SearchResult], signals: &Signals) -> Vec<usize> {
    let mut used = vec![false; pool.len()];
    let mut chosen = Vec::with_capacity(pool.len());
    let mut order = Vec::with_capacity(pool.len());
    while let Some(j) = pick(pool, &used, &chosen, signals, |j| -(j as f64)) {
        used[j] = true;
        chosen.push(MomentRef::of_hit(&pool[j]));
        order.push(j);
    }
    order
}

/// A semantic value per position that respects `pool` order: position j
/// is worth the j-th best score in the pool, over the best. For a
/// score-sorted pool that is each hit's own score; for a rotated
/// `--variations` pool it keeps the rotation's order while staying on the
/// score scale the motion bonus is weighed against.
fn rank_values(pool: &[SearchResult]) -> Vec<f64> {
    let mut scores: Vec<f64> = pool.iter().map(|r| r.score as f64).collect();
    scores.sort_by(|a, b| b.total_cmp(a));
    let top = scores.first().copied().unwrap_or(1.0).max(f64::EPSILON);
    scores.into_iter().map(|s| s / top).collect()
}

/// Each hit's measured motion scaled to the pool's measured range (0..1).
/// Photos and unmeasured videos get 0 — no bonus, and no penalty against
/// each other. With fewer than two distinct measurements there is nothing
/// to prefer, so everything is 0 and planning falls back to score order.
fn motion_norm(pool: &[SearchResult], signals: &Signals) -> Vec<f64> {
    let m: Vec<Option<f64>> = pool
        .iter()
        .map(|r| {
            r.frame_ts
                .and(signals.motion(MomentRef::of_hit(r)))
                .map(f64::from)
                .filter(|m| m.is_finite())
        })
        .collect();
    let lo = m.iter().flatten().copied().fold(f64::INFINITY, f64::min);
    let hi = m
        .iter()
        .flatten()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    m.into_iter()
        .map(|v| match v {
            Some(v) if hi > lo => (v - lo) / (hi - lo),
            _ => 0.0,
        })
        .collect()
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
///
/// `output` is only replaced once every ffmpeg step has succeeded: the
/// final encode lands in a staging directory beside it and is renamed into
/// place, so a failed render leaves any previous file at `output` intact
/// and, on ordinary errors, no partial file behind.
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
    // Beside `output` so the publishing rename stays on one filesystem;
    // same file name so ffmpeg picks the same muxer. Removed on drop, i.e.
    // on every early return below.
    let file_name = output
        .file_name()
        .with_context(|| format!("{} is not a file path", output.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".eidetic-render-")
        .tempdir_in(crate::project::parent_dir(output))
        .with_context(|| format!("create a staging dir next to {}", output.display()))?;
    let staged = staging.path().join(file_name);
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
            SlotKind::Photo => ken_burns_filter(width, height, rendered),
            SlotKind::Video | SlotKind::PhotoScenic => still_filter(slot, width, height, fit),
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
        None => staged.clone(),
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
            .arg(&staged)
            .status()
            .context("run ffmpeg audio mux")?;
        if !status.success() {
            bail!("ffmpeg audio mux failed");
        }
    }
    std::fs::rename(&staged, output)
        .with_context(|| format!("failed to publish the render to {}", output.display()))?;
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

/// Shortest span a cut may make; absorbs very fast tempi.
const MIN_SPAN: f64 = 0.6;

/// One music span the planner fills with one shot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    /// The cut into this span lands on an *inferred* downbeat. Always false
    /// for Ellis grids: their bars are a guess, and nothing downstream may
    /// treat them as real.
    pub on_downbeat: bool,
}

/// Cut spans from a beat grid. With inferred downbeats (goals-v0.9.md
/// phase 2) they are bar-quantized: one span per bar, split at the middle
/// beat in high-energy sections, whatever the meter — a 3-beat bar is a
/// 3-beat span, and pickup beats before the first bar become their own
/// lead-in (or merge into the first bar when too short to hold a shot).
/// Without them (Ellis) each span starts on a beat and runs 4 beats in
/// normal sections, 2 in high-energy ones — the convention auto-editors
/// converge on. Either way a 0.6 s floor absorbs very fast tempi and spans
/// stop at `total_secs`.
pub fn beat_spans(grid: &BeatGrid, total_secs: f64) -> Vec<Span> {
    if grid.downbeats_inferred() && !grid.downbeats.is_empty() {
        return bar_spans(grid, total_secs);
    }
    pulse_spans(grid, total_secs)
        .into_iter()
        .map(|(start, end)| Span {
            start,
            end,
            on_downbeat: false,
        })
        .collect()
}

/// Bar-quantized spans over inferred downbeats; see [`beat_spans`].
fn bar_spans(grid: &BeatGrid, total_secs: f64) -> Vec<Span> {
    // A downbeat this close to t=0 means the reel opens on the bar line.
    const OPENS_ON_BAR: f64 = 0.1;
    let (beats, bars) = (&grid.beats, &grid.downbeats);
    let bar_time = |k: usize| {
        bars.get(k)
            .map_or(total_secs, |&i| beats[i].min(total_secs))
    };

    // Candidate cut points: each bar line, plus the middle beat of a
    // high-energy bar when both halves can hold a shot.
    let mut cuts: Vec<(f64, bool)> = Vec::new();
    for (k, &i) in bars.iter().enumerate() {
        let start = beats[i];
        if start >= total_secs {
            break;
        }
        cuts.push((start, true));
        if grid.is_high_energy(start) {
            let next = bars.get(k + 1).copied().unwrap_or(beats.len());
            let mid = i + (next - i).div_ceil(2);
            if mid < next {
                let t = beats[mid];
                if t - start >= MIN_SPAN && bar_time(k + 1) - t >= MIN_SPAN && t < total_secs {
                    cuts.push((t, false));
                }
            }
        }
    }

    // The pickup: beats before the first bar are a lead-in span of their
    // own, unless too short, in which case the first bar starts at 0.
    let mut bounds: Vec<(f64, bool)> = match cuts.first() {
        Some(&(t0, _)) if t0 < MIN_SPAN => {
            cuts.remove(0);
            vec![(0.0, t0 < OPENS_ON_BAR)]
        }
        _ => vec![(0.0, false)],
    };
    for (t, on_downbeat) in cuts {
        let last = bounds.last().map_or(0.0, |b| b.0);
        if t - last >= MIN_SPAN {
            bounds.push((t, on_downbeat));
        }
    }

    let mut spans: Vec<Span> = bounds
        .windows(2)
        .map(|w| Span {
            start: w[0].0,
            end: w[1].0,
            on_downbeat: w[0].1,
        })
        .collect();
    let (last, on_downbeat) = bounds[bounds.len() - 1];
    if total_secs - last >= MIN_SPAN || spans.is_empty() {
        spans.push(Span {
            start: last,
            end: total_secs,
            on_downbeat,
        });
    }
    spans
}

/// The every-N-beats spans for grids without inferred bars.
fn pulse_spans(grid: &BeatGrid, total_secs: f64) -> Vec<(f64, f64)> {
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
/// hero shots are saved for the drops. Each high-energy section's hero span
/// gets the best remaining hit; every other span takes the rest in score
/// order — so the reel still opens strong, but its strongest moment lands
/// where the song peaks. With inferred downbeats the hero span is the
/// first span that starts on a bar line inside the section (a pickup,
/// half-bar, or a bar that begins before the drop is passed over); with
/// Ellis grids it is simply the section's first span.
///
/// Quality signals (goals-v0.9.md phase 2) refine the choice without
/// widening the pool: every pick comes from the same strong prefix, so the
/// semantic floor (and whatever filters produced `results`) still binds.
/// High-energy spans value measured motion ([`MOTION_WEIGHT`]) alongside
/// score, and near-duplicates of shots already placed are deferred until
/// nothing else is left ([`pick`]). With empty `signals` the plan is
/// exactly the score-order plan.
pub fn plan_synced(
    results: &[SearchResult],
    grid: &BeatGrid,
    total_secs: f64,
    library_dir: &Path,
    scenes: &HashMap<AssetId, Vec<f64>>,
    focus: &HashMap<AssetId, f64>,
    signals: &Signals,
) -> Vec<Slot> {
    let spans = beat_spans(grid, total_secs);
    let strong = strong_prefix(results);

    let n = spans.len().min(strong.len());
    let hot: Vec<bool> = spans
        .iter()
        .map(|s| grid.is_high_energy((s.start + s.end) / 2.0))
        .collect();
    // Which span is filled when: each section's hero span first (so the
    // drops get the best hits), then the rest in timeline order.
    let mut heroes = Vec::new();
    let mut i = 0;
    while i < n {
        if !hot[i] {
            i += 1;
            continue;
        }
        let run_end = (i..n).find(|&j| !hot[j]).unwrap_or(n);
        // A run is "hot" by span midpoints, so its first span can start
        // before the section does (a bar straddling the drop). With real
        // bars the hero goes on the first bar line *inside* the section —
        // possibly the span right after the run, when the section ends
        // before that bar's midpoint.
        let hero = if grid.downbeats_inferred() {
            let starts_hot = |j: &usize| grid.is_high_energy(spans[*j].start);
            (i..(run_end + 1).min(n))
                .filter(starts_hot)
                .find(|&j| spans[j].on_downbeat)
                .or_else(|| (i..run_end).find(starts_hot))
                .unwrap_or(i)
        } else {
            i
        };
        heroes.push(hero);
        i = run_end;
    }
    let fill_order: Vec<usize> = heroes
        .iter()
        .copied()
        .chain((0..n).filter(|i| !heroes.contains(i)))
        .collect();
    let base = rank_values(strong);
    let motion = motion_norm(strong, signals);
    let mut used = vec![false; strong.len()];
    let mut chosen = Vec::with_capacity(n);
    let mut hit_for_span = vec![0usize; n];
    for i in fill_order {
        let value = |j: usize| {
            let bonus = if hot[i] {
                MOTION_WEIGHT * motion[j]
            } else {
                0.0
            };
            base[j] + bonus
        };
        let j = pick(strong, &used, &chosen, signals, value)
            .expect("n <= strong.len(), so a hit is left for every span");
        used[j] = true;
        chosen.push(MomentRef::of_hit(&strong[j]));
        hit_for_span[i] = j;
    }

    let mut slots = Vec::new();
    let mut total = 0.0;
    for (i, s) in spans.iter().enumerate().take(n) {
        let span = s.end - total;
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

/// How `slot`'s picture sits in a `w`x`h` frame, as one still: exactly the
/// render's framing for videos and scenic photos, and for photos the
/// cover-crop the Ken Burns push-in starts from. Shared with the preview
/// sheet so a tile shows what the reel will.
pub(crate) fn still_filter(slot: &Slot, w: u32, h: u32, fit: Fit) -> String {
    match slot.kind {
        SlotKind::Video => match slot.focus_x {
            Some(fx) => focus_filter(w, h, fit.resolve(true), fx),
            None => normalize_filter(w, h, fit.resolve(false)),
        },
        SlotKind::Photo => normalize_filter(w, h, Fit::Cover),
        SlotKind::PhotoScenic => blur_fill_filter(w, h),
    }
}

/// Cover-crop centred on a horizontal focus point (0..1 of source width) —
/// the crop window slides to keep faces in frame, clamped to the image.
/// Pad mode has no crop to steer, so focus degrades to the plain filter.
fn focus_filter(w: u32, h: u32, fit: Fit, fx: f64) -> String {
    match fit {
        Fit::Pad | Fit::Blur | Fit::Auto => normalize_filter(w, h, fit),
        Fit::Cover => format!("{},fps=30,format=yuv420p", cover_filter(w, h, Some(fx))),
    }
}

/// Cover-fit to `w`x`h`: crop the source to the frame's aspect (the window
/// steered to horizontal focus `fx`, else centred), then scale. The same
/// picture as scaling to cover and cropping after, but ffmpeg never holds an
/// enlarged intermediate — scale-first turns a 32000x16 panorama covering a
/// 180x320 frame into a 640000x320 one. Here nothing is ever larger than the
/// source or the frame.
fn cover_filter(w: u32, h: u32, fx: Option<f64>) -> String {
    let x = match fx {
        Some(fx) => format!("clip(iw*{fx:.4}-ow/2,0,iw-ow)"),
        None => "(iw-ow)/2".to_string(),
    };
    format!("crop='min(iw,ih*{w}/{h})':'min(ih,iw*{h}/{w})':'{x}':'(ih-oh)/2',scale={w}:{h}")
}

/// Fit the input to the frame per `fit`, normalise fps/pixfmt so concat's
/// `-c copy` join is legal across segments.
fn normalize_filter(w: u32, h: u32, fit: Fit) -> String {
    match fit {
        Fit::Pad => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps=30,format=yuv420p"
        ),
        Fit::Cover => format!("{},fps=30,format=yuv420p", cover_filter(w, h, None)),
        Fit::Blur | Fit::Auto => blur_fill_filter(w, h),
    }
}

/// The cinematic mismatched-aspect treatment: the frame is a blurred,
/// darkened cover of the clip, with the clip itself fitted inside intact.
///
/// The blur radius is 20 wherever the frame allows it (every render size),
/// and shrinks for frames boxblur would otherwise refuse: a radius must stay
/// under half of each plane, so a 320x32 preview tile of a 10:1 reel caps
/// luma at 15 and 4:2:0 chroma at 7.
fn blur_fill_filter(w: u32, h: u32) -> String {
    format!(
        "split[bg][fg];\
         [bg]{},\
         boxblur=luma_radius='min(20,min(w,h)/2-1)':luma_power=2:\
         chroma_radius='min(20,min(cw,ch)/2-1)':chroma_power=2,\
         eq=brightness=-0.15[b];\
         [fg]scale={w}:{h}:force_original_aspect_ratio=decrease[f];\
         [b][f]overlay=(W-w)/2:(H-h)/2,fps=30,format=yuv420p",
        cover_filter(w, h, None)
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
        "{},zoompan=z='min(zoom+{rate:.6},1.15)':\
         x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':\
         d={frames}:s={w}x{h}:fps=30,format=yuv420p",
        cover_filter(w, h, None)
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
        let slots = plan(&results, 10.0, &lib(), &HashMap::new(), &Signals::default());
        assert_eq!(slots.len(), 3);
        assert!((total_secs(&slots) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn plan_drops_weak_tail_even_with_room() {
        let results = vec![hit(0.5, None, None), hit(0.1, None, None)];
        let slots = plan(&results, 60.0, &lib(), &HashMap::new(), &Signals::default());
        assert_eq!(slots.len(), 1, "0.1 < 35% of 0.5 is padding");
    }

    #[test]
    fn plan_drops_non_positive_scores() {
        let results = vec![hit(0.0, None, None), hit(-0.2, None, None)];
        assert!(plan(&results, 30.0, &lib(), &HashMap::new(), &Signals::default()).is_empty());
    }

    #[test]
    fn video_clip_leads_into_the_moment_and_respects_bounds() {
        // Moment at 9s of a 10s video: start = 7.5, len clamped to 2.5.
        let results = vec![hit(0.5, Some(9.0), Some(10.0))];
        let slots = plan(&results, 30.0, &lib(), &HashMap::new(), &Signals::default());
        assert_eq!(slots[0].kind, SlotKind::Video);
        assert!((slots[0].start - 7.5).abs() < 1e-9);
        assert!((slots[0].duration - 2.5).abs() < 1e-9);

        // Moment at 0.5s: lead-in clamps to the file start.
        let results = vec![hit(0.5, Some(0.5), Some(10.0))];
        let slots = plan(&results, 30.0, &lib(), &HashMap::new(), &Signals::default());
        assert_eq!(slots[0].start, 0.0);
        assert!((slots[0].duration - 4.0).abs() < 1e-9);
    }

    #[test]
    fn slots_carry_identity_and_score() {
        let r = hit(0.7, Some(2.0), Some(30.0));
        let slots = plan(
            std::slice::from_ref(&r),
            30.0,
            &lib(),
            &HashMap::new(),
            &Signals::default(),
        );
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
        let slots = plan(
            &[thumbed],
            30.0,
            &lib(),
            &HashMap::new(),
            &Signals::default(),
        );
        assert_eq!(slots[0].kind, SlotKind::Photo);
        let s = slots[0].source.to_str().unwrap();
        assert!(s.contains(".thumbs/m/"), "expected thumb path, got {s}");
        assert!(s.ends_with(".jpg"));

        // No thumbnail -> the original is used as-is.
        let raw = hit(0.5, None, None);
        let slots = plan(&[raw], 30.0, &lib(), &HashMap::new(), &Signals::default());
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
            tracker: eidetic_ingest::beats::Tracker::Ellis,
            model: None,
            fallback: None,
        }
    }

    /// A Beat This grid: 0.5 s beats for `secs`, bars starting at the given
    /// beat indices (which is what makes meter and pickups expressible).
    fn inferred_grid(secs: f64, bars: Vec<usize>) -> BeatGrid {
        let mut g = grid_120bpm(secs);
        g.downbeats = bars;
        g.tracker = eidetic_ingest::beats::Tracker::BeatThis;
        g.model = Some(eidetic_ingest::beats::ModelProvenance {
            name: "beat_this_small".into(),
            revision: "test".into(),
            sha256: "test".into(),
        });
        g
    }

    fn starts(spans: &[Span]) -> Vec<f64> {
        spans.iter().map(|s| s.start).collect()
    }

    #[test]
    fn beat_spans_cut_every_four_beats_normally() {
        let spans = beat_spans(&grid_120bpm(20.0), 10.0);
        // 4 beats at 120 BPM = 2 s per span, 5 spans in 10 s.
        assert_eq!(spans.len(), 5, "{spans:?}");
        assert!((spans[0].end - spans[0].start - 2.0).abs() < 1e-9);
        assert_eq!(spans[0].start, 0.0, "lead-in merges into the first span");
        assert!(
            spans.iter().all(|s| !s.on_downbeat),
            "Ellis bars are never real"
        );
    }

    /// Ellis's downbeats are a guess: even when they say "bar every 3",
    /// the planner keeps its 4-beat pulse and never marks a downbeat.
    #[test]
    fn heuristic_downbeats_do_not_drive_bar_spans() {
        let mut grid = grid_120bpm(20.0);
        grid.downbeats = (0..grid.beats.len()).step_by(3).collect();
        let spans = beat_spans(&grid, 10.0);
        assert_eq!(starts(&spans), vec![0.0, 2.0, 4.0, 6.0, 8.0]);
        assert!(spans.iter().all(|s| !s.on_downbeat));
    }

    #[test]
    fn inferred_three_four_bars_cut_every_three_beats() {
        let bars: Vec<usize> = (0..40).step_by(3).collect();
        let spans = beat_spans(&inferred_grid(20.0, bars), 9.0);
        // 3 beats x 0.5 s = 1.5 s bars, not the 2 s a 4-beat guess makes.
        assert_eq!(starts(&spans), vec![0.0, 1.5, 3.0, 4.5, 6.0, 7.5]);
        assert!(spans.iter().all(|s| s.on_downbeat), "{spans:?}");
        assert!((spans[5].end - 9.0).abs() < 1e-9);
    }

    /// Mixed meter (7/8-style 3+4 bars): each span is exactly one bar.
    #[test]
    fn inferred_irregular_bars_follow_the_model() {
        let spans = beat_spans(&inferred_grid(20.0, vec![0, 3, 7, 10, 14]), 8.5);
        assert_eq!(starts(&spans), vec![0.0, 1.5, 3.5, 5.0, 7.0]);
        let lens: Vec<f64> = spans.iter().map(|s| s.end - s.start).collect();
        assert_eq!(lens, vec![1.5, 2.0, 1.5, 2.0, 1.5]);
    }

    #[test]
    fn a_long_pickup_becomes_its_own_lead_in() {
        // Two pickup beats (1.0 s) before the first bar at beat 2.
        let bars: Vec<usize> = (2..40).step_by(4).collect();
        let spans = beat_spans(&inferred_grid(20.0, bars), 9.0);
        assert_eq!(starts(&spans), vec![0.0, 1.0, 3.0, 5.0, 7.0]);
        assert!(!spans[0].on_downbeat, "the pickup is not a bar");
        assert!(spans[1..].iter().all(|s| s.on_downbeat));
    }

    #[test]
    fn a_short_pickup_merges_into_the_first_bar() {
        // One pickup beat (0.5 s < the 0.6 s floor): no flash-cut lead-in.
        let bars: Vec<usize> = (1..40).step_by(4).collect();
        let spans = beat_spans(&inferred_grid(20.0, bars), 8.5);
        assert_eq!(starts(&spans), vec![0.0, 2.5, 4.5, 6.5]);
        assert!(!spans[0].on_downbeat, "the cut at 0.5 s was absorbed");
        assert!((spans[0].end - 2.5).abs() < 1e-9);
    }

    #[test]
    fn inferred_hot_bars_split_at_the_middle_beat() {
        let bars: Vec<usize> = (0..40).step_by(4).collect();
        let mut grid = inferred_grid(20.0, bars);
        grid.high_energy = vec![(4.0, 8.0)];
        let spans = beat_spans(&grid, 10.0);
        assert_eq!(starts(&spans), vec![0.0, 2.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        let on: Vec<bool> = spans.iter().map(|s| s.on_downbeat).collect();
        assert_eq!(on, vec![true, true, true, false, true, false, true]);
    }

    #[test]
    fn beat_spans_densify_in_high_energy_sections() {
        let mut grid = grid_120bpm(20.0);
        grid.high_energy = vec![(4.0, 8.0)];
        let spans = beat_spans(&grid, 12.0);
        let in_hot: Vec<f64> = spans
            .iter()
            .filter(|s| s.start >= 4.0 && s.start < 8.0)
            .map(|s| s.end - s.start)
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
            &Signals::default(),
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
            &Signals::default(),
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
                grid.is_high_energy((spans[i - 1].start + spans[i - 1].end) / 2.0)
                    != grid.is_high_energy((spans[i].start + spans[i].end) / 2.0)
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
            &Signals::default(),
        );
        // Spans: [0,2) [2,4) | hot [4,5) [5,6) [6,7) [7,8) | [8,10) [10,12).
        // The first hot span (index 2) is the drop: it gets the best hit.
        assert!((slots[2].score - 0.5).abs() < 1e-6, "hero on the drop");
        // The reel still opens with the best remaining hit.
        assert!((slots[0].score - 0.49).abs() < 1e-6);
        // Everything else keeps score order down the timeline.
        assert!(slots[3].score > slots[4].score);
    }

    /// The pickup-boundary case: the drop starts at t=0 with a 1 s pickup
    /// before the first real bar at 1.0 s. The hero belongs on that bar
    /// line, not on the pickup — while an Ellis grid, which has no real
    /// bars, keeps the old first-span rule.
    #[test]
    fn inferred_hero_lands_on_the_first_bar_line_of_the_drop() {
        let bars: Vec<usize> = (2..60).step_by(4).collect();
        let mut grid = inferred_grid(30.0, bars);
        grid.high_energy = vec![(0.0, 6.0)];
        let results: Vec<_> = (0..16)
            .map(|i| hit(0.5 - i as f32 * 0.01, None, None))
            .collect();
        let plan = |g: &BeatGrid| {
            plan_synced(
                &results,
                g,
                10.0,
                &lib(),
                &HashMap::new(),
                &HashMap::new(),
                &Signals::default(),
            )
        };

        let spans = beat_spans(&grid, 10.0);
        assert_eq!(spans[0].start, 0.0);
        assert!(!spans[0].on_downbeat && spans[1].on_downbeat);
        assert_eq!(spans[1].start, 1.0);
        let slots = plan(&grid);
        assert!((slots[1].score - 0.5).abs() < 1e-6, "hero on the bar line");
        assert!(
            (slots[0].score - 0.49).abs() < 1e-6,
            "pickup gets the next best"
        );
        assert!((slots[0].duration - 1.0).abs() < 1e-9, "pickup span is 1 s");

        let mut heuristic = grid.clone();
        heuristic.tracker = eidetic_ingest::beats::Tracker::Ellis;
        heuristic.model = None;
        let slots = plan(&heuristic);
        assert!((slots[0].score - 0.5).abs() < 1e-6, "Ellis: first span");
    }

    /// The drop starts between bar lines (t=1, bars at 0/2/4/6). The bar
    /// [0,2) straddles it and its midpoint is hot, but it starts before the
    /// drop: the hero belongs on the first bar line inside it, t=2.
    #[test]
    fn inferred_hero_skips_a_bar_that_starts_before_the_drop() {
        let bars: Vec<usize> = (0..60).step_by(4).collect();
        let mut grid = inferred_grid(30.0, bars);
        grid.high_energy = vec![(1.0, 7.0)];
        let results: Vec<_> = (0..16)
            .map(|i| hit(0.5 - i as f32 * 0.01, None, None))
            .collect();
        let plan = |g: &BeatGrid| {
            plan_synced(
                &results,
                g,
                10.0,
                &lib(),
                &HashMap::new(),
                &HashMap::new(),
                &Signals::default(),
            )
        };

        let spans = beat_spans(&grid, 10.0);
        assert_eq!(starts(&spans)[..2], [0.0, 2.0]);
        assert!(grid.is_high_energy((spans[0].start + spans[0].end) / 2.0));
        let slots = plan(&grid);
        assert!((slots[1].score - 0.5).abs() < 1e-6, "hero on the t=2 bar");
        assert!((slots[0].score - 0.49).abs() < 1e-6, "not before the drop");

        // Ellis keeps its first-hot-span rule unchanged.
        let mut heuristic = grid.clone();
        heuristic.tracker = eidetic_ingest::beats::Tracker::Ellis;
        heuristic.model = None;
        let slots = plan(&heuristic);
        assert!((slots[0].score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn plan_synced_cuts_three_four_on_every_bar() {
        let bars: Vec<usize> = (0..60).step_by(3).collect();
        let grid = inferred_grid(30.0, bars);
        let results: Vec<_> = (0..8).map(|_| hit(0.5, None, None)).collect();
        let slots = plan_synced(
            &results,
            &grid,
            6.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
            &Signals::default(),
        );
        let lens: Vec<f64> = slots.iter().map(|s| s.duration).collect();
        assert_eq!(lens, vec![1.5, 1.5, 1.5, 1.5]);
    }

    #[test]
    fn plan_always_takes_at_least_one_strong_hit() {
        // Target shorter than the first segment still yields that segment.
        let results = vec![hit(0.5, Some(5.0), Some(60.0))];
        assert_eq!(
            plan(&results, 1.0, &lib(), &HashMap::new(), &Signals::default()).len(),
            1
        );
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
        let mut slots = plan(
            std::slice::from_ref(&r),
            30.0,
            &lib(),
            &HashMap::new(),
            &Signals::default(),
        );
        slots[0].pinned = true;
        slots[0].transition_in = Transition::Crossfade { secs: 0.5 };
        let json = serde_json::to_string(&slots).unwrap();
        let back: Vec<Slot> = serde_json::from_str(&json).unwrap();
        assert_eq!(back[0].asset, slots[0].asset);
        assert_eq!(back[0].kind, SlotKind::Video);
        assert!(back[0].pinned);
        assert_eq!(back[0].transition_in, Transition::Crossfade { secs: 0.5 });
    }

    // -- quality signals (deterministic vectors, no database) ---------------

    /// Signals for `hits`: (embedding, motion) per hit, same order, keyed
    /// by each hit's matched moment.
    fn signals(hits: &[SearchResult], per: &[(Option<[f32; 3]>, Option<f32>)]) -> Signals {
        let mut s = Signals::default();
        for (h, (e, m)) in hits.iter().zip(per) {
            s.insert(
                MomentRef::of_hit(h),
                Moment {
                    frame: h.frame_ts,
                    embedding: e.map(|v| v.to_vec()),
                    motion: *m,
                },
            );
        }
        s
    }

    /// Signals keyed by each slot's own moment (what [`annotate`] reads):
    /// (resolved frame, embedding, motion) per slot.
    type SlotMoment = (Option<f64>, Option<[f32; 3]>, Option<f32>);

    fn slot_signals(slots: &[Slot], per: &[SlotMoment]) -> Signals {
        let mut s = Signals::default();
        for (slot, (frame, e, m)) in slots.iter().zip(per) {
            s.insert(
                MomentRef::of_slot(slot),
                Moment {
                    frame: *frame,
                    embedding: e.map(|v| v.to_vec()),
                    motion: *m,
                },
            );
        }
        s
    }

    fn video_at(asset: AssetId, start: f64) -> Slot {
        Slot {
            asset,
            source: PathBuf::from("/v.mp4"),
            kind: SlotKind::Video,
            start,
            duration: 2.0,
            focus_x: None,
            score: 0.3,
            pinned: false,
            transition_in: Transition::Cut,
            motion: None,
            geometry: None,
            near_duplicates: None,
        }
    }

    #[test]
    fn two_moments_of_one_video_keep_their_own_signals() {
        // Video A at 2s is a beach, at 20s a forest; photo-like slot B is
        // the beach again. Orthogonal embeddings, different motion.
        let a = AssetId::new();
        let mut slots = vec![
            video_at(a, 1.0),
            video_at(a, 19.0),
            video_at(AssetId::new(), 4.0),
        ];
        let sig = slot_signals(
            &slots,
            &[
                (Some(2.0), Some(BEACH), Some(0.1)),
                (Some(20.0), Some(FOREST), Some(0.7)),
                (Some(5.0), Some(BEACH_AGAIN), None),
            ],
        );
        annotate(&mut slots, &sig);
        assert_eq!(slots[0].motion, Some(0.1));
        assert_eq!(slots[1].motion, Some(0.7), "not the first moment's motion");
        assert_eq!(
            similar_to(&slots, 0),
            Some(vec![3]),
            "beach repeats beach only"
        );
        assert_eq!(
            similar_to(&slots, 1),
            Some(vec![]),
            "the forest moment is distinct"
        );
        assert_eq!(similar_to(&slots, 2), Some(vec![1]));
    }

    #[test]
    fn the_same_footage_twice_is_a_repeat_even_without_embeddings() {
        let a = AssetId::new();
        // Same video, same in-point: identical footage.
        let mut slots = vec![video_at(a, 1.0), video_at(a, 1.0), video_at(a, 30.0)];
        annotate(&mut slots, &Signals::default());
        assert_eq!(similar_to(&slots, 0), Some(vec![2]));
        assert_eq!(similar_to(&slots, 1), Some(vec![1]));
        assert_eq!(
            similar_to(&slots, 2),
            None,
            "another moment, nothing to compare"
        );
    }

    #[test]
    fn variation_rotations_still_change_the_plan() {
        let hits = vec![
            hit(0.50, None, None),
            hit(0.49, None, None),
            hit(0.40, None, None),
        ];
        let ids = |s: &[Slot]| s.iter().map(|s| s.asset).collect::<Vec<_>>();
        let mut seen = Vec::new();
        for k in 0..3 {
            let mut pool = hits.clone();
            pool.rotate_left(k);
            let want: Vec<_> = pool.iter().map(|h| h.id).collect();
            let unsynced = plan(&pool, 30.0, &lib(), &HashMap::new(), &Signals::default());
            assert_eq!(ids(&unsynced), want, "rotation {k} keeps its order");
            let synced = plan_synced(
                &pool,
                &grid_120bpm(30.0),
                6.0,
                &lib(),
                &HashMap::new(),
                &HashMap::new(),
                &Signals::default(),
            );
            assert_eq!(ids(&synced), want, "rotation {k} opens differently");
            seen.push(ids(&synced)[0]);
        }
        seen.dedup();
        assert_eq!(seen.len(), 3, "every variation opens on a different shot");

        // Diversity still applies inside a rotated pool: the repeat of the
        // opener is deferred.
        let mut pool = hits.clone();
        pool.rotate_left(1); // 0.49, 0.40, 0.50
        let sig = signals(
            &pool,
            &[
                (Some(BEACH), None),
                (Some(FOREST), None),
                (Some(BEACH_AGAIN), None),
            ],
        );
        let slots = plan(&pool, 6.0, &lib(), &HashMap::new(), &sig);
        assert_eq!(ids(&slots), vec![pool[0].id, pool[1].id]);
    }

    #[test]
    fn describe_shows_geometry_or_says_it_is_unknown() {
        let mut s = video_at(AssetId::new(), 0.0);
        assert!(s.describe().contains("[size unknown]"), "{}", s.describe());
        s.geometry = Some(Geometry {
            width: 1080,
            height: 1920,
            rotation_cw: Some(90),
            rotation_applied: true,
        });
        let d = s.describe();
        assert!(d.contains("[1080x1920, metadata rotation 90°]"), "{d}");
        assert!(!d.contains("sideways"), "{d}");
        s.geometry = Some(Geometry {
            width: 4000,
            height: 3000,
            rotation_cw: None,
            rotation_applied: false,
        });
        assert!(s.describe().contains("[4000x3000, no rotation metadata]"));
        s.geometry = Some(Geometry {
            width: 4000,
            height: 3000,
            rotation_cw: Some(270),
            rotation_applied: false,
        });
        assert!(s.describe().contains("may come out sideways"));
    }

    const BEACH: [f32; 3] = [1.0, 0.0, 0.0];
    const BEACH_AGAIN: [f32; 3] = [0.99, 0.1, 0.0]; // cos ≈ 0.995
    const FOREST: [f32; 3] = [0.0, 1.0, 0.0];
    const CITY: [f32; 3] = [0.0, 0.0, 1.0];

    #[test]
    fn near_identical_shots_are_deferred_behind_different_ones() {
        // Two files of one scene rank 1 and 2; a different scene ranks 3.
        let hits = vec![
            hit(0.50, None, None),
            hit(0.49, None, None),
            hit(0.40, None, None),
        ];
        let sig = signals(
            &hits,
            &[
                (Some(BEACH), None),
                (Some(BEACH_AGAIN), None),
                (Some(FOREST), None),
            ],
        );
        let slots = plan(&hits, 6.0, &lib(), &HashMap::new(), &sig);
        let ids: Vec<_> = slots.iter().map(|s| s.asset).collect();
        assert_eq!(
            ids,
            vec![hits[0].id, hits[2].id],
            "the repeat lost its slot"
        );

        // Semantically different candidates are left in score order.
        let sig = signals(
            &hits,
            &[
                (Some(BEACH), None),
                (Some(CITY), None),
                (Some(FOREST), None),
            ],
        );
        let slots = plan(&hits, 6.0, &lib(), &HashMap::new(), &sig);
        assert_eq!(slots[1].asset, hits[1].id);
    }

    #[test]
    fn scarcity_uses_the_repeat_and_says_so() {
        let hits = vec![hit(0.50, None, None), hit(0.49, None, None)];
        let sig = signals(&hits, &[(Some(BEACH), None), (Some(BEACH_AGAIN), None)]);
        let mut slots = plan(&hits, 6.0, &lib(), &HashMap::new(), &sig);
        assert_eq!(slots.len(), 2, "a repeat beats a shorter reel");
        annotate(&mut slots, &sig);
        assert_eq!(similar_to(&slots, 1), Some(vec![1]));
        assert_eq!(similar_to(&slots, 0), Some(vec![2]));

        // Without comparable embeddings the check is reported as not run.
        let sig = signals(&hits, &[(None, None), (Some(BEACH), None)]);
        annotate(&mut slots, &sig);
        assert_eq!(similar_to(&slots, 0), None, "unchecked, not 'no repeats'");
        assert_eq!(similar_to(&slots, 1), Some(vec![]));
    }

    fn hot_grid() -> BeatGrid {
        let mut grid = grid_120bpm(30.0);
        // Spans: [0,2) [2,4) | hot [4,5) [5,6) [6,7) [7,8) | [8,10) [10,12).
        grid.high_energy = vec![(4.0, 8.0)];
        grid
    }

    #[test]
    fn hot_spans_prefer_measured_motion_within_the_strong_pool() {
        // A still photo scores best; a moving clip scores a little lower;
        // a very-high-motion clip is below the weak-tail floor.
        let mut hits = vec![
            hit(0.50, None, None),
            hit(0.45, Some(5.0), Some(60.0)),
            hit(0.44, None, None),
            hit(0.43, None, None),
            hit(0.42, None, None),
            hit(0.41, None, None),
            hit(0.40, None, None),
            hit(0.39, Some(5.0), Some(60.0)),
            hit(0.10, Some(5.0), Some(60.0)),
        ];
        let motion = [
            None,
            Some(0.08),
            None,
            None,
            None,
            None,
            None,
            Some(0.01),
            Some(0.5),
        ];
        let per: Vec<_> = motion.iter().map(|m| (None, *m)).collect();
        let sig = signals(&hits, &per);
        let slots = plan_synced(
            &hits,
            &hot_grid(),
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
            &sig,
        );
        // The drop (span 2) takes the moving clip over the better-scoring
        // still; the calm opener still gets the best remaining hit.
        assert_eq!(slots[2].asset, hits[1].id, "motion on the drop");
        assert_eq!(slots[0].asset, hits[0].id);
        assert!(
            slots.iter().all(|s| s.asset != hits[8].id),
            "high motion never buys a way past the semantic floor"
        );

        // No measured motion anywhere: exactly the score-order plan.
        hits.truncate(8);
        let sig = signals(&hits, &[(None, None); 8]);
        let with = plan_synced(
            &hits,
            &hot_grid(),
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
            &sig,
        );
        let without = plan_synced(
            &hits,
            &hot_grid(),
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
            &Signals::default(),
        );
        let ids = |s: &[Slot]| s.iter().map(|s| s.asset).collect::<Vec<_>>();
        assert_eq!(ids(&with), ids(&without));
        assert_eq!(with[2].asset, hits[0].id, "hero on the drop, as before");
    }

    #[test]
    fn calm_spans_ignore_motion() {
        let hits = vec![hit(0.50, None, None), hit(0.45, Some(5.0), Some(60.0))];
        let sig = signals(&hits, &[(None, None), (None, Some(0.9))]);
        let slots = plan_synced(
            &hits,
            &grid_120bpm(30.0),
            12.0,
            &lib(),
            &HashMap::new(),
            &HashMap::new(),
            &sig,
        );
        assert_eq!(slots[0].asset, hits[0].id);
    }

    #[test]
    fn annotations_mark_video_motion_and_leave_photos_without_one() {
        let hits = vec![
            hit(0.5, None, None),
            hit(0.5, Some(5.0), Some(60.0)),
            hit(0.5, Some(9.0), Some(60.0)),
        ];
        let mut slots = plan(&hits, 30.0, &lib(), &HashMap::new(), &Signals::default());
        let motion = |id| {
            if id == hits[1].id { Some(0.0) } else { None }
        };
        let per: Vec<_> = slots
            .iter()
            .map(|s| (None, None, motion(s.asset)))
            .collect();
        let sig = slot_signals(&slots, &per);
        annotate(&mut slots, &sig);
        let by = |id| slots.iter().find(|s| s.asset == id).unwrap();
        assert_eq!(by(hits[0].id).motion, None);
        assert_eq!(
            by(hits[1].id).motion,
            Some(0.0),
            "measured still, not unknown"
        );
        assert_eq!(by(hits[2].id).motion, None);
        assert!(by(hits[2].id).describe().contains("motion unmeasured"));
        assert!(!by(hits[0].id).describe().contains("motion"));
    }

    #[test]
    fn geometry_flags_only_a_rotation_the_render_skips() {
        let g = |rotation_cw, rotation_applied| Geometry {
            width: 4000,
            height: 3000,
            rotation_cw,
            rotation_applied,
        };
        assert!(g(Some(90), false).may_render_sideways());
        assert!(
            !g(Some(90), true).may_render_sideways(),
            "thumbnail is upright"
        );
        assert!(!g(Some(180), false).may_render_sideways());
        assert!(
            !g(Some(0), false).may_render_sideways(),
            "metadata cannot prove sideways"
        );
        assert!(!g(None, false).may_render_sideways());
    }

    #[test]
    fn projects_from_before_signals_still_load() {
        let old = serde_json::json!({
            "asset": AssetId::new(), "source": "/x.mp4", "kind": "video",
            "start": 1.0, "duration": 2.0, "score": 0.3,
        });
        let slot: Slot = serde_json::from_value(old).unwrap();
        assert_eq!(slot.motion, None);
        assert_eq!(slot.geometry, None);
        assert_eq!(slot.near_duplicates, None);
        let back = serde_json::to_value(&slot).unwrap();
        assert!(
            back.get("motion").is_none(),
            "unknown fields are not invented"
        );
    }

    // -- render publication (synthetic media; skipped without ffmpeg) -------

    /// A one-frame solid-colour PNG, or None when ffmpeg is not installed.
    fn synthetic_photo(dir: &Path) -> Option<PathBuf> {
        let ffmpeg = eidetic_ingest::video::ffmpeg()?;
        let path = dir.join("red.png");
        let ok = Command::new(ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x36",
            ])
            .args(["-frames:v", "1"])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok, "ffmpeg could not make a synthetic photo");
        Some(path)
    }

    fn photo_slot(source: PathBuf) -> Slot {
        Slot {
            asset: AssetId::new(),
            source,
            kind: SlotKind::Photo,
            start: 0.0,
            duration: 0.5,
            focus_x: None,
            score: 0.0,
            pinned: false,
            transition_in: Transition::Cut,
            motion: None,
            geometry: None,
            near_duplicates: None,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn render_replaces_the_output_only_when_ffmpeg_succeeds() {
        let media = tempfile::tempdir().unwrap();
        let Some(photo) = synthetic_photo(media.path()) else {
            eprintln!("skipping: ffmpeg not installed");
            return;
        };
        let out_dir = tempfile::tempdir().unwrap();
        let output = out_dir.path().join("reel.mp4");
        std::fs::write(&output, b"previous reel").unwrap();

        // Failure mid-pipeline (the second segment's source is gone): the
        // previous file survives byte for byte and nothing is left beside it.
        let slots = vec![
            photo_slot(photo.clone()),
            photo_slot(media.path().join("missing.png")),
        ];
        let err = render(&slots, &output, 64, 36, Fit::Blur, None).unwrap_err();
        assert!(err.to_string().contains("segment 2"), "{err:#}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous reel");
        assert_eq!(names(out_dir.path()), vec!["reel.mp4"]);

        // Failure in the final ffmpeg run AFTER it has opened its output:
        // AAC cannot encode 16 channels, which ffmpeg only discovers once
        // the output file exists. Writing straight to `output` with -y
        // truncated the previous reel here.
        let track = media.path().join("sixteen.wav");
        let ok = Command::new(eidetic_ingest::video::ffmpeg().unwrap())
            .args(["-y", "-v", "error", "-f", "lavfi"])
            .args([
                "-i",
                "anullsrc=channel_layout=hexadecagonal:sample_rate=48000",
            ])
            .args(["-t", "1"])
            .arg(&track)
            .status()
            .unwrap()
            .success();
        assert!(ok, "ffmpeg could not make a synthetic 16-channel track");
        let slots = vec![photo_slot(photo.clone()), photo_slot(photo.clone())];
        let err = render(&slots, &output, 64, 36, Fit::Blur, Some(&track)).unwrap_err();
        assert!(err.to_string().contains("audio mux"), "{err:#}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous reel");
        assert_eq!(names(out_dir.path()), vec!["reel.mp4"]);

        // Success publishes a real video over it, again without litter.
        render(&slots, &output, 64, 36, Fit::Blur, None).unwrap();
        let probe = eidetic_ingest::video::probe(&output)
            .unwrap()
            .expect("ffprobe ships with ffmpeg");
        assert!(probe.duration_secs.unwrap_or(0.0) > 0.5, "{probe:?}");
        assert_eq!(names(out_dir.path()), vec!["reel.mp4"]);
    }
}
