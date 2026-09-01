//! Reel project files (goals-v0.8.md phase 0).
//!
//! A project file is the persisted cut list plus everything needed to
//! re-render it: JSON, written next to every rendered reel. It is the
//! object follow-up edits (`eidetic reel edit`) and, later, MCP agents
//! inspect and mutate turn by turn — without one, every instruction is a
//! full regeneration.

use crate::reel::{self, Slot};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use eidetic_ingest::beats::BeatGrid;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped when the schema changes incompatibly; loaders refuse newer files.
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ReelProject {
    pub version: u32,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    /// The prompt the reel was generated from (context for follow-ups; a
    /// swap query searches fresh, it does not re-embed this).
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    /// Framing style as given on the CLI: auto / cover / blur / pad.
    pub frame: String,
    /// The music (or narration) track laid under the cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<PathBuf>,
    /// The beat grid the cuts were synced to, kept so edits can reason
    /// about the music without re-decoding it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<BeatGrid>,
    /// Where the last render was written.
    pub output: PathBuf,
    pub slots: Vec<Slot>,
}

impl ReelProject {
    /// Where the project file for a render at `output` lives:
    /// `reel.mp4` → `reel.eidetic.json`, always side by side.
    pub fn path_for(output: &Path) -> PathBuf {
        output.with_extension("eidetic.json")
    }

    pub fn total_secs(&self) -> f64 {
        reel::total_secs(&self.slots)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        let project: ReelProject = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a reel project file", path.display()))?;
        if project.version > VERSION {
            anyhow::bail!(
                "{} is a version {} project; this build reads up to {}",
                path.display(),
                project.version,
                VERSION
            );
        }
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("serialize project")?;
        std::fs::write(path, json)
            .with_context(|| format!("failed to write project file {}", path.display()))?;
        Ok(())
    }

    /// A fresh project for a just-planned reel. Relative paths are made
    /// absolute so the file re-renders from any working directory.
    pub fn new(
        prompt: String,
        (width, height): (u32, u32),
        frame: String,
        audio: Option<PathBuf>,
        grid: Option<BeatGrid>,
        output: &Path,
        slots: Vec<Slot>,
    ) -> Self {
        let now = Utc::now();
        ReelProject {
            version: VERSION,
            created: now,
            modified: now,
            prompt,
            width,
            height,
            frame,
            audio: audio.map(|a| absolute(&a)),
            grid,
            output: absolute(output),
            slots,
        }
    }

    /// Print the cut list the way the reel command does — the shared
    /// vocabulary between renders, edits and (later) agent output.
    pub fn print_cut_list(&self) {
        println!(
            "Cut list for {:?} ({:.1}s from {} segment(s)):",
            self.prompt,
            self.total_secs(),
            self.slots.len()
        );
        for (i, slot) in self.slots.iter().enumerate() {
            println!("  {:>2}. {}", i + 1, slot.describe());
        }
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

// ---------------------------------------------------------------------------
// Generation: prompt → planned project(s). Shared by the CLI arm and the MCP
// create_reel tool, so NOTHING here may write to stdout — under `eidetic mcp`
// stdout is the protocol channel. Progress goes to stderr.
// ---------------------------------------------------------------------------

use eidetic_core::Config;
use eidetic_db::{AssetsRepo, SearchResult};

/// Everything `eidetic reel` (or an agent) specifies about a reel.
pub struct ReelSpec {
    pub prompt: String,
    pub duration: f64,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub frame: String,
    pub audio: Option<PathBuf>,
    pub min_sharpness: f64,
    pub variations: u32,
}

/// Plan project(s) for a spec — search, gate, beat/voiceover analysis, shot
/// assignment — without rendering anything. One project normally; several
/// with variations (rotated shot pools, cycled joint styles), named
/// `<output>.vN.mp4`.
pub async fn plan_projects(spec: &ReelSpec, config: &Config) -> Result<Vec<ReelProject>> {
    let models_dir = config.paths.models_cache.clone();
    let prompt = spec.prompt.clone();
    let query_emb = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<f32>> {
        let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
        embedder.embed_text(&prompt)
    })
    .await
    .context("embedder thread panicked")?
    .context("text embedding failed")?;

    let pool = eidetic_db::connect(config)
        .await
        .context("failed to connect to database")?;
    let repo = AssetsRepo::new(pool);

    // Over-fetch: the plan trims to the target duration and drops the weak
    // tail, so more candidates only ever improve the cut.
    let candidates = (spec.duration / 2.0).ceil() as u32 + 10;
    let results = repo
        .search(
            &spec.prompt,
            query_emb.as_slice(),
            candidates,
            &eidetic_db::SearchFilters::default(),
        )
        .await
        .context("search failed")?;

    // The sharpness gate (goals-v0.8.md phase 0): a quality floor for shots
    // picked blind. If it would empty the pool entirely, a soft reel beats
    // no reel — keep the ungated list.
    let results = if spec.min_sharpness > 0.0 {
        let library_dir = config.paths.library_dir.clone();
        let floor = spec.min_sharpness;
        tokio::task::spawn_blocking(move || {
            let gated: Vec<_> = results
                .iter()
                .filter(|r| reel::sharp_enough(r, &library_dir, floor))
                .cloned()
                .collect();
            if gated.is_empty() && !results.is_empty() {
                eprintln!("sharpness gate would drop every candidate; keeping them all");
                results
            } else {
                gated
            }
        })
        .await
        .context("sharpness thread panicked")?
    } else {
        results
    };

    let mut scenes = fetch_scenes(&repo, &results).await?;

    // With audio: detect beats and sync cuts to them; the reel runs at most
    // as long as the track. A track with no stable tempo falls back to
    // voiceover mode (speech builds) or fixed windows.
    let mut grid = None;
    #[allow(unused_mut)]
    let mut voiceover_segments: Option<Vec<(f64, f64, String)>> = None;
    let mut total = spec.duration;
    if let Some(track) = &spec.audio {
        anyhow::ensure!(track.exists(), "audio file not found: {}", track.display());
        let track_clone = track.clone();
        let pcm = tokio::task::spawn_blocking(move || {
            eidetic_ingest::video::extract_audio_pcm(&track_clone)
        })
        .await
        .context("audio thread panicked")?
        .context("failed to decode the audio track")?
        .ok_or_else(|| anyhow::anyhow!("ffmpeg is required for audio-driven reels"))?;
        let audio_len = pcm.len() as f64 / eidetic_ingest::beats::SAMPLE_RATE;
        total = spec.duration.min(audio_len);
        let pcm_for_beats = pcm.clone();
        grid =
            tokio::task::spawn_blocking(move || eidetic_ingest::beats::track_beats(&pcm_for_beats))
                .await
                .context("beat thread panicked")?;
        match &grid {
            Some(g) => eprintln!(
                "Beat grid: {:.1} BPM, {} beats, {} high-energy section(s)",
                g.bpm,
                g.beats.len(),
                g.high_energy.len()
            ),
            None => {
                // Not music — maybe narration. With a speech build, Whisper
                // decides: spoken words drive the shot list (voiceover
                // mode); otherwise fixed-length cuts.
                #[cfg(feature = "speech")]
                {
                    let models_dir = config.paths.models_cache.clone();
                    let segments = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<_> {
                        let t = eidetic_ml::speech::SpeechTranscriber::load(&models_dir)?;
                        t.transcribe(&pcm)
                    })
                    .await
                    .context("transcription thread panicked")?
                    .context("failed to transcribe the audio track")?;
                    if segments.is_empty() {
                        eprintln!(
                            "No stable tempo and no speech in {}; using fixed-length cuts.",
                            track.display()
                        );
                    } else {
                        eprintln!(
                            "Voiceover mode: {} spoken segment(s) drive the shot list",
                            segments.len()
                        );
                        voiceover_segments = Some(
                            segments
                                .into_iter()
                                .map(|s| (s.start_secs, s.end_secs, s.text))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
                #[cfg(not(feature = "speech"))]
                {
                    let _unused = pcm;
                    eprintln!(
                        "No stable tempo found in {} (voiceover mode needs a \
                         --features speech build); using fixed-length cuts.",
                        track.display()
                    );
                }
            }
        }
    }

    let mut focus = framing_focus(config, &repo, &results).await?;

    // Voiceover: each spoken span becomes a search; its best
    // un-recently-used hit becomes the shot for exactly that span.
    let mut voiceover_pairs: Option<Vec<(f64, SearchResult)>> = None;
    if let Some(segments) = &voiceover_segments {
        let spans = reel::speech_spans(segments, 2.5, 8.0, total);
        let texts: Vec<String> = spans.iter().map(|s| s.2.clone()).collect();
        let models_dir = config.paths.models_cache.clone();
        let embs = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<Vec<f32>>> {
            let mut e = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
            texts.iter().map(|t| e.embed_text(t)).collect()
        })
        .await
        .context("embedder thread panicked")?
        .context("failed to embed narration")?;

        let mut pairs = Vec::with_capacity(spans.len());
        let mut prev: Option<eidetic_core::AssetId> = None;
        for ((start, end, text), emb) in spans.iter().zip(&embs) {
            let hits = repo
                .search(text, emb, 5, &eidetic_db::SearchFilters::default())
                .await
                .context("voiceover search failed")?;
            let chosen = hits
                .iter()
                .find(|h| Some(h.id) != prev)
                .or_else(|| hits.first());
            if let Some(hit) = chosen {
                prev = Some(hit.id);
                pairs.push((end - start, hit.clone()));
            }
        }
        voiceover_pairs = Some(pairs);
    }

    // --variations: N candidate cut lists — the shot pool rotated so each
    // opens differently, the joint style cycled. The human (or agent) picks;
    // the winner's project re-renders full-res.
    if spec.variations > 1 {
        anyhow::ensure!(
            voiceover_pairs.is_none(),
            "variations need music or silence; a voiceover shot list is \
             determined by the narration"
        );
        let strong = reel::strong_prefix(&results).to_vec();
        let mut projects = Vec::new();
        for v in 1..=spec.variations as usize {
            let mut pool = strong.clone();
            if !pool.is_empty() {
                let k = (v - 1) % pool.len();
                pool.rotate_left(k);
            }
            let mut slots = match &grid {
                Some(g) => {
                    reel::plan_synced(&pool, g, total, &config.paths.library_dir, &scenes, &focus)
                }
                None => reel::plan(&pool, total, &config.paths.library_dir, &scenes),
            };
            reel::restyle(&mut slots, v - 1);
            anyhow::ensure!(
                !slots.is_empty(),
                "nothing in the library matches {:?} confidently enough for a reel",
                spec.prompt
            );
            projects.push(ReelProject::new(
                spec.prompt.clone(),
                (spec.width, spec.height),
                spec.frame.clone(),
                spec.audio.clone(),
                grid.clone(),
                &spec.output.with_extension(format!("v{v}.mp4")),
                slots,
            ));
        }
        return Ok(projects);
    }

    let slots = match (&voiceover_pairs, &grid) {
        (Some(pairs), _) => {
            // Framing and scene data for the chosen shots (they may not
            // overlap the prompt's search results).
            let chosen: Vec<SearchResult> = pairs.iter().map(|(_, r)| r.clone()).collect();
            focus.extend(framing_focus(config, &repo, &chosen).await?);
            scenes.extend(fetch_scenes(&repo, &chosen).await?);
            reel::plan_assigned(pairs, &config.paths.library_dir, &scenes, &focus)
        }
        (None, Some(g)) => reel::plan_synced(
            &results,
            g,
            total,
            &config.paths.library_dir,
            &scenes,
            &focus,
        ),
        (None, None) => reel::plan(&results, total, &config.paths.library_dir, &scenes),
    };
    anyhow::ensure!(
        !slots.is_empty(),
        "nothing in the library matches {:?} confidently enough for a reel",
        spec.prompt
    );

    Ok(vec![ReelProject::new(
        spec.prompt.clone(),
        (spec.width, spec.height),
        spec.frame.clone(),
        spec.audio.clone(),
        grid,
        &spec.output,
        slots,
    )])
}

/// Render a project to its recorded output — or to a smaller preview
/// geometry when `size` overrides (variation previews).
pub async fn render_project(proj: &ReelProject, size: Option<(u32, u32)>) -> Result<()> {
    let (w, h) = size.unwrap_or((proj.width, proj.height));
    let fit = reel::Fit::parse(&proj.frame, proj.height > proj.width)?;
    if let Some(track) = &proj.audio {
        anyhow::ensure!(
            track.exists(),
            "the project's audio track has moved: {}",
            track.display()
        );
    }
    let slots = proj.slots.clone();
    let output = proj.output.clone();
    let audio = proj.audio.clone();
    tokio::task::spawn_blocking(move || reel::render(&slots, &output, w, h, fit, audio.as_deref()))
        .await
        .context("render thread panicked")?
}

/// Scene boundaries per video hit, so cuts snap to shots. Missing detection
/// (imported pre-v0.5, or ffmpeg-less embed) just means plain windows.
async fn fetch_scenes(
    repo: &AssetsRepo,
    results: &[SearchResult],
) -> Result<std::collections::HashMap<eidetic_core::AssetId, Vec<f64>>> {
    let mut scenes = std::collections::HashMap::new();
    for r in results {
        if r.frame_ts.is_some()
            && let Some(s) = repo
                .fetch_video_scenes(r.id)
                .await
                .context("failed to load scene boundaries")?
        {
            scenes.insert(r.id, s);
        }
    }
    Ok(scenes)
}

/// Face-aware framing data (goals-v0.7.md phase 1b): which assets have
/// faces (auto framing: faces → follow them with a cover-crop; none →
/// scenic blur-fill), and the weighted horizontal face centre for videos.
pub(crate) async fn framing_focus(
    config: &Config,
    repo: &AssetsRepo,
    results: &[SearchResult],
) -> Result<std::collections::HashMap<eidetic_core::AssetId, f64>> {
    let mut focus = std::collections::HashMap::new();
    let faces_repo = eidetic_db::FacesRepo::new(
        eidetic_db::connect(config)
            .await
            .context("failed to connect to database")?,
    );
    for r in results {
        if focus.contains_key(&r.id) {
            continue;
        }
        let faces = faces_repo
            .fetch_face_boxes_for_asset(r.id)
            .await
            .context("failed to load faces for framing")?;
        if faces.is_empty() {
            continue;
        }
        // Photos only need presence (Ken Burns centres itself); videos get
        // a weighted horizontal centre when the pixel width is known.
        let mut fx = 0.5;
        if r.frame_ts.is_some()
            && let Some(width) = repo
                .fetch_by_id(r.id)
                .await
                .context("failed to load asset for framing")?
                .and_then(|d| d.pixel_width)
                .filter(|w| *w > 0)
        {
            let mut weight = 0.0f64;
            let mut cx = 0.0f64;
            for ((x, _, w, _), score) in &faces {
                cx += (x + w / 2.0) as f64 * *score as f64;
                weight += *score as f64;
            }
            if weight > 0.0 {
                fx = (cx / weight / width as f64).clamp(0.0, 1.0);
            }
        }
        focus.insert(r.id, fx);
    }
    Ok(focus)
}

// ---------------------------------------------------------------------------
// Edit ops: the structured mutations `reel edit` flags and the MCP edit_reel
// tool both compile down to.
// ---------------------------------------------------------------------------

/// One structured mutation of a cut list. `slot` numbers are 1-based
/// against the cut list as last printed; a batch resolves every number
/// against that same numbering before anything is applied.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// Protect a slot from swap/drop.
    Pin {
        slot: usize,
    },
    Unpin {
        slot: usize,
    },
    /// Set a slot's length in seconds (videos clamp to real media).
    Retime {
        slot: usize,
        secs: f64,
    },
    /// Set the transition INTO a slot: cut, crossfade, dip, slide, whip —
    /// optionally with :secs (e.g. "crossfade:0.8").
    Transition {
        slot: usize,
        style: String,
    },
    /// Replace a slot with the best library hit for a fresh query; the
    /// slot's length is kept so beat sync survives.
    Swap {
        slot: usize,
        query: String,
    },
    /// Remove a slot (later cuts move earlier).
    Drop {
        slot: usize,
    },
    /// Permute the surviving slots; must list each exactly once.
    Reorder {
        order: Vec<usize>,
    },
}

/// Apply a batch of ops to a project, in the documented fixed order:
/// pin/unpin → retime → transition → swap → drop → reorder. Progress and
/// swap decisions go to stderr; the caller prints (or returns) the new cut
/// list.
pub async fn apply_ops(proj: &mut ReelProject, ops: &[EditOp], config: &Config) -> Result<()> {
    use std::collections::HashSet;

    let n = proj.slots.len();
    let idx = |slot: usize| -> Result<usize> {
        anyhow::ensure!(
            (1..=n).contains(&slot),
            "no slot {slot}; the cut list has {n} slot(s)"
        );
        Ok(slot - 1)
    };

    // Resolve and validate everything up front so a bad op fails before
    // any mutation.
    let mut pins = Vec::new();
    let mut unpins = Vec::new();
    let mut retimes = Vec::new();
    let mut transitions = Vec::new();
    let mut swaps: Vec<(usize, String)> = Vec::new();
    let mut drops: HashSet<usize> = HashSet::new();
    let mut reorder: Option<Vec<usize>> = None;
    for op in ops {
        match op {
            EditOp::Pin { slot } => pins.push(idx(*slot)?),
            EditOp::Unpin { slot } => unpins.push(idx(*slot)?),
            EditOp::Retime { slot, secs } => {
                anyhow::ensure!(
                    *secs >= 0.3,
                    "retime {slot} to {secs}s: slots shorter than 0.3s read as glitches"
                );
                retimes.push((idx(*slot)?, *secs));
            }
            EditOp::Transition { slot, style } => {
                transitions.push((idx(*slot)?, reel::Transition::parse(style)?));
            }
            EditOp::Swap { slot, query } => swaps.push((idx(*slot)?, query.clone())),
            EditOp::Drop { slot } => {
                drops.insert(idx(*slot)?);
            }
            EditOp::Reorder { order } => {
                anyhow::ensure!(reorder.is_none(), "at most one reorder per edit");
                reorder = Some(
                    order
                        .iter()
                        .map(|&s| idx(s))
                        .collect::<Result<Vec<usize>>>()?,
                );
            }
        }
    }

    for i in pins {
        proj.slots[i].pinned = true;
    }
    for i in unpins {
        proj.slots[i].pinned = false;
    }
    for &i in &drops {
        anyhow::ensure!(
            !proj.slots[i].pinned,
            "slot {} is pinned; unpin it first if you really want to drop it",
            i + 1
        );
    }

    for (i, secs) in retimes {
        let slot = &mut proj.slots[i];
        let mut secs = secs;
        // Clamp a video to the media past its in-point; a shortened file
        // would silently desync everything after it.
        if slot.kind == reel::SlotKind::Video
            && let Ok(Some(p)) = eidetic_ingest::video::probe(&slot.source)
            && let Some(d) = p.duration_secs
        {
            let avail = (d - slot.start).max(0.3);
            if secs > avail {
                eprintln!(
                    "slot {}: only {avail:.1}s of media past the in-point; clamping",
                    i + 1
                );
                secs = avail;
            }
        }
        slot.duration = secs;
    }

    for (i, t) in transitions {
        if i == 0 {
            eprintln!("slot 1 opens the reel and has no joint; transition ignored");
            continue;
        }
        proj.slots[i].transition_in = t;
    }

    if !swaps.is_empty() {
        for (i, _) in &swaps {
            anyhow::ensure!(
                !proj.slots[*i].pinned,
                "slot {} is pinned; unpin it first if you really want to swap it",
                i + 1
            );
        }
        let queries: Vec<String> = swaps.iter().map(|(_, q)| q.clone()).collect();
        let models_dir = config.paths.models_cache.clone();
        let embs = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<Vec<f32>>> {
            let mut e = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
            queries.iter().map(|q| e.embed_text(q)).collect()
        })
        .await
        .context("embedder thread panicked")?
        .context("failed to embed swap queries")?;

        let pool = eidetic_db::connect(config)
            .await
            .context("failed to connect to database")?;
        let repo = AssetsRepo::new(pool);
        let mut used: std::collections::HashSet<eidetic_core::AssetId> =
            proj.slots.iter().map(|s| s.asset).collect();

        for ((i, query), emb) in swaps.iter().zip(&embs) {
            let hits = repo
                .search(query, emb, 10, &eidetic_db::SearchFilters::default())
                .await
                .context("swap search failed")?;
            // Prefer something not already in the reel and sharp enough; a
            // swap that hands back a shot the user is looking at (or a
            // blurry one) helps nobody. Degrade gracefully: unused beats
            // sharp beats give-up.
            let lib = config.paths.library_dir.clone();
            let hit = hits
                .iter()
                .find(|h| {
                    h.score > 0.0
                        && !used.contains(&h.id)
                        && reel::sharp_enough(h, &lib, reel::MIN_SHARPNESS)
                })
                .or_else(|| hits.iter().find(|h| h.score > 0.0 && !used.contains(&h.id)))
                .or_else(|| hits.iter().find(|h| h.score > 0.0))
                .with_context(|| format!("nothing in the library matches {query:?}"))?;
            let scenes = fetch_scenes(&repo, std::slice::from_ref(hit)).await?;
            let focus = framing_focus(config, &repo, std::slice::from_ref(hit)).await?;
            let old = &proj.slots[*i];
            let mut slot = reel::fill_span(
                hit,
                old.duration,
                &config.paths.library_dir,
                &scenes,
                &focus,
            );
            // The joint belongs to the timeline position, not the shot.
            slot.transition_in = old.transition_in;
            used.insert(slot.asset);
            eprintln!("slot {}: {} -> {}", i + 1, old.describe(), slot.describe());
            proj.slots[*i] = slot;
        }
    }

    // Drops and the reorder both work over ORIGINAL indices: the reorder
    // must name each surviving slot exactly once.
    let survivors: Vec<usize> = (0..n).filter(|i| !drops.contains(i)).collect();
    let order = match reorder {
        None => survivors,
        Some(idxs) => {
            let mut sorted = idxs.clone();
            sorted.sort_unstable();
            anyhow::ensure!(
                sorted == survivors,
                "the reorder must list each remaining slot exactly once ({} slot(s): {})",
                survivors.len(),
                survivors
                    .iter()
                    .map(|i| (i + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            idxs
        }
    };
    anyhow::ensure!(
        !order.is_empty(),
        "every slot was dropped; nothing left to render"
    );
    let mut old: Vec<Option<Slot>> = proj.slots.drain(..).map(Some).collect();
    proj.slots = order
        .iter()
        .map(|&i| {
            old[i]
                .take()
                .expect("order is a permutation, so each index is taken exactly once")
        })
        .collect();
    proj.modified = Utc::now();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reel::{SlotKind, Transition};
    use eidetic_core::AssetId;

    fn slot() -> Slot {
        Slot {
            asset: AssetId::new(),
            source: PathBuf::from("/lib/ab/cd/abcd.mp4"),
            kind: SlotKind::Video,
            start: 3.2,
            duration: 2.0,
            focus_x: Some(0.4),
            score: 0.31,
            pinned: true,
            transition_in: Transition::Crossfade { secs: 0.5 },
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("reel.mp4");
        let p = ReelProject::new(
            "sunset".into(),
            (1080, 1920),
            "auto".into(),
            Some(PathBuf::from("/music/track.m4a")),
            None,
            &out,
            vec![slot()],
        );
        let path = ReelProject::path_for(&out);
        assert!(path.ends_with("reel.eidetic.json"));
        p.save(&path).unwrap();
        let back = ReelProject::load(&path).unwrap();
        assert_eq!(back.version, VERSION);
        assert_eq!(back.prompt, "sunset");
        assert_eq!(back.slots.len(), 1);
        assert_eq!(back.slots[0].asset, p.slots[0].asset);
        assert!(back.slots[0].pinned);
        assert_eq!(
            back.slots[0].transition_in,
            Transition::Crossfade { secs: 0.5 }
        );
        assert!(back.output.is_absolute());
    }

    #[test]
    fn refuses_files_from_the_future() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.eidetic.json");
        let mut p = ReelProject::new(
            "x".into(),
            (1920, 1080),
            "pad".into(),
            None,
            None,
            Path::new("/tmp/x.mp4"),
            vec![],
        );
        p.version = VERSION + 1;
        p.save(&path).unwrap();
        let err = ReelProject::load(&path).unwrap_err().to_string();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn rejects_non_project_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.json");
        std::fs::write(&path, b"{\"hello\": 1}").unwrap();
        let err = ReelProject::load(&path).unwrap_err().to_string();
        assert!(err.contains("not a reel project"), "{err}");
    }
}
