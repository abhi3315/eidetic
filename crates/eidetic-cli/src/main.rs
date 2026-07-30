mod eval;

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
    },
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
    "path", "score", "date", "make", "model", "lat", "lon", "mime",
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
        .unwrap_or_else(|| EnvFilter::new("info,ort=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

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

            if unembedded.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            println!("Loading model (downloads ~1.4 GiB on first run)…");

            // Channel of (path, reply) jobs sent to the worker thread. The
            // worker owns the embedder; the async loop sends paths and awaits
            // results via per-job oneshot replies. Channel capacity 1 keeps
            // the worker tightly coupled to the async loop, so no large queue
            // of pending embeds builds up if the DB write side stalls.
            type EmbedJob = (PathBuf, oneshot::Sender<eidetic_ml::Result<Vec<f32>>>);
            let (job_tx, mut job_rx) = mpsc::channel::<EmbedJob>(1);

            let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                let mut embedder = eidetic_ml::SiglipEmbedder::load(&models_dir)?;
                while let Some((path, reply)) = job_rx.blocking_recv() {
                    let result = embedder.embed(&path);
                    // If the receiver was dropped (e.g. caller gave up), keep
                    // serving the next job rather than aborting the worker.
                    let _ = reply.send(result);
                }
                Ok(())
            });

            let total = unembedded.len();
            println!("Found {total} images to embed");

            let mut embedded = 0u32;
            let mut skipped = 0u32;
            let mut failed = 0u32;

            for (i, (id, path)) in unembedded.into_iter().enumerate() {
                let (reply_tx, reply_rx) = oneshot::channel();
                // If `send` fails, the worker died. Surface its error below.
                if job_tx.send((path.clone(), reply_tx)).await.is_err() {
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

            let total = pending.len();
            println!("Generating thumbnails for {total} images…");

            let mut generated = 0u32;
            let mut failed = 0u32;

            for (i, (id, hash, storage_path)) in pending.into_iter().enumerate() {
                let lib = config.paths.library_dir.clone();
                let storage_clone = storage_path.clone();
                let hash_clone = hash.clone();

                let result = tokio::task::spawn_blocking(move || {
                    eidetic_ingest::thumbnail::generate_thumbnails(
                        &storage_clone,
                        &hash_clone,
                        &lib,
                    )
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
            let repo = eidetic_db::AssetsRepo::new(pool);

            let deps = eidetic_server::ServerDeps {
                repo,
                library_dir: config.paths.library_dir.clone(),
                models_cache: config.paths.models_cache.clone(),
            };

            println!("Loading model (downloads ~1.4 GiB on first run)…");
            eidetic_server::serve(addr, deps).await?;
        }

        Command::Search {
            query,
            limit,
            fields,
            json,
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

            let results = repo
                .search_similar(query_emb.as_slice(), limit)
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
                    println!("{}", r.storage_path.display());
                }
            }
        }
    }

    Ok(())
}
