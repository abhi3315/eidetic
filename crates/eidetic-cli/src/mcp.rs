//! `eidetic mcp` — the agentic edit engine's socket (goals-v0.8.md phase 1;
//! ADR-0012).
//!
//! A stdio MCP server exposing the library and the reel pipeline as tools
//! for Claude Code / Codex / Cursor / local LM Studio clients. Two hard
//! rules, both from the market research:
//!
//! 1. **Metadata only.** Tools return paths, timecodes, scores and
//!    transcript excerpts — never pixels. Connecting a cloud agent sends
//!    tool *results* to that provider; keeping pixels out bounds what can
//!    leak (and the fully-local LM Studio path stays first-class).
//! 2. **Compact output.** Agent sessions compound context fast; every
//!    result is small JSON, capped lists, rounded numbers.
//!
//! stdout belongs to the protocol. Anything human-facing goes to stderr
//! (tracing already does).

use crate::project::{self, EditOp, ReelProject, ReelSpec};
use crate::{otio, reel};
use eidetic_core::Config;
use eidetic_db::{AssetsRepo, FacesRepo, SearchFilters};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct EideticMcp {
    config: Config,
    repo: AssetsRepo,
    faces: FacesRepo,
}

/// Serve the library over stdio until the client disconnects.
pub async fn serve() -> anyhow::Result<()> {
    use anyhow::Context;
    let config = Config::from_env();
    let pool = eidetic_db::connect(&config)
        .await
        .context("failed to connect to database")?;
    let faces = FacesRepo::new(pool.clone());
    let server = EideticMcp {
        config,
        repo: AssetsRepo::new(pool),
        faces,
    };
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP initialize failed")?;
    service.waiting().await.context("MCP server errored")?;
    Ok(())
}

/// Tool-level failure the agent should read (vs. a protocol error).
fn reply(r: anyhow::Result<Value>) -> Result<CallToolResult, ErrorData> {
    match r {
        Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::text(
            v.to_string(),
        )])),
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "{e:#}"
        ))])),
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct SearchParams {
    /// What to look for, e.g. "sunset at the beach". Matches photo content,
    /// video moments, and spoken words.
    query: String,
    /// Max hits (default 10, cap 50).
    limit: Option<u32>,
    /// Only assets containing this (named) person.
    person: Option<String>,
    /// Only assets taken on/after this date, YYYY-MM-DD.
    after: Option<String>,
    /// Only assets taken before this date (exclusive), YYYY-MM-DD.
    before: Option<String>,
    /// Only assets whose place/state/country matches this substring.
    place: Option<String>,
    /// "image" or "video".
    kind: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AssetParams {
    /// Asset reference: id, content hash, storage path, or an unambiguous
    /// original filename.
    asset: String,
}

#[derive(Deserialize, JsonSchema)]
struct BeatsParams {
    /// Path to an audio file (or video; its audio track is used).
    audio: String,
}

#[derive(Deserialize, JsonSchema)]
struct CreateReelParams {
    /// What the reel is about, e.g. "our trip to the mountains".
    prompt: String,
    /// Target length in seconds (default 30; with audio, capped at the
    /// track length).
    duration: Option<f64>,
    /// Music track to cut to: beats are detected and cuts land on them.
    /// A speech track (voiceover) drives shots from the narration instead.
    audio: Option<String>,
    /// Vertical 1080x1920 (Instagram/Shorts). Default false = 1920x1080.
    portrait: Option<bool>,
    /// Framing: auto (faces followed with a cover-crop, scenic shots over
    /// a blurred backdrop), cover, blur, pad. Default auto.
    frame: Option<String>,
    /// Plan N candidate cut lists instead of one (different shot pools and
    /// joint styles); render each as a preview and pick.
    variations: Option<u32>,
    /// Output path for the future render (default reel-<timestamp>.mp4 in
    /// the working directory). The project file sits next to it.
    output: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct EditReelParams {
    /// Path to the project file (reel.eidetic.json).
    project: String,
    /// Mutations, applied as one batch: slot numbers are 1-based against
    /// the current cut list, all resolved before anything changes.
    ops: Vec<EditOp>,
}

#[derive(Deserialize, JsonSchema)]
struct RenderParams {
    /// Path to the project file (reel.eidetic.json).
    project: String,
    /// Render a fast half-resolution preview instead of full size.
    preview: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct ProjectParams {
    /// Path to the project file (reel.eidetic.json).
    project: String,
}

#[derive(Deserialize, JsonSchema)]
struct EmptyParams {}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl EideticMcp {
    async fn resolve(&self, needle: &str) -> anyhow::Result<eidetic_core::AssetId> {
        use anyhow::Context;
        self.repo
            .resolve_asset(needle)
            .await
            .context("asset lookup failed")?
            .with_context(|| format!("no asset matches {needle:?}"))
    }

    async fn embed(&self, text: String) -> anyhow::Result<Vec<f32>> {
        use anyhow::Context;
        let models_dir = self.config.paths.models_cache.clone();
        tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<f32>> {
            let mut e = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
            e.embed_text(&text)
        })
        .await
        .context("embedder thread panicked")?
        .context("text embedding failed")
    }

    #[tool(
        description = "Semantic search over the whole library: photo content, video moments (with the matching timestamp), and spoken words. Returns scored hits with asset ids and file paths."
    )]
    async fn search_library(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.search_impl(p).await)
    }

    #[tool(
        description = "List the named (and unnamed) people the library knows, with face counts."
    )]
    async fn list_persons(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let persons = self.faces.list_persons().await?;
                Ok(json!({
                    "persons": persons.iter().map(|p| json!({
                        "id": p.id.to_string(),
                        "name": p.name,
                        "faces": p.face_count,
                    })).collect::<Vec<_>>()
                }))
            }
            .await,
        )
    }

    #[tool(description = "Who appears in one asset (photo or video): person ids and names.")]
    async fn faces_in(
        &self,
        Parameters(p): Parameters<AssetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let id = self.resolve(&p.asset).await?;
                let faces = self.faces.fetch_faces_for_asset(id).await?;
                let mut people: Vec<Value> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for f in &faces {
                    if seen.insert(f.person_id) {
                        people.push(json!({
                            "person_id": f.person_id.to_string(),
                            "name": f.person_name,
                        }));
                    }
                }
                Ok(json!({ "asset": id.to_string(), "faces": faces.len(), "people": people }))
            }
            .await,
        )
    }

    #[tool(
        description = "Scene (shot) boundaries of a video, in seconds. Cuts that respect these look intentional."
    )]
    async fn scenes(
        &self,
        Parameters(p): Parameters<AssetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let id = self.resolve(&p.asset).await?;
                let scenes = self.repo.fetch_video_scenes(id).await?;
                Ok(json!({
                    "asset": id.to_string(),
                    "boundaries": scenes.map(|s| s.into_iter().map(round2).collect::<Vec<_>>()),
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Analyse a music file: BPM, beat grid, and high-energy sections (choruses/drops — where the hero shots belong). No stable tempo means the track is speech or ambience."
    )]
    async fn beats(
        &self,
        Parameters(p): Parameters<BeatsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.beats_impl(p).await)
    }

    #[tool(
        description = "The stored transcript of a video asset: what is said, with start/end seconds per segment. Empty if never transcribed or no speech."
    )]
    async fn transcript(
        &self,
        Parameters(p): Parameters<AssetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let id = self.resolve(&p.asset).await?;
                let segments = self.repo.fetch_transcript(id).await?;
                Ok(json!({
                    "asset": id.to_string(),
                    "segments": segments.iter().map(|(a, b, t)| json!({
                        "start": round2(*a), "end": round2(*b), "text": t,
                    })).collect::<Vec<_>>(),
                }))
            }
            .await,
        )
    }

    #[tool(description = "Library totals: asset counts, embedded counts, size, date range.")]
    async fn library_stats(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let s = self.repo.fetch_stats().await?;
                Ok(json!({
                    "assets": s.total, "images": s.images, "videos": s.videos,
                    "embedded": s.embedded, "needs_embed": s.needs_embed,
                    "bytes": s.total_bytes,
                    "earliest": s.earliest.map(|d| d.format("%Y-%m-%d").to_string()),
                    "latest": s.latest.map(|d| d.format("%Y-%m-%d").to_string()),
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Plan a reel from a prompt: search, quality-gate, beat-sync (with audio), assign shots. Writes a project file per plan and returns the cut list(s) — nothing is rendered yet. Iterate with edit_reel, then call render."
    )]
    async fn create_reel(
        &self,
        Parameters(p): Parameters<CreateReelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.create_impl(p).await)
    }

    #[tool(
        description = "Mutate a reel project: swap a slot for a fresh search, drop, pin/unpin, retime, set transitions (cut/crossfade/dip/slide/whip, e.g. \"crossfade:0.8\"), reorder. Saves the project and returns the new cut list. Render separately."
    )]
    async fn edit_reel(
        &self,
        Parameters(p): Parameters<EditReelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let path = PathBuf::from(&p.project);
                let mut proj = ReelProject::load(&path)?;
                project::apply_ops(&mut proj, &p.ops, &self.config).await?;
                proj.save(&path)?;
                Ok(cut_list_json(&proj, &path))
            }
            .await,
        )
    }

    #[tool(
        description = "Render a reel project to its output file with ffmpeg (H.264 + the audio track). preview=true renders half-resolution for fast review."
    )]
    async fn render(
        &self,
        Parameters(p): Parameters<RenderParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let path = PathBuf::from(&p.project);
                let proj = ReelProject::load(&path)?;
                let size = p
                    .preview
                    .unwrap_or(false)
                    .then_some(((proj.width / 2) & !1, (proj.height / 2) & !1));
                project::render_project(&proj, size).await?;
                let (w, h) = size.unwrap_or((proj.width, proj.height));
                Ok(json!({
                    "output": proj.output,
                    "size": format!("{w}x{h}"),
                    "seconds": round2(proj.total_secs()),
                    "preview": size.is_some(),
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Export a reel project as OpenTimelineIO (.otio) for hand-polish in Kdenlive 25.04+ or DaVinci Resolve 18.5+. Cuts only; transitions are re-added in the editor."
    )]
    async fn export_otio(
        &self,
        Parameters(p): Parameters<ProjectParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let path = PathBuf::from(&p.project);
                let proj = ReelProject::load(&path)?;
                let otio_path = proj.output.with_extension("otio");
                otio::export(&proj, &otio_path)?;
                Ok(json!({ "otio": otio_path }))
            }
            .await,
        )
    }
}

impl EideticMcp {
    async fn search_impl(&self, p: SearchParams) -> anyhow::Result<Value> {
        use anyhow::Context;
        let parse_day = |s: &str, label: &str| -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
            let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .with_context(|| format!("invalid {label} {s:?}, expected YYYY-MM-DD"))?;
            Ok(chrono::DateTime::from_naive_utc_and_offset(
                date.and_hms_opt(0, 0, 0).expect("midnight exists"),
                chrono::Utc,
            ))
        };
        if let Some(k) = &p.kind
            && k != "image"
            && k != "video"
        {
            anyhow::bail!("invalid kind {k:?}; valid values: image, video");
        }
        let filters = SearchFilters {
            person: p.person,
            after: p
                .after
                .as_deref()
                .map(|s| parse_day(s, "after"))
                .transpose()?,
            before: p
                .before
                .as_deref()
                .map(|s| parse_day(s, "before"))
                .transpose()?,
            place: p.place,
            kind: p.kind,
        };
        let emb = self.embed(p.query.clone()).await?;
        let limit = p.limit.unwrap_or(10).min(50);
        let results = self
            .repo
            .search(&p.query, &emb, limit, &filters)
            .await
            .context("search failed")?;
        Ok(json!({
            "hits": results.iter().map(|r| {
                let mut hit = json!({
                    "asset": r.id.to_string(),
                    "path": r.storage_path,
                    "kind": if r.frame_ts.is_some() { "video" } else { "image" },
                    "score": round2(r.score as f64),
                });
                let o = hit.as_object_mut().expect("hit is an object literal");
                if let Some(ts) = r.frame_ts {
                    o.insert("moment_secs".into(), json!(round2(ts)));
                }
                if let Some(d) = r.duration_secs {
                    o.insert("duration_secs".into(), json!(round2(d)));
                }
                if let Some(d) = r.date_taken {
                    o.insert("date".into(), json!(d.format("%Y-%m-%d").to_string()));
                }
                if let Some(s) = &r.speech {
                    o.insert("speech".into(), json!(s));
                }
                hit
            }).collect::<Vec<_>>()
        }))
    }

    async fn beats_impl(&self, p: BeatsParams) -> anyhow::Result<Value> {
        use anyhow::Context;
        let track = PathBuf::from(&p.audio);
        anyhow::ensure!(track.exists(), "audio file not found: {}", track.display());
        let grid = tokio::task::spawn_blocking(
            move || -> anyhow::Result<(Option<eidetic_ingest::beats::BeatGrid>, f64)> {
                let pcm = eidetic_ingest::video::extract_audio_pcm(&track)?
                    .context("ffmpeg is required to decode audio")?;
                let secs = pcm.len() as f64 / eidetic_ingest::beats::SAMPLE_RATE;
                Ok((eidetic_ingest::beats::track_beats(&pcm), secs))
            },
        )
        .await
        .context("beat thread panicked")?;
        let (grid, secs) = grid?;
        Ok(match grid {
            None => json!({
                "duration_secs": round2(secs),
                "grid": Value::Null,
                "note": "no stable tempo (speech or ambience); a reel cut to this track uses voiceover mode or fixed windows",
            }),
            Some(g) => json!({
                "duration_secs": round2(secs),
                "bpm": round2(g.bpm),
                "beat_count": g.beats.len(),
                "first_beats": g.beats.iter().take(8).map(|b| round2(*b)).collect::<Vec<_>>(),
                "high_energy_sections": g.high_energy.iter()
                    .map(|(a, b)| json!([round2(*a), round2(*b)])).collect::<Vec<_>>(),
            }),
        })
    }

    async fn create_impl(&self, p: CreateReelParams) -> anyhow::Result<Value> {
        let portrait = p.portrait.unwrap_or(false);
        let (w, h) = if portrait { (1080, 1920) } else { (1920, 1080) };
        let frame = p.frame.unwrap_or_else(|| "auto".to_string());
        reel::Fit::parse(&frame, portrait)?;
        let output = match p.output {
            Some(o) => PathBuf::from(o),
            None => PathBuf::from(format!(
                "reel-{}.mp4",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            )),
        };
        let spec = ReelSpec {
            prompt: p.prompt,
            duration: p.duration.unwrap_or(30.0).clamp(3.0, 600.0),
            output,
            width: w,
            height: h,
            frame,
            audio: p.audio.map(PathBuf::from),
            min_sharpness: reel::MIN_SHARPNESS,
            variations: p.variations.unwrap_or(1).clamp(1, 9),
        };
        let projects = project::plan_projects(&spec, &self.config).await?;
        // Never clobber an existing project — it may hold edits. The agent
        // should edit it, or pick another output name.
        for proj in &projects {
            let path = ReelProject::path_for(&proj.output);
            anyhow::ensure!(
                !path.exists(),
                "{} already exists; edit it with edit_reel, or pass a \
                 different output",
                path.display()
            );
        }
        let mut out = Vec::new();
        for proj in &projects {
            let path = ReelProject::path_for(&proj.output);
            proj.save(&path)?;
            out.push(cut_list_json(proj, &path));
        }
        Ok(json!({ "projects": out }))
    }
}

/// The compact cut-list shape every edit-loop tool returns.
fn cut_list_json(proj: &ReelProject, project_path: &Path) -> Value {
    json!({
        "project": project_path,
        "output": proj.output,
        "prompt": proj.prompt,
        "size": format!("{}x{}", proj.width, proj.height),
        "seconds": round2(proj.total_secs()),
        "audio": proj.audio,
        "bpm": proj.grid.as_ref().map(|g| round2(g.bpm)),
        "slots": proj.slots.iter().enumerate().map(|(i, s)| {
            json!({
                "n": i + 1,
                "asset": s.asset.to_string(),
                "source": s.source,
                "kind": match s.kind {
                    reel::SlotKind::Video => "video",
                    reel::SlotKind::Photo => "photo",
                    reel::SlotKind::PhotoScenic => "photo_scenic",
                },
                "start": round2(s.start),
                "secs": round2(s.duration),
                "score": round2(s.score as f64),
                "pinned": s.pinned,
                "transition_in": match s.transition_in {
                    reel::Transition::Cut => "cut".to_string(),
                    reel::Transition::Crossfade { secs } => format!("crossfade:{secs}"),
                    reel::Transition::DipToBlack { secs } => format!("dip:{secs}"),
                    reel::Transition::Slide { secs } => format!("slide:{secs}"),
                    reel::Transition::Whip { secs } => format!("whip:{secs}"),
                },
            })
        }).collect::<Vec<_>>(),
    })
}

#[tool_handler]
impl ServerHandler for EideticMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(
            "Eidetic: a fully local media library (photos, videos, faces, \
             speech) with a beat-synced reel editor. Search and inspect \
             with the context tools; direct an edit with create_reel → \
             edit_reel (iterate on the cut list — check every slot's \
             score, swap weak ones, keep the hero shot on the drop) → \
             render (preview first, full-res when happy) → export_otio \
             for hand-polish. Tools return metadata only — paths, \
             timecodes, scores — never pixels."
                .to_string(),
        );
        info
    }
}
