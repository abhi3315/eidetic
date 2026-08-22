# Eidetic

Self-hosted media intelligence system. A personal photo and video library with semantic search over photos *and* video moments, deduplication, face grouping, and prompt-driven reel generation.

Written in Rust.

## What works today

- `eidetic import <file|dir>`: import photos/videos into the library
  - Filters non-media files by magic bytes (not extension)
  - Deduplicates by content hash
  - Extracts EXIF: date taken, GPS, camera make/model, lens, focal length, aperture, shutter, ISO, orientation, altitude, plus a JSONB tail with every other tag kamadak-exif can parse
  - Reverse-geocodes GPS coordinates to country, state, and nearest-place name using an offline GeoNames cities500 dataset (~13 MB, auto-downloaded into `~/.cache/eidetic/geonames/` on first import; photo coordinates never leave the machine)
  - Stores files in a content-addressable layout under `EIDETIC_LIBRARY_DIR`
  - Supports: JPEG, PNG, WebP, GIF, BMP, TIFF, HEIC/HEIF (via libheif, on by default — see [Build features](#build-features)), DNG/Apple ProRAW (via embedded JPEG preview), and videos (MP4/MOV/MKV/WebM and friends)
  - Videos are probed with ffprobe ([ADR-0011](docs/adr/0011-video-pipeline.md)): duration, codec, rotation-aware dimensions, creation date, and the QuickTime GPS tag iPhones write — which geocodes offline exactly like photo EXIF. A representative frame becomes the thumbnail. ffmpeg is a *runtime* dependency: without it videos still import, and one warning tells you what to install.
- `eidetic embed`: generate SigLIP 2 embeddings for all imported images and videos (~1.4 GiB model download on first run). Videos are scene-detected once (ffmpeg), then sampled one frame per shot (duration-scaled, up to 24) with each frame stored under its timestamp; single-shot clips fall back to even sampling.
- `eidetic search "dog on beach"`: find photos *and video moments* by natural-language description
  - Video results carry the timestamp of the best-matching frame (`@7.0s` on default output, `ts` as a field)
  - `--limit N`: number of results (default 10)
  - `--fields score,ts,date,path`: tab-separated column output
  - `--json`: full JSON array
- `eidetic reel "sunset at the beach"`: cut a short mp4 from the best-matching photos and video moments (needs ffmpeg)
  - Video hits become ~4s clips around the matched frame, snapped inside the shot's scene boundaries so cuts never splice across shots; photos hold 3s with a Ken Burns push-in
  - `--duration N` target seconds (default 30), `--size WxH` (default 1920x1080), `--output file`, `--dry-run` to print the cut list
- `eidetic faces`: detect faces in imported images and videos, then group them into people
  - Video faces come from the sampled frames (needs ffmpeg), pass a stricter quality gate, and may *join* existing people but never form new ones — motion blur and compression artifacts are the classic cluster poison. A person's page deep-links to the moment they appear
  - Detection is YuNet, embedding is AuraFace (512-dim) by default — both permissively licensed, see [ADR-0010](docs/adr/0010-face-stack.md)
  - `EIDETIC_FACE_MODEL`: `auraface` (default), `sface` (smaller/faster, 128-dim), or `buffalo_l` (InsightFace; better accuracy but **non-commercial weights** you download yourself)
  - `--cluster-only`: re-run grouping over already-detected faces without scanning again
  - A face joins an existing person when it's close enough; leftovers are grouped into new candidates once at least 3 agree
  - Your corrections are durable: naming, merges and splits are stored as constraints, so re-running clustering never discards them
- `eidetic persons`: list grouped people with face counts
- `eidetic name-person <id> <name>`: name a person
- `eidetic transcribe`: transcribe speech in videos with Whisper (needs a `--features speech` build), so search finds spoken words — "ask not what your country" returns the video where it's said, at the moment it's said, ranked above visual matches. Transcripts are recall fuel, never displayed as subtitles: casual home audio transcribes too roughly to show. Silent tracks are skipped by an energy gate
- `eidetic transcode`: generate browser-playable H.264 copies of videos whose codec or container browsers can't stream (HEVC iPhone footage, MKV). Cheap remux when only the container is wrong; originals never touched; the web player picks the copy automatically
- `eidetic serve`: localhost web viewer — thumbnail grid (videos play inline with seeking), semantic search, per-asset detail, and a People section with face-crop grids per person (naming stays in the CLI)
- `eidetic stats`: library summary (asset counts, size, date range, embedding coverage)
- `eidetic hash <file>`: SHA-256 a file, no setup needed
- `eidetic eval --coco-csv <path> --coco-images <dir> [--limit N]`: text-to-image retrieval eval on the COCO 5K Karpathy split. Reports R@1/5/10 + MRR; bypasses the database.

## Quick start

```bash
# 0. Install the CLI (or prefix every `eidetic` below with `cargo run --release -p eidetic-cli --`)
cargo install --path crates/eidetic-cli

# 1. Import your photos
#    The SQLite database is created and migrated on first connect — there is
#    no server to start and nothing to configure.
eidetic import ~/Pictures/

# 2. Generate embeddings (downloads SigLIP 2 model ~1.4 GiB on first run)
eidetic embed

# 3. Search
eidetic search "golden hour at the beach"
eidetic search "birthday cake" --limit 5 --fields score,date,path

# 4. See what's in the library
eidetic stats
```

## Configuration

All settings have sensible defaults (`~/.cache/eidetic/`). Override with environment variables:

| Variable | Default | Description |
|---|---|---|
| `EIDETIC_DATABASE_PATH` | `~/.cache/eidetic/eidetic.db` | SQLite database file. Created on first run; no server process. |
| `EIDETIC_LIBRARY_DIR` | `~/.cache/eidetic/library` | Content-addressable file store |
| `EIDETIC_MODELS_CACHE` | `~/.cache/eidetic/models` | SigLIP 2 ONNX model cache |
| `EIDETIC_LOG` | `info,ort=error` | Log level (trace/debug/info/warn/error). `ort=error` mutes ONNX Runtime's non-actionable noise — CoreML graph partitioning, and the per-initializer warnings some model exports emit. Real ort failures still surface. |
| `EIDETIC_MODEL` | `base` | SigLIP 2 variant: `base` (768-dim, 1.4 GB download), `large` (1024-dim, 3.6 GB), or `so400m` (1152-dim, best retrieval quality — practical on a GPU). Unknown values are rejected rather than silently falling back. Changing this requires re-embedding the library. |
| `EIDETIC_ACCELERATOR` | _unset_ (= CPU) | ONNX Runtime execution provider: `cpu`, `cuda`, or `coreml`. `cuda` needs a build with `--features cuda` plus an ONNX Runtime ≥ 1.27 CUDA build at runtime (see [ADR-0006](docs/adr/0006-gpu-execution-provider.md)). CoreML is macOS-only and **does not currently accelerate this workload** — see "Why CoreML is opt-in" below. |

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

Embedded SQLite — a single file at `~/.cache/eidetic/eidetic.db` (override with `EIDETIC_DATABASE_PATH`). No server, no Docker, no connection string. SQLite is compiled into the binary; the file is created on first run.

```bash
# Inspect it with the standard CLI
sqlite3 ~/.cache/eidetic/eidetic.db '.tables'

# Back it up (safe while eidetic is running)
sqlite3 ~/.cache/eidetic/eidetic.db ".backup '/tmp/eidetic-backup.db'"

# Wipe everything and start fresh
rm ~/.cache/eidetic/eidetic.db*
```

Migrations run automatically on every `eidetic` startup that opens the database. They are idempotent and safe to run repeatedly.

Embeddings live in their own `embeddings` table as raw f32 blobs, and search is **exact** brute-force cosine computed in-process — 100% recall, no ANN index to tune. See [ADR-0005](docs/adr/0005-vector-storage-sqlite.md) for why, and for the escalation path if a library ever outgrows it.

## System dependencies

Eidetic uses `libheif` to decode HEIC/HEIF photos (the format iPhones produce by default). Install it once:

- **macOS:** `brew install libheif`
- **Ubuntu / Debian:** `sudo apt install libheif-dev libheif-plugin-x265 libheif-plugin-libde265`
- **Other Linux:** install the `libheif` development package plus the x265 (encoder) and libde265 (decoder) plugins via your distribution's package manager.

The x265 plugin is only needed if you run the test suite (which encodes synthetic HEIC fixtures at setup). The libde265 plugin is needed any time you decode an existing HEIC file. macOS Homebrew's `libheif` bundles both directly, so no extra step there.

If `libheif` isn't installed, Eidetic builds fine but fails at runtime with a dynamic-linker error when a HEIC file is encountered.

Video features (thumbnails, frame embeddings, reels) shell out to **ffmpeg/ffprobe** at runtime — no build dependency at all ([ADR-0011](docs/adr/0011-video-pipeline.md)). Install with `brew install ffmpeg` / `sudo apt install ffmpeg`, or point `EIDETIC_FFMPEG_PATH` / `EIDETIC_FFPROBE_PATH` at binaries elsewhere. Without ffmpeg everything else works; videos import but stay un-thumbnailed and un-searchable until you install it and run `eidetic thumbnail` + `eidetic embed`.

## Build

```bash
# Install pre-commit hook (runs fmt + clippy on every commit)
./scripts/install-hooks.sh

# Build
cargo build --workspace

# Test (no services required — tests create their own temp SQLite database)
cargo test --workspace
```

### Build features

| Feature | Default | What it does |
|---|---|---|
| `heic` | **on** | HEIC/HEIF decoding via libheif. Needs the libheif C library installed at build time (`libheif-dev` / `libheif-devel`). |
| `cuda` | off | NVIDIA GPU inference via the ONNX Runtime CUDA execution provider. Requires an ONNX Runtime ≥ 1.27 CUDA build supplied at runtime through `ORT_DYLIB_PATH`, plus CUDA 13 + cuDNN 9 on the host. See [ADR-0006](docs/adr/0006-gpu-execution-provider.md). |
| `speech` | off | Speech-to-text over video audio via whisper.cpp (`eidetic transcribe` + spoken-word search). Off by default because whisper-rs compiles C through cmake — the default build keeps its no-native-build-deps guarantee. Model (~466 MB, `EIDETIC_SPEECH_MODEL`: `base`, `small` default, `large-v3-turbo`) downloads on first run. |
| `speech-cuda` | off | `speech` with whisper.cpp's CUDA kernels — GPU transcription. Needs nvcc at build time with a host compiler your CUDA toolkit supports: nvcc 12.9 cannot parse gcc 16 headers (Fedora 44+), so install a compat gcc (`sudo dnf install gcc14-c++`) and build with `CUDAHOSTCXX=$(which g++-14)`, or use a CUDA 13 toolkit. |

```bash
# Default: HEIC support, CPU inference. Needs libheif installed.
cargo build --release -p eidetic-cli

# No system dependencies at all — pure-Rust decoders only (no HEIC).
cargo build --release -p eidetic-cli --no-default-features

# GPU build
cargo build --release -p eidetic-cli --features cuda
```

### Running the GPU build

A `cuda` build loads ONNX Runtime dynamically and refuses to start without `ORT_DYLIB_PATH` (otherwise ort would silently download a CPU-only runtime). Verified working recipe on an RTX 5070 Ti (Blackwell, sm_120):

```bash
# One-time: fetch an ORT >= 1.27 CUDA build and cuDNN 9 (no login needed)
D=~/.cache/eidetic/ort && mkdir -p $D && cd $D
curl -LO https://github.com/microsoft/onnxruntime/releases/download/v1.29.0/onnxruntime-linux-x64-gpu_cuda12-1.29.0.tgz
curl -LO https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/linux-x86_64/cudnn-linux-x86_64-9.25.0.15_cuda12-archive.tar.xz
tar xzf onnxruntime-*.tgz && tar xJf cudnn-*.tar.xz

# Every run
export ORT_DYLIB_PATH=$D/onnxruntime-linux-x64-gpu_cuda12-1.29.0/lib/libonnxruntime.so
export LD_LIBRARY_PATH=$D/onnxruntime-linux-x64-gpu_cuda12-1.29.0/lib:$D/cudnn-linux-x86_64-9.25.0.15_cuda12-archive/lib
EIDETIC_ACCELERATOR=cuda eidetic embed
```

Measured on the RTX 5070 Ti: embedding CPU time drops ~22× (133 s → 6 s of CPU work for an 80-asset library); embeddings are bit-identical in ranking to the CPU path.

HEIC is on by default because it is the primary format for iPhone photos and libheif is the mature, correct decoder for it (LGPL, so linking it from this MIT/Apache codebase is distribution-clean). The trade-off is a system dependency; `--no-default-features` drops it entirely at the cost of HEIC support. See [ADR-0008](docs/adr/0008-heic-decode.md).

## Documentation

- [`AGENTS.md`](AGENTS.md): context for AI agents working on this codebase
- [`docs/adr/`](docs/adr/): architecture decisions
- [`goals.md`](goals.md): what this project is and isn't

## Acknowledgments

Geographic data © [GeoNames](https://www.geonames.org/), licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Used for the
offline reverse geocoding that populates the place columns during import.

## License

MIT OR Apache-2.0 at your option.
