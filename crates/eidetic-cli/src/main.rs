mod dupes;
mod eval;
mod otio;
mod project;
mod reel;

use anyhow::Context;
use clap::{Parser, Subcommand};
use eidetic_core::Config;
use eidetic_ingest::ImportOutcome;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "eidetic",
    version,
    about = "Personal media intelligence engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// One value of this enum exists per process, parsed once; the size skew
// clippy flags (Reel's subcommand + flags) costs nothing worth boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Compute the SHA-256 hash of a file.
    Hash {
        /// Path to the file.
        path: PathBuf,
    },
    /// Import a file or directory into the library.
    Import {
        /// File or directory to import.
        path: PathBuf,
    },
    /// Show library statistics.
    Stats,
    /// Generate embeddings for all imported images that don't have one yet.
    Embed,
    /// Generate missing thumbnails for previously-imported images.
    Thumbnail,
    /// Detect and group faces in imported images.
    ///
    /// Runs detection over images not yet scanned, then clusters: each new face
    /// joins an existing person if it is close enough, and the remaining
    /// unassigned faces are grouped into new candidate people.
    Faces {
        /// Skip detection and only re-run clustering over stored faces.
        #[arg(long)]
        cluster_only: bool,
    },
    /// List grouped people and their face counts.
    Persons,
    /// Give a person a name, so their faces stop being an unnamed candidate.
    NamePerson {
        /// Person UUID, as shown by `eidetic persons`.
        id: String,
        /// The name to assign.
        name: String,
    },
    /// Start the HTTP server (localhost-bound).
    Serve {
        /// Address to bind. Defaults to 127.0.0.1:8080.
        /// Override via --bind or EIDETIC_BIND env var.
        #[arg(long)]
        bind: Option<String>,
    },
    /// Run COCO 5K (Karpathy) text-to-image retrieval eval. Bypasses the
    /// library entirely; embeds images and captions in-memory.
    Eval {
        /// Path to the COCO captions CSV (e.g. test_5k_mscoco_2014.csv).
        #[arg(long)]
        coco_csv: PathBuf,
        /// Directory containing the COCO images named per the CSV.
        #[arg(long)]
        coco_images: PathBuf,
        /// Limit to first N images for fast iteration. Omit for full 5K.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Search the library by natural-language description.
    Search {
        /// The text query, e.g. "dog on beach".
        query: String,

        /// Maximum number of results to return.
        #[arg(long, default_value = "10")]
        limit: u32,

        /// Comma-separated fields to include in output.
        /// Valid: path, score, date, make, model, lat, lon, mime.
        /// Defaults to path-only when omitted.
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,

        /// Output results as a JSON array with all fields.
        #[arg(long)]
        json: bool,

        /// Only assets containing this (named) person.
        #[arg(long)]
        person: Option<String>,

        /// Only assets taken on/after this date (YYYY-MM-DD).
        #[arg(long)]
        after: Option<String>,

        /// Only assets taken before this date (YYYY-MM-DD, exclusive).
        #[arg(long)]
        before: Option<String>,

        /// Only assets whose place/state/country matches this substring.
        #[arg(long)]
        place: Option<String>,

        /// Only this kind of asset: image or video.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Cut a short reel from the library for a prompt (needs ffmpeg).
    /// Every render writes a project file next to the output; `reel edit`
    /// mutates it.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Reel {
        #[command(subcommand)]
        action: Option<ReelAction>,

        /// What the reel is about, e.g. "sunset at the beach".
        #[arg(required = true)]
        prompt: Option<String>,

        /// Target length in seconds.
        #[arg(long, default_value = "30")]
        duration: f64,

        /// Output file. Refuses to overwrite an existing file.
        #[arg(long, short, default_value = "reel.mp4")]
        output: PathBuf,

        /// Output frame size, WIDTHxHEIGHT.
        #[arg(long, default_value = "1920x1080")]
        size: String,

        /// Music track to cut to: beats are detected and cuts land on them;
        /// the audio is laid under the reel with a fade-out.
        #[arg(long)]
        audio: Option<PathBuf>,

        /// Vertical 1080x1920 — the Instagram/Shorts format. Overrides
        /// --size. Framing defaults to auto: see --frame.
        #[arg(long)]
        portrait: bool,

        /// Framing style: auto (faces → follow them with a cover-crop;
        /// scenic shots → fit over a blurred backdrop), cover, blur, pad.
        #[arg(long, default_value = "auto")]
        frame: String,

        /// Also export the timeline for an NLE: `--export otio` writes
        /// <output>.otio (opens in Kdenlive 25.04+ / Resolve 18.5+).
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,

        /// Blur floor (Laplacian variance at 640px): candidates whose
        /// matched frame or photo scores below this never enter the reel.
        /// 0 disables the gate.
        #[arg(long, default_value_t = reel::MIN_SHARPNESS)]
        min_sharpness: f64,

        /// Cut N candidate reels instead of one — different shot pools and
        /// transition styles, rendered as half-resolution previews
        /// (reel.v1.mp4 …). Pick with your eyes, then re-render the winner
        /// full-res: `eidetic reel edit reel.v2.eidetic.json`.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=9))]
        variations: u32,

        /// Print the cut list without rendering.
        #[arg(long)]
        dry_run: bool,
    },
    /// Generate browser-playable copies of videos whose codec or container
    /// a browser can't stream (needs ffmpeg). Originals are never modified.
    Transcode,
    /// Transcribe speech in videos so search can find spoken words
    /// (needs a build with --features speech, plus ffmpeg).
    Transcribe,
    /// Report perceptual duplicates: re-exported photos and re-encoded or
    /// truncated copies of videos. Reporting only — nothing is deleted.
    Dupes {
        /// Similarity floor for photos (videos use a slightly looser one).
        #[arg(long, default_value_t = dupes::PHOTO_THRESHOLD)]
        threshold: f32,
    },
    /// Remove assets from the library: the database rows, the stored
    /// original, thumbnails, playback copies and face crops. Without
    /// --force this only prints what would be removed.
    Rm {
        /// Asset references: id, content hash, storage path, or an
        /// unambiguous original filename.
        refs: Vec<String>,

        /// Actually delete. The default is a dry run.
        #[arg(long)]
        force: bool,
    },
    /// Verify library integrity: re-hash every stored original against the
    /// database and report corruption, missing files and orphans.
    Verify,
}

#[derive(Subcommand)]
enum ReelAction {
    /// Edit a reel project file and re-render it. All slot numbers refer
    /// to the cut list as last printed (1-based); when several ops are
    /// combined in one call they are resolved against that same numbering,
    /// then applied as pin/unpin → retime → swap → drop → reorder.
    Edit {
        /// The project file a previous render wrote (reel.eidetic.json).
        project: PathBuf,

        /// Replace slot N with the best library hit for a fresh query:
        /// --swap N "the beach clip". Repeatable. Keeps the slot's length,
        /// so beat sync survives. Refuses pinned slots.
        #[arg(long, num_args = 2, value_names = ["SLOT", "QUERY"])]
        swap: Vec<String>,

        /// Remove slot N (later cuts move earlier). Repeatable. Refuses
        /// pinned slots.
        #[arg(long)]
        drop: Vec<usize>,

        /// Pin slot N: --swap and --drop refuse to touch it. Repeatable.
        #[arg(long)]
        pin: Vec<usize>,

        /// Unpin slot N. Repeatable.
        #[arg(long)]
        unpin: Vec<usize>,

        /// Set slot N's length in seconds: --retime N 2.5. Repeatable.
        /// Video slots are clamped to the media past their in-point.
        #[arg(long, num_args = 2, value_names = ["SLOT", "SECS"])]
        retime: Vec<String>,

        /// Set the transition INTO slot N: --transition N crossfade.
        /// Styles: cut, crossfade, dip, slide, whip — optionally with
        /// :secs (e.g. crossfade:0.8). Repeatable. Slot 1 has no joint.
        #[arg(long, num_args = 2, value_names = ["SLOT", "STYLE"])]
        transition: Vec<String>,

        /// Reorder the (surviving) slots: a comma-separated permutation
        /// using the printed numbers, e.g. --reorder 3,1,2.
        #[arg(long)]
        reorder: Option<String>,

        /// Render target. Defaults to overwriting the project's own
        /// output; any other existing path is refused.
        #[arg(long, short)]
        output: Option<PathBuf>,

        /// Also export the timeline for an NLE: `--export otio` writes
        /// <output>.otio (opens in Kdenlive 25.04+ / Resolve 18.5+).
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,

        /// Apply and save the edits and print the new cut list, but skip
        /// the render.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Validate --export and hand back the target path for a render output.
fn export_path(format: Option<&str>, output: &std::path::Path) -> anyhow::Result<Option<PathBuf>> {
    match format {
        None => Ok(None),
        Some("otio") => Ok(Some(output.with_extension("otio"))),
        Some(other) => anyhow::bail!("unknown --export format {other:?}; supported: otio"),
    }
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    const KB: u64 = 1 << 10;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// Returns None on failure so a flaky network or busted cache never blocks an
// import; the user sees a stderr warning and place columns stay NULL.
fn open_geocoder() -> Option<eidetic_core::geocoder::Geocoder> {
    use eidetic_core::geocoder::Geocoder;
    let dir = Geocoder::default_data_dir();
    if let Err(e) = Geocoder::ensure_dataset(&dir) {
        eprintln!("warning: GeoNames dataset unavailable ({e}); skipping reverse geocoding");
        return None;
    }
    match Geocoder::open(&dir) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!(
                "warning: GeoNames dataset present but failed to load ({e}); skipping reverse geocoding"
            );
            None
        }
    }
}

const VALID_FIELDS: &[&str] = &[
    "path", "score", "date", "make", "model", "lat", "lon", "mime", "ts", "speech",
];

fn format_result(r: &eidetic_db::SearchResult, fields: &[String]) -> String {
    let mut parts = Vec::new();
    for field in fields {
        let value = match field.as_str() {
            "path" => r.storage_path.display().to_string(),
            "score" => format!("{:.3}", r.score),
            "date" => r
                .date_taken
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            "make" => r.camera_make.clone().unwrap_or_default(),
            "model" => r.camera_model.clone().unwrap_or_default(),
            "lat" => r.latitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "lon" => r.longitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "mime" => r.mime_type.clone().unwrap_or_default(),
            // For videos: the second offset of the best-matching moment.
            "ts" => r.frame_ts.map(|ts| format!("{ts:.1}")).unwrap_or_default(),
            "speech" => r.speech.clone().unwrap_or_default(),
            other => unreachable!(
                "unknown field {other:?} should have been rejected by VALID_FIELDS check"
            ),
        };
        parts.push(value);
    }
    parts.join("\t")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // EIDETIC_LOG → RUST_LOG → default. Fall through silently on parse errors;
    // there's no logger yet to warn through.
    let filter = std::env::var("EIDETIC_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|s| EnvFilter::try_new(&s).ok())
        // `ort=error` rather than `ort=warn`: ONNX Runtime emits a warning per
        // graph initializer for some exports (SFace produces eight on every
        // load) and CoreML logs its graph partitioning. Neither is actionable
        // by the user. Real ort failures are logged at error and still show.
        .unwrap_or_else(|| EnvFilter::new("info,ort=error"));
    // Logs go to stderr: stdout is the data channel (search --json, --fields,
    // reel cut lists) and must stay pipeable into jq/awk without EIDETIC_LOG
    // gymnastics.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Hash { path } => {
            let hash = eidetic_ingest::hash_file(&path)
                .with_context(|| format!("failed to hash {}", path.display()))?;
            println!("{hash}  {}", path.display());
        }

        Command::Import { path } => {
            if !path.is_file() && !path.is_dir() {
                anyhow::bail!("{} is not a file or directory", path.display());
            }

            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let geocoder = open_geocoder();

            if path.is_file() {
                match eidetic_ingest::import_file(&path, &repo, &config.paths, geocoder.as_ref())
                    .await
                {
                    ImportOutcome::Imported(id) => {
                        println!("Imported  {} ({})", path.display(), id);
                    }
                    ImportOutcome::Duplicate(id) => {
                        println!(
                            "Duplicate {} (already in library as {})",
                            path.display(),
                            id
                        );
                    }
                    ImportOutcome::Skipped => {
                        println!("Skipped   {} (not a media file)", path.display());
                    }
                    ImportOutcome::Failed(e) => {
                        eprintln!("Failed    {}: {e}", path.display());
                        std::process::exit(1);
                    }
                }
            } else if path.is_dir() {
                let summary =
                    eidetic_ingest::import_dir(&path, &repo, &config.paths, geocoder.as_ref())
                        .await
                        .with_context(|| format!("cannot import {}", path.display()))?;

                println!("Imported   {:>6} files", summary.imported);
                println!("Duplicates {:>6} files", summary.duplicates);
                println!("Skipped    {:>6} files", summary.skipped);
                println!("Failed     {:>6} files", summary.failed.len());
                for (p, e) in &summary.failed {
                    eprintln!("  {}: {e}", p.display());
                }

                if !summary.failed.is_empty() {
                    std::process::exit(1);
                }
            }
        }

        Command::Stats => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);
            let s = repo.fetch_stats().await.context("failed to fetch stats")?;

            println!("Assets     {:>8}", s.total);
            println!("  images   {:>8}", s.images);
            println!("  videos   {:>8}", s.videos);
            println!("Embedded   {:>8}", s.embedded);
            println!("Not yet    {:>8}", s.needs_embed);
            println!("Thumbs pending {:>4}", s.thumbnails_pending);
            println!("Size       {:>8}", format_bytes(s.total_bytes as u64));
            if let Some(earliest) = s.earliest {
                println!("Earliest   {}", earliest.format("%Y-%m-%d"));
            }
            if let Some(latest) = s.latest {
                println!("Latest     {}", latest.format("%Y-%m-%d"));
            }
        }

        Command::Eval {
            coco_csv,
            coco_images,
            limit,
        } => {
            tokio::task::spawn_blocking(move || eval::run(&coco_csv, &coco_images, limit))
                .await
                .context("eval thread panicked")??;
        }

        Command::Embed => {
            let config = Config::from_env();

            let models_dir = config.paths.models_cache.clone();

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let unembedded = repo
                .fetch_unembedded()
                .await
                .context("failed to fetch unembedded assets")?;
            let videos_unembedded = repo
                .fetch_videos_unembedded()
                .await
                .context("failed to fetch unembedded videos")?;

            // Videos need ffmpeg for frame extraction AND ffprobe for
            // durations (ADR-0011) — sampling without a duration would store
            // a lone t=0 frame and never revisit. Without either binary they
            // are skipped loudly, not failed.
            let ffmpeg_ok = eidetic_ingest::video::ffmpeg().is_some()
                && eidetic_ingest::video::ffprobe().is_some();
            if !videos_unembedded.is_empty() && !ffmpeg_ok {
                eprintln!(
                    "Skipping {} video(s): ffmpeg/ffprobe not found. Install ffmpeg (or set \
                     EIDETIC_FFMPEG_PATH / EIDETIC_FFPROBE_PATH) and re-run `eidetic embed`.",
                    videos_unembedded.len()
                );
            }
            let videos_unembedded = if ffmpeg_ok {
                videos_unembedded
            } else {
                Vec::new()
            };

            if unembedded.is_empty() && videos_unembedded.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            println!("Loading model (downloads ~1.4 GiB on first run)…");

            // Channel of (path, reply) jobs sent to the worker thread. The
            // worker owns the embedder; the async loop sends paths and awaits
            // results via per-job oneshot replies. Channel capacity 1 keeps
            // the worker tightly coupled to the async loop, so no large queue
            // of pending embeds builds up if the DB write side stalls.
            /// (ts, embedding) pairs for a video's sampled frames, or a
            /// display-ready reason the whole video was skipped.
            type FrameReply = oneshot::Sender<Result<Vec<(f64, Vec<f32>)>, String>>;
            enum EmbedJob {
                Image(PathBuf, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>),
                // Sampled timestamps in, (ts, embedding) pairs out. Frame
                // extraction runs on the worker too — it is subprocess-bound,
                // and interleaving it with inference keeps one job in flight.
                Video(PathBuf, Vec<f64>, FrameReply),
            }
            let (job_tx, mut job_rx) = mpsc::channel::<EmbedJob>(1);

            let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                while let Some(job) = job_rx.blocking_recv() {
                    // If a receiver was dropped (e.g. caller gave up), keep
                    // serving the next job rather than aborting the worker.
                    match job {
                        EmbedJob::Image(path, reply) => {
                            let _ = reply.send(embedder.embed(&path));
                        }
                        EmbedJob::Video(path, timestamps, reply) => {
                            // Frames that embedded stay embedded: a container
                            // whose tail is unreadable (truncated file, audio
                            // outlasting video) still contributes its good
                            // frames. Only a video with NO usable frame is
                            // reported as an error, and retried next run.
                            let mut frames = Vec::with_capacity(timestamps.len());
                            let mut first_err = None;
                            for ts in timestamps {
                                let result = eidetic_ingest::video::extract_frame(&path, ts)
                                    .map_err(|e| e.to_string())
                                    .and_then(|f| {
                                        f.ok_or_else(|| "ffmpeg disappeared mid-run".to_string())
                                    })
                                    .and_then(|frame| {
                                        embedder.embed_image(&frame).map_err(|e| e.to_string())
                                    });
                                match result {
                                    Ok(v) => frames.push((ts, v)),
                                    Err(e) => first_err = first_err.or(Some(e)),
                                }
                            }
                            let _ = reply.send(match (frames.is_empty(), first_err) {
                                (true, Some(e)) => Err(e),
                                (_, _) => Ok(frames),
                            });
                        }
                    }
                }
                Ok(())
            });

            let total = unembedded.len() + videos_unembedded.len();
            println!(
                "Found {} image(s) and {} video(s) to embed",
                unembedded.len(),
                videos_unembedded.len()
            );

            let mut embedded = 0u32;
            let mut skipped = 0u32;
            let mut failed = 0u32;

            for (i, (id, path)) in unembedded.into_iter().enumerate() {
                let (reply_tx, reply_rx) = oneshot::channel();
                // If `send` fails, the worker died. Surface its error below.
                if job_tx
                    .send(EmbedJob::Image(path.clone(), reply_tx))
                    .await
                    .is_err()
                {
                    break;
                }
                let result = reply_rx.await.context(
                    "embed worker panicked or died mid-job; check for OOM or ONNX error above",
                )?;

                match result {
                    Ok(emb) => match repo.store_embedding(id, &emb).await {
                        Ok(()) => {
                            println!("[{}/{}] {}", i + 1, total, path.display());
                            embedded += 1;
                        }
                        Err(e) => {
                            // Keep going. Rows with NULL embedding are retried on the next run.
                            eprintln!("  failed to store {}: {e}", path.display());
                            failed += 1;
                        }
                    },
                    Err(e) => {
                        eprintln!("  skipped {}: {e}", path.display());
                        skipped += 1;
                    }
                }
            }

            let images_attempted = total - videos_unembedded.len();
            for (i, (id, path, duration)) in videos_unembedded.into_iter().enumerate() {
                // A video imported without ffprobe has no stored duration;
                // re-probe now and backfill the metadata while we are here.
                let duration = match duration {
                    Some(d) => Some(d),
                    None => {
                        let probe_path = path.clone();
                        let probed = tokio::task::spawn_blocking(move || {
                            eidetic_ingest::video::probe(&probe_path)
                        })
                        .await
                        .context("probe thread panicked")?;
                        match probed {
                            Ok(Some(p)) => {
                                repo.update_video_probe(
                                    id,
                                    p.duration_secs,
                                    p.video_codec.as_deref(),
                                    p.width,
                                    p.height,
                                )
                                .await
                                .context("failed to store probe metadata")?;
                                p.duration_secs
                            }
                            Ok(None) => None,
                            Err(e) => {
                                eprintln!("  skipped {}: {e}", path.display());
                                skipped += 1;
                                continue;
                            }
                        }
                    }
                };

                let timestamps = scene_timestamps(&repo, id, &path, duration).await?;
                let (reply_tx, reply_rx) = oneshot::channel();
                if job_tx
                    .send(EmbedJob::Video(path.clone(), timestamps, reply_tx))
                    .await
                    .is_err()
                {
                    break;
                }
                let result = reply_rx.await.context(
                    "embed worker panicked or died mid-job; check for OOM or ONNX error above",
                )?;

                match result {
                    Ok(frames) => match repo.store_frame_embeddings(id, &frames).await {
                        Ok(()) => {
                            println!(
                                "[{}/{}] {} ({} frame(s))",
                                images_attempted + i + 1,
                                total,
                                path.display(),
                                frames.len()
                            );
                            embedded += 1;
                        }
                        Err(e) => {
                            eprintln!("  failed to store {}: {e}", path.display());
                            failed += 1;
                        }
                    },
                    Err(e) => {
                        eprintln!("  skipped {}: {e}", path.display());
                        skipped += 1;
                    }
                }
            }

            // Drop the sender so the worker's `blocking_recv` returns `None`
            // and the worker exits. Then await the worker's result so a load
            // or panic during embed surfaces here instead of being lost.
            drop(job_tx);
            worker
                .await
                .context("embed worker thread panicked")?
                .context("failed to load SigLIP 2 model; check your internet connection and that ~/.cache/eidetic/models is intact")?;

            if failed > 0 {
                println!(
                    "Done. Embedded {embedded}, skipped {skipped}, failed {failed}. \
                     Re-run `eidetic embed` to retry the failed rows."
                );
                std::process::exit(1);
            } else {
                println!("Done. Embedded {embedded}, skipped {skipped}.");
            }
        }

        Command::Thumbnail => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let pending = repo
                .fetch_unthumbnailed()
                .await
                .context("failed to fetch unthumbnailed assets")?;

            if pending.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            // Without ffmpeg, videos cannot thumbnail — leave them pending
            // (with one warning) rather than counting them as failures.
            let ffmpeg_ok = eidetic_ingest::video::ffmpeg().is_some();
            let (pending, held_back): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .partition(|a| ffmpeg_ok || !a.mime_type.starts_with("video/"));
            if !held_back.is_empty() {
                eprintln!(
                    "Skipping {} video(s): ffmpeg not found. Install ffmpeg (or set \
                     EIDETIC_FFMPEG_PATH) and re-run `eidetic thumbnail`.",
                    held_back.len()
                );
            }
            if pending.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            let total = pending.len();
            println!("Generating thumbnails for {total} asset(s)…");

            let mut generated = 0u32;
            let mut failed = 0u32;

            for (i, asset) in pending.into_iter().enumerate() {
                let lib = config.paths.library_dir.clone();
                let storage_path = asset.storage_path.clone();
                let storage_clone = asset.storage_path.clone();
                let hash_clone = asset.hash.clone();
                let id = asset.id;
                let is_video = asset.mime_type.starts_with("video/");

                // A video imported while ffprobe was absent has no stored
                // duration; re-probe and backfill so the representative frame
                // lands at 10% instead of an opening black frame.
                let mut duration = asset.duration_secs;
                if is_video && duration.is_none() {
                    let probe_path = storage_path.clone();
                    if let Ok(Some(p)) = tokio::task::spawn_blocking(move || {
                        eidetic_ingest::video::probe(&probe_path)
                    })
                    .await
                    .context("probe thread panicked")?
                    {
                        repo.update_video_probe(
                            id,
                            p.duration_secs,
                            p.video_codec.as_deref(),
                            p.width,
                            p.height,
                        )
                        .await
                        .context("failed to store probe metadata")?;
                        duration = p.duration_secs;
                    }
                }

                let result = tokio::task::spawn_blocking(move || {
                    if is_video {
                        eidetic_ingest::thumbnail::generate_video_thumbnails(
                            &storage_clone,
                            &hash_clone,
                            &lib,
                            duration,
                        )
                    } else {
                        eidetic_ingest::thumbnail::generate_thumbnails(
                            &storage_clone,
                            &hash_clone,
                            &lib,
                        )
                    }
                })
                .await
                .context("thumbnail thread panicked")?;

                match result {
                    Ok(()) => match repo.mark_thumbnailed(id).await {
                        Ok(()) => {
                            println!("[{}/{}] {}", i + 1, total, storage_path.display());
                            generated += 1;
                        }
                        Err(e) => {
                            // Keep going. Rows whose mark failed stay false
                            // and are retried on the next run, same pattern
                            // as eidetic embed.
                            eprintln!(
                                "  [{}/{}] mark failed for {}: {e}",
                                i + 1,
                                total,
                                storage_path.display()
                            );
                            failed += 1;
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "  [{}/{}] generate failed for {}: {e}",
                            i + 1,
                            total,
                            storage_path.display()
                        );
                        failed += 1;
                    }
                }
            }

            if failed > 0 {
                println!("Done. Generated {generated}, failed {failed}.");
                std::process::exit(1);
            } else {
                println!("Done. Generated {generated}.");
            }
        }

        Command::Faces { cluster_only } => {
            let config = Config::from_env();
            let models_dir = config.paths.models_cache.clone();

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let assets_repo = eidetic_db::AssetsRepo::new(pool.clone());
            let faces_repo = eidetic_db::FacesRepo::new(pool);

            if !cluster_only {
                let pending = faces_repo
                    .fetch_undetected()
                    .await
                    .context("failed to fetch undetected assets")?;
                let videos = faces_repo
                    .fetch_videos_undetected()
                    .await
                    .context("failed to fetch undetected videos")?;
                let ffmpeg_ok = eidetic_ingest::video::ffmpeg().is_some()
                    && eidetic_ingest::video::ffprobe().is_some();
                if !videos.is_empty() && !ffmpeg_ok {
                    eprintln!(
                        "Skipping {} video(s): ffmpeg/ffprobe not found. Install ffmpeg and \
                         re-run `eidetic faces`.",
                        videos.len()
                    );
                }
                let videos = if ffmpeg_ok { videos } else { Vec::new() };

                if pending.is_empty() && videos.is_empty() {
                    println!("No new assets to scan.");
                } else {
                    println!("Loading face models (downloads on first run)…");

                    // Same worker shape as `embed`: the blocking thread owns the
                    // models, the async loop feeds it paths one at a time.
                    // Capacity 1 keeps the worker in lockstep with the DB
                    // writes, so no queue builds up if storage stalls.
                    type ImageReply =
                        oneshot::Sender<eidetic_ml::Result<Vec<eidetic_ml::AnalyzedFace>>>;
                    /// (ts, faces at that ts) per decodable sampled frame, or
                    /// a display-ready reason the video was skipped entirely.
                    type VideoReply =
                        oneshot::Sender<Result<Vec<(f64, Vec<eidetic_ml::AnalyzedFace>)>, String>>;
                    enum FaceJob {
                        Image(PathBuf, ImageReply),
                        Video(PathBuf, Vec<f64>, VideoReply),
                    }
                    let (job_tx, mut job_rx) = mpsc::channel::<FaceJob>(1);

                    let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                        let mut analyzer = eidetic_ml::FaceAnalyzer::load(&models_dir)?;
                        while let Some(job) = job_rx.blocking_recv() {
                            match job {
                                FaceJob::Image(path, reply) => {
                                    let _ = reply.send(analyzer.analyze_path(&path));
                                }
                                FaceJob::Video(path, timestamps, reply) => {
                                    // Same partial-tolerance as embed: frames
                                    // that decode contribute; a video with no
                                    // decodable frame reports its first error.
                                    let mut per_ts = Vec::new();
                                    let mut first_err = None;
                                    for ts in timestamps {
                                        let result =
                                            eidetic_ingest::video::extract_frame(&path, ts)
                                                .map_err(|e| e.to_string())
                                                .and_then(|f| {
                                                    f.ok_or_else(|| {
                                                        "ffmpeg disappeared mid-run".to_string()
                                                    })
                                                })
                                                .and_then(|frame| {
                                                    analyzer
                                                        .analyze(&frame)
                                                        .map_err(|e| e.to_string())
                                                });
                                        match result {
                                            Ok(faces) => per_ts.push((ts, faces)),
                                            Err(e) => first_err = first_err.or(Some(e)),
                                        }
                                    }
                                    let _ = reply.send(match (per_ts.is_empty(), first_err) {
                                        (true, Some(e)) => Err(e),
                                        (_, _) => Ok(per_ts),
                                    });
                                }
                            }
                        }
                        Ok(())
                    });

                    let total = pending.len();
                    if total > 0 {
                        println!("Scanning {total} images for faces");
                    }

                    let (mut scanned, mut found, mut failed) = (0u32, 0u32, 0u32);
                    for (i, (asset_id, path)) in pending.into_iter().enumerate() {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if job_tx
                            .send(FaceJob::Image(path.clone(), reply_tx))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        let result = reply_rx
                            .await
                            .context("face worker panicked or died mid-job")?;

                        match result {
                            Ok(analyzed) => {
                                let new_faces: Vec<_> = analyzed
                                    .iter()
                                    .map(|f| eidetic_db::NewFace {
                                        asset_id,
                                        bbox: (
                                            f.detection.bbox.x,
                                            f.detection.bbox.y,
                                            f.detection.bbox.width,
                                            f.detection.bbox.height,
                                        ),
                                        landmarks: f.detection.landmarks.as_template_order(),
                                        score: f.detection.score,
                                        embedding: f.embedding.clone(),
                                        ts_secs: None,
                                    })
                                    .collect();

                                match faces_repo.record_detection(asset_id, &new_faces).await {
                                    Ok(_) => {
                                        println!(
                                            "[{}/{}] {} — {} face(s)",
                                            i + 1,
                                            total,
                                            path.display(),
                                            new_faces.len()
                                        );
                                        scanned += 1;
                                        found += new_faces.len() as u32;
                                    }
                                    Err(e) => {
                                        // No detection run recorded, so this
                                        // asset is retried on the next pass.
                                        eprintln!("  failed to store {}: {e}", path.display());
                                        failed += 1;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("  skipped {}: {e}", path.display());
                                failed += 1;
                            }
                        }
                    }

                    // ---- video pass (goals-v0.5.md #2) ----
                    // Video faces may MATCH existing people but never seed
                    // clusters (enforced in cluster_faces); on top of that a
                    // stricter quality gate keeps blurred/compressed crops
                    // out entirely, and near-identical detections across a
                    // video's sampled frames collapse to the best one.
                    let video_total = videos.len();
                    if video_total > 0 {
                        println!("Scanning {video_total} video(s) for faces");
                    }
                    let mut vid_found = 0u32;
                    for (i, (asset_id, path, duration)) in videos.into_iter().enumerate() {
                        let timestamps =
                            scene_timestamps(&assets_repo, asset_id, &path, duration).await?;
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if job_tx
                            .send(FaceJob::Video(path.clone(), timestamps, reply_tx))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        let result = reply_rx
                            .await
                            .context("face worker panicked or died mid-job")?;

                        match result {
                            Ok(per_ts) => {
                                let new_faces = video_faces_to_record(asset_id, &per_ts);
                                match faces_repo.record_detection(asset_id, &new_faces).await {
                                    Ok(_) => {
                                        println!(
                                            "[{}/{}] {} — {} face(s)",
                                            i + 1,
                                            video_total,
                                            path.display(),
                                            new_faces.len()
                                        );
                                        scanned += 1;
                                        vid_found += new_faces.len() as u32;
                                    }
                                    Err(e) => {
                                        eprintln!("  failed to store {}: {e}", path.display());
                                        failed += 1;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("  skipped {}: {e}", path.display());
                                failed += 1;
                            }
                        }
                    }
                    found += vid_found;

                    drop(job_tx);
                    worker
                        .await
                        .context("face worker thread panicked")?
                        .context("failed to load face models; check your connection and that the model cache is intact")?;

                    println!("Scanned {scanned} assets, found {found} faces, failed {failed}.");
                }
            }

            let summary = cluster_faces(&faces_repo).await?;
            println!(
                "Clustering: {} face(s) joined an existing person, {} new person(s) proposed, \
                 {} face(s) still unassigned.",
                summary.attached, summary.new_people, summary.unassigned
            );
            if summary.new_people > 0 {
                println!(
                    "Run `eidetic persons` to see them, then `eidetic name-person <id> <name>`."
                );
            }
        }

        Command::Persons => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::FacesRepo::new(pool);

            let people = repo
                .list_persons()
                .await
                .context("failed to list persons")?;
            if people.is_empty() {
                println!("No people yet. Run `eidetic faces` first.");
                return Ok(());
            }
            for p in people {
                let name = p.name.unwrap_or_else(|| "(unnamed)".to_string());
                println!("{}\t{}\t{} face(s)", p.id, name, p.face_count);
            }
        }

        Command::NamePerson { id, name } => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::FacesRepo::new(pool);

            let uuid = uuid::Uuid::parse_str(&id)
                .with_context(|| format!("{id:?} is not a valid person UUID"))?;
            repo.name_person(eidetic_core::PersonId::from(uuid), &name)
                .await
                .context("failed to name person")?;
            println!("Named {id} → {name}");
        }

        Command::Serve { bind } => {
            use std::net::SocketAddr;

            let config = Config::from_env();
            let addr_str = bind
                .or_else(|| std::env::var("EIDETIC_BIND").ok())
                .unwrap_or_else(|| "127.0.0.1:8080".to_string());
            let addr: SocketAddr = addr_str
                .parse()
                .with_context(|| format!("invalid bind address: {addr_str}"))?;

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool.clone());
            let faces = eidetic_db::FacesRepo::new(pool);

            let deps = eidetic_server::ServerDeps {
                repo,
                faces,
                library_dir: config.paths.library_dir.clone(),
                models_cache: config.paths.models_cache.clone(),
            };

            println!("Loading model (downloads ~1.4 GiB on first run)…");
            eidetic_server::serve(addr, deps).await?;
        }

        Command::Reel {
            action:
                Some(ReelAction::Edit {
                    project,
                    swap,
                    drop,
                    pin,
                    unpin,
                    retime,
                    transition,
                    reorder,
                    output,
                    export,
                    dry_run,
                }),
            ..
        } => {
            reel_edit(
                &project,
                &swap,
                &drop,
                &pin,
                &unpin,
                &retime,
                &transition,
                reorder,
                output,
                export.as_deref(),
                dry_run,
            )
            .await?;
        }

        Command::Reel {
            action: None,
            prompt,
            duration,
            output,
            size,
            audio,
            portrait,
            frame,
            export,
            min_sharpness,
            variations,
            dry_run,
        } => {
            let prompt = prompt.expect("clap enforces a prompt when no reel subcommand is given");
            let (w, h) = size
                .split_once('x')
                .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
                .filter(|(w, h)| *w > 0 && *h > 0)
                .with_context(|| format!("invalid --size {size:?}, expected e.g. 1920x1080"))?;
            let (w, h) = if portrait { (1080, 1920) } else { (w, h) };
            // Validate the frame style before doing any expensive work.
            reel::Fit::parse(&frame, portrait)?;
            // With variations only the reel.vN.mp4 paths are written; they
            // get their own overwrite checks once planned.
            if !dry_run && variations <= 1 && output.exists() {
                anyhow::bail!(
                    "{} already exists; pass a different --output",
                    output.display()
                );
            }

            let config = Config::from_env();
            let spec = project::ReelSpec {
                prompt,
                duration,
                output,
                width: w,
                height: h,
                frame,
                audio,
                min_sharpness,
                variations,
            };
            let projects = project::plan_projects(&spec, &config).await?;
            if !dry_run {
                for p in projects.iter().filter(|p| p.output != spec.output) {
                    if p.output.exists() {
                        anyhow::bail!(
                            "{} already exists; pass a different --output",
                            p.output.display()
                        );
                    }
                }
            }

            // Previews trade pixels for speed; the project files remember
            // the real geometry so the winner re-renders full-res.
            let preview = (variations > 1).then_some(((w / 2) & !1, (h / 2) & !1));

            for proj in &projects {
                proj.print_cut_list();
                if dry_run {
                    continue;
                }
                let (rw, rh) = preview.unwrap_or((w, h));
                println!("Rendering {rw}x{rh} @ 30fps…");
                project::render_project(proj, preview).await?;
                println!("Wrote {}", proj.output.display());

                // Every render persists its cut list (goals-v0.8.md phase
                // 0): the project file is what `reel edit` and agents
                // mutate.
                let proj_path = project::ReelProject::path_for(&proj.output);
                proj.save(&proj_path)?;
                println!("Project {}", proj_path.display());
                if let Some(otio_path) = export_path(export.as_deref(), &proj.output)? {
                    otio::export(proj, &otio_path)?;
                    println!("OTIO    {}", otio_path.display());
                }
            }
            if let Some((rw, rh)) = preview
                && !dry_run
            {
                println!(
                    "Previews are {rw}x{rh}; re-render the winner full-res with \
                     `eidetic reel edit <winner>.eidetic.json`"
                );
            }
        }

        Command::Transcode => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            if eidetic_ingest::video::ffmpeg().is_none() {
                anyhow::bail!(
                    "ffmpeg not found. Install it (or set EIDETIC_FFMPEG_PATH) and re-run."
                );
            }

            let candidates = repo
                .fetch_videos_no_playback()
                .await
                .context("failed to fetch videos")?;

            // The codec policy lives in eidetic-core; a video whose original
            // already plays in every browser needs no copy and is skipped
            // permanently (playback_path stays NULL and it keeps being
            // re-checked here, which costs one in-memory filter pass).
            let pending: Vec<_> = candidates
                .into_iter()
                .filter(|c| {
                    !eidetic_core::playback::is_browser_playable(
                        c.video_codec.as_deref(),
                        &c.mime_type,
                    )
                })
                .collect();

            if pending.is_empty() {
                println!("Nothing to do: every video already plays in a browser.");
                return Ok(());
            }

            let total = pending.len();
            println!("Transcoding {total} video(s) for browser playback…");

            let mut done = 0u32;
            let mut failed = 0u32;
            for (i, c) in pending.into_iter().enumerate() {
                // Imported without ffprobe? Probe now so remux-vs-reencode is
                // decided on real data, and backfill the metadata.
                let codec = match c.video_codec {
                    Some(codec) => Some(codec),
                    None => {
                        let probe_path = c.storage_path.clone();
                        match tokio::task::spawn_blocking(move || {
                            eidetic_ingest::video::probe(&probe_path)
                        })
                        .await
                        .context("probe thread panicked")?
                        {
                            Ok(Some(p)) => {
                                repo.update_video_probe(
                                    c.id,
                                    p.duration_secs,
                                    p.video_codec.as_deref(),
                                    p.width,
                                    p.height,
                                )
                                .await
                                .context("failed to store probe metadata")?;
                                p.video_codec
                            }
                            _ => None,
                        }
                    }
                };

                let dest =
                    eidetic_core::playback::playback_path(&config.paths.library_dir, &c.hash);
                let src = c.storage_path.clone();
                let codec_safe = eidetic_core::playback::is_safe_codec(codec.as_deref());
                let dest_clone = dest.clone();
                let result = tokio::task::spawn_blocking(move || {
                    eidetic_ingest::video::transcode_to_playback(&src, &dest_clone, codec_safe)
                })
                .await
                .context("transcode thread panicked")?;

                match result {
                    Ok(Some(())) => match repo.set_playback_path(c.id, &dest).await {
                        Ok(()) => {
                            let how = if codec_safe { "remuxed" } else { "re-encoded" };
                            println!("[{}/{}] {} ({how})", i + 1, total, c.storage_path.display());
                            done += 1;
                        }
                        Err(e) => {
                            eprintln!("  failed to record {}: {e}", c.storage_path.display());
                            failed += 1;
                        }
                    },
                    Ok(None) => anyhow::bail!("ffmpeg disappeared mid-run"),
                    Err(e) => {
                        eprintln!("  [{}/{}] {e}", i + 1, total);
                        failed += 1;
                    }
                }
            }

            if failed > 0 {
                println!("Done. Transcoded {done}, failed {failed}. Re-run to retry.");
                std::process::exit(1);
            } else {
                println!("Done. Transcoded {done}.");
            }
        }

        Command::Transcribe => {
            #[cfg(not(feature = "speech"))]
            {
                anyhow::bail!(
                    "this build has no speech support; rebuild with \
                     `cargo install --path crates/eidetic-cli --features speech` \
                     (needs cmake and a C compiler for whisper.cpp)"
                );
            }
            #[cfg(feature = "speech")]
            {
                let config = Config::from_env();
                let models_dir = config.paths.models_cache.clone();
                let pool = eidetic_db::connect(&config)
                    .await
                    .context("failed to connect to database")?;
                let repo = eidetic_db::AssetsRepo::new(pool);

                if eidetic_ingest::video::ffmpeg().is_none() {
                    anyhow::bail!(
                        "ffmpeg not found. Install it (or set EIDETIC_FFMPEG_PATH) and re-run."
                    );
                }

                let pending = repo
                    .fetch_videos_untranscribed()
                    .await
                    .context("failed to fetch videos")?;
                if pending.is_empty() {
                    println!("Nothing to do.");
                    return Ok(());
                }

                let total = pending.len();
                println!(
                    "Transcribing {total} video(s) (downloads the whisper model on first run)…"
                );

                // Worker owns the whisper model; jobs carry decoded PCM.
                type SpeechReply =
                    oneshot::Sender<eidetic_ml::Result<Vec<eidetic_ml::speech::SpeechSegment>>>;
                let (job_tx, mut job_rx) = mpsc::channel::<(Vec<f32>, SpeechReply)>(1);
                let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                    let transcriber = eidetic_ml::speech::SpeechTranscriber::load(&models_dir)?;
                    while let Some((pcm, reply)) = job_rx.blocking_recv() {
                        let _ = reply.send(transcriber.transcribe(&pcm));
                    }
                    Ok(())
                });

                let (mut done, mut silent, mut failed) = (0u32, 0u32, 0u32);
                for (i, (id, path)) in pending.into_iter().enumerate() {
                    let audio_path = path.clone();
                    let pcm = tokio::task::spawn_blocking(move || {
                        eidetic_ingest::video::extract_audio_pcm(&audio_path)
                    })
                    .await
                    .context("audio thread panicked")?;

                    let pcm = match pcm {
                        Ok(Some(pcm)) => pcm,
                        Ok(None) => anyhow::bail!("ffmpeg disappeared mid-run"),
                        Err(e) => {
                            eprintln!("  [{}/{}] audio decode failed: {e}", i + 1, total);
                            failed += 1;
                            continue;
                        }
                    };

                    // Silent or speechless tracks are a completed run with
                    // zero segments — whisper hallucinates on silence, so it
                    // never sees them.
                    if !eidetic_ingest::video::has_audible_content(&pcm) {
                        repo.store_transcript(id, &[])
                            .await
                            .context("failed to record silent run")?;
                        println!(
                            "[{}/{}] {} (no audible audio)",
                            i + 1,
                            total,
                            path.display()
                        );
                        silent += 1;
                        continue;
                    }

                    let (reply_tx, reply_rx) = oneshot::channel();
                    if job_tx.send((pcm, reply_tx)).await.is_err() {
                        break;
                    }
                    let result = reply_rx
                        .await
                        .context("speech worker panicked or died mid-job")?;
                    match result {
                        Ok(segments) => {
                            let rows: Vec<(f64, f64, String)> = segments
                                .into_iter()
                                .map(|s| (s.start_secs, s.end_secs, s.text))
                                .collect();
                            match repo.store_transcript(id, &rows).await {
                                Ok(()) => {
                                    println!(
                                        "[{}/{}] {} ({} segment(s))",
                                        i + 1,
                                        total,
                                        path.display(),
                                        rows.len()
                                    );
                                    done += 1;
                                }
                                Err(e) => {
                                    eprintln!("  failed to store {}: {e}", path.display());
                                    failed += 1;
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("  skipped {}: {e}", path.display());
                            failed += 1;
                        }
                    }
                }

                drop(job_tx);
                worker
                    .await
                    .context("speech worker thread panicked")?
                    .context("failed to load the whisper model")?;

                if failed > 0 {
                    println!(
                        "Done. Transcribed {done}, silent {silent}, failed {failed}. Re-run to retry."
                    );
                    std::process::exit(1);
                } else {
                    println!("Done. Transcribed {done}, silent {silent}.");
                }
            }
        }

        Command::Dupes { threshold } => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let photos = repo
                .fetch_all_image_embeddings()
                .await
                .context("failed to load image embeddings")?;
            let videos = repo
                .fetch_all_frame_embedding_sets()
                .await
                .context("failed to load frame embeddings")?;

            // Videos use their own default; a custom --threshold shifts both
            // by the same amount relative to the photo default.
            let video_threshold =
                (dupes::VIDEO_THRESHOLD + (threshold - dupes::PHOTO_THRESHOLD)).clamp(0.5, 1.0);
            let mut pairs = dupes::photo_pairs(&photos, threshold);
            pairs.extend(dupes::video_pairs(&videos, video_threshold));

            if pairs.is_empty() {
                println!(
                    "No perceptual duplicates among {} photo(s) and {} video(s). \
                     (Exact duplicates never import in the first place.)",
                    photos.len(),
                    videos.len()
                );
                return Ok(());
            }

            let groups = dupes::group(&pairs);
            println!(
                "{} duplicate group(s) across {} asset(s):",
                groups.len(),
                groups.iter().map(Vec::len).sum::<usize>()
            );
            for (i, members) in groups.iter().enumerate() {
                println!("group {}:", i + 1);
                for (_, path) in members {
                    println!("  {}", path.display());
                }
            }
            println!("Nothing was changed; review and delete by hand if warranted.");
        }

        Command::Rm { refs, force } => {
            if refs.is_empty() {
                anyhow::bail!("nothing to remove; pass asset ids, hashes or paths");
            }
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let mut manifests = Vec::new();
            for needle in &refs {
                let id = repo
                    .resolve_asset(needle)
                    .await
                    .with_context(|| format!("failed to resolve {needle:?}"))?
                    .with_context(|| format!("no asset matches {needle:?}"))?;
                let manifest = repo
                    .fetch_removal_manifest(id)
                    .await?
                    .with_context(|| format!("asset {id} vanished mid-run"))?;
                manifests.push(manifest);
            }

            for m in &manifests {
                println!(
                    "{}  {}  ({} face(s){})",
                    m.id,
                    m.original_filename,
                    m.face_ids.len(),
                    if m.playback_path.is_some() {
                        ", playback copy"
                    } else {
                        ""
                    }
                );
            }
            if !force {
                println!(
                    "Dry run: {} asset(s) listed. Re-run with --force to delete them, \
                     their thumbnails, playback copies and face crops.",
                    manifests.len()
                );
                return Ok(());
            }

            let lib = &config.paths.library_dir;
            let mut removed = 0u32;
            for m in manifests {
                // DB first: if this fails nothing on disk is touched; if a
                // file removal fails afterwards, verify will report the
                // orphan rather than the library lying about a half-gone
                // asset.
                if !repo.delete_asset(m.id).await? {
                    eprintln!("  {} was already gone from the database", m.id);
                    continue;
                }
                let mut paths = vec![
                    m.storage_path.clone(),
                    eidetic_ingest::thumbnail::thumbnail_path(
                        lib,
                        &m.hash,
                        eidetic_ingest::thumbnail::ThumbSize::Small,
                    ),
                    eidetic_ingest::thumbnail::thumbnail_path(
                        lib,
                        &m.hash,
                        eidetic_ingest::thumbnail::ThumbSize::Medium,
                    ),
                ];
                if let Some(p) = &m.playback_path {
                    paths.push(p.clone());
                }
                for f in &m.face_ids {
                    paths.push(lib.join(".faces").join(format!("{}.jpg", f.as_uuid())));
                }
                for p in paths {
                    match std::fs::remove_file(&p) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => eprintln!("  warning: could not remove {}: {e}", p.display()),
                    }
                }
                println!("Removed {} ({})", m.id, m.original_filename);
                removed += 1;
            }
            println!("Done. Removed {removed} asset(s).");
        }

        Command::Verify => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let assets = repo
                .fetch_all_hashes()
                .await
                .context("failed to list assets")?;
            let total = assets.len();
            println!("Verifying {total} asset(s)…");

            let mut known = std::collections::HashSet::new();
            let (mut ok, mut corrupt, mut missing) = (0u32, 0u32, 0u32);
            for (id, expected, path) in assets {
                known.insert(expected.clone());
                let p = path.clone();
                let hashed = tokio::task::spawn_blocking(move || eidetic_ingest::hash_file(&p))
                    .await
                    .context("hash thread panicked")?;
                match hashed {
                    Ok(actual) if actual.to_string() == expected => ok += 1,
                    Ok(actual) => {
                        eprintln!(
                            "CORRUPT  {id}  {}\n  expected {expected}\n  actual   {actual}",
                            path.display()
                        );
                        corrupt += 1;
                    }
                    Err(e) => {
                        eprintln!("MISSING  {id}  {} ({e})", path.display());
                        missing += 1;
                    }
                }
            }

            // Orphans: CAS files (two-level hash shards) whose stem no row
            // claims. Derived trees (.thumbs/.playback/.faces) are skipped —
            // they are regenerable and cleaned by rm.
            let mut orphans = 0u32;
            let lib = &config.paths.library_dir;
            if lib.is_dir() {
                for shard in std::fs::read_dir(lib).into_iter().flatten().flatten() {
                    let name = shard.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with('.') || !shard.path().is_dir() || name.len() != 2 {
                        continue;
                    }
                    for sub in std::fs::read_dir(shard.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                    {
                        for file in std::fs::read_dir(sub.path())
                            .into_iter()
                            .flatten()
                            .flatten()
                        {
                            let path = file.path();
                            let stem = path
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            if !known.contains(&stem) {
                                eprintln!("ORPHAN   {}", path.display());
                                orphans += 1;
                            }
                        }
                    }
                }
            }

            println!(
                "Done. {ok} ok, {corrupt} corrupt, {missing} missing, {orphans} orphan file(s)."
            );
            if corrupt > 0 || missing > 0 {
                std::process::exit(1);
            }
        }

        Command::Search {
            query,
            limit,
            fields,
            json,
            person,
            after,
            before,
            place,
            kind,
        } => {
            // Validate fields early to fail fast before any model loading.
            if let Some(ref f) = fields {
                for field in f {
                    if !VALID_FIELDS.contains(&field.as_str()) {
                        anyhow::bail!(
                            "Unknown field: '{field}'. Valid fields: {}",
                            VALID_FIELDS.join(", ")
                        );
                    }
                }
            }

            let config = Config::from_env();

            let models_dir = config.paths.models_cache.clone();
            let query_clone = query.clone();
            let query_emb = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<Vec<f32>> {
                let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                embedder.embed_text(&query_clone)
            })
            .await
            .context("embedder thread panicked")?
            .context("text embedding failed; check your internet connection and that the SigLIP 2 model is downloaded")?;

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::AssetsRepo::new(pool);

            let parse_day =
                |s: &str, label: &str| -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
                    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .with_context(|| format!("invalid --{label} {s:?}, expected YYYY-MM-DD"))?;
                    Ok(chrono::DateTime::from_naive_utc_and_offset(
                        date.and_hms_opt(0, 0, 0).expect("midnight exists"),
                        chrono::Utc,
                    ))
                };
            if let Some(k) = &kind
                && k != "image"
                && k != "video"
            {
                anyhow::bail!("invalid --kind {k:?}; valid values: image, video");
            }
            let filters = eidetic_db::SearchFilters {
                person,
                after: after
                    .as_deref()
                    .map(|s| parse_day(s, "after"))
                    .transpose()?,
                before: before
                    .as_deref()
                    .map(|s| parse_day(s, "before"))
                    .transpose()?,
                place,
                kind,
            };

            let results = repo
                .search(&query, query_emb.as_slice(), limit, &filters)
                .await
                .context("search failed")?;

            if results.is_empty() {
                println!("No results.");
                return Ok(());
            }

            if json {
                let arr: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "path": r.storage_path.to_string_lossy(),
                            "score": r.score,
                            "date": r.date_taken.map(|d| d.to_rfc3339()),
                            "make": r.camera_make,
                            "model": r.camera_model,
                            "lat": r.latitude,
                            "lon": r.longitude,
                            "mime": r.mime_type,
                            "ts": r.frame_ts,
                            "speech": r.speech,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
            } else if let Some(ref field_list) = fields {
                for r in &results {
                    println!("{}", format_result(r, field_list));
                }
            } else {
                for r in &results {
                    match (&r.speech, r.frame_ts) {
                        (Some(said), Some(ts)) => {
                            println!("{}\t@{ts:.1}s\tsaid: {said:?}", r.storage_path.display())
                        }
                        (_, Some(ts)) => println!("{}\t@{ts:.1}s", r.storage_path.display()),
                        _ => println!("{}", r.storage_path.display()),
                    }
                }
            }
        }
    }

    Ok(())
}

/// `eidetic reel edit` (goals-v0.8.md phase 0): structured mutations of a
/// persisted cut list, then a re-render. Slot numbers are 1-based against
/// the cut list as last printed; combined ops are resolved against that
/// same numbering, then applied as pin/unpin → retime → transition → swap
/// → drop → reorder (see `project::apply_ops`).
#[allow(clippy::too_many_arguments)]
async fn reel_edit(
    project_path: &std::path::Path,
    swap: &[String],
    drop: &[usize],
    pin: &[usize],
    unpin: &[usize],
    retime: &[String],
    transition: &[String],
    reorder: Option<String>,
    output: Option<PathBuf>,
    export: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use project::EditOp;

    // Flag pairs → structured ops. Number parsing happens here; slot
    // validation happens in apply_ops against the loaded project.
    let parse_slot = |flag: &str, s: &str| -> anyhow::Result<usize> {
        s.parse()
            .with_context(|| format!("--{flag} expects a slot number, got {s:?}"))
    };
    let mut ops: Vec<EditOp> = Vec::new();
    for &slot in pin {
        ops.push(EditOp::Pin { slot });
    }
    for &slot in unpin {
        ops.push(EditOp::Unpin { slot });
    }
    for c in retime.chunks(2) {
        ops.push(EditOp::Retime {
            slot: parse_slot("retime", &c[0])?,
            secs: c[1]
                .parse()
                .with_context(|| format!("--retime expects seconds, got {:?}", c[1]))?,
        });
    }
    for c in transition.chunks(2) {
        ops.push(EditOp::Transition {
            slot: parse_slot("transition", &c[0])?,
            style: c[1].clone(),
        });
    }
    for c in swap.chunks(2) {
        ops.push(EditOp::Swap {
            slot: parse_slot("swap", &c[0])?,
            query: c[1].clone(),
        });
    }
    for &slot in drop {
        ops.push(EditOp::Drop { slot });
    }
    if let Some(spec) = reorder {
        let order = spec
            .split(',')
            .map(|t| parse_slot("reorder", t.trim()))
            .collect::<anyhow::Result<Vec<usize>>>()?;
        ops.push(EditOp::Reorder { order });
    }

    let mut proj = project::ReelProject::load(project_path)?;
    let config = Config::from_env();
    project::apply_ops(&mut proj, &ops, &config).await?;

    let render_output = output.unwrap_or_else(|| proj.output.clone());
    let render_output =
        std::path::absolute(&render_output).unwrap_or_else(|_| render_output.clone());
    // Overwriting THIS project's previous render is the whole point of an
    // edit; anything else existing is protected like `reel -o` protects it.
    if render_output != proj.output && render_output.exists() {
        anyhow::bail!(
            "{} already exists; pass a different --output",
            render_output.display()
        );
    }
    proj.output = render_output;
    proj.save(project_path)?;
    proj.print_cut_list();
    if let Some(otio_path) = export_path(export, &proj.output)? {
        otio::export(&proj, &otio_path)?;
        println!("OTIO    {}", otio_path.display());
    }
    if dry_run {
        return Ok(());
    }

    println!("Rendering {}x{} @ 30fps…", proj.width, proj.height);
    project::render_project(&proj, None).await?;
    println!("Wrote {}", proj.output.display());
    Ok(())
}

/// Scene-aware sample timestamps for a video: detect scene boundaries once
/// (a full decode), persist them, and reuse forever after. Both the embed
/// and faces passes call this, so whichever runs first pays the decode.
async fn scene_timestamps(
    repo: &eidetic_db::AssetsRepo,
    id: eidetic_core::AssetId,
    path: &std::path::Path,
    duration: Option<f64>,
) -> anyhow::Result<Vec<f64>> {
    use anyhow::Context;
    let scenes = match repo
        .fetch_video_scenes(id)
        .await
        .context("failed to load scene boundaries")?
    {
        Some(scenes) => scenes,
        None => {
            let p = path.to_path_buf();
            let detected =
                tokio::task::spawn_blocking(move || eidetic_ingest::video::detect_scenes(&p))
                    .await
                    .context("scene detection thread panicked")?;
            match detected {
                Ok(Some(s)) => {
                    repo.store_video_scenes(id, &s)
                        .await
                        .context("failed to store scene boundaries")?;
                    s
                }
                // ffmpeg missing or the file wouldn't decode for detection:
                // fall back to even sampling rather than blocking the pass.
                Ok(None) => Vec::new(),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e,
                        "scene detection failed; using even sampling");
                    Vec::new()
                }
            }
        }
    };
    Ok(eidetic_ingest::video::scene_sample_timestamps(
        duration, &scenes,
    ))
}

/// Turn per-frame video detections into rows worth storing: a stricter
/// quality gate than photos (video crops carry motion blur and compression
/// artifacts — the known cluster-poisoning failure mode), then near-identical
/// embeddings across the sampled frames collapse to their best-scoring
/// instance so one person lingering through a clip becomes one face, not
/// five.
fn video_faces_to_record(
    asset_id: eidetic_core::AssetId,
    per_ts: &[(f64, Vec<eidetic_ml::AnalyzedFace>)],
) -> Vec<eidetic_db::NewFace> {
    const MIN_VIDEO_FACE_SCORE: f32 = 0.7;
    const MIN_VIDEO_FACE_PIXELS: f32 = 48.0;
    /// Cosine similarity above which two detections are the same face.
    const DEDUP_SIMILARITY: f32 = 0.9;

    let mut candidates: Vec<(f64, &eidetic_ml::AnalyzedFace)> = per_ts
        .iter()
        .flat_map(|(ts, faces)| faces.iter().map(move |f| (*ts, f)))
        .filter(|(_, f)| {
            f.detection.score >= MIN_VIDEO_FACE_SCORE
                && f.detection.bbox.max_side() >= MIN_VIDEO_FACE_PIXELS
        })
        .collect();
    // Best score first, so the kept instance of each near-dup group is the
    // sharpest crop of that person.
    candidates.sort_by(|a, b| b.1.detection.score.total_cmp(&a.1.detection.score));

    let mut kept: Vec<(f64, &eidetic_ml::AnalyzedFace)> = Vec::new();
    for (ts, face) in candidates {
        let dup = kept.iter().any(|(_, k)| {
            face.embedding
                .iter()
                .zip(&k.embedding)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                > DEDUP_SIMILARITY
        });
        if !dup {
            kept.push((ts, face));
        }
    }

    kept.into_iter()
        .map(|(ts, f)| eidetic_db::NewFace {
            asset_id,
            bbox: (
                f.detection.bbox.x,
                f.detection.bbox.y,
                f.detection.bbox.width,
                f.detection.bbox.height,
            ),
            landmarks: f.detection.landmarks.as_template_order(),
            score: f.detection.score,
            embedding: f.embedding.clone(),
            ts_secs: Some(ts),
        })
        .collect()
}

/// What one clustering pass did.
struct ClusterSummary {
    attached: usize,
    new_people: usize,
    unassigned: usize,
}

/// Assign stored faces to people (ADR-0010).
///
/// Two phases, in order:
///   1. every unassigned face is offered to the people that already exist;
///   2. whatever is left goes through Chinese Whispers to propose new people.
///
/// Phase 1 first is deliberate: extending a known person is cheaper and more
/// reliable than inventing one, and it stops a new candidate being created for
/// someone who is already named.
///
/// Faces the user has pinned are never touched — the repo enforces that in SQL,
/// and they are also skipped here so they cannot be counted as work.
async fn cluster_faces(repo: &eidetic_db::FacesRepo) -> anyhow::Result<ClusterSummary> {
    use anyhow::Context;
    use eidetic_db::cluster;

    let params = cluster::ClusterParams::default();

    let (must_link, must_not_link) = repo.fetch_links().await.context("failed to load links")?;
    let rejections = repo
        .fetch_rejections()
        .await
        .context("failed to load rejections")?;
    let constraints = cluster::Constraints::new(rejections, must_link, must_not_link);

    // Build each existing person's exemplars from the faces already attributed
    // to them. Recomputed per run rather than cached: it is cheap at this scale
    // and a stale exemplar set silently degrades every future assignment.
    let all = repo
        .fetch_embeddings(false)
        .await
        .context("failed to load face embeddings")?;

    let mut by_person: std::collections::HashMap<eidetic_core::PersonId, Vec<&_>> =
        std::collections::HashMap::new();
    for face in &all {
        // Video faces never serve as exemplars: a blurred video crop that
        // matched correctly would still drag the person's matching set
        // toward low quality (goals-v0.5.md #2).
        if face.ts_secs.is_some() {
            continue;
        }
        if let Some(p) = face.person_id {
            by_person.entry(p).or_default().push(face);
        }
    }

    let people: Vec<cluster::PersonExemplars> = by_person
        .iter()
        .map(|(&person, faces)| {
            let picked = cluster::select_exemplars(faces.as_slice(), params.max_exemplars);
            cluster::PersonExemplars {
                person,
                exemplars: picked
                    .into_iter()
                    .map(|i| faces[i].embedding.clone())
                    .collect(),
            }
        })
        .collect();

    // Phase 1: offer unassigned faces to existing people.
    let mut attached = 0usize;
    let mut still_unassigned = Vec::new();
    for face in all.iter().filter(|f| f.person_id.is_none()) {
        match cluster::assign_to_existing(face, &people, &constraints, &params) {
            Some(person) => {
                repo.assign_face(face.id, person, false)
                    .await
                    .context("failed to assign face")?;
                attached += 1;
            }
            None => still_unassigned.push(face.clone()),
        }
    }

    // Phase 2: propose new people from what is left — photo faces only.
    // Video faces may match existing people (phase 1) but must never seed a
    // cluster: compression artifacts make them false bridges between people.
    let seed_pool: Vec<_> = still_unassigned
        .iter()
        .filter(|f| f.ts_secs.is_none())
        .cloned()
        .collect();
    let proposed = cluster::propose_people(&seed_pool, &constraints, &params);
    let mut grouped = 0usize;
    for group in &proposed {
        let person = repo
            .create_person()
            .await
            .context("failed to create person")?;
        for &face in &group.faces {
            repo.assign_face(face, person, false)
                .await
                .context("failed to assign face to new person")?;
            grouped += 1;
        }
    }

    Ok(ClusterSummary {
        attached,
        new_people: proposed.len(),
        unassigned: still_unassigned.len() - grouped,
    })
}
