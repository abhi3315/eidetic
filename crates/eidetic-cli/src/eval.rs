use anyhow::{Context, Result, bail};
use eidetic_core::Config;
use eidetic_ml::SiglipEmbedder;
use std::path::Path;

struct EvalImage {
    filename: String,
    captions: Vec<String>,
}

pub fn run(csv_path: &Path, images_dir: &Path, limit: Option<usize>) -> Result<()> {
    let mut images = parse_csv(csv_path)?;
    if let Some(n) = limit {
        images.truncate(n);
    }
    let n_images = images.len();
    let n_captions: usize = images.iter().map(|i| i.captions.len()).sum();
    if n_images == 0 {
        bail!("no images parsed from {}", csv_path.display());
    }
    println!(
        "Loaded {n_images} images, {n_captions} captions from {}",
        csv_path.display()
    );

    let config = Config::from_env();
    println!("Loading model (downloads ~1.4 GiB on first run)…");
    let mut embedder = SiglipEmbedder::load(&config.paths.models_cache)
        .context("failed to load SigLIP 2 model")?;

    // 1. Embed every image. Order in `img_embeddings` matches `images`.
    println!("Embedding {n_images} images…");
    let mut img_embeddings: Vec<Vec<f32>> = Vec::with_capacity(n_images);
    for (i, image) in images.iter().enumerate() {
        let path = images_dir.join(&image.filename);
        let emb = embedder
            .embed(&path)
            .with_context(|| format!("embed image {}", path.display()))?;
        img_embeddings.push(emb);
        if (i + 1).is_multiple_of(100) || i + 1 == n_images {
            println!("  images: {} / {n_images}", i + 1);
        }
    }

    // 2. For each caption, embed and rank against all images.
    //    Rank is 1-based; rank 1 means the source image was top-1.
    println!("Evaluating {n_captions} caption→image queries…");
    let mut ranks: Vec<usize> = Vec::with_capacity(n_captions);
    let mut q_done = 0usize;
    for (correct_idx, image) in images.iter().enumerate() {
        for caption in &image.captions {
            let q_emb = embedder
                .embed_text(caption)
                .with_context(|| format!("embed caption: {caption:?}"))?;
            let q = q_emb.as_slice();

            // Both sides are L2-normalized in SigLIP, so dot = cosine similarity.
            // Count how many images outrank the correct one. Ties broken by
            // index, which is fine; they're effectively rare on f32.
            let correct_score = dot(q, &img_embeddings[correct_idx]);
            let beat_or_tied = img_embeddings
                .iter()
                .enumerate()
                .filter(|(i, e)| *i != correct_idx && dot(q, e) > correct_score)
                .count();
            ranks.push(beat_or_tied + 1);

            q_done += 1;
            if q_done.is_multiple_of(500) || q_done == n_captions {
                println!("  captions: {q_done} / {n_captions}");
            }
        }
    }

    // 3. Metrics.
    let recall_at = |k: usize| -> f64 {
        let hits = ranks.iter().filter(|&&r| r <= k).count();
        hits as f64 / ranks.len() as f64
    };
    let mrr: f64 = ranks.iter().map(|&r| 1.0 / r as f64).sum::<f64>() / ranks.len() as f64;

    println!();
    println!("Results for text→image retrieval over {n_images} images, {n_captions} captions:");
    println!("  Recall@1   {:6.2}%", recall_at(1) * 100.0);
    println!("  Recall@5   {:6.2}%", recall_at(5) * 100.0);
    println!("  Recall@10  {:6.2}%", recall_at(10) * 100.0);
    println!("  MRR        {mrr:.4}");
    println!();
    println!("Reference (SigLIP 2 base patch16 256, paper, full 5K split):");
    println!("  Recall@1 ≈ 47%, Recall@5 ≈ 72%, Recall@10 ≈ 80%");
    if limit.is_some() && n_images < 5000 {
        println!();
        println!(
            "Note: ran on {n_images} images (vs 5000 in the paper). \
             Smaller image pools inflate recall."
        );
    }

    Ok(())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn parse_csv(path: &Path) -> Result<Vec<EvalImage>> {
    let mut reader =
        csv::Reader::from_path(path).with_context(|| format!("open csv {}", path.display()))?;

    let headers = reader.headers()?.clone();
    let filename_idx = column_index(&headers, "filename")?;
    let raw_idx = column_index(&headers, "raw")?;

    let mut out = Vec::new();
    for record in reader.records() {
        let r = record?;
        let filename = r
            .get(filename_idx)
            .context("filename column missing in row")?
            .to_string();
        let raw = r.get(raw_idx).context("raw column missing in row")?;
        let captions: Vec<String> =
            serde_json::from_str(raw).with_context(|| format!("parse captions for {filename}"))?;
        out.push(EvalImage { filename, captions });
    }
    Ok(out)
}

fn column_index(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|h| h == name)
        .with_context(|| format!("CSV header missing required column {name:?}"))
}
