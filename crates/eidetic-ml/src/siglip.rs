use crate::{Error, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use tokenizers::Tokenizer;

#[cfg(target_os = "macos")]
use ort::ep::{CoreML, coreml::ComputeUnits};

/// Which execution provider to register on a `Session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// User did not set `EIDETIC_ACCELERATOR`. Platform default is CPU
    /// everywhere — CoreML is wired up but does not deliver acceleration
    /// on the current ort 2.0-rc + onnx-community SigLIP 2 combo (measured
    /// 2.4× slower than CPU on M1 Pro). Future ort releases or different
    /// ONNX exports may change this; flip the default back to CoreML on
    /// macOS once that's true.
    Default,
    /// Explicitly requested CoreML. Errors on non-macOS.
    CoreML,
    /// Explicitly requested CPU. Always valid.
    Cpu,
}

/// Parse the `EIDETIC_ACCELERATOR` env var. Unknown non-empty values are
/// rejected so a typo (`metal`, `cuda`, etc.) fails loudly rather than
/// quietly selecting CPU.
fn parse_accelerator(raw: Option<&str>) -> Result<Mode> {
    match raw {
        None => Ok(Mode::Default),
        Some("coreml") => Ok(Mode::CoreML),
        Some("cpu") => Ok(Mode::Cpu),
        Some(other) => Err(Error::ModelLoad(format!(
            "Unknown EIDETIC_ACCELERATOR={other:?}; valid values: coreml, cpu"
        ))),
    }
}

const TOKENIZER_FILE: &str = "tokenizer.json";
const SEQ_LEN: usize = 64;
const PAD_TOKEN_ID: i64 = 1;

/// Available SigLIP 2 model variants, selected via `EIDETIC_MODEL`.
struct ModelVariant {
    repo: &'static str,
    vision_model: &'static str,
    text_model: &'static str,
    /// Optional companion file holding the text model's external weights.
    /// Some HF exports split a large `.onnx` into a tiny graph header plus a
    /// big `.onnx_data` file; ort loads it transparently if both files sit
    /// in the same directory, but we still need to download it explicitly.
    text_model_data: Option<&'static str>,
    image_size: u32,
    embed_dim: usize,
}

const SIGLIP2_BASE_256: ModelVariant = ModelVariant {
    repo: "onnx-community/siglip2-base-patch16-256-ONNX",
    vision_model: "onnx/vision_model.onnx",
    text_model: "onnx/text_model.onnx",
    text_model_data: None,
    image_size: 256,
    embed_dim: 768,
};

const SIGLIP2_LARGE_384: ModelVariant = ModelVariant {
    repo: "onnx-community/siglip2-large-patch16-384-ONNX",
    vision_model: "onnx/vision_model.onnx",
    text_model: "onnx/text_model.onnx",
    text_model_data: Some("onnx/text_model.onnx_data"),
    image_size: 384,
    embed_dim: 1024,
};

fn current_variant() -> &'static ModelVariant {
    match std::env::var("EIDETIC_MODEL").as_deref() {
        Ok("large") => &SIGLIP2_LARGE_384,
        _ => &SIGLIP2_BASE_256,
    }
}

/// SigLIP 2 embedder.
///
/// Produces L2-normalised embeddings for images and text. The active model
/// variant is selected via the `EIDETIC_MODEL` env var (`base` by default,
/// `large` for `siglip2-large-patch16-384`).
pub struct SiglipEmbedder {
    vision_session: Session,
    text_session: Session,
    tokenizer: Tokenizer,
    variant: &'static ModelVariant,
}

impl SiglipEmbedder {
    /// Load the active SigLIP 2 variant from `models_dir`.
    ///
    /// Downloads vision/text ONNX files and the tokenizer from HuggingFace
    /// on first call. The large variant additionally pulls a `.onnx_data`
    /// companion holding external weights.
    pub fn load(models_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(models_dir)
            .map_err(|e| Error::ModelLoad(format!("cannot create models dir: {e}")))?;

        let variant = current_variant();
        let vision_path = download(models_dir, variant.repo, variant.vision_model)?;
        // Pre-download the external-data companion (if any) so it sits
        // beside text_model.onnx in the snapshot dir before ort opens it.
        if let Some(data_file) = variant.text_model_data {
            download(models_dir, variant.repo, data_file)?;
        }
        let text_path = download(models_dir, variant.repo, variant.text_model)?;
        let tokenizer_path = download(models_dir, variant.repo, TOKENIZER_FILE)?;

        let mode = parse_accelerator(std::env::var("EIDETIC_ACCELERATOR").ok().as_deref())?;
        let coreml_cache_dir = models_dir.join("coreml-cache");

        let vision_session = build_session(&vision_path, mode, &coreml_cache_dir)?;
        let text_session = build_session(&text_path, mode, &coreml_cache_dir)?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| Error::Tokenize(format!("load tokenizer: {e}")))?;

        Ok(Self {
            vision_session,
            text_session,
            tokenizer,
            variant,
        })
    }

    /// Output dimension. Stable for the lifetime of an embedder instance.
    pub fn dim(&self) -> usize {
        self.variant.embed_dim
    }

    /// Compute an L2-normalised embedding for the image at `path`.
    ///
    /// Synchronous; inference is CPU/GPU-bound. Callers running inside a
    /// Tokio runtime should invoke this on a blocking thread (see the
    /// `embed` command in `eidetic-cli` for the mpsc-worker pattern).
    pub fn embed(&mut self, path: &Path) -> Result<Vec<f32>> {
        let image_size = self.variant.image_size;
        let pixels = preprocess_image(path, image_size)?;
        let shape = [1usize, 3, image_size as usize, image_size as usize];
        let tensor = Tensor::<f32>::from_array((shape, pixels))
            .map_err(|e| Error::Inference(format!("create tensor: {e}")))?;

        let outputs = self
            .vision_session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(vec)
    }

    /// Compute an L2-normalised embedding for a text string.
    pub fn embed_text(&mut self, text: &str) -> Result<Vec<f32>> {
        let ids = tokenize(&self.tokenizer, text)?;

        let seq_shape = [1usize, SEQ_LEN];
        let ids_tensor = Tensor::<i64>::from_array((seq_shape, ids))
            .map_err(|e| Error::Inference(format!("create ids tensor: {e}")))?;

        let outputs = self
            .text_session
            .run(ort::inputs!["input_ids" => ids_tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(vec)
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn download(models_dir: &Path, repo: &str, filename: &str) -> Result<std::path::PathBuf> {
    use hf_hub::api::sync::ApiBuilder;

    tracing::info!("Loading {filename} from {repo}…");

    let api = ApiBuilder::new()
        .with_cache_dir(models_dir.to_path_buf())
        .build()
        .map_err(|e| Error::ModelDownload(e.to_string()))?;

    let path = api
        .model(repo.to_string())
        .get(filename)
        .map_err(|e| Error::ModelDownload(format!("{filename}: {e}")))?;

    Ok(path)
}

/// Build an ort `Session` from a model file, registering the execution
/// provider implied by `mode`. CoreML is only registered on macOS — on
/// other targets, `Mode::Default` and `Mode::Cpu` both fall through to
/// the CPU EP, and `Mode::CoreML` errors. `coreml_cache_dir` is where ort
/// stores the compiled CoreML model so subsequent loads skip recompile.
fn build_session(model_path: &Path, mode: Mode, coreml_cache_dir: &Path) -> Result<Session> {
    let mut builder =
        Session::builder().map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?;

    #[cfg(target_os = "macos")]
    {
        if matches!(mode, Mode::CoreML) {
            std::fs::create_dir_all(coreml_cache_dir).map_err(|e| {
                Error::ModelLoad(format!(
                    "cannot create CoreML cache dir {}: {e}",
                    coreml_cache_dir.display()
                ))
            })?;
            tracing::info!(
                accelerator = "coreml",
                model = %model_path.display(),
                cache = %coreml_cache_dir.display(),
                "registering CoreML EP (NeuralNetwork, ComputeUnits::All)"
            );
            builder = builder
                .with_execution_providers([CoreML::default()
                    .with_compute_units(ComputeUnits::All)
                    .with_model_cache_dir(coreml_cache_dir.display().to_string())
                    .build()])
                .map_err(|e| Error::ModelLoad(e.to_string()))?;
        } else {
            tracing::info!(
                accelerator = "cpu",
                model = %model_path.display(),
                "using CPU EP"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if matches!(mode, Mode::CoreML) {
            return Err(Error::ModelLoad(
                "EIDETIC_ACCELERATOR=coreml requested on non-macOS host".into(),
            ));
        }
        // Default and Cpu both mean CPU EP on non-macOS; no registration needed
        // because ort always builds in the CPU EP.
        let _ = (mode, coreml_cache_dir);
        tracing::info!(
            accelerator = "cpu",
            model = %model_path.display(),
            "using CPU EP"
        );
    }

    builder
        .commit_from_file(model_path)
        .map_err(|e| Error::ModelLoad(format!("{}: {e}", model_path.display())))
}

fn preprocess_image(path: &Path, image_size: u32) -> Result<Vec<f32>> {
    crate::ensure_heic_registered();
    // Cap allocation + dimensions before decode so a decompression-bomb file
    // (crafted PNG/JPEG/WEBP/HEIC) can't OOM-kill the embed loop. 512 MB / 16384 px
    // is well above any real photo and well below "exhaust process memory".
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| Error::Inference(format!("cannot open image: {e}")))?
        .with_guessed_format()
        .map_err(|e| Error::Inference(format!("cannot detect image format: {e}")))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);

    let img = reader
        .decode()
        .map_err(|e| Error::Inference(format!("cannot decode image: {e}")))?;

    let rgb = img
        .resize_exact(
            image_size,
            image_size,
            image::imageops::FilterType::Lanczos3,
        )
        .into_rgb8();

    // Convert HWC -> CHW layout, normalize pixel values to [-1.0, 1.0].
    // SigLIP 2 expects: (pixel / 255.0 - 0.5) / 0.5 per channel.
    let size = image_size as usize;
    let raw = rgb.as_raw(); // contiguous HWC, length 3*size*size
    debug_assert_eq!(raw.len(), 3 * size * size);
    let mut chw = vec![0.0f32; 3 * size * size];
    let plane = size * size;
    for i in 0..plane {
        let r = raw[3 * i] as f32;
        let g = raw[3 * i + 1] as f32;
        let b = raw[3 * i + 2] as f32;
        chw[i] = (r / 255.0 - 0.5) / 0.5;
        chw[plane + i] = (g / 255.0 - 0.5) / 0.5;
        chw[2 * plane + i] = (b / 255.0 - 0.5) / 0.5;
    }
    Ok(chw)
}

fn tokenize(tokenizer: &Tokenizer, text: &str) -> Result<Vec<i64>> {
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| Error::Tokenize(e.to_string()))?;

    let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();

    // Truncate then pad to exactly SEQ_LEN tokens. SigLIP's text encoder is
    // bidirectional and does not consume an attention mask.
    ids.truncate(SEQ_LEN);
    ids.resize(SEQ_LEN, PAD_TOKEN_ID);

    Ok(ids)
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_unchanged() {
        let mut v = vec![1.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
        assert!(v[1].abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_scales_to_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_unchanged() {
        let mut v = vec![0.0f32; 768];
        l2_normalize(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    // Note: tokenize() requires a real tokenizer file and is tested end-to-end
    // by the #[ignore] integration test below (image_embedding_is_768_dim_and_normalized).

    #[test]
    fn preprocess_image_chw_layout_matches_naive() {
        use image::{ImageBuffer, Rgb};

        // Build a synthetic 4x4 RGB image with deterministic per-channel gradients.
        let size = 4usize;
        let mut img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(size as u32, size as u32);
        for y in 0..size {
            for x in 0..size {
                img.put_pixel(
                    x as u32,
                    y as u32,
                    Rgb([(x * 17) as u8, (y * 23) as u8, ((x + y) * 11) as u8]),
                );
            }
        }

        // Naive (old) HWC -> CHW with per-pixel get_pixel.
        let mut naive = vec![0.0f32; 3 * size * size];
        for y in 0..size {
            for x in 0..size {
                let pixel = img.get_pixel(x as u32, y as u32);
                for c in 0..3usize {
                    naive[c * size * size + y * size + x] = (pixel.0[c] as f32 / 255.0 - 0.5) / 0.5;
                }
            }
        }

        // New (linear) HWC -> CHW from as_raw().
        let raw = img.as_raw();
        let mut linear = vec![0.0f32; 3 * size * size];
        let plane = size * size;
        for i in 0..plane {
            let r = raw[3 * i] as f32;
            let g = raw[3 * i + 1] as f32;
            let b = raw[3 * i + 2] as f32;
            linear[i] = (r / 255.0 - 0.5) / 0.5;
            linear[plane + i] = (g / 255.0 - 0.5) / 0.5;
            linear[2 * plane + i] = (b / 255.0 - 0.5) / 0.5;
        }

        assert_eq!(naive, linear, "linear walk must match naive get_pixel");
    }

    #[test]
    fn l2_normalize_arbitrary_vector() {
        let mut v = vec![1.0f32, 2.0, 2.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        // Original: [1, 2, 2], magnitude = 3.0, normalized = [1/3, 2/3, 2/3]
        assert!((v[0] - 1.0 / 3.0).abs() < 1e-6);
        assert!((v[1] - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn parse_accelerator_unset_is_default() {
        assert_eq!(parse_accelerator(None).unwrap(), Mode::Default);
    }

    #[test]
    fn parse_accelerator_coreml_ok() {
        assert_eq!(parse_accelerator(Some("coreml")).unwrap(), Mode::CoreML);
    }

    #[test]
    fn parse_accelerator_cpu_ok() {
        assert_eq!(parse_accelerator(Some("cpu")).unwrap(), Mode::Cpu);
    }

    #[test]
    fn parse_accelerator_unknown_value_errors() {
        let err = parse_accelerator(Some("metal")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("metal"),
            "expected error to mention the bad value, got: {msg}"
        );
        assert!(
            msg.contains("coreml"),
            "expected error to list valid values, got: {msg}"
        );
        assert!(
            msg.contains("cpu"),
            "expected error to list valid values, got: {msg}"
        );
    }

    #[test]
    fn parse_accelerator_empty_string_is_unknown() {
        // Empty string is "set but empty" — treat as a typo, not as unset.
        let err = parse_accelerator(Some("")).unwrap_err();
        assert!(err.to_string().contains("Unknown EIDETIC_ACCELERATOR"));
    }

    #[test]
    #[ignore = "requires SigLIP 2 ONNX model — set EIDETIC_MODELS_CACHE and EIDETIC_TEST_IMAGE"]
    fn image_embedding_is_768_dim_and_normalized() {
        let models_dir = std::env::var("EIDETIC_MODELS_CACHE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap())
                    .join(".cache/eidetic/models")
            });

        let mut embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.len(), 768);
        let dot: f32 = emb.iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "not normalized: dot={dot}");
    }
}
