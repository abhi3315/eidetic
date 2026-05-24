# Eidetic

Self-hosted media intelligence system. A personal photo and video library with semantic search, deduplication, and (eventually) face grouping and prompt-driven reel generation.

Written in Rust.

## What works today

- `eidetic import <file|dir>`: import photos/videos into the library
  - Filters non-media files by magic bytes (not extension)
  - Deduplicates by content hash
  - Extracts EXIF: date taken, GPS, camera make/model
  - Stores files in a content-addressable layout under `EIDETIC_LIBRARY_DIR`
  - Supports: JPEG, PNG, WebP, GIF, BMP, TIFF, HEIC/HEIF (via libheif), and DNG/Apple ProRAW (via embedded JPEG preview)
- `eidetic embed`: generate SigLIP 2 embeddings for all imported images (~1.4 GiB model download on first run)
- `eidetic search "dog on beach"`: find photos by natural-language description
  - `--limit N`: number of results (default 10)
  - `--fields score,date,path`: tab-separated column output
  - `--json`: full JSON array
- `eidetic stats`: library summary (asset counts, size, date range, embedding coverage)
- `eidetic hash <file>`: SHA-256 a file, no setup needed
- `eidetic eval --coco-csv <path> --coco-images <dir> [--limit N]`: text-to-image retrieval eval on the COCO 5K Karpathy split. Reports R@1/5/10 + MRR; bypasses the database.

## Quick start

```bash
# 0. Install the CLI (or prefix every `eidetic` below with `cargo run --release -p eidetic-cli --`)
cargo install --path crates/eidetic-cli

# 1. Start Postgres (VectorChord variant required for vector search)
docker compose up -d

# 2. Set env vars (or copy .env.example to .env and source it)
export EIDETIC_DATABASE_URL="postgres://eidetic:eidetic@localhost:5432/eidetic"

# 3. Import your photos (migrations run automatically on first connect)
eidetic import ~/Pictures/

# 4. Generate embeddings (downloads SigLIP 2 model ~1.4 GiB on first run)
eidetic embed

# 5. Search
eidetic search "golden hour at the beach"
eidetic search "birthday cake" --limit 5 --fields score,date,path

# 6. See what's in the library
eidetic stats
```

## Configuration

All settings have sensible defaults (`~/.cache/eidetic/`). Override with environment variables:

| Variable | Default | Description |
|---|---|---|
| `EIDETIC_DATABASE_URL` | `postgres://eidetic:eidetic@localhost:5432/eidetic` | Postgres connection string |
| `EIDETIC_LIBRARY_DIR` | `~/.cache/eidetic/library` | Content-addressable file store |
| `EIDETIC_MODELS_CACHE` | `~/.cache/eidetic/models` | SigLIP 2 ONNX model cache |
| `EIDETIC_LOG` | `info,ort=warn` | Log level (trace/debug/info/warn/error). `ort=warn` mutes the CoreML EP's verbose graph-partition output. |
| `EIDETIC_MODEL` | `base` | SigLIP 2 variant: `base` (768-dim, 1.4 GB download) or `large` (1024-dim, 3.6 GB, ~5x slower). |
| `EIDETIC_ACCELERATOR` | _unset_ (= CPU) | ONNX Runtime execution provider. `cpu` or `coreml`. CoreML is wired up and can be enabled, but **does not currently accelerate this workload**. See "Why CoreML is opt-in" below. |

Copy `.env.example` to `.env` and adjust as needed. There is no config file; env vars are the only configuration layer for now.

### Why CoreML is opt-in

CoreML EP is wired up via `ort 2.0.0-rc.12` and registers correctly on macOS, but in practice it does not accelerate SigLIP 2 inference on Apple Silicon (M1 Pro tested). Per-image vision-encoder benchmarks via Python `onnxruntime`:

| Model preprocessing | CPU EP | CoreML EP |
|---|---|---|
| As-shipped from `onnx-community` | **148 ms/img** | 265 ms/img |
| With `onnxruntime.transformers.optimizer` fusions | 151 ms/img | 231 ms/img |

End-to-end COCO 5K eval (Rust): CPU 35 min, CoreML 2.36× slower. Recall@1, R@5, R@10 are bit-identical between the two, so accuracy is not the issue.

Why this is happening (all documented unfixed bugs):

- The `onnx-community` SigLIP 2 export uses `auto_pad=SAME_LOWER` on the patch-embedding Conv. CoreML's `MLProgram` compiler refuses to compile this op ([apple/coremltools#2127](https://github.com/apple/coremltools/issues/2127), open since Jan 2024). The legacy `NeuralNetwork` format compiles, but fragments the graph into ~95 CoreML subgraphs that never reach ANE.
- fp16 model weights produce slightly faster CoreML execution, but ort's optimizer crashes on `SimplifiedLayerNormFusion` for fp16 transformers ([microsoft/onnxruntime#25824](https://github.com/microsoft/onnxruntime/issues/25824)). Forcing `GraphOptimizationLevel::Level1` works around the crash but fp16 on Apple's CPU is itself slower than fp32 (no native fp16 ALUs).
- ANE never engages even when CoreML runs the fp16 path. Measured 0% utilization, 0 W.

Current behavior: `EIDETIC_ACCELERATOR=coreml` registers the CoreML EP and runs correctly, but for typical workloads (importing a photo library, ad-hoc searches) you should leave the variable unset and use CPU. The flag and the wiring are kept so that future ort releases or alternate ONNX exports of SigLIP 2 don't require a code change.

## Database

Requires Postgres with the [VectorChord](https://github.com/tensorchord/VectorChord) extension (`pgvector` compatible, built-in ANN index). The `docker-compose.yml` uses the official image.

```bash
# Start
docker compose up -d

# Stop (data persists in Docker volume)
docker compose down

# Wipe everything and start fresh
docker compose down -v
```

Migrations run automatically on every `eidetic` startup that connects to the database. They are idempotent and safe to run repeatedly.

## System dependencies

Eidetic uses `libheif` to decode HEIC/HEIF photos (the format iPhones produce by default). Install it once:

- **macOS:** `brew install libheif`
- **Ubuntu / Debian:** `sudo apt install libheif-dev libheif-plugin-x265`
- **Other Linux:** install the `libheif` development package and the x265 encoder plugin via your distribution's package manager.

`libheif-plugin-x265` is only needed if you run the test suite (the HEIC tests encode synthetic fixtures at setup). For just building and running Eidetic against existing HEIC files, the decoder side of `libheif-dev` is enough. macOS Homebrew's `libheif` bundles x265 directly, so no extra step there.

If `libheif` isn't installed, Eidetic builds fine but fails at runtime with a dynamic-linker error when a HEIC file is encountered.

## Build

```bash
# Install pre-commit hook (runs fmt + clippy on every commit)
./scripts/install-hooks.sh

# Build
cargo build --workspace

# Test (integration tests require Docker)
cargo test --workspace
```

## Documentation

- [`AGENTS.md`](AGENTS.md): context for AI agents working on this codebase
- [`docs/adr/`](docs/adr/): architecture decisions
- [`goals.md`](goals.md): what this project is and isn't

## License

MIT OR Apache-2.0 at your option.
