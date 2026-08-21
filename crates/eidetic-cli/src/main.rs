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
        // `ort=error` rather than `ort=warn`: ONNX Runtime emits a warning per
        // graph initializer for some exports (SFace produces eight on every
        // load) and CoreML logs its graph partitioning. Neither is actionable
        // by the user. Real ort failures are logged at error and still show.
        .unwrap_or_else(|| EnvFilter::new("info,ort=error"));
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

        Command::Faces { cluster_only } => {
            let config = Config::from_env();
            let models_dir = config.paths.models_cache.clone();

            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let faces_repo = eidetic_db::FacesRepo::new(pool);

            if !cluster_only {
                let pending = faces_repo
                    .fetch_undetected()
                    .await
                    .context("failed to fetch undetected assets")?;

                if pending.is_empty() {
                    println!("No new images to scan.");
                } else {
                    println!("Loading face models (downloads on first run)…");

                    // Same worker shape as `embed`: the blocking thread owns the
                    // models, the async loop feeds it paths one at a time.
                    // Capacity 1 keeps the worker in lockstep with the DB
                    // writes, so no queue builds up if storage stalls.
                    type FaceJob = (
                        PathBuf,
                        oneshot::Sender<eidetic_ml::Result<Vec<eidetic_ml::AnalyzedFace>>>,
                    );
                    let (job_tx, mut job_rx) = mpsc::channel::<FaceJob>(1);

                    let worker = tokio::task::spawn_blocking(move || -> eidetic_ml::Result<()> {
                        let mut analyzer = eidetic_ml::FaceAnalyzer::load(&models_dir)?;
                        while let Some((path, reply)) = job_rx.blocking_recv() {
                            let _ = reply.send(analyzer.analyze_path(&path));
                        }
                        Ok(())
                    });

                    let total = pending.len();
                    println!("Scanning {total} images for faces");

                    let (mut scanned, mut found, mut failed) = (0u32, 0u32, 0u32);
                    for (i, (asset_id, path)) in pending.into_iter().enumerate() {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if job_tx.send((path.clone(), reply_tx)).await.is_err() {
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

                    drop(job_tx);
                    worker
                        .await
                        .context("face worker thread panicked")?
                        .context("failed to load face models; check your connection and that the model cache is intact")?;

                    println!("Scanned {scanned} images, found {found} faces, failed {failed}.");
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

    // Phase 2: propose new people from what is left.
    let proposed = cluster::propose_people(&still_unassigned, &constraints, &params);
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
