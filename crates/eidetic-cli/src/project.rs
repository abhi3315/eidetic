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

/// Refuse a render output that is not an `.mp4` path, so a render an agent
/// asks for can never replace a file that is not a video.
pub fn ensure_mp4(output: &Path) -> Result<()> {
    anyhow::ensure!(
        output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4")),
        "output must be an .mp4 path, got {}",
        output.display()
    );
    Ok(())
}

/// Project files are a few KB to a few hundred; anything bigger is not one.
const MAX_PROJECT_BYTES: u64 = 8 << 20;
/// Reading a small local file; this only catches a hung network mount.
const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Headroom over create_reel's densest cut: its 600 s cap at the planner's
/// 0.6 s span floor is 1000 slots, and edits never add slots. CLI reels have
/// no 600 s cap, so a long dense one can pass this and is refused here.
const MAX_SLOTS: usize = 2000;
/// A preview halves each side and rounds down to even, so a side under 4
/// would render 0 px; 16 leaves a usable preview. 8192 covers 8K.
const MIN_SIDE: u32 = 16;
const MAX_SIDE: u32 = 8192;
/// An hour is far past any reel; past it the numbers are not a reel's.
const MAX_REEL_SECS: f64 = 3600.0;
/// How to repair a project every MCP tool refuses to load, so outside MCP.
const REPAIR_HINT: &str = "fix the project file by hand (`eidetic reel edit` can retime, \
    drop or re-transition its slots), or plan the reel again with create_reel";

/// Identifies one reel project in everything the pipeline writes for it: the
/// project file, the mp4 `comment` tag of its renders and the metadata of
/// its OTIO export. MCP tools replace an existing file only when it carries
/// the id of the project they were handed (see [`check_destination`]).
///
/// An identity, not a secret: anyone who can read a render can copy it.
/// Lives here rather than beside the entity ids in `eidetic-core` because
/// nothing outside the reel pipeline names a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(uuid::Uuid);

/// What precedes the id in a render's `comment` tag.
const RENDER_TAG_PREFIX: &str = "eidetic-project:";

impl ProjectId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The mp4 `comment` tag a render of this project carries.
    pub fn render_tag(self) -> String {
        format!("{RENDER_TAG_PREFIX}{self}")
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReelProject {
    pub version: u32,
    /// Missing from projects planned before outputs were tagged. The CLI
    /// fills it in on load; MCP tools ask for an adoption first (see
    /// [`require_id`](Self::require_id)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ProjectId>,
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

    /// Load a project from a path the operator chose (the CLI). A project
    /// without an id gets a fresh one, which the next save records and the
    /// next render or export carries.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read project file {}", path.display()))?;
        let mut project = Self::parse(path, &bytes)?;
        project.id.get_or_insert_with(ProjectId::generate);
        Ok(project)
    }

    /// The project's id, or how to adopt a project that has none. `tool`
    /// is the MCP tool that needs it, named in the refusal.
    pub fn require_id(&self, project_path: &Path, tool: &str) -> Result<ProjectId> {
        self.id.ok_or_else(|| {
            let p = project_path.display();
            let adopt = serde_json::json!({ "project": project_path, "ops": [] });
            anyhow::anyhow!(
                "{p} has no project id (it was planned before Eidetic tracked which files \
                 it wrote); nothing was written. Adopt it by calling edit_reel with {adopt}, \
                 then call {tool} again. Files an earlier version already rendered or \
                 exported for it cannot be proved to be its own and stay protected: ask the \
                 user to move them aside, or to re-render the full reel with \
                 `eidetic reel edit {p}` (add `--export otio` for the .otio), which marks \
                 them as this project's"
            )
        })
    }

    /// Load a project from a path nobody vetted (an MCP agent's): a regular
    /// file of at most [`MAX_PROJECT_BYTES`], read off the async executor
    /// within [`LOAD_TIMEOUT`], whose numbers then pass
    /// [`validate`](Self::validate). A read stuck in the kernel (a dead
    /// network mount) cannot be cancelled, but it no longer holds the
    /// server: the call fails and the blocked thread is abandoned.
    pub async fn load_untrusted(path: &Path) -> Result<Self> {
        let owned = path.to_path_buf();
        let load = tokio::task::spawn_blocking(move || Self::load_bounded(&owned));
        let proj = tokio::time::timeout(LOAD_TIMEOUT, load)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "reading {} took longer than {}s; is it on a reachable disk?",
                    path.display(),
                    LOAD_TIMEOUT.as_secs()
                )
            })?
            .context("project reader panicked")??;
        proj.validate().map_err(|e| {
            anyhow::anyhow!("{} is out of range: {e:#}; {REPAIR_HINT}", path.display())
        })?;
        // render replaces the output, so a project nobody vetted may only
        // name the kind of file create_reel would have written.
        ensure_mp4(&proj.output)
            .with_context(|| format!("{} cannot be rendered through MCP", path.display()))?;
        Ok(proj)
    }

    /// Refuses anything but a regular file (a FIFO would block the read
    /// forever, a device would never end) and reads at most
    /// [`MAX_PROJECT_BYTES`], so a hostile or mistaken path costs a bounded
    /// allocation.
    fn load_bounded(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, MAX_PROJECT_BYTES, "reel project file")?;
        Self::parse(path, &bytes)
    }

    /// Refuse a project whose numbers no render could use: slot count,
    /// frame size and style, every slot's span and joint, and the total
    /// length. Pure arithmetic, so it runs before any ffmpeg or write.
    /// Sources, audio and the output path are not checked here; the
    /// preview sheet checks sources against the library itself.
    pub fn validate(&self) -> Result<()> {
        use anyhow::ensure;
        let count = self.slots.len();
        ensure!(count > 0, "the project has no slots");
        ensure!(
            count <= MAX_SLOTS,
            "the project has {count} slots; at most {MAX_SLOTS}"
        );
        let sides = MIN_SIDE..=MAX_SIDE;
        ensure!(
            sides.contains(&self.width) && sides.contains(&self.height),
            "project frame {}x{} is outside {MIN_SIDE}..{MAX_SIDE} px per side",
            self.width,
            self.height
        );
        reel::Fit::parse(&self.frame, self.height > self.width)?;
        for (i, s) in self.slots.iter().enumerate() {
            let n = i + 1;
            ensure!(
                s.duration.is_finite() && s.duration > 0.0,
                "slot {n} has an unusable duration ({})",
                s.duration
            );
            ensure!(
                s.start.is_finite() && s.start >= 0.0,
                "slot {n} has an unusable start ({})",
                s.start
            );
            let t = s.transition_in.secs();
            ensure!(
                (0.0..=reel::MAX_TRANSITION_SECS).contains(&t),
                "slot {n}'s transition lasts {t}s; transitions are 0..{}s",
                reel::MAX_TRANSITION_SECS
            );
        }
        // Also catches a sum that overflowed to infinity.
        let total = self.total_secs();
        ensure!(
            total <= MAX_REEL_SECS,
            "the reel runs {total}s; at most {MAX_REEL_SECS}s"
        );
        Ok(())
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
        let json = serde_json::to_vec_pretty(self).context("serialize project")?;
        write_atomically(path, &json, ".eidetic-project-", "project file", false)
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
            id: Some(ProjectId::generate()),
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
            let similar = match reel::similar_to(&self.slots, i) {
                Some(v) if !v.is_empty() => {
                    let v: Vec<String> = v.iter().map(usize::to_string).collect();
                    format!(" [similar to {}]", v.join(", "))
                }
                _ => String::new(),
            };
            println!("  {:>2}. {}{similar}", i + 1, slot.describe());
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

/// Replace `path` with `bytes` through a complete, synced sibling temp file
/// named `<prefix>…`, renamed into place: a failure before the rename leaves
/// the old file intact and no temp file behind, and a link at `path` is
/// replaced rather than written through. `what` names the file in errors.
///
/// The temp file is created owner-only; `shared` creates it with the
/// permissions a plain write would give it (0666 less the umask) instead,
/// for files other programs open.
pub(crate) fn write_atomically(
    path: &Path,
    bytes: &[u8],
    prefix: &str,
    what: &str,
    shared: bool,
) -> Result<()> {
    use std::io::Write;

    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix).suffix(".tmp");
    #[cfg(unix)]
    if shared {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    #[cfg(not(unix))]
    let _ = shared;
    let mut tmp = builder
        .tempfile_in(parent_dir(path))
        .with_context(|| format!("failed to create a temp file next to {}", path.display()))?;
    // NamedTempFile removes itself on drop, so every early return below
    // leaves the destination alone and no litter in the directory.
    tmp.write_all(bytes)
        .and_then(|()| tmp.flush())
        .and_then(|()| tmp.as_file().sync_all())
        .with_context(|| format!("failed to write {what} {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("failed to replace {what} {}", path.display()))?;
    Ok(())
}

/// Read a regular file of at most `max` bytes. Anything else is refused
/// before it is opened (a FIFO would block the read forever, a device would
/// never end), so a hostile or mistaken path costs a bounded allocation.
/// `what` names the expected file in errors.
pub(crate) fn read_bounded(path: &Path, max: u64, what: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let not_a_file = || anyhow::anyhow!("{} is not a regular file; pass a {what}", path.display());
    // Checked before opening: opening a FIFO for reading blocks until a
    // writer appears. Re-checked on the handle in case the path was swapped
    // in between.
    let meta = std::fs::metadata(path)
        .with_context(|| format!("failed to read {what} {}", path.display()))?;
    anyhow::ensure!(meta.is_file(), not_a_file());
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to read {what} {}", path.display()))?;
    anyhow::ensure!(file.metadata()?.is_file(), not_a_file());
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {what} {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= max,
        "{} is larger than {max} bytes; that is not a {what}",
        path.display()
    );
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Output ownership at MCP write boundaries (ADR-0012, output ownership).
// ---------------------------------------------------------------------------

/// What a write may find at its destination and still go ahead.
#[derive(Clone, Copy)]
pub(crate) enum Claim<'a> {
    /// create_reel's claims: nothing may exist there yet. `remedy` finishes
    /// the refusal when something does.
    New { remedy: &'a str },
    /// A full render (`preview: false`) or preview of the project file at
    /// `project`: an existing mp4 must carry `id` as its comment tag.
    Render {
        id: ProjectId,
        project: &'a Path,
        preview: bool,
    },
    /// The OTIO export of the project at `project`: an existing timeline
    /// must carry `id` in its metadata.
    Otio { id: ProjectId, project: &'a Path },
    /// The project file, saved back where it was loaded: it must still hold
    /// `loaded` (the project as the call read it, as a JSON value), so an
    /// edit never saves over another project or over someone else's save.
    ProjectFile { loaded: &'a serde_json::Value },
}

/// Whether an MCP tool may write `dest`. Refused, without following or
/// touching anything:
///
/// - a path with a `..` component;
/// - a destination that is a symbolic link, or under a directory that is
///   one, or whose directory does not exist;
/// - anything whose directory resolves inside `library_dir`: MCP tools
///   never write into the library, even over a file they could prove;
/// - an existing non-regular file, and an existing regular file `claim`
///   does not prove is this project's.
///
/// Blocking (filesystem calls, and ffprobe for an existing render), so
/// async callers run it on a blocking thread. Every refusal names the path
/// and what the agent can do. This keeps a project's write manifest off
/// unrelated files; it does not confine the server.
pub(crate) fn check_destination(dest: &Path, library_dir: &Path, claim: Claim<'_>) -> Result<()> {
    use std::io::ErrorKind::NotFound;
    let dest = std::path::absolute(dest)
        .with_context(|| format!("cannot resolve {}; nothing was written", dest.display()))?;
    let shown = dest.display();
    anyhow::ensure!(
        !dest
            .components()
            .any(|c| c == std::path::Component::ParentDir),
        "{shown} contains `..`; MCP tools write only to paths without it; nothing was \
         written. Pass the path without `..`"
    );
    let parent = dest
        .parent()
        .with_context(|| format!("{shown} is not a file path; nothing was written"))?;

    // The destination and every directory above it, nearest first, each
    // inspected without following it.
    let mut existing = None;
    for (depth, at) in dest.ancestors().enumerate() {
        match std::fs::symlink_metadata(at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let real = std::fs::canonicalize(&dest)
                    .or_else(|_| std::fs::canonicalize(parent))
                    .map(|p| format!(" (it resolves to {})", p.display()))
                    .unwrap_or_default();
                anyhow::bail!(
                    "{shown}: {} is a symbolic link{real}; MCP tools never write through \
                     links, nothing was written. Use a path in a real directory, or ask the \
                     user",
                    at.display()
                );
            }
            Ok(meta) if depth == 0 => existing = Some(meta),
            Ok(_) => {}
            Err(e) if e.kind() == NotFound && depth == 0 => {}
            Err(e) if e.kind() == NotFound && depth == 1 => anyhow::bail!(
                "{shown}: the directory {} does not exist; nothing was written. Use an \
                 output in an existing directory",
                at.display()
            ),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "cannot inspect {}; nothing was written to {shown}",
                        at.display()
                    )
                });
            }
        }
    }

    // A library that does not exist yet cannot contain an existing parent.
    match std::fs::canonicalize(library_dir) {
        Ok(lib) => {
            let real_parent = std::fs::canonicalize(parent).with_context(|| {
                format!("cannot resolve {}; nothing was written", parent.display())
            })?;
            anyhow::ensure!(
                !real_parent.starts_with(&lib),
                "{shown} is inside the library directory {}; MCP tools never write into \
                 the library, nothing was written. Plan the reel with create_reel and an \
                 output outside the library, or ask the user to render it with `eidetic \
                 reel`",
                lib.display()
            );
        }
        Err(e) if e.kind() == NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "cannot resolve the library directory {}; nothing was written to {shown}",
                    library_dir.display()
                )
            });
        }
    }

    let Some(meta) = existing else {
        return Ok(());
    };
    anyhow::ensure!(
        meta.is_file(),
        "{shown} exists and is not a regular file; nothing was written. Use another output, \
         or ask the user"
    );
    let not_ours = |id: ProjectId, hint: String| {
        anyhow::anyhow!(
            "{shown} already exists and was not written by Eidetic for this project ({id}); \
             nothing was written. Ask the user to move or delete it, or plan a new reel with \
             create_reel and a different output.{hint}"
        )
    };
    match claim {
        Claim::New { remedy } => anyhow::bail!("{shown} already exists; {remedy}"),
        Claim::Render {
            id,
            project,
            preview,
        } => {
            if !render_carries(&dest, id)? {
                let hint = if preview {
                    " If it is an older preview of this project, it is safe to delete.".into()
                } else {
                    format!(
                        " If it is this project's render from before Eidetic marked its \
                         renders, the user can re-render it with `eidetic reel edit {}`.",
                        project.display()
                    )
                };
                return Err(not_ours(id, hint));
            }
        }
        Claim::Otio { id, project } => {
            if !crate::otio::carries(&dest, id) {
                return Err(not_ours(
                    id,
                    format!(
                        " If it is this project's export from before Eidetic marked its \
                         exports, the user can re-export it with `eidetic reel edit {} \
                         --export otio --dry-run`.",
                        project.display()
                    ),
                ));
            }
        }
        Claim::ProjectFile { loaded } => {
            let same = read_bounded(&dest, MAX_PROJECT_BYTES, "reel project file")
                .and_then(|bytes| ReelProject::parse(&dest, &bytes))
                .and_then(|p| serde_json::to_value(p).context("serialize the project"))
                .is_ok_and(|p| p == *loaded);
            anyhow::ensure!(
                same,
                "{shown} is no longer the project this call loaded; nothing was saved. Call \
                 edit_reel again on the file as it is now"
            );
        }
    }
    Ok(())
}

/// Whether the mp4 at `path` carries `id` as its comment tag. The mov
/// demuxer is forced so that a playlist or concat list disguised as an mp4
/// cannot make ffprobe open other files or URLs.
fn render_carries(path: &Path, id: ProjectId) -> Result<bool> {
    let ffprobe = eidetic_ingest::video::ffprobe().with_context(|| {
        format!(
            "{} already exists, and proving it is this project's render needs ffprobe; \
             nothing was written. Install ffprobe (it ships with ffmpeg) or set \
             EIDETIC_FFPROBE_PATH, or ask the user to move the file aside",
            path.display()
        )
    })?;
    let probe = eidetic_ingest::subprocess::run(
        std::process::Command::new(ffprobe)
            .args(["-v", "error", "-f", "mov"])
            .args(["-show_entries", "format_tags=comment"])
            .args(["-of", "default=noprint_wrappers=1:nokey=1"])
            .arg(path),
        path,
        OWNERSHIP_PROBE_DEADLINE,
    );
    match probe {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).trim() == id.render_tag()),
        // Not an mp4 ffprobe can read: not one of this project's renders.
        Err(eidetic_ingest::Error::SubprocessFailed { .. }) => Ok(false),
        // A probe that hangs or cannot run proves nothing; refuse the write.
        Err(e) => Err(e).with_context(|| {
            format!(
                "{} already exists and could not be checked as this project's render; \
                 nothing was written",
                path.display()
            )
        }),
    }
}

/// Reading one tag from a local file; far past any real probe.
const OWNERSHIP_PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

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

impl ReelSpec {
    /// The render output of every project [`plan_projects`] makes for this
    /// spec: the output itself, or `<output>.vN.mp4` per variation.
    pub fn outputs(&self) -> Vec<PathBuf> {
        if self.variations <= 1 {
            return vec![self.output.clone()];
        }
        (1..=self.variations)
            .map(|v| self.output.with_extension(format!("v{v}.mp4")))
            .collect()
    }
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
    let signals = load_signals(&repo, model_id, &result_keys(&results)).await?;

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
        // Beat tracking and transcription read the same samples; share them.
        let pcm = std::sync::Arc::new(pcm);
        let pcm_for_beats = std::sync::Arc::clone(&pcm);
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
        let mut span_signals = reel::Signals::default();
        let mut placed: Vec<reel::MomentRef> = Vec::new();
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
            // Only this span's hits are new; placed moments are loaded.
            extend_signals(&repo, model_id, &mut span_signals, &result_keys(&hits)).await?;
            if let Some(hit) = choose_voiceover(&hits, prev, &placed, &span_signals) {
                prev = Some(hit.id);
                placed.push(reel::MomentRef::of_hit(hit));
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
        for (v, output) in (1..).zip(spec.outputs()) {
            let mut pool = strong.clone();
            if !pool.is_empty() {
                let k = (v - 1) % pool.len();
                pool.rotate_left(k);
            }
            let mut slots = match &grid {
                Some(g) => reel::plan_synced(
                    &pool,
                    g,
                    total,
                    &config.paths.library_dir,
                    &scenes,
                    &focus,
                    &signals,
                ),
                None => reel::plan(&pool, total, &config.paths.library_dir, &scenes, &signals),
            };
            reel::restyle(&mut slots, v - 1);
            finish_slots(&repo, model_id, &mut slots, &results).await?;
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
                &output,
                slots,
            ));
        }
        return Ok(projects);
    }

    let mut slots = match (&voiceover_pairs, &grid) {
        (Some(pairs), _) => {
            // Framing and scene data for the chosen shots (they may not
            // overlap the prompt's search results).
            let chosen: Vec<SearchResult> = pairs.iter().map(|(_, r)| r.clone()).collect();
            focus.extend(framing_focus(config, &repo, &chosen).await?);
            scenes.extend(fetch_scenes(&repo, &chosen).await?);
            let mut slots = reel::plan_assigned(pairs, &config.paths.library_dir, &scenes, &focus);
            finish_slots(&repo, model_id, &mut slots, &chosen).await?;
            slots
        }
        (None, Some(g)) => reel::plan_synced(
            &results,
            g,
            total,
            &config.paths.library_dir,
            &scenes,
            &focus,
            &signals,
        ),
        (None, None) => reel::plan(
            &results,
            total,
            &config.paths.library_dir,
            &scenes,
            &signals,
        ),
    };
    if voiceover_pairs.is_none() {
        finish_slots(&repo, model_id, &mut slots, &results).await?;
    }
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

/// Re-checks a render's destination; an error stops the render from
/// replacing it.
pub type PublishCheck = std::sync::Arc<dyn Fn() -> Result<()> + Send + Sync>;

/// Where a render lands and at what size.
pub struct RenderTarget {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Run just before the finished render replaces `path` (see
    /// [`reel::Publish`]). Unset for the operator's own CLI renders.
    pub before_publish: Option<PublishCheck>,
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
                before_publish: None,
            };
        }
        RenderTarget {
            path: preview_path(&self.output),
            width: (self.width / 2) & !1,
            height: (self.height / 2) & !1,
            before_publish: None,
        }
    }
}

/// Where the preview of a render at `output` goes: `reel.mp4` →
/// `reel.preview.mp4`.
pub fn preview_path(output: &Path) -> PathBuf {
    let ext = output
        .extension()
        .map_or_else(|| "mp4".into(), |e| e.to_string_lossy().into_owned());
    output.with_extension(format!("preview.{ext}"))
}

/// Render a project's cut list to `target` (see
/// [`ReelProject::render_target`]), tagged with the project's id.
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
    let tag = proj.id.map(ProjectId::render_tag);
    let check = target.before_publish.clone();
    tokio::task::spawn_blocking(move || {
        let publish = reel::Publish {
            tag: tag.as_deref(),
            check: check.as_deref(),
        };
        reel::render(&slots, &output, w, h, fit, audio.as_deref(), &publish)
    })
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

/// Signal lookups for search hits: the matched frame of a video hit, the
/// photo itself otherwise.
fn result_keys(results: &[SearchResult]) -> Vec<reel::MomentRef> {
    results.iter().map(reel::MomentRef::of_hit).collect()
}

/// Signal lookups for slots already on the timeline: a video slot's moment
/// is the sampled frame nearest the middle of its span — which works for
/// project files that never recorded the matched timestamp.
fn slot_keys(slots: &[Slot]) -> Vec<reel::MomentRef> {
    slots.iter().map(reel::MomentRef::of_slot).collect()
}

/// Stored quality signals for `refs` in one batched lookup, each kept under
/// its own asset + moment (two moments of one video never share signals).
/// Only embeddings whose stored identity is exactly `model_id` (the pinned
/// id the query was embedded with) are compared; any other — another
/// variant, or a legacy unpinned row — is a different similarity space, so
/// it is dropped and counted on stderr, and a half-migrated library
/// explains why near-duplicate checks went quiet.
async fn load_signals(
    repo: &AssetsRepo,
    model_id: &str,
    refs: &[reel::MomentRef],
) -> Result<reel::Signals> {
    let mut signals = reel::Signals::default();
    extend_signals(repo, model_id, &mut signals, refs).await?;
    Ok(signals)
}

/// [`load_signals`] into an existing set (voiceover grows one per span).
async fn extend_signals(
    repo: &AssetsRepo,
    model_id: &str,
    signals: &mut reel::Signals,
    refs: &[reel::MomentRef],
) -> Result<()> {
    let keys: Vec<eidetic_db::MomentKey> = refs
        .iter()
        .map(|r| eidetic_db::MomentKey {
            asset: r.asset,
            at: r.at,
        })
        .collect();
    let fetched = repo
        .fetch_moment_signals(model_id, &keys)
        .await
        .context("failed to load quality signals")?;
    let mut other_model = 0usize;
    for (r, s) in refs.iter().zip(fetched) {
        other_model += usize::from(s.other_model.is_some());
        signals.insert(
            *r,
            reel::Moment {
                frame: s.ts_secs,
                embedding: s.embedding,
                motion: s.motion,
            },
        );
    }
    if other_model > 0 {
        eprintln!(
            "{other_model} candidate(s) were embedded by a different model than {model_id}; \
             near-duplicate checks skip them until `eidetic embed` re-embeds them."
        );
    }
    Ok(())
}

/// Everything a planned cut list carries besides its shots: motion and
/// repeat annotations read at each slot's own moment (one batched lookup),
/// and probed geometry (the photo's original comes from `results`, for its
/// EXIF rotation). Scarcity that forced a repeat in is reported, not hidden.
async fn finish_slots(
    repo: &AssetsRepo,
    model_id: &str,
    slots: &mut [Slot],
    results: &[SearchResult],
) -> Result<()> {
    let signals = load_signals(repo, model_id, &slot_keys(slots)).await?;
    reel::annotate(slots, &signals);
    let originals: std::collections::HashMap<_, _> = results
        .iter()
        .map(|r| (r.id, r.storage_path.clone()))
        .collect();
    probe_geometry(slots, &originals).await?;
    let repeats = slots
        .iter()
        .filter(|s| s.near_duplicates.as_ref().is_some_and(|d| !d.is_empty()))
        .count();
    if repeats > 0 {
        eprintln!(
            "{repeats} slot(s) are near-duplicates of another shot (not enough distinct \
             candidates); see similar_to in the cut list."
        );
    }
    Ok(())
}

/// Fill each slot's [`reel::Geometry`] from its files (ffprobe for videos,
/// the image header + original's EXIF for photos). Per slot, never per
/// library; runs off the async runtime.
async fn probe_geometry(
    slots: &mut [Slot],
    originals: &std::collections::HashMap<eidetic_core::AssetId, PathBuf>,
) -> Result<()> {
    let work: Vec<(Slot, PathBuf)> = slots
        .iter()
        .map(|s| {
            let original = originals
                .get(&s.asset)
                .cloned()
                .unwrap_or_else(|| s.source.clone());
            (s.clone(), original)
        })
        .collect();
    let geometry = tokio::task::spawn_blocking(move || {
        work.iter()
            .map(|(s, o)| reel::probe_geometry(s, o))
            .collect::<Vec<_>>()
    })
    .await
    .context("geometry probe thread panicked")?;
    for (slot, g) in slots.iter_mut().zip(geometry) {
        slot.geometry = g;
    }
    Ok(())
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

/// The swap winner among `hits` (search order), taken from the first
/// non-empty tier, sharp before soft within each:
///
/// 1. an unused asset, distinct from every other slot;
/// 2. a distinct moment of an asset already in the reel (another scene of
///    a used video) — still no repeat on screen, and never the footage
///    `replaced` is showing now;
/// 3. an unused asset that repeats a scene already placed;
/// 4. any positive hit.
///
/// "Distinct" = not the same footage and not a near-duplicate by stored
/// embedding; a hit with no comparable embedding counts as distinct
/// (unknown is not a repeat). A swap that hands back a shot already on
/// screen — or a new file of the same scene — helps nobody, but a repeat
/// beats giving up, and the refreshed similar_to annotation shows it.
/// `sharp` is called lazily: it decodes pixels.
pub(crate) fn choose_swap<'a>(
    hits: &'a [SearchResult],
    used: &std::collections::HashSet<eidetic_core::AssetId>,
    others: &[reel::MomentRef],
    replaced: reel::MomentRef,
    signals: &reel::Signals,
    sharp: impl Fn(&SearchResult) -> bool,
) -> Option<&'a SearchResult> {
    let positive = |h: &SearchResult| h.score > 0.0;
    let fresh = |h: &SearchResult| positive(h) && !used.contains(&h.id);
    // Moment-aware: the same footage as another slot is a repeat even when
    // nothing about it is embedded.
    let distinct = |h: &SearchResult| !signals.repeats_any(reel::MomentRef::of_hit(h), others);
    let new_moment = |h: &SearchResult| {
        positive(h) && distinct(h) && !signals.same_footage(reel::MomentRef::of_hit(h), replaced)
    };
    let tier = |ok: &dyn Fn(&SearchResult) -> bool| {
        hits.iter()
            .find(|h| ok(h) && sharp(h))
            .or_else(|| hits.iter().find(|h| ok(h)))
    };
    tier(&|h| fresh(h) && distinct(h))
        .or_else(|| tier(&new_moment))
        .or_else(|| tier(&fresh))
        .or_else(|| tier(&positive))
}

/// The shot for one narration span, from that span's hits (search order).
/// Diversity only ever chooses among the semantically eligible hits — the
/// same weak-tail rules as every planner ([`reel::strong_prefix`]) — so a
/// relevant repeat always beats an unrelated alternative: eligible,
/// distinct from every shot already placed and not the previous asset →
/// eligible and not the previous asset → the best hit. Distinct = not the
/// same footage and not within [`reel::NEAR_DUPLICATE`] by stored
/// embedding; unknown embeddings count as distinct. Scarcity falls through
/// to a repeat, which the finished cut list's similar_to then shows.
pub(crate) fn choose_voiceover<'a>(
    hits: &'a [SearchResult],
    prev: Option<eidetic_core::AssetId>,
    placed: &[reel::MomentRef],
    signals: &reel::Signals,
) -> Option<&'a SearchResult> {
    let eligible = reel::strong_prefix(hits);
    let not_prev = |h: &SearchResult| Some(h.id) != prev;
    eligible
        .iter()
        .find(|h| not_prev(h) && !signals.repeats_any(reel::MomentRef::of_hit(h), placed))
        .or_else(|| eligible.iter().find(|h| not_prev(h)))
        .or_else(|| hits.first())
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

    // Media length of every retimed video, probed off the async executor
    // because ffprobe blocks.
    let to_probe: Vec<(usize, PathBuf)> = retimes
        .iter()
        .filter(|(i, _)| proj.slots[*i].kind == reel::SlotKind::Video)
        .map(|(i, _)| (*i, proj.slots[*i].source.clone()))
        .collect();
    let media_secs: std::collections::HashMap<usize, f64> =
        tokio::task::spawn_blocking(move || {
            to_probe
                .into_iter()
                .filter_map(|(i, source)| {
                    let probe = eidetic_ingest::video::probe(&source).ok()??;
                    Some((i, probe.duration_secs?))
                })
                .collect()
        })
        .await
        .context("probe thread panicked")?;

    let mut retimed_video = false;
    for (i, secs) in retimes {
        let slot = &mut proj.slots[i];
        retimed_video |= slot.kind == reel::SlotKind::Video;
        let mut secs = secs;
        // Clamp a video to the media past its in-point; a shortened file
        // would silently desync everything after it.
        if let Some(&d) = media_secs.get(&i) {
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

    // A retime moves a video slot's midpoint — the moment its motion and
    // similar_to are read at — and a swap changes which shots repeat, so
    // either one re-annotates the whole list once, after the swaps.
    let repo = if retimed_video || !swaps.is_empty() {
        Some(AssetsRepo::new(
            eidetic_db::connect(config)
                .await
                .context("failed to connect to database")?,
        ))
    } else {
        None
    };

    if let Some(repo) = repo.as_ref().filter(|_| !swaps.is_empty()) {
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

        let mut used: std::collections::HashSet<eidetic_core::AssetId> =
            proj.slots.iter().map(|s| s.asset).collect();

        for ((i, query), emb) in swaps.iter().zip(&embs) {
            let hits = repo
                .search(
                    query,
                    emb,
                    model_id,
                    20,
                    &eidetic_db::SearchFilters::default(),
                )
                .await
                .context("swap search failed")?;
            // Near-duplicate screening compares each hit's matched moment
            // with every OTHER slot as it stands now (earlier swaps in this
            // batch included; the slot being replaced excluded).
            // Keys carry asset + moment, so a hit from video A at 20s and a
            // slot of A at 2s keep their own signals.
            let other_moments: Vec<reel::MomentRef> = proj
                .slots
                .iter()
                .enumerate()
                .filter(|(j, _)| j != i)
                .map(|(_, s)| reel::MomentRef::of_slot(s))
                .collect();
            let replaced = reel::MomentRef::of_slot(&proj.slots[*i]);
            let mut keys = result_keys(&hits);
            keys.extend(other_moments.iter().copied());
            keys.push(replaced);
            let signals = load_signals(repo, model_id, &keys).await?;
            let lib = config.paths.library_dir.clone();
            let hit = choose_swap(&hits, &used, &other_moments, replaced, &signals, |h| {
                reel::sharp_enough(h, &lib, reel::MIN_SHARPNESS)
            })
            .with_context(|| format!("nothing in the library matches {query:?}"))?;
            let scenes = fetch_scenes(repo, std::slice::from_ref(hit)).await?;
            let focus = framing_focus(config, repo, std::slice::from_ref(hit)).await?;
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
            let mut geometry_for = std::collections::HashMap::new();
            geometry_for.insert(hit.id, hit.storage_path.clone());
            probe_geometry(std::slice::from_mut(&mut slot), &geometry_for).await?;
            used.insert(slot.asset);
            eprintln!("slot {}: {} -> {}", i + 1, old.describe(), slot.describe());
            proj.slots[*i] = slot;
        }
    }

    // Every slot is re-annotated, pinned ones included: their footage is
    // untouched, but what repeats them (and, after a retime, the moment
    // their motion describes) may have changed.
    if let Some(repo) = &repo {
        let (model_id, _dim) =
            eidetic_ml::active_model().context("failed to resolve the active embedding model")?;
        let signals = load_signals(repo, model_id, &slot_keys(&proj.slots)).await?;
        reel::annotate(&mut proj.slots, &signals);
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
            motion: None,
            geometry: None,
            near_duplicates: None,
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

    /// Retiming a video slot moves its midpoint — the moment its motion
    /// and similar_to are read at. Against a real temporary SQLite library
    /// (video V: beach at 2s, forest at 20s; photo P: the beach again), a
    /// retime alone, with no swap in the batch, must re-read both.
    #[tokio::test]
    async fn retime_refreshes_moment_annotations_without_a_swap() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let repo = AssetsRepo::new(eidetic_db::connect(&config).await.unwrap());
        let (model_id, _) = eidetic_ml::active_model().unwrap();
        let insert = |n: u8, mime: &str| {
            let asset = eidetic_db::NewAsset {
                hash: format!("{n:02x}").repeat(32),
                original_filename: format!("f{n}"),
                storage_path: dir.path().join(format!("f{n}")),
                file_size: 1,
                mime_type: Some(mime.to_string()),
                date_taken: None,
                latitude: None,
                longitude: None,
                camera_make: None,
                camera_model: None,
                lens_make: None,
                lens_model: None,
                focal_length: None,
                focal_length_35mm: None,
                aperture: None,
                shutter: None,
                iso: None,
                orientation: None,
                altitude: None,
                gps_direction: None,
                exif_raw: None,
                country_code: None,
                country_name: None,
                admin1: None,
                place: None,
                place_distance_m: None,
                thumbnails_generated: false,
                duration_secs: mime.starts_with("video/").then_some(60.0),
                video_codec: None,
                pixel_width: None,
                pixel_height: None,
            };
            let repo = &repo;
            async move {
                match repo.insert_asset(asset).await.unwrap() {
                    eidetic_db::InsertOutcome::Inserted(id) => id,
                    eidetic_db::InsertOutcome::Existing(_) => panic!("fresh database"),
                }
            }
        };
        let video = insert(1, "video/mp4").await;
        let photo = insert(2, "image/jpeg").await;
        let frame = |ts: f64, vector: Vec<f32>, motion: f32| eidetic_db::NewFrame {
            ts_secs: ts,
            vector,
            motion: Some(motion),
        };
        repo.store_frame_embeddings(
            video,
            &[
                frame(2.0, vec![1.0, 0.0], 0.1),
                frame(20.0, vec![0.0, 1.0], 0.7),
            ],
            model_id,
        )
        .await
        .unwrap();
        repo.store_embedding(photo, &[0.99, 0.05], model_id)
            .await
            .unwrap();

        let mut p = four();
        p.slots.truncate(2);
        // Slot 1: V from 1s for 2s (midpoint 2s, the beach). The source
        // does not exist, so the retime clamp cannot probe it.
        p.slots[0] = Slot {
            asset: video,
            source: dir.path().join("missing.mp4"),
            kind: SlotKind::Video,
            start: 1.0,
            duration: 2.0,
            ..p.slots[0].clone()
        };
        p.slots[1].asset = photo;
        let ops = |v| serde_json::from_value::<Vec<EditOp>>(v).unwrap();

        // Retime to 38s: midpoint 1 + 19 = 20s, the forest.
        apply_ops(
            &mut p,
            &ops(serde_json::json!([{"op": "retime", "slot": 1, "secs": 38.0}])),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(p.slots[0].motion, Some(0.7), "motion of the new midpoint");
        assert_eq!(reel::similar_to(&p.slots, 0), Some(vec![]));
        assert_eq!(
            reel::similar_to(&p.slots, 1),
            Some(vec![]),
            "the photo no longer repeats the video"
        );

        // And back to 2s: the beach again, which the photo repeats.
        apply_ops(
            &mut p,
            &ops(serde_json::json!([{"op": "retime", "slot": 1, "secs": 2.0}])),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(p.slots[0].motion, Some(0.1));
        assert_eq!(reel::similar_to(&p.slots, 0), Some(vec![2]));
        assert_eq!(reel::similar_to(&p.slots, 1), Some(vec![1]));
    }

    // -- swap candidate selection (deterministic vectors) --------------------

    fn result(score: f32) -> SearchResult {
        SearchResult {
            id: AssetId::new(),
            storage_path: PathBuf::from("/x"),
            score,
            frame_ts: None,
            duration_secs: None,
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

    fn photo_ref(id: AssetId) -> reel::MomentRef {
        reel::MomentRef {
            asset: id,
            at: None,
        }
    }

    /// (moment, resolved frame, embedding) for one stored moment.
    type StoredMoment = (reel::MomentRef, Option<f64>, Option<[f32; 2]>);

    fn with_embeddings(pairs: &[StoredMoment]) -> reel::Signals {
        let mut s = reel::Signals::default();
        for (at, frame, e) in pairs {
            s.insert(
                *at,
                reel::Moment {
                    frame: *frame,
                    embedding: e.map(|v| v.to_vec()),
                    motion: None,
                },
            );
        }
        s
    }

    #[test]
    fn swap_skips_new_files_of_a_scene_already_in_the_reel() {
        let on_screen = photo_ref(AssetId::new());
        let hits = vec![result(0.5), result(0.4), result(0.3)];
        let sig = with_embeddings(&[
            (on_screen, None, Some([1.0, 0.0])),
            (photo_ref(hits[0].id), None, Some([0.99, 0.05])), // same scene
            (photo_ref(hits[1].id), None, Some([0.0, 1.0])),
            (photo_ref(hits[2].id), None, None), // unknown: not a repeat
        ]);
        // The slot being swapped out is some other photo.
        let replaced = photo_ref(AssetId::new());
        let used = std::collections::HashSet::from([on_screen.asset, replaced.asset]);
        let others = [on_screen];
        let pick = choose_swap(&hits, &used, &others, replaced, &sig, |_| true).unwrap();
        assert_eq!(pick.id, hits[1].id);

        // Sharpness still ranks within the distinct tier.
        let pick = choose_swap(&hits, &used, &others, replaced, &sig, |h| {
            h.id != hits[1].id
        })
        .unwrap();
        assert_eq!(pick.id, hits[2].id);

        // Scarcity: only the repeat is left, so it is used rather than
        // failing the swap.
        let pick = choose_swap(&hits[..1], &used, &others, replaced, &sig, |_| true).unwrap();
        assert_eq!(pick.id, hits[0].id);
        assert!(choose_swap(&[result(0.0)], &used, &[], replaced, &sig, |_| true).is_none());
    }

    /// A hit from video A at 20s must not overwrite the signals of the
    /// slot showing A at 2s: the beach-again candidate is compared with the
    /// beach slot, not with A's forest moment.
    #[test]
    fn swap_compares_candidates_with_each_slots_own_moment() {
        let a = AssetId::new();
        let mut a_hit = result(0.6);
        a_hit.id = a;
        a_hit.frame_ts = Some(20.0);
        let beach_again = result(0.5);
        let city = result(0.4);
        let hits = vec![a_hit.clone(), beach_again.clone(), city.clone()];
        let a_slot = reel::MomentRef {
            asset: a,
            at: Some(2.0),
        };
        // Hits first, then the slot — the load order swaps use.
        let sig = with_embeddings(&[
            (
                reel::MomentRef::of_hit(&a_hit),
                Some(20.0),
                Some([0.0, 1.0]),
            ),
            (photo_ref(beach_again.id), None, Some([0.99, 0.05])),
            (photo_ref(city.id), None, Some([0.6, -0.8])),
            (a_slot, Some(2.0), Some([1.0, 0.0])),
        ]);
        assert!(sig.repeats(photo_ref(beach_again.id), a_slot));
        let replaced = photo_ref(AssetId::new());
        let used = std::collections::HashSet::from([a, replaced.asset]);
        let pick = choose_swap(&hits, &used, &[a_slot], replaced, &sig, |_| true).unwrap();
        assert_eq!(pick.id, city.id);
    }

    /// With no fresh distinct candidate, another scene of a video already in
    /// the reel (A at 20s, a forest) beats a fresh file repeating the beach
    /// A shows at 2s; sharpness still orders that tier; the footage being
    /// swapped out is never handed back as "distinct".
    #[test]
    fn swap_prefers_a_distinct_moment_of_a_used_video_over_a_repeat() {
        let a = AssetId::new();
        let moment = |ts: f64, score: f32| {
            let mut h = result(score);
            h.id = a;
            h.frame_ts = Some(ts);
            h
        };
        let (a20, a30) = (moment(20.0, 0.6), moment(30.0, 0.55));
        let beach_again = result(0.5);
        let a_slot = reel::MomentRef {
            asset: a,
            at: Some(2.0),
        };
        let sig = with_embeddings(&[
            (reel::MomentRef::of_hit(&a20), Some(20.0), Some([0.0, 1.0])),
            (reel::MomentRef::of_hit(&a30), Some(30.0), Some([0.6, -0.8])),
            (photo_ref(beach_again.id), None, Some([0.99, 0.05])),
            (a_slot, Some(2.0), Some([1.0, 0.0])),
        ]);
        let replaced = photo_ref(AssetId::new());
        let used = std::collections::HashSet::from([a, replaced.asset]);
        let hits = vec![beach_again.clone(), a20.clone(), a30.clone()];
        let pick = choose_swap(&hits, &used, &[a_slot], replaced, &sig, |_| true).unwrap();
        assert_eq!(
            (pick.id, pick.frame_ts),
            (a, Some(20.0)),
            "the forest, not beach again"
        );

        // Sharp before soft inside the distinct-moment tier, and still
        // ahead of the sharp fresh repeat.
        let pick = choose_swap(&hits, &used, &[a_slot], replaced, &sig, |h| {
            h.frame_ts != Some(20.0)
        })
        .unwrap();
        assert_eq!(pick.frame_ts, Some(30.0));

        // Swapping out A@20s itself: that footage is not a new moment, so
        // the fresh repeat wins over handing the same shot back.
        let replaced = reel::MomentRef {
            asset: a,
            at: Some(20.0),
        };
        let pick = choose_swap(&hits[..2], &used, &[a_slot], replaced, &sig, |_| true).unwrap();
        assert_eq!(pick.id, beach_again.id);
    }

    /// Swapping B in [A, B] when A is the only positive hit yields A twice,
    /// and the refreshed annotation says so instead of reporting nothing.
    #[test]
    fn same_asset_scarcity_is_annotated_not_silent() {
        let mut p = four();
        p.slots.truncate(2);
        let a = p.slots[0].clone();
        let mut hit_a = result(0.5);
        hit_a.id = a.asset;
        let used: std::collections::HashSet<_> = p.slots.iter().map(|s| s.asset).collect();
        let others = [reel::MomentRef::of_slot(&a)];
        let replaced = reel::MomentRef::of_slot(&p.slots[1]);
        let sig = reel::Signals::default();
        let pick = choose_swap(
            std::slice::from_ref(&hit_a),
            &used,
            &others,
            replaced,
            &sig,
            |_| true,
        )
        .expect("a repeat beats giving up");
        assert_eq!(pick.id, a.asset);

        p.slots[1] = Slot {
            transition_in: p.slots[1].transition_in,
            ..a.clone()
        };
        reel::annotate(&mut p.slots, &sig);
        assert_eq!(reel::similar_to(&p.slots, 0), Some(vec![2]));
        assert_eq!(reel::similar_to(&p.slots, 1), Some(vec![1]));
    }

    #[test]
    fn voiceover_prefers_a_distinct_scene_and_repeats_only_when_it_must() {
        let placed_beach = result(0.5);
        let beach_again = result(0.45);
        let forest = result(0.3);
        let sig = with_embeddings(&[
            (photo_ref(placed_beach.id), None, Some([1.0, 0.0])),
            (photo_ref(beach_again.id), None, Some([0.99, 0.05])),
            (photo_ref(forest.id), None, Some([0.0, 1.0])),
        ]);
        let placed = [photo_ref(placed_beach.id)];
        let hits = vec![beach_again.clone(), forest.clone()];
        let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
        assert_eq!(pick.id, forest.id, "another file of the beach loses");

        // Scarcity: only the repeat matches this span, so it is used.
        let hits = vec![beach_again.clone()];
        let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
        assert_eq!(pick.id, beach_again.id);
        // Never the previous shot while anything else exists.
        let hits = vec![placed_beach.clone(), beach_again.clone()];
        let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
        assert_eq!(pick.id, beach_again.id);
    }

    /// Diversity never abandons the narration query: a relevant repeat of
    /// the beach beats a distinct but unrelated shot that scores zero,
    /// negative, or below the weak-tail floor (35% of the span's best).
    #[test]
    fn voiceover_keeps_a_relevant_repeat_over_unrelated_alternatives() {
        let placed_beach = result(0.5);
        let placed = [photo_ref(placed_beach.id)];
        for unrelated_score in [0.0, -0.1, 0.1] {
            let beach_again = result(0.45);
            let unrelated = result(unrelated_score);
            let sig = with_embeddings(&[
                (photo_ref(placed_beach.id), None, Some([1.0, 0.0])),
                (photo_ref(beach_again.id), None, Some([0.99, 0.05])),
                (photo_ref(unrelated.id), None, Some([0.0, 1.0])),
            ]);
            let hits = vec![beach_again.clone(), unrelated.clone()];
            let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
            assert_eq!(
                pick.id, beach_again.id,
                "a {unrelated_score} alternative must not beat the relevant repeat"
            );

            // Even the previous shot itself beats an ineligible alternative.
            let hits = vec![placed_beach.clone(), unrelated.clone()];
            let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
            assert_eq!(pick.id, placed_beach.id, "{unrelated_score}");
        }

        // A distinct shot inside the floor still wins over the repeat.
        let beach_again = result(0.45);
        let forest = result(0.2);
        let sig = with_embeddings(&[
            (photo_ref(placed_beach.id), None, Some([1.0, 0.0])),
            (photo_ref(beach_again.id), None, Some([0.99, 0.05])),
            (photo_ref(forest.id), None, Some([0.0, 1.0])),
        ]);
        let hits = vec![beach_again, forest.clone()];
        let pick = choose_voiceover(&hits, Some(placed_beach.id), &placed, &sig).unwrap();
        assert_eq!(pick.id, forest.id);
    }

    #[test]
    fn rejects_non_project_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.json");
        std::fs::write(&path, b"{\"hello\": 1}").unwrap();
        let err = ReelProject::load(&path).unwrap_err().to_string();
        assert!(err.contains("not a reel project"), "{err}");
    }

    #[test]
    fn validate_accepts_planned_and_legacy_projects() {
        project("ok", Path::new("/tmp/x.mp4")).validate().unwrap();
        // The v0.8 shape: no optional field present at all.
        let legacy: ReelProject = serde_json::from_value(serde_json::json!({
            "version": 1,
            "created": "2026-01-01T00:00:00Z",
            "modified": "2026-01-01T00:00:00Z",
            "prompt": "old",
            "width": 1920,
            "height": 1080,
            "frame": "auto",
            "output": "/tmp/old.mp4",
            "slots": [{
                "asset": AssetId::new().to_string(),
                "source": "/lib/a.jpg",
                "kind": "photo",
                "duration": 2.0,
            }],
        }))
        .unwrap();
        legacy.validate().unwrap();
    }

    #[test]
    fn validate_names_every_out_of_range_number() {
        let base = || project("x", Path::new("/tmp/x.mp4"));
        type Breaker = Box<dyn Fn(&mut ReelProject)>;
        let cases: Vec<(&str, Breaker)> = vec![
            ("has no slots", Box::new(|p| p.slots.clear())),
            (
                "2001 slots; at most 2000",
                Box::new(|p| p.slots = (0..2001).map(|_| slot()).collect()),
            ),
            ("0x1920 is outside", Box::new(|p| p.width = 0)),
            ("1080x9000 is outside", Box::new(|p| p.height = 9000)),
            (
                "2x2 is outside",
                Box::new(|p| {
                    p.width = 2;
                    p.height = 2;
                }),
            ),
            ("invalid frame style", Box::new(|p| p.frame = "zoom".into())),
            (
                "slot 1 has an unusable duration (0)",
                Box::new(|p| p.slots[0].duration = 0.0),
            ),
            (
                "slot 1 has an unusable duration (-1)",
                Box::new(|p| p.slots[0].duration = -1.0),
            ),
            (
                "slot 1 has an unusable duration (NaN)",
                Box::new(|p| p.slots[0].duration = f64::NAN),
            ),
            // Finite and positive, so only the total refuses it.
            ("the reel runs", Box::new(|p| p.slots[0].duration = 1e308)),
            (
                "the reel runs inf",
                Box::new(|p| {
                    p.slots[0].duration = 1e308;
                    p.slots.push(Slot {
                        duration: 1e308,
                        ..slot()
                    });
                }),
            ),
            (
                "the reel runs 3601s",
                Box::new(|p| p.slots[0].duration = 3601.0),
            ),
            (
                "slot 1 has an unusable start (-1)",
                Box::new(|p| p.slots[0].start = -1.0),
            ),
            (
                "slot 1's transition lasts 99s",
                Box::new(|p| p.slots[0].transition_in = Transition::Whip { secs: 99.0 }),
            ),
            (
                "slot 1's transition lasts NaNs",
                Box::new(|p| p.slots[0].transition_in = Transition::Crossfade { secs: f64::NAN }),
            ),
        ];
        for (want, break_it) in cases {
            let mut p = base();
            break_it(&mut p);
            let err = format!("{:#}", p.validate().unwrap_err());
            assert!(err.contains(want), "want {want:?}: {err}");
        }
    }

    #[test]
    fn validate_accepts_the_longest_joint_an_edit_can_set() {
        let mut p = project("x", Path::new("/tmp/x.mp4"));
        let longest = format!("crossfade:{}", reel::MAX_TRANSITION_SECS);
        p.slots[0].transition_in = Transition::parse(&longest).unwrap();
        p.validate().unwrap();
        p.slots[0].transition_in = Transition::Crossfade {
            secs: reel::MAX_TRANSITION_SECS + 0.01,
        };
        let err = format!("{:#}", p.validate().unwrap_err());
        assert!(err.contains("slot 1's transition lasts"), "{err}");
    }

    #[tokio::test]
    async fn load_untrusted_refuses_what_it_cannot_read_boundedly() {
        let dir = tempfile::tempdir().unwrap();
        let err_of = |r: Result<ReelProject>| format!("{:#}", r.unwrap_err());

        let err = err_of(ReelProject::load_untrusted(dir.path()).await);
        assert!(err.contains("not a regular file"), "{err}");

        let huge = dir.path().join("huge.eidetic.json");
        std::fs::write(&huge, vec![b' '; 9 << 20]).unwrap();
        let err = err_of(ReelProject::load_untrusted(&huge).await);
        assert!(err.contains("larger than"), "{err}");

        let junk = dir.path().join("junk.eidetic.json");
        std::fs::write(&junk, b"{\"hello\": 1}").unwrap();
        let err = err_of(ReelProject::load_untrusted(&junk).await);
        assert!(err.contains("not a reel project file"), "{err}");

        let good = dir.path().join("good.eidetic.json");
        project("keep me", Path::new("/tmp/x.mp4"))
            .save(&good)
            .unwrap();
        let p = ReelProject::load_untrusted(&good).await.unwrap();
        assert_eq!(p.prompt, "keep me");

        let mut bad = project("bad", Path::new("/tmp/x.mp4"));
        bad.width = 0;
        bad.save(&good).unwrap();
        let err = err_of(ReelProject::load_untrusted(&good).await);
        assert!(
            err.contains("is out of range: project frame 0x1920"),
            "{err}"
        );
        assert!(err.ends_with(REPAIR_HINT), "{err}");

        // render replaces the output, so only an .mp4 one is accepted.
        project("not a video", Path::new("/tmp/notes.txt"))
            .save(&good)
            .unwrap();
        let err = err_of(ReelProject::load_untrusted(&good).await);
        assert!(
            err.contains("cannot be rendered through MCP: output must be an .mp4 path"),
            "{err}"
        );
    }

    // -- output ownership ----------------------------------------------------

    #[tokio::test]
    async fn new_projects_carry_an_id_and_only_the_cli_fills_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reel.eidetic.json");
        let p = project("x", &dir.path().join("reel.mp4"));
        let id = p.id.expect("a planned project has an id");
        p.save(&path).unwrap();
        assert_eq!(ReelProject::load(&path).unwrap().id, Some(id));
        assert_eq!(
            ReelProject::load_untrusted(&path).await.unwrap().id,
            Some(id)
        );
        assert_eq!(p.require_id(&path, "render").unwrap(), id);

        ReelProject {
            id: None,
            ..project("legacy", &dir.path().join("reel.mp4"))
        }
        .save(&path)
        .unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            raw.get("id").is_none(),
            "an absent id is not written as null"
        );
        assert!(ReelProject::load(&path).unwrap().id.is_some());
        let legacy = ReelProject::load_untrusted(&path).await.unwrap();
        assert_eq!(legacy.id, None);
        let err = format!("{:#}", legacy.require_id(&path, "render").unwrap_err());
        assert!(err.contains("nothing was written"), "{err}");
        assert!(err.contains("edit_reel"), "{err}");
        assert!(err.contains(r#""ops":[]"#), "{err}");
        assert!(err.contains("call render again"), "{err}");
    }

    /// A real (symlink-free) scratch root: a temp dir under a symlinked
    /// `/tmp` would be refused as a link, as it should.
    fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    fn refusal(dest: &Path, lib: &Path, claim: Claim<'_>) -> String {
        format!("{:#}", check_destination(dest, lib, claim).unwrap_err())
    }

    const NEW: Claim<'static> = Claim::New {
        remedy: "pick another",
    };

    #[test]
    fn free_paths_outside_the_library_are_writable() {
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        // A library that does not exist yet contains nothing.
        check_destination(&root.join("reel.mp4"), &lib, NEW).unwrap();
        std::fs::create_dir(&lib).unwrap();
        check_destination(&root.join("reel.mp4"), &lib, NEW).unwrap();
        // A sibling whose name merely starts like the library is outside it.
        std::fs::create_dir(root.join("library2")).unwrap();
        check_destination(&root.join("library2").join("reel.mp4"), &lib, NEW).unwrap();
    }

    #[test]
    fn dotdot_missing_parents_and_non_files_are_refused() {
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        std::fs::create_dir(root.join("a")).unwrap();
        let err = refusal(&root.join("a").join("..").join("reel.mp4"), &lib, NEW);
        assert!(err.contains("contains `..`"), "{err}");
        let err = refusal(&root.join("missing").join("reel.mp4"), &lib, NEW);
        assert!(err.contains("does not exist"), "{err}");

        let id = ProjectId::generate();
        let render = Claim::Render {
            id,
            project: Path::new("/p.eidetic.json"),
            preview: false,
        };
        let err = refusal(&root.join("a"), &lib, render);
        assert!(err.contains("is not a regular file"), "{err}");
        #[cfg(unix)]
        {
            let fifo = root.join("fifo.mp4");
            if std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .is_ok_and(|s| s.success())
            {
                let err = refusal(&fifo, &lib, render);
                assert!(err.contains("is not a regular file"), "{err}");
            }
        }

        std::fs::write(root.join("taken.mp4"), b"x").unwrap();
        let err = refusal(&root.join("taken.mp4"), &lib, NEW);
        assert!(err.ends_with("already exists; pick another"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_at_or_above_the_destination_are_refused() {
        use std::os::unix::fs::symlink;
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, root.join("via")).unwrap();
        symlink(root.join("gone"), root.join("dangling.mp4")).unwrap();
        let err = refusal(&root.join("via").join("reel.mp4"), &lib, NEW);
        assert!(
            err.contains(&format!(
                "{} is a symbolic link",
                root.join("via").display()
            )),
            "{err}"
        );
        assert!(
            err.contains(&format!("resolves to {}", real.display())),
            "{err}"
        );
        let err = refusal(&root.join("dangling.mp4"), &lib, NEW);
        assert!(err.contains("is a symbolic link"), "{err}");
        assert!(!root.join("gone").exists());
    }

    #[test]
    fn nothing_new_goes_into_the_library_even_through_a_link_to_it() {
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        std::fs::create_dir_all(lib.join("ab")).unwrap();
        for dest in [lib.join("reel.mp4"), lib.join("ab").join("reel.otio")] {
            let err = refusal(&dest, &lib, NEW);
            assert!(err.contains("inside the library directory"), "{err}");
        }
        #[cfg(unix)]
        {
            let link = root.join("library-link");
            std::os::unix::fs::symlink(&lib, &link).unwrap();
            let err = refusal(&lib.join("reel.mp4"), &link, NEW);
            assert!(err.contains("inside the library directory"), "{err}");
        }
    }

    #[test]
    fn an_existing_otio_is_replaceable_only_with_this_projects_id() {
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        let path = root.join("reel.otio");
        let p = project("x", &root.join("reel.mp4"));
        let id = p.id.unwrap();
        let claim = Claim::Otio { id, project: &root };
        std::fs::write(&path, crate::otio::timeline(&p).unwrap()).unwrap();
        check_destination(&path, &lib, claim).unwrap();

        let other = project("y", &root.join("reel.mp4"));
        std::fs::write(&path, crate::otio::timeline(&other).unwrap()).unwrap();
        let err = refusal(&path, &lib, claim);
        assert!(
            err.contains("was not written by Eidetic for this project"),
            "{err}"
        );
        assert!(err.contains("--export otio"), "{err}");
        std::fs::write(&path, b"not json").unwrap();
        refusal(&path, &lib, claim);
    }

    #[test]
    fn a_project_file_is_saved_back_only_while_it_is_the_one_loaded() {
        let (_guard, root) = real_tempdir();
        let lib = root.join("library");
        let path = root.join("reel.eidetic.json");
        let save = |p: &ReelProject| p.save(&path).unwrap();
        let load = || serde_json::to_value(ReelProject::load_bounded(&path).unwrap()).unwrap();
        let output = root.join("reel.mp4");

        let mut p = project("x", &output);
        let mut legacy = project("old", &output);
        legacy.id = None;
        let mut other_legacy = project("someone else's", &output);
        other_legacy.id = None;
        for (loaded, replacement) in [(&p, &project("y", &output)), (&legacy, &other_legacy)] {
            save(loaded);
            let as_loaded = load();
            let claim = Claim::ProjectFile { loaded: &as_loaded };
            check_destination(&path, &lib, claim).unwrap();
            save(replacement);
            let err = refusal(&path, &lib, claim);
            assert!(
                err.contains("no longer the project this call loaded"),
                "{err}"
            );
        }

        // The same project, saved meanwhile by someone else.
        save(&p);
        let as_loaded = load();
        p.prompt = "edited elsewhere".into();
        save(&p);
        let err = refusal(&path, &lib, Claim::ProjectFile { loaded: &as_loaded });
        assert!(err.contains("no longer the project"), "{err}");
    }
}
