//! `eidetic mcp` — the agentic edit engine's socket (goals-v0.8.md phase 1;
//! ADR-0012).
//!
//! A stdio MCP server exposing the library and the reel pipeline as tools
//! for Claude Code / Codex / Cursor / local LM Studio clients. Two hard
//! rules, both from the market research:
//!
//! 1. **Metadata only, unless the operator opts in.** Tools return paths,
//!    timecodes, scores and transcript excerpts — never pixels. Connecting
//!    a cloud agent sends tool *results* to that provider; keeping pixels
//!    out bounds what can leak (and the fully-local LM Studio path stays
//!    first-class). The one exception is `preview_sheet` (goals-v0.9.md
//!    phase 1), which exists only when the server was started with
//!    `--allow-previews`: without it the tool is not listed, and a direct
//!    call is refused before any file is read.
//! 2. **Compact output.** Agent sessions compound context fast; every
//!    result is small JSON, capped lists, rounded numbers.
//!
//! stdout belongs to the protocol. Anything human-facing goes to stderr
//! (tracing already does).

use crate::project::{self, EditOp, ProjectId, ReelProject, ReelSpec};
use crate::{otio, preview, reel};
use eidetic_core::Config;
use eidetic_db::{AssetsRepo, FacesRepo, SearchFilters};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::sync::{OnceCell, mpsc, oneshot};

/// A text-embedding request and the channel its vector comes back on.
type EmbedJob = (String, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);

pub struct EideticMcp {
    config: Config,
    repo: AssetsRepo,
    faces: FacesRepo,
    /// Handle to a resident SigLIP worker, started on the first semantic
    /// query and kept for the life of the session.
    ///
    /// Loading the model costs ~1.4 GiB and several seconds, so it must
    /// happen at most once per session — but lazily, because an agent that
    /// only reads metadata (`scenes`, `transcript`, `library_stats`) should never
    /// pay for it at all. Same resident-worker shape as `eidetic-server`,
    /// with the eager load traded for a `OnceCell`.
    /// Paired with the loaded model's `model_id`.
    embedder: OnceCell<(&'static str, mpsc::Sender<EmbedJob>)>,
    /// `eidetic mcp --allow-previews`: may a tool return pixels?
    allow_previews: bool,
}

/// What a preview call gets from a metadata-only server.
const PREVIEWS_OFF: &str = "preview_sheet is disabled: this server is metadata-only. \
    The sheet shows real frames from the library to the model, so the operator must opt \
    in by restarting the server as `eidetic mcp --allow-previews`. Until then, render \
    with preview=true and ask the human to watch it.";

/// Serve the library over stdio until the client disconnects.
pub async fn serve(allow_previews: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    let config = Config::from_env()?;
    let pool = eidetic_db::connect(&config)
        .await
        .context("failed to connect to database")?;
    let faces = FacesRepo::new(pool.clone());
    let server = EideticMcp {
        config,
        repo: AssetsRepo::new(pool),
        faces,
        embedder: OnceCell::new(),
        allow_previews,
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

/// Run filesystem or subprocess work off the async executor.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    use anyhow::Context;
    tokio::task::spawn_blocking(work)
        .await
        .context("worker thread panicked")?
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
    /// Mutations, applied as one all-or-nothing batch. Every slot number
    /// (reorder included) is 1-based against the cut list as it was BEFORE
    /// this call, whatever order the ops are listed in; they then apply as
    /// pin/unpin → retime → transition → swap → drop → reorder.
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
struct PreviewParams {
    /// Path to the project file (reel.eidetic.json).
    project: String,
    /// First slot on the sheet, 1-based (default 1). A sheet holds at most
    /// 24 slots; longer reels page with first=25, first=49, …
    first: Option<u32>,
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

    /// Embed `text`, returning the embedder's `model_id` with the vector so
    /// search ranks only rows from that same model.
    async fn embed(&self, text: String) -> anyhow::Result<(&'static str, Vec<f32>)> {
        use anyhow::Context;

        // First semantic query in the session pays the model load; every
        // later one is a channel round trip to the worker that already owns
        // the session. Reloading per call made an agent's second search cost
        // as much as its first.
        let &(model_id, ref tx) = self
            .embedder
            .get_or_try_init(|| async {
                let models_dir = self.config.paths.models_cache.clone();
                let embedder = tokio::task::spawn_blocking(move || {
                    eidetic_ml::SiglipEmbedder::load(&models_dir)
                })
                .await
                .context("embedder thread panicked")?
                .context("failed to load SigLIP 2 model")?;
                let model_id = embedder.model_id();

                let (tx, mut rx) = mpsc::channel::<EmbedJob>(8);
                tokio::task::spawn_blocking(move || {
                    let mut embedder = embedder;
                    while let Some((text, reply)) = rx.blocking_recv() {
                        let _ = reply.send(embedder.embed_text(&text));
                    }
                });
                Ok::<_, anyhow::Error>((model_id, tx))
            })
            .await?;

        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send((text, reply_tx))
            .await
            .context("embed worker is gone")?;
        let emb = reply_rx
            .await
            .context("embed worker panicked or died mid-job")?
            .context("text embedding failed")?;
        Ok((model_id, emb))
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
        description = "Analyse a music file: BPM, beat grid, bar starts, and high-energy sections (choruses/drops — where the hero shots belong). provenance.downbeats says whether bars were inferred by the Beat This model or are the Ellis tracker's every-4th-beat guess; fallback explains why a requested model was not used. No stable tempo means the track is speech or ambience."
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

    #[tool(
        description = "Library totals: asset counts, size, date range, and embedding readiness. embedded counts assets with a stored vector from any model; model is the active embedding model, ready is how many assets search can rank under it, and pending.images/pending.videos are what `eidetic embed` would process for it."
    )]
    async fn library_stats(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                // From EIDETIC_MODEL alone: stats must not pay the model load.
                let (model_id, _dim) = eidetic_ml::active_model()?;
                let s = self.repo.fetch_stats(model_id).await?;
                Ok(json!({
                    "assets": s.total, "images": s.images, "videos": s.videos,
                    "embedded": s.embedded,
                    "model": model_id, "ready": s.ready(),
                    "pending": {"images": s.pending_images, "videos": s.pending_videos},
                    "bytes": s.total_bytes,
                    "earliest": s.earliest.map(|d| d.format("%Y-%m-%d").to_string()),
                    "latest": s.latest.map(|d| d.format("%Y-%m-%d").to_string()),
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Plan a reel from a prompt: search, quality-gate, beat-sync (with audio), assign shots (high-energy spans prefer measured motion; near-duplicate scenes are held back). Writes a project file per plan and returns the cut list(s) — nothing is rendered yet; each slot carries motion, similar_to and geometry fields explained in the cut list's signals note. Iterate with edit_reel, then call render. \
            The output, its preview, its .otio and the project file must not exist yet, must not be reached through a symbolic link, and must lie outside the library directory; each project gets an id that its renders and exports carry, so later calls can replace those files and nothing else."
    )]
    async fn create_reel(
        &self,
        Parameters(p): Parameters<CreateReelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.create_impl(p).await)
    }

    #[tool(
        description = "Mutate a reel project: swap a slot for a fresh search, drop, pin/unpin, retime, set transitions (cut/crossfade/dip/slide/whip, e.g. \"crossfade:0.8\"), reorder. \
            Index basis: every slot number in one call — reorder included — is the 1-based \"n\" from the cut list BEFORE the call; a drop does not renumber later ops. \
            Ops apply in a fixed order regardless of how they are listed: pin/unpin, retime, transition, swap, drop, reorder. \
            A reorder lists every slot not dropped in this call exactly once (4 slots, drop 2 → order [4,1,3]). \
            Pinned slots refuse swap and drop; pinning in the same call already protects, unpinning in the same call already releases. \
            A transition belongs to its shot and moves with it on reorder; the shot that ends up first has no joint, so its transition is ignored. \
            To put a transition into a NEW position, use two calls: first {\"op\":\"reorder\",\"order\":[3,1,2]}, then, against the returned numbering, {\"op\":\"transition\",\"slot\":2,\"style\":\"crossfade:0.8\"} (the joint between the new 1st and 2nd shots). \
            All-or-nothing: an invalid op (bad slot number, pinned slot, bad reorder) changes nothing on disk. \
            On success saves the project and returns the new cut list. Render separately. \
            The project file is saved back where it was loaded, never through a symbolic link or into the library directory. \
            A project planned before Eidetic recorded project ids gets one on save (adopted: true); ops: [] adopts without changing the cut."
    )]
    async fn edit_reel(
        &self,
        Parameters(p): Parameters<EditReelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                use anyhow::Context;
                let path = PathBuf::from(&p.project);
                let mut proj = ReelProject::load_untrusted(&path).await?;
                let loaded = serde_json::to_value(&proj).context("serialize the project")?;
                // Checked before the ops (a swap can load the embedder) and
                // again just before the save replaces the file.
                let check = {
                    let (path, lib) = (path.clone(), self.config.paths.library_dir.clone());
                    move || {
                        project::check_destination(
                            &path,
                            &lib,
                            project::Claim::ProjectFile { loaded: &loaded },
                        )
                    }
                };
                blocking(check.clone()).await?;
                project::apply_ops(&mut proj, &p.ops, &self.config).await?;
                // An edit must not save what the next load would refuse.
                proj.validate()
                    .context("the edit would leave the project out of range; nothing was saved")?;
                let adopted = proj.id.is_none();
                proj.id.get_or_insert_with(ProjectId::generate);
                let mut out = cut_list_json(&proj, &path);
                if adopted {
                    out["adopted"] = json!(true);
                }
                blocking(move || {
                    check()?;
                    proj.save(&path)
                })
                .await?;
                Ok(out)
            }
            .await,
        )
    }

    #[tool(
        description = "Render a reel project with ffmpeg (H.264 + the audio track) and return the path written. Full renders replace the project's output; preview=true renders half-resolution to a separate file whose extension is replaced by .preview.mp4 (output reel.mp4 → reel.preview.mp4), leaving the full-res output and the project file untouched. A failed render leaves any earlier file in place. \
            Replaces an existing file only if it is a render of this same project (renders carry the project id); never writes through a symbolic link or into the library directory. \
            A project without an id is refused until edit_reel with ops: [] adopts it."
    )]
    async fn render(
        &self,
        Parameters(p): Parameters<RenderParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let path = PathBuf::from(&p.project);
                let proj = ReelProject::load_untrusted(&path).await?;
                let id = proj.require_id(&path, "render")?;
                let preview = p.preview.unwrap_or(false);
                let mut target = proj.render_target(preview);
                // Checked before any ffmpeg run, and again just before the
                // finished render replaces the destination.
                let check = {
                    let (dest, lib) = (target.path.clone(), self.config.paths.library_dir.clone());
                    move || {
                        project::check_destination(
                            &dest,
                            &lib,
                            project::Claim::Render {
                                id,
                                project: &path,
                                preview,
                            },
                        )
                    }
                };
                blocking(check.clone()).await?;
                target.before_publish = Some(std::sync::Arc::new(check));
                project::render_project(&proj, &target).await?;
                Ok(json!({
                    "output": target.path,
                    "size": format!("{}x{}", target.width, target.height),
                    "seconds": round2(proj.total_secs()),
                    "preview": preview,
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Export a reel project as OpenTimelineIO (.otio) for hand-polish in Kdenlive 25.04+ or DaVinci Resolve 18.5+. Cuts only; transitions are re-added in the editor. \
            Writes beside the output (reel.mp4 → reel.otio), replacing an existing file only if it is an export of this same project (exports carry the project id in their metadata); never writes through a symbolic link or into the library directory. \
            A project without an id is refused until edit_reel with ops: [] adopts it."
    )]
    async fn export_otio(
        &self,
        Parameters(p): Parameters<ProjectParams>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            async {
                let path = PathBuf::from(&p.project);
                let proj = ReelProject::load_untrusted(&path).await?;
                let id = proj.require_id(&path, "export_otio")?;
                let otio_path = otio::path_for(&proj.output);
                let target = otio_path.clone();
                let lib = self.config.paths.library_dir.clone();
                // Checked before any ffprobe run, and again once the
                // timeline is built, just before it replaces the file.
                blocking(move || {
                    let check = || {
                        project::check_destination(
                            &target,
                            &lib,
                            project::Claim::Otio { id, project: &path },
                        )
                    };
                    check()?;
                    let timeline = otio::timeline(&proj)?;
                    check()?;
                    otio::write(&target, &timeline)
                })
                .await?;
                Ok(json!({ "otio": otio_path }))
            }
            .await,
        )
    }

    #[tool(
        description = "PRIVACY OPT-IN: available only when the operator started the server with `eidetic mcp --allow-previews`; otherwise it is refused and nothing is read. \
            Returns a JPEG contact sheet of a reel project so you can judge the cut with your own vision: one tile per slot (the midpoint frame of a video slot's span, or the photo), framed and rotated the way the render frames it, labelled with slot number, duration and incoming transition (OPEN on slot 1). \
            These are real frames from the user's library; with a cloud client they are sent to its provider. \
            Every slot source must resolve to a regular file inside the library directory: a project naming any other file (an absolute path elsewhere, a `..` escape, or a symlink out of the library) is refused before anything is decoded, and the error names the slot. \
            At most 24 slots per sheet (page with first); tiles are at most 320px on the long edge. \
            A text block with the per-tile timestamps comes first, so clients that cannot show images still get the metadata. \
            Reads only: the project file and any rendered video are left untouched, and no sheet file is written."
    )]
    async fn preview_sheet(
        &self,
        Parameters(p): Parameters<PreviewParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Checked before the project is even opened: a metadata-only server
        // must not touch pixels, not merely withhold them.
        if !self.allow_previews {
            return reply(Err(anyhow::anyhow!(PREVIEWS_OFF)));
        }
        let sheet = async {
            let path = PathBuf::from(&p.project);
            let proj = ReelProject::load_untrusted(&path).await?;
            let first = p.first.unwrap_or(1) as usize;
            let sheet =
                preview::contact_sheet(&proj, first, &self.config.paths.library_dir).await?;
            let last = sheet.tiles.last().map_or(first, |t| t.n);
            let total = proj.slots.len();
            let meta = json!({
                "project": path,
                "format": "image/jpeg",
                "size": format!("{}x{}", sheet.width, sheet.height),
                "slots": format!("{first}-{last} of {total}"),
                "next_first": (last < total).then_some(last + 1),
                "tiles": sheet.tiles.iter().map(|t| json!({
                    "n": t.n,
                    "kind": t.kind,
                    "at_secs": round2(t.at),
                    "secs": round2(t.duration),
                    "transition_in": transition_spec(t.transition_in),
                })).collect::<Vec<_>>(),
            });
            Ok::<_, anyhow::Error>((meta, sheet.jpeg))
        }
        .await;
        match sheet {
            Ok((meta, jpeg)) => {
                use base64::Engine;
                let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
                Ok(CallToolResult::success(vec![
                    ContentBlock::text(meta.to_string()),
                    ContentBlock::image(data, "image/jpeg"),
                ]))
            }
            Err(e) => reply(Err(e)),
        }
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
        let (model_id, emb) = self.embed(p.query.clone()).await?;
        let limit = p.limit.unwrap_or(10).min(50);
        let results = self
            .repo
            .search(&p.query, &emb, model_id, limit, &filters)
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
        let models_dir = self.config.paths.models_cache.clone();
        let grid = tokio::task::spawn_blocking(
            move || -> anyhow::Result<(Option<eidetic_ingest::beats::BeatGrid>, f64)> {
                let choice = project::TrackerChoice::from_env()?;
                let pcm = eidetic_ingest::video::extract_audio_pcm(&track)?
                    .context("ffmpeg is required to decode audio")?;
                let secs = pcm.len() as f64 / eidetic_ingest::beats::SAMPLE_RATE;
                let grid = project::detect_beats(choice, &track, &pcm, &models_dir)?;
                Ok((grid, secs))
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
                "bar_count": g.downbeats.len(),
                "first_bars": g.downbeats.iter().take(8)
                    .filter_map(|&i| g.beats.get(i)).map(|t| round2(*t)).collect::<Vec<_>>(),
                "high_energy_sections": g.high_energy.iter()
                    .map(|(a, b)| json!([round2(*a), round2(*b)])).collect::<Vec<_>>(),
                "provenance": grid_provenance(&g),
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
        // The output name is LLM-controlled, so on this boundary it must not
        // be able to name a file the user cares about. Requiring .mp4 stops
        // it from *creating* meaningful non-video files (~/.bashrc), and
        // claiming only free paths (see `claim_new`) stops it from
        // *destroying* anything at plan time; later writes prove ownership
        // through the project id. The CLI keeps full freedom — the operator
        // is trusted; the agent is not.
        project::ensure_mp4(&output)?;
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
        // Claimed before planning, so a refusal costs no model load, and
        // again before saving, in case a path was taken meanwhile.
        let lib = self.config.paths.library_dir.clone();
        let outputs = spec.outputs();
        blocking({
            let lib = lib.clone();
            move || outputs.iter().try_for_each(|o| claim_new(o, &lib))
        })
        .await?;
        let projects = project::plan_projects(&spec, &self.config).await?;
        blocking(move || {
            for proj in &projects {
                // Every other tool refuses a project out of range; never save one.
                proj.validate()?;
                claim_new(&proj.output, &lib)?;
            }
            let mut out = Vec::new();
            for proj in &projects {
                let path = ReelProject::path_for(&proj.output);
                proj.save(&path)?;
                out.push(cut_list_json(proj, &path));
            }
            Ok(json!({ "projects": out }))
        })
        .await
    }
}

/// Refuse to plan a reel at `output` unless every path its project will
/// write is free: the render, its preview, the .otio export_otio derives
/// from it, and the project file. Never clobber an existing project, which
/// may hold edits, or any other file: a path that already exists belongs to
/// someone, and it is not this reel.
fn claim_new(output: &Path, library_dir: &Path) -> anyhow::Result<()> {
    use project::Claim;
    let pick = Claim::New {
        remedy: "pick a different output (MCP tools only overwrite files they wrote for \
                 the same project)",
    };
    for path in [
        output.to_path_buf(),
        project::preview_path(output),
        otio::path_for(output),
    ] {
        project::check_destination(&path, library_dir, pick)?;
    }
    project::check_destination(
        &ReelProject::path_for(output),
        library_dir,
        Claim::New {
            remedy: "edit it with edit_reel, or pass a different output",
        },
    )
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
        "beat_grid": proj.grid.as_ref().map(grid_provenance),
        "signals": SIGNALS_NOTE,
        "slots": proj.slots.iter().enumerate().map(|(i, s)| {
            let mut slot = json!({
                "n": i + 1,
                "asset": s.asset.to_string(),
                "source": s.source,
                "kind": s.kind,
                "start": round2(s.start),
                "secs": round2(s.duration),
                "score": round2(s.score as f64),
                "pinned": s.pinned,
                "transition_in": transition_spec(s.transition_in),
                "similar_to": reel::similar_to(&proj.slots, i),
            });
            let o = slot.as_object_mut().expect("slot is an object literal");
            // Photos carry no motion key at all: a still is not a
            // zero-motion video. null = a video nobody measured.
            if s.kind == reel::SlotKind::Video {
                o.insert(
                    "motion".into(),
                    json!(s.motion.map(|m| (m as f64 * 1000.0).round() / 1000.0)),
                );
            }
            if let Some(g) = s.geometry {
                o.insert("width".into(), json!(g.width));
                o.insert("height".into(), json!(g.height));
                o.insert("rotation_cw".into(), json!(g.rotation_cw));
                o.insert("rotation_applied".into(), json!(g.rotation_applied));
                if g.may_render_sideways() {
                    o.insert("may_render_sideways".into(), json!(true));
                }
            }
            slot
        }).collect::<Vec<_>>(),
    })
}

/// Where a grid's beats and bars came from — so an agent never mistakes
/// Ellis's every-4th-beat guess for detected downbeats.
fn grid_provenance(g: &eidetic_ingest::beats::BeatGrid) -> Value {
    json!({
        "tracker": g.tracker,
        "downbeats": if g.downbeats_inferred() { "inferred" } else { "heuristic-every-4th-beat" },
        "model": g.model,
        "fallback": g.fallback,
    })
}

/// How to read the per-slot quality fields; sent with every cut list so an
/// agent never mistakes a heuristic for a measurement it is not.
const SIGNALS_NOTE: &str = "motion (videos only): mean pixel change between the matched frame and one 0.2s later, 0..1, uncalibrated — compare slots, don't threshold; null = not measured. \
     similar_to: other slots that repeat this one — the same footage (same photo, or the same moment of the same video) or a near-duplicate stored embedding (same scene, maybe a different file); null = could not be checked. \
     width/height/rotation_cw come from file metadata: they can flag a rotation the render will not apply (may_render_sideways), but rotation 0 does not prove the picture is upright.";

/// A transition in the edit-op vocabulary, e.g. `crossfade:0.8`.
fn transition_spec(t: reel::Transition) -> String {
    match t {
        reel::Transition::Cut => "cut".to_string(),
        reel::Transition::Crossfade { secs } => format!("crossfade:{secs}"),
        reel::Transition::DipToBlack { secs } => format!("dip:{secs}"),
        reel::Transition::Slide { secs } => format!("slide:{secs}"),
        reel::Transition::Whip { secs } => format!("whip:{secs}"),
    }
}

#[tool_handler]
impl ServerHandler for EideticMcp {
    /// The tool list, minus `preview_sheet` on a metadata-only server: an
    /// agent should not be offered what it would be refused.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        let mut tools = Self::tool_router().list_all();
        if !self.allow_previews {
            tools.retain(|t| t.name != "preview_sheet");
        }
        Ok(rmcp::model::ListToolsResult::with_all_items(tools))
    }

    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        let (look, pixels) = if self.allow_previews {
            (
                "preview_sheet (look at every slot: same-scene repeats, \
                 sideways or weak shots) → ",
                "The operator opted into previews: preview_sheet returns real \
                 frames from the library as an image; every other tool \
                 returns metadata only.",
            )
        } else {
            (
                "",
                "Tools return metadata only — paths, timecodes, scores — \
                 never pixels.",
            )
        };
        info.instructions = Some(format!(
            "Eidetic: a fully local media library (photos, videos, faces, \
             speech) with a beat-synced reel editor. Search and inspect \
             with the context tools; direct an edit with create_reel → \
             edit_reel (iterate on the cut list — check every slot's \
             score, swap weak ones, keep the hero shot on the drop) → \
             {look}render (preview first, full-res when happy) → \
             export_otio for hand-polish. {pixels}"
        ));
        info
    }
}
