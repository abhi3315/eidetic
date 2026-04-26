//! `eidetic` — personal media intelligence engine, command-line interface.
//!
//! This binary contains no business logic. Each subcommand wires up the
//! library crates (`eidetic-core`, `eidetic-ingest`, `eidetic-db`, etc.)
//! and dispatches to them. Logic lives in the libraries.

use anyhow::Context;
use clap::{Parser, Subcommand};
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
}

fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_env("EIDETIC_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    match cli.command {
        Command::Hash { path } => {
            let hash = eidetic_ingest::hash_file(&path)
                .with_context(|| format!("failed to hash {}", path.display()))?;
            println!("{hash}  {}", path.display());
        }
    }

    Ok(())
}
