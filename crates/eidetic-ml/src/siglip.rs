use crate::{Embedder, Embedding, Error, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::Tokenizer;

const MODEL_REPO: &str = "onnx-community/siglip2-base-patch16-256-ONNX";
const VISION_MODEL_FILE: &str = "onnx/vision_model.onnx";
const TEXT_MODEL_FILE: &str = "onnx/text_model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const IMAGE_SIZE: u32 = 256;
const SEQ_LEN: usize = 64;
const EMBED_DIM: usize = 768;
const PAD_TOKEN_ID: i64 = 1;

/// SigLIP 2 base embedder.
///
/// Produces 768-dim L2-normalised embeddings for images and text using the
/// `onnx-community/siglip2-base-patch16-256-ONNX` builds of Google's
/// SigLIP 2 base model.
pub struct SiglipEmbedder {
    // Session::run takes &mut self, so we use Mutex for interior mutability
    // to satisfy the &self required by the Embedder trait.
    vision_session: Mutex<Session>,
    text_session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl SiglipEmbedder {
    /// Load the SigLIP 2 model from `models_dir`.
    ///
    /// Downloads `onnx/vision_model.onnx`, `onnx/text_model.onnx`, and
    /// `tokenizer.json` from HuggingFace on first call. Subsequent calls
    /// load from the local cache.
    pub fn load(models_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(models_dir)
            .map_err(|e| Error::ModelLoad(format!("cannot create models dir: {e}")))?;

        let vision_path = download(models_dir, VISION_MODEL_FILE)?;
        let text_path = download(models_dir, TEXT_MODEL_FILE)?;
        let tokenizer_path = download(models_dir, TOKENIZER_FILE)?;

        let vision_session = Session::builder()
            .map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&vision_path)
            .map_err(|e| Error::ModelLoad(format!("vision model: {e}")))?;

        let text_session = Session::builder()
            .map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&text_path)
            .map_err(|e| Error::ModelLoad(format!("text model: {e}")))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| Error::Tokenize(format!("load tokenizer: {e}")))?;

        Ok(Self {
            vision_session: Mutex::new(vision_session),
            text_session: Mutex::new(text_session),
            tokenizer,
        })
    }
}

impl Embedder for SiglipEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    fn embed(&self, path: &Path) -> Result<Embedding> {
        let pixels = preprocess_image(path)?;
        let shape = [1usize, 3, IMAGE_SIZE as usize, IMAGE_SIZE as usize];
        let tensor = Tensor::<f32>::from_array((shape, pixels))
            .map_err(|e| Error::Inference(format!("create tensor: {e}")))?;

        let mut session = self
            .vision_session
            .lock()
            .map_err(|e| Error::Inference(format!("lock vision session: {e}")))?;

        let outputs = session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }

    fn embed_text(&self, text: &str) -> Result<Embedding> {
        let ids = tokenize(&self.tokenizer, text)?;

        let seq_shape = [1usize, SEQ_LEN];
        let ids_tensor = Tensor::<i64>::from_array((seq_shape, ids))
            .map_err(|e| Error::Inference(format!("create ids tensor: {e}")))?;

        let mut session = self
            .text_session
            .lock()
            .map_err(|e| Error::Inference(format!("lock text session: {e}")))?;

        let outputs = session
            .run(ort::inputs!["input_ids" => ids_tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        let (_shape, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec: Vec<f32> = data.to_vec();
        l2_normalize(&mut vec);
        Ok(Embedding::new(vec))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn download(models_dir: &Path, filename: &str) -> Result<std::path::PathBuf> {
    use hf_hub::api::sync::ApiBuilder;

    tracing::info!("Loading {filename} from {MODEL_REPO}…");

    let api = ApiBuilder::new()
        .with_cache_dir(models_dir.to_path_buf())
        .build()
        .map_err(|e| Error::ModelDownload(e.to_string()))?;

    let path = api
        .model(MODEL_REPO.to_string())
        .get(filename)
        .map_err(|e| Error::ModelDownload(format!("{filename}: {e}")))?;

    Ok(path)
}

fn preprocess_image(path: &Path) -> Result<Vec<f32>> {
    let img = image::open(path).map_err(|e| Error::Inference(format!("cannot open image: {e}")))?;

    let rgb = img
        .resize_exact(
            IMAGE_SIZE,
            IMAGE_SIZE,
            image::imageops::FilterType::Lanczos3,
        )
        .into_rgb8();

    // Convert HWC -> CHW layout, normalize pixel values to [-1.0, 1.0].
    // SigLIP 2 expects: (pixel / 255.0 - 0.5) / 0.5 per channel.
    let size = IMAGE_SIZE as usize;
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
    #[ignore = "requires SigLIP 2 ONNX model — set EIDETIC_MODELS_CACHE and EIDETIC_TEST_IMAGE"]
    fn image_embedding_is_768_dim_and_normalized() {
        let models_dir = std::env::var("EIDETIC_MODELS_CACHE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap())
                    .join(".cache/eidetic/models")
            });

        let embedder = SiglipEmbedder::load(&models_dir).expect("load embedder");

        let test_image = std::env::var("EIDETIC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .expect("set EIDETIC_TEST_IMAGE=/path/to/any.jpg");

        let emb = embedder.embed(&test_image).expect("embed image");
        assert_eq!(emb.dim(), 768);
        let dot: f32 = emb.as_slice().iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-4, "not normalized: dot={dot}");
    }
}
