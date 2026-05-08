//! `eidetic` — personal media intelligence engine, command-line interface.
//!
//! This binary contains no business logic. Each subcommand wires up the
//! library crates and dispatches to them. Logic lives in the libraries.

use anyhow::Context;
use clap::{Parser, Subcommand};
use eidetic_core::Config;
use eidetic_ingest::ImportOutcome;
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_env("EIDETIC_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    match cli.command {
        Command::Hash { path } => {
            let hash = eidetic_ingest::hash_file(&path)
                .with_context(|| format!("failed to hash {}", path.display()))?;
            println!("{hash}  {}", path.display());
        }

        Command::Import { path } => {
            let config = Config::from_env().context("failed to load config")?;
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
                        println!("Skipped   {} (unsupported file type)", path.display());
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
                println!("Failed     {:>6} files", summary.failed.len());
                for (p, e) in &summary.failed {
                    eprintln!("  {} — {e}", p.display());
                }

                if !summary.failed.is_empty() {
                    std::process::exit(1);
                }
            } else {
                anyhow::bail!("{} is not a file or directory", path.display());
            }
        }
    }

    Ok(())
}
