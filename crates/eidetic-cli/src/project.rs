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
        Self::parse(path, &bytes)
    }

    /// [`load`](Self::load) for paths nobody vetted: refuses anything but a
    /// regular file (a FIFO would block the read forever, a device would
    /// never end) and reads at most `max_bytes`, so a hostile or mistaken
    /// path costs a bounded allocation.
    pub fn load_bounded(path: &Path, max_bytes: u64) -> Result<Self> {
        use std::io::Read;
        let not_a_file = || {
            anyhow::anyhow!(
                "{} is not a regular file; pass a reel project file",
                path.display()
            )
        };
        // Checked before opening: opening a FIFO for reading blocks until a
        // writer appears. Re-checked on the handle in case the path was
        // swapped in between.
        let meta = std::fs::metadata(path)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        anyhow::ensure!(meta.is_file(), not_a_file());
        let file = std::fs::File::open(path)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        anyhow::ensure!(file.metadata()?.is_file(), not_a_file());
        let mut bytes = Vec::new();
        file.take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        anyhow::ensure!(
            bytes.len() as u64 <= max_bytes,
            "{} is larger than {max_bytes} bytes; that is not a reel project file",
            path.display()
        );
        Self::parse(path, &bytes)
    }

    fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        let project: ReelProject = serde_json::from_slice(bytes)
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

    /// Atomically replace the project using a complete, synced sibling file.
    /// Editing decisions cannot be regenerated, so failures before replacement
    /// must leave the previous project intact. Existing handles and hard links
    /// keep reading the old file rather than observing an in-place truncation.
    ///
    /// The parent directory is not synced: this protects against interrupted
    /// writes, but does not guarantee persistence of the rename after power loss.
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;

        let json = serde_json::to_vec_pretty(self).context("serialize project")?;
        let dir = parent_dir(path);
        let mut tmp = tempfile::Builder::new()
            .prefix(".eidetic-project-")
            .suffix(".json.tmp")
            .tempfile_in(dir)
            .with_context(|| format!("failed to create a temp file next to {}", path.display()))?;
        // NamedTempFile removes itself on drop, so every early return below
        // leaves the destination alone and no litter in the directory.
        tmp.write_all(&json)
            .and_then(|()| tmp.flush())
            .and_then(|()| tmp.as_file().sync_all())
            .with_context(|| format!("failed to write project file {}", path.display()))?;
        tmp.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("failed to replace project file {}", path.display()))?;
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

/// The directory `path` lives in, for staging a temp file beside it — the
/// rename has to stay on one filesystem. A bare filename like
/// `reel.eidetic.json` has an empty parent, which means the current
/// directory.
pub(crate) fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

// ---------------------------------------------------------------------------
// Beat tracking backend selection (docs/beat-tracking.md).
// ---------------------------------------------------------------------------

/// Which tracker `EIDETIC_BEAT_TRACKER` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerChoice {
    /// Ellis only; no model is ever touched.
    Ellis,
    /// The default: Beat This when this build has it *and* its model is
    /// already cached and verifies; otherwise Ellis, with the reason kept
    /// on the grid. Never downloads.
    Auto,
    /// Beat This, downloading the pinned model on first use. Any failure is
    /// an error — the user asked for inferred bars, so heuristic ones are
    /// not substituted.
    BeatThis,
}

impl TrackerChoice {
    pub fn from_env() -> Result<Self> {
        Self::parse(std::env::var("EIDETIC_BEAT_TRACKER").ok().as_deref())
    }

    /// Unknown values fail loudly (the `EIDETIC_ACCELERATOR` rule): a typo
    /// must not silently mean "Ellis".
    fn parse(v: Option<&str>) -> Result<Self> {
        match v.map(str::trim) {
            None | Some("") | Some("auto") => Ok(Self::Auto),
            Some("ellis") => Ok(Self::Ellis),
            Some("beat-this") => Ok(Self::BeatThis),
            Some(other) => anyhow::bail!(
                "EIDETIC_BEAT_TRACKER={other:?} is not one of: auto, ellis, beat-this"
            ),
        }
    }
}

/// Beat grid for `track` (whose 16 kHz decode is `pcm`) from the chosen
/// backend. `None` means no stable tempo. Blocking; run off the runtime.
pub fn detect_beats(
    choice: TrackerChoice,
    track: &Path,
    pcm: &[f32],
    models_dir: &Path,
) -> Result<Option<BeatGrid>> {
    use eidetic_ingest::beats::track_beats;
    match choice {
        TrackerChoice::Ellis => Ok(track_beats(pcm)),
        // Without the feature there is nothing to try: plain Ellis, and no
        // "fallback" note on every grid for a backend that was never built.
        TrackerChoice::Auto if !cfg!(feature = "downbeats") => Ok(track_beats(pcm)),
        TrackerChoice::Auto => match infer_beats(track, pcm, models_dir, false) {
            Ok(grid) => Ok(grid),
            Err(e) => {
                let reason = format!("{e:#}");
                eprintln!("Beat This unavailable ({reason}); using the Ellis tracker");
                Ok(track_beats(pcm).map(|mut g| {
                    g.fallback = Some(reason);
                    g
                }))
            }
        },
        TrackerChoice::BeatThis => infer_beats(track, pcm, models_dir, true).context(
            "EIDETIC_BEAT_TRACKER=beat-this failed (set it to auto or ellis to fall back)",
        ),
    }
}

#[cfg(feature = "downbeats")]
fn infer_beats(
    track: &Path,
    pcm: &[f32],
    models_dir: &Path,
    download: bool,
) -> Result<Option<BeatGrid>> {
    use eidetic_ml::downbeats::{self, BeatThisTracker, Fetch};
    let fetch = if download {
        Fetch::Download
    } else {
        Fetch::CachedOnly
    };
    let mut tracker = BeatThisTracker::load(models_dir, fetch)?;
    let pcm22 = eidetic_ingest::video::extract_audio_pcm_at(track, downbeats::SAMPLE_RATE)
        .context("failed to decode the audio track for Beat This")?
        .context("ffmpeg is required to decode audio")?;
    let out = tracker.analyze(&pcm22)?;
    Ok(BeatGrid::from_inferred(
        &out.beats,
        &out.downbeats,
        pcm,
        eidetic_ingest::beats::ModelProvenance {
            name: downbeats::MODEL_NAME.into(),
            revision: downbeats::REVISION.into(),
            sha256: downbeats::BEAT_MODEL_SHA256.into(),
        },
    ))
}

#[cfg(not(feature = "downbeats"))]
fn infer_beats(_: &Path, _: &[f32], _: &Path, _: bool) -> Result<Option<BeatGrid>> {
    anyhow::bail!("this build has no Beat This support; rebuild with --features downbeats")
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
    let (model_id, query_emb) =
        tokio::task::spawn_blocking(move || -> eidetic_ml::Result<(&'static str, Vec<f32>)> {
            let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
            Ok((embedder.model_id(), embedder.embed_text(&prompt)?))
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
            model_id,
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
        let (track_for_beats, models_dir) = (track.clone(), config.paths.models_cache.clone());
        grid = tokio::task::spawn_blocking(move || {
            detect_beats(
                TrackerChoice::from_env()?,
                &track_for_beats,
                &pcm_for_beats,
                &models_dir,
            )
        })
        .await
        .context("beat thread panicked")??;
        match &grid {
            Some(g) => eprintln!(
                "Beat grid: {:.1} BPM, {} beats, {} {} bar(s), {} high-energy section(s)",
                g.bpm,
                g.beats.len(),
                g.downbeats.len(),
                if g.downbeats_inferred() {
                    "inferred"
                } else {
                    "heuristic"
                },
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
        let (model_id, embs) = tokio::task::spawn_blocking(
            move || -> eidetic_ml::Result<(&'static str, Vec<Vec<f32>>)> {
                let mut e = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                let embs = texts
                    .iter()
                    .map(|t| e.embed_text(t))
                    .collect::<eidetic_ml::Result<_>>()?;
                Ok((e.model_id(), embs))
            },
        )
        .await
        .context("embedder thread panicked")?
        .context("failed to embed narration")?;

        let mut pairs = Vec::with_capacity(spans.len());
        let mut prev: Option<eidetic_core::AssetId> = None;
        for ((start, end, text), emb) in spans.iter().zip(&embs) {
            let hits = repo
                .search(
                    text,
                    emb,
                    model_id,
                    5,
                    &eidetic_db::SearchFilters::default(),
                )
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

/// Where a render lands and at what size.
pub struct RenderTarget {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

impl ReelProject {
    /// The full-resolution render at the project's output, or a
    /// half-resolution preview beside it: `reel.mp4` → `reel.preview.mp4`.
    /// A preview never lands on the output, so reviewing a cut cannot
    /// replace the full-res reel, and the project's own geometry and output
    /// stay what the next full render uses.
    pub fn render_target(&self, preview: bool) -> RenderTarget {
        if !preview {
            return RenderTarget {
                path: self.output.clone(),
                width: self.width,
                height: self.height,
            };
        }
        let ext = self
            .output
            .extension()
            .map_or_else(|| "mp4".into(), |e| e.to_string_lossy().into_owned());
        RenderTarget {
            path: self.output.with_extension(format!("preview.{ext}")),
            width: (self.width / 2) & !1,
            height: (self.height / 2) & !1,
        }
    }
}

/// Render a project's cut list to `target` (see
/// [`ReelProject::render_target`]).
pub async fn render_project(proj: &ReelProject, target: &RenderTarget) -> Result<()> {
    let (w, h) = (target.width, target.height);
    let fit = reel::Fit::parse(&proj.frame, proj.height > proj.width)?;
    if let Some(track) = &proj.audio {
        anyhow::ensure!(
            track.exists(),
            "the project's audio track has moved: {}",
            track.display()
        );
    }
    let slots = proj.slots.clone();
    let output = target.path.clone();
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
/// against that same numbering before anything is applied, whatever order
/// the ops are listed in. A drop does not renumber later ops in the batch.
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
    /// Set the transition INTO a slot's shot: cut, crossfade, dip, slide,
    /// whip — optionally with :secs (e.g. "crossfade:0.8"). The transition
    /// moves with the shot on reorder; the shot that ends up first has no
    /// joint, so its transition is ignored.
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
    /// The new order, as original slot numbers: must list every slot not
    /// dropped in this batch exactly once (e.g. drop 2 of 4, then order
    /// [4, 1, 3]). At most one per batch.
    Reorder {
        order: Vec<usize>,
    },
}

/// Apply a batch of ops to a project, in the documented fixed order:
/// pin/unpin → retime → transition → swap → drop → reorder, every slot
/// number meaning the ORIGINAL position. Pins apply first, so a batch that
/// pins a slot also refuses to swap or drop it, and one that unpins it
/// allows both. Any error leaves `proj` partly mutated in memory; callers
/// save only on success. Progress and swap decisions go to stderr; the
/// caller prints (or returns) the new cut list.
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

    // A transition belongs to its shot and travels with it through the
    // reorder below; only the shot that ends up first has no joint.
    let transitioned: HashSet<usize> = transitions.iter().map(|(i, _)| *i).collect();
    for (i, t) in transitions {
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
        let (model_id, embs) = tokio::task::spawn_blocking(
            move || -> eidetic_ml::Result<(&'static str, Vec<Vec<f32>>)> {
                let mut e = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                let embs = queries
                    .iter()
                    .map(|q| e.embed_text(q))
                    .collect::<eidetic_ml::Result<_>>()?;
                Ok((e.model_id(), embs))
            },
        )
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
                .search(
                    query,
                    emb,
                    model_id,
                    10,
                    &eidetic_db::SearchFilters::default(),
                )
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
            // A swap replaces the footage, not the edit decision: the
            // slot keeps its joint (and its length, above).
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
    if transitioned.contains(&order[0]) {
        eprintln!(
            "slot {} opens the reel and has no joint; its transition is ignored",
            order[0] + 1
        );
    }
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
    fn tracker_choice_defaults_to_auto_and_refuses_typos() {
        assert_eq!(TrackerChoice::parse(None).unwrap(), TrackerChoice::Auto);
        assert_eq!(TrackerChoice::parse(Some("")).unwrap(), TrackerChoice::Auto);
        assert_eq!(
            TrackerChoice::parse(Some("ellis")).unwrap(),
            TrackerChoice::Ellis
        );
        assert_eq!(
            TrackerChoice::parse(Some("beat-this")).unwrap(),
            TrackerChoice::BeatThis
        );
        let err = TrackerChoice::parse(Some("beatthis"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("auto, ellis, beat-this"), "{err}");
    }

    /// A v0.8 project file: its grid has no tracker/model/fallback. It must
    /// load, and must not claim its every-4th-beat bars were inferred.
    #[test]
    fn pre_v09_projects_load_with_heuristic_bars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.eidetic.json");
        let json = r#"{
            "version": 1,
            "created": "2026-08-01T10:00:00Z",
            "modified": "2026-08-01T10:00:00Z",
            "prompt": "rafting",
            "width": 1080, "height": 1920, "frame": "auto",
            "audio": "/music/track.m4a",
            "grid": {"bpm": 120.0, "beats": [0.0, 0.5, 1.0, 1.5, 2.0],
                     "downbeats": [0, 4], "high_energy": [[1.0, 2.0]]},
            "output": "/out/reel.mp4",
            "slots": []
        }"#;
        std::fs::write(&path, json).unwrap();
        let p = ReelProject::load(&path).unwrap();
        let g = p.grid.expect("grid kept");
        assert_eq!(g.tracker, eidetic_ingest::beats::Tracker::Ellis);
        assert!(!g.downbeats_inferred());
        assert_eq!(g.downbeats, vec![0, 4]);
    }

    #[test]
    fn inferred_grid_provenance_round_trips() {
        use eidetic_ingest::beats::{BeatGrid, ModelProvenance};
        let beats: Vec<f64> = (0..12).map(|i| i as f64 * 0.5).collect();
        let grid = BeatGrid::from_inferred(
            &beats,
            &[0.0, 1.5, 3.0],
            &[],
            ModelProvenance {
                name: "beat_this_small".into(),
                revision: "rev".into(),
                sha256: "digest".into(),
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("reel.mp4");
        let p = ReelProject::new(
            "x".into(),
            (1080, 1920),
            "auto".into(),
            None,
            Some(grid),
            &out,
            vec![],
        );
        let path = ReelProject::path_for(&out);
        p.save(&path).unwrap();
        let g = ReelProject::load(&path).unwrap().grid.unwrap();
        assert!(g.downbeats_inferred());
        assert_eq!(g.downbeats, vec![0, 3, 6]);
        assert_eq!(g.model.unwrap().sha256, "digest");
    }

    /// 120 BPM clicks at 16 kHz — enough for Ellis to lock on.
    fn clicks_16k(secs: f64) -> Vec<f32> {
        let n = (secs * 16_000.0) as usize;
        let mut seed = 0x2545F4914F6CDD1Du64;
        (0..n)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let r = (seed as f64 / u64::MAX as f64) as f32 - 0.5;
                let since = i % 8000;
                r * 0.01
                    + if since < 800 {
                        r * (1.0 - since as f32 / 800.0)
                    } else {
                        0.0
                    }
            })
            .collect()
    }

    #[test]
    fn ellis_choice_never_touches_models() {
        let models = tempfile::tempdir().unwrap();
        let g = detect_beats(
            TrackerChoice::Ellis,
            Path::new("/nonexistent.wav"),
            &clicks_16k(15.0),
            models.path(),
        )
        .unwrap()
        .expect("tempo");
        assert_eq!(g.tracker, eidetic_ingest::beats::Tracker::Ellis);
        assert!(!g.downbeats_inferred() && g.fallback.is_none());
        assert_eq!(std::fs::read_dir(models.path()).unwrap().count(), 0);
    }

    #[cfg(not(feature = "downbeats"))]
    #[test]
    fn default_build_auto_is_plain_ellis_and_explicit_beat_this_errors() {
        let models = tempfile::tempdir().unwrap();
        let pcm = clicks_16k(15.0);
        let track = Path::new("/nonexistent.wav");
        let g = detect_beats(TrackerChoice::Auto, track, &pcm, models.path())
            .unwrap()
            .expect("tempo");
        assert_eq!(g.tracker, eidetic_ingest::beats::Tracker::Ellis);
        assert!(g.fallback.is_none());
        let err = detect_beats(TrackerChoice::BeatThis, track, &pcm, models.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("--features downbeats"),
            "{err:#}"
        );
    }

    /// Auto with nothing cached: Ellis, the reason recorded, and nothing
    /// downloaded or written into the models dir.
    #[cfg(feature = "downbeats")]
    #[test]
    fn auto_without_a_cached_model_falls_back_and_says_why() {
        let models = tempfile::tempdir().unwrap();
        let g = detect_beats(
            TrackerChoice::Auto,
            Path::new("/nonexistent.wav"),
            &clicks_16k(15.0),
            models.path(),
        )
        .unwrap()
        .expect("tempo");
        assert_eq!(g.tracker, eidetic_ingest::beats::Tracker::Ellis);
        assert!(!g.downbeats_inferred());
        let why = g.fallback.expect("fallback reason");
        assert!(why.contains("not cached"), "{why}");
        assert_eq!(std::fs::read_dir(models.path()).unwrap().count(), 0);
    }

    #[cfg(feature = "downbeats")]
    #[test]
    fn auto_with_a_corrupt_cached_model_falls_back_and_says_why() {
        let models = tempfile::tempdir().unwrap();
        let cache = eidetic_ml::downbeats::cache_dir(models.path());
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("mel_spectrogram.onnx"), b"truncated").unwrap();
        let g = detect_beats(
            TrackerChoice::Auto,
            Path::new("/nonexistent.wav"),
            &clicks_16k(15.0),
            models.path(),
        )
        .unwrap()
        .expect("tempo");
        assert!(!g.downbeats_inferred());
        let why = g.fallback.expect("fallback reason");
        assert!(why.contains("digest"), "{why}");
    }

    /// The whole production path — ffmpeg decode at 22.05 kHz, digest
    /// check, ort inference, grid construction — on a synthetic WAV with
    /// the supplied (never downloaded) model files. Skips, saying so, when
    /// the files or ffmpeg are absent.
    #[cfg(feature = "downbeats")]
    #[test]
    fn auto_with_cached_models_infers_downbeats_end_to_end() {
        let src = std::env::var_os("EIDETIC_BEAT_THIS_MODELS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tmp/beat-this-support")
            });
        if !src.join("beat_this_small.onnx").is_file() || eidetic_ingest::video::ffmpeg().is_none()
        {
            eprintln!("SKIPPED: needs supplied Beat This models and ffmpeg");
            return;
        }
        let models = tempfile::tempdir().unwrap();
        let cache = eidetic_ml::downbeats::cache_dir(models.path());
        std::fs::create_dir_all(&cache).unwrap();
        for name in ["mel_spectrogram.onnx", "beat_this_small.onnx"] {
            std::fs::copy(src.join(name), cache.join(name)).unwrap();
        }
        let pcm = clicks_16k(20.0);
        let wav = models.path().join("clicks.wav");
        write_wav_16k(&wav, &pcm);

        let g = detect_beats(TrackerChoice::Auto, &wav, &pcm, models.path())
            .unwrap()
            .expect("tempo");
        assert_eq!(g.tracker, eidetic_ingest::beats::Tracker::BeatThis);
        assert!(g.downbeats_inferred(), "{g:?}");
        assert!(g.fallback.is_none());
        assert!((g.bpm - 120.0).abs() < 6.0, "bpm {}", g.bpm);
        assert!(!g.downbeats.is_empty());
        assert_eq!(
            g.model.as_ref().unwrap().sha256,
            eidetic_ml::downbeats::BEAT_MODEL_SHA256
        );
    }

    #[cfg(feature = "downbeats")]
    fn write_wav_16k(path: &Path, pcm: &[f32]) {
        let data: Vec<u8> = pcm
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&16_000u32.to_le_bytes());
        out.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
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

    fn project(prompt: &str, out: &Path) -> ReelProject {
        ReelProject::new(
            prompt.into(),
            (1080, 1920),
            "auto".into(),
            None,
            None,
            out,
            vec![slot()],
        )
    }

    #[test]
    fn save_replaces_an_existing_project_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reel.eidetic.json");

        project("first", &path).save(&path).unwrap();
        project("second", &path).save(&path).unwrap();

        assert_eq!(ReelProject::load(&path).unwrap().prompt, "second");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("reel.eidetic.json")]);
    }

    /// Replacement, not overwrite-in-place. The hard link is a second name
    /// for the saved file's inode; a `save` that truncates and rewrites that
    /// inode — as `fs::write` did — changes what the link resolves to, and
    /// a save that fails halfway leaves the link on a half-written project.
    /// After a rename the link still names the old inode, complete.
    ///
    /// This is the property the previous implementation did not have, and the
    /// closest a test can get to "a reader mid-save sees a whole project"
    /// without racing a second thread.
    #[test]
    fn save_replaces_the_file_rather_than_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reel.eidetic.json");
        project("first", &path).save(&path).unwrap();

        let link = dir.path().join("first.hardlink.json");
        std::fs::hard_link(&path, &link).unwrap();

        project("second", &path).save(&path).unwrap();

        let old = ReelProject::load(&link).expect("old inode is still a whole project");
        assert_eq!(old.prompt, "first");
        assert_eq!(old.slots.len(), 1);
        assert_eq!(ReelProject::load(&path).unwrap().prompt, "second");

        // And the two names are now different files, not one shared inode.
        assert_ne!(
            std::fs::read(&link).unwrap(),
            std::fs::read(&path).unwrap(),
            "save wrote through the existing inode"
        );
    }

    /// What this covers: a save that fails at the rename removes its temp
    /// file, does not disturb the destination, and does not touch unrelated
    /// projects in the same directory. It does *not* cover an overwrite of
    /// an existing project failing partway — see
    /// `save_replaces_the_file_rather_than_writing_through_it` for that.
    ///
    /// A directory at the destination is the portable way to make `persist`
    /// fail after the temp file has been fully written and synced.
    #[test]
    fn failed_save_removes_its_temp_file_and_leaves_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("reel.eidetic.json");
        project("keep me", &good).save(&good).unwrap();

        let blocked = dir.path().join("blocked.eidetic.json");
        std::fs::create_dir(&blocked).unwrap();
        let err = project("never lands", &blocked)
            .save(&blocked)
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked.eidetic.json"), "{err}");

        assert_eq!(ReelProject::load(&good).unwrap().prompt, "keep me");
        assert!(blocked.is_dir());
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                std::ffi::OsString::from("blocked.eidetic.json"),
                std::ffi::OsString::from("reel.eidetic.json"),
            ],
            "a temp file was left behind"
        );
    }

    #[test]
    fn bare_filenames_stage_in_the_current_directory() {
        assert_eq!(parent_dir(Path::new("reel.eidetic.json")), Path::new("."));
        assert_eq!(
            parent_dir(Path::new("out/reel.eidetic.json")),
            Path::new("out")
        );
        assert_eq!(
            parent_dir(Path::new("/tmp/reel.eidetic.json")),
            Path::new("/tmp")
        );
    }

    #[test]
    fn previews_render_beside_the_output_at_half_size() {
        let p = ReelProject::new(
            "x".into(),
            (1080, 1920),
            "auto".into(),
            None,
            None,
            Path::new("/out/reel.v2.mp4"),
            vec![],
        );
        let full = p.render_target(false);
        assert_eq!(full.path, PathBuf::from("/out/reel.v2.mp4"));
        assert_eq!((full.width, full.height), (1080, 1920));
        let preview = p.render_target(true);
        assert_eq!(preview.path, PathBuf::from("/out/reel.v2.preview.mp4"));
        assert_eq!((preview.width, preview.height), (540, 960));
    }

    // -- apply_ops -----------------------------------------------------------
    //
    // None of these swap successfully, so no model or database is touched:
    // the config points into an empty temp dir, and a swap that got as far
    // as loading SigLIP would fail loudly rather than download anything.

    fn config(dir: &Path) -> Config {
        Config {
            paths: eidetic_core::config::Paths {
                library_dir: dir.join("library"),
                models_cache: dir.join("models"),
            },
            database_path: dir.join("eidetic.db"),
        }
    }

    /// Four unpinned photo slots with hard cuts (photos: retime never
    /// probes).
    fn four() -> ReelProject {
        let slots = (0..4)
            .map(|i| Slot {
                kind: SlotKind::Photo,
                start: 0.0,
                duration: 2.0 + i as f64,
                focus_x: None,
                pinned: false,
                transition_in: Transition::Cut,
                ..slot()
            })
            .collect();
        ReelProject::new(
            "x".into(),
            (320, 180),
            "auto".into(),
            None,
            None,
            Path::new("/tmp/x.mp4"),
            slots,
        )
    }

    fn assets(p: &ReelProject) -> Vec<AssetId> {
        p.slots.iter().map(|s| s.asset).collect()
    }

    async fn apply(p: &mut ReelProject, ops: serde_json::Value) -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let ops: Vec<EditOp> = serde_json::from_value(ops).unwrap();
        apply_ops(p, &ops, &config(dir.path())).await
    }

    #[tokio::test]
    async fn a_mixed_batch_resolves_every_number_against_the_original_list() {
        let mut p = four();
        let a = assets(&p);
        // Listed in the reverse of the application order on purpose: the
        // listing order must not matter.
        apply(
            &mut p,
            serde_json::json!([
                {"op": "reorder", "order": [4, 1, 3]},
                {"op": "drop", "slot": 2},
                {"op": "transition", "slot": 1, "style": "crossfade:0.4"},
                {"op": "retime", "slot": 4, "secs": 1.5},
                {"op": "pin", "slot": 3},
            ]),
        )
        .await
        .unwrap();

        assert_eq!(assets(&p), vec![a[3], a[0], a[2]]);
        assert_eq!(p.slots[0].duration, 1.5, "retime followed original slot 4");
        assert_eq!(
            p.slots[1].transition_in,
            Transition::Crossfade { secs: 0.4 },
            "the transition travels with original slot 1's shot"
        );
        assert!(p.slots[2].pinned, "the pin followed original slot 3");
        assert!(!p.slots[0].pinned && !p.slots[1].pinned);
    }

    /// The two-call recipe the edit_reel description gives for putting a
    /// joint into a position the reorder creates.
    #[tokio::test]
    async fn transition_after_a_reorder_uses_the_new_numbering() {
        let mut p = four();
        let a = assets(&p);
        apply(
            &mut p,
            serde_json::json!([{"op": "reorder", "order": [3, 1, 2, 4]}]),
        )
        .await
        .unwrap();
        apply(
            &mut p,
            serde_json::json!([{"op": "transition", "slot": 2, "style": "dip"}]),
        )
        .await
        .unwrap();
        assert_eq!(assets(&p), vec![a[2], a[0], a[1], a[3]]);
        assert_eq!(p.slots[1].asset, a[0]);
        assert!(matches!(
            p.slots[1].transition_in,
            Transition::DipToBlack { .. }
        ));
        assert!(
            p.slots
                .iter()
                .enumerate()
                .all(|(i, s)| i == 1 || s.transition_in == Transition::Cut)
        );
    }

    /// Transition and reorder in ONE call: the transition names the shot,
    /// so moving slot 1 later still gives it its joint (it used to be
    /// dropped as "slot 1 has no joint"), and the shot moved to the front
    /// carries its transition inertly.
    #[tokio::test]
    async fn transition_and_reorder_in_one_call_follow_the_shot() {
        let mut p = four();
        let a = assets(&p);
        apply(
            &mut p,
            serde_json::json!([
                {"op": "transition", "slot": 1, "style": "whip"},
                {"op": "transition", "slot": 3, "style": "crossfade"},
                {"op": "reorder", "order": [3, 1, 2, 4]},
            ]),
        )
        .await
        .unwrap();
        assert_eq!(assets(&p), vec![a[2], a[0], a[1], a[3]]);
        assert!(matches!(p.slots[1].transition_in, Transition::Whip { .. }));
        // First on the timeline: stored, but render never reads slot 0's
        // joint.
        assert!(matches!(
            p.slots[0].transition_in,
            Transition::Crossfade { .. }
        ));
    }

    #[tokio::test]
    async fn pins_guard_drop_and_swap_within_the_same_batch() {
        let mut p = four();
        p.slots[1].pinned = true;

        for ops in [
            serde_json::json!([{"op": "drop", "slot": 2}]),
            serde_json::json!([{"op": "swap", "slot": 2, "query": "beach"}]),
            serde_json::json!([{"op": "pin", "slot": 3}, {"op": "drop", "slot": 3}]),
            serde_json::json!([{"op": "swap", "slot": 3, "query": "beach"}, {"op": "pin", "slot": 3}]),
        ] {
            let mut q = four();
            q.slots[1].pinned = true;
            let err = apply(&mut q, ops.clone()).await.unwrap_err().to_string();
            assert!(err.contains("is pinned"), "{ops}: {err}");
        }

        // Unpinning in the same batch releases the slot.
        let a = assets(&p);
        apply(
            &mut p,
            serde_json::json!([{"op": "drop", "slot": 2}, {"op": "unpin", "slot": 2}]),
        )
        .await
        .unwrap();
        assert_eq!(assets(&p), vec![a[0], a[2], a[3]]);
    }

    #[tokio::test]
    async fn invalid_numbers_and_orders_are_refused() {
        for (ops, want) in [
            (serde_json::json!([{"op": "pin", "slot": 0}]), "no slot 0"),
            (serde_json::json!([{"op": "drop", "slot": 5}]), "no slot 5"),
            (
                serde_json::json!([{"op": "transition", "slot": 9, "style": "cut"}]),
                "no slot 9",
            ),
            (
                serde_json::json!([{"op": "reorder", "order": [1, 2, 3, 5]}]),
                "no slot 5",
            ),
            (
                serde_json::json!([{"op": "reorder", "order": [1, 1, 2, 3]}]),
                "exactly once",
            ),
            (
                serde_json::json!([{"op": "reorder", "order": [3, 1, 2]}]),
                "exactly once",
            ),
            // A reorder beside a drop still uses the ORIGINAL numbers:
            // meaning "4,1,3" in post-drop numbering as [3,1,2] names the
            // dropped slot 2 and misses slot 4...
            (
                serde_json::json!([{"op": "drop", "slot": 2}, {"op": "reorder", "order": [3, 1, 2]}]),
                "exactly once",
            ),
            (
                serde_json::json!([
                    {"op": "reorder", "order": [1, 2, 3, 4]},
                    {"op": "reorder", "order": [4, 3, 2, 1]},
                ]),
                "at most one reorder",
            ),
            (
                serde_json::json!([{"op": "retime", "slot": 1, "secs": 0.1}]),
                "0.3s",
            ),
            (
                serde_json::json!([{"op": "transition", "slot": 2, "style": "wipe"}]),
                "wipe",
            ),
            (
                serde_json::json!(
                    [1, 2, 3, 4].map(|s| serde_json::json!({"op": "drop", "slot": s}))
                ),
                "every slot was dropped",
            ),
        ] {
            let mut p = four();
            let err = apply(&mut p, ops.clone()).await.unwrap_err().to_string();
            assert!(err.contains(want), "{ops}: {err}");
        }

        // ...while naming the survivors by their original numbers works.
        let mut p = four();
        let a = assets(&p);
        apply(
            &mut p,
            serde_json::json!([{"op": "drop", "slot": 2}, {"op": "reorder", "order": [4, 1, 3]}]),
        )
        .await
        .unwrap();
        assert_eq!(assets(&p), vec![a[3], a[0], a[2]]);
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
