//! `eidetic` — personal media intelligence engine, command-line interface.
//!
//! This binary contains no business logic. Each subcommand wires up the
//! library crates and dispatches to them. Logic lives in the libraries.

mod eval;

use anyhow::Context;
use clap::{Parser, Subcommand};
use eidetic_core::Config;
use eidetic_ingest::ImportOutcome;
use eidetic_ml::Embedder;
use std::path::PathBuf;
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
    /// Run COCO 5K (Karpathy) text-to-image retrieval eval. Bypasses the
    /// library entirely — embeds images and captions in-memory.
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
    let filter =
        EnvFilter::try_from_env("EIDETIC_LOG").unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
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
            let repo = eidetic_db::PgAssetsRepo::new(pool);

            if path.is_file() {
                match eidetic_ingest::import_file(&path, &repo, &config.paths).await {
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
                        eprintln!("Failed    {} — {e}", path.display());
                        std::process::exit(1);
                    }
                }
            } else if path.is_dir() {
                let summary = eidetic_ingest::import_dir(&path, &repo, &config.paths)
                    .await
                    .with_context(|| format!("cannot import {}", path.display()))?;

                println!("Imported   {:>6} files", summary.imported);
                println!("Duplicates {:>6} files", summary.duplicates);
                println!("Skipped    {:>6} files", summary.skipped);
                println!("Failed     {:>6} files", summary.failed.len());
                for (p, e) in &summary.failed {
                    eprintln!("  {} — {e}", p.display());
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
            let repo = eidetic_db::PgAssetsRepo::new(pool);
            let s = repo.fetch_stats().await.context("failed to fetch stats")?;

            println!("Assets     {:>8}", s.total);
            println!("  images   {:>8}", s.images);
            println!("  videos   {:>8}", s.videos);
            println!("Embedded   {:>8}", s.embedded);
            println!("Not yet    {:>8}", s.needs_embed);
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
            println!("Loading model (downloads ~1.4 GiB on first run)…");
            let embedder = std::sync::Arc::new(
                tokio::task::spawn_blocking(move || eidetic_ml::SiglipEmbedder::load(&models_dir))
                    .await
                    .context("embedder thread panicked")?
                    .context("failed to load SigLIP 2 model — check your internet connection")?,
            );

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::PgAssetsRepo::new(pool);

            let unembedded = repo
                .fetch_unembedded()
                .await
                .context("failed to fetch unembedded assets")?;

            if unembedded.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            let total = unembedded.len();
            println!("Found {total} images to embed");

            let mut embedded = 0u32;
            let mut skipped = 0u32;

            for (i, (id, path)) in unembedded.into_iter().enumerate() {
                let embedder = std::sync::Arc::clone(&embedder);
                let path_clone = path.clone();

                let result = tokio::task::spawn_blocking(move || embedder.embed(&path_clone))
                    .await
                    .context("embedder thread panicked")?;

                match result {
                    Ok(emb) => {
                        repo.store_embedding(id, emb.as_slice())
                            .await
                            .context("failed to store embedding")?;
                        println!("[{}/{}] {}", i + 1, total, path.display());
                        embedded += 1;
                    }
                    Err(e) => {
                        eprintln!("  skipped {}: {e}", path.display());
                        skipped += 1;
                    }
                }
            }

            println!("Done. Embedded {embedded}, skipped {skipped}.");
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
            let embedder =
                tokio::task::spawn_blocking(move || eidetic_ml::SiglipEmbedder::load(&models_dir))
                    .await
                    .context("embedder thread panicked")?
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to load model: {e}. Run 'eidetic embed' first to download it."
                        )
                    })?;

            let query_clone = query.clone();
            let query_emb = tokio::task::spawn_blocking(move || embedder.embed_text(&query_clone))
                .await
                .context("embedder thread panicked")?
                .context("text embedding failed")?;

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::PgAssetsRepo::new(pool);

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
