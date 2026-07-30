//! Loads the two ONNX sessions and runs detect -> align -> embed (ADR-0010).

use super::align::{self, align_face};
use super::detect::{self, Detection, INPUT_SIZE, STRIDES, StridePlanes};
use crate::{Error, Result};
use image::{DynamicImage, RgbImage, imageops::FilterType};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

/// Detector confidence floor. Below this a "face" is usually texture noise.
const DEFAULT_SCORE_THRESHOLD: f32 = 0.6;
/// IoU above which two boxes are treated as the same face.
const DEFAULT_NMS_THRESHOLD: f32 = 0.3;
/// Faces whose longest side is under this many pixels are too small to embed
/// reliably. ADR-0010 calls for quality filtering before clustering; this is
/// the cheapest useful signal.
const MIN_FACE_PIXELS: f32 = 32.0;

#[derive(Debug)]
struct ModelPair {
    detector_repo: &'static str,
    detector_file: &'static str,
    embedder_repo: &'static str,
    embedder_file: &'static str,
    embed_dim: usize,
}

/// Permissive default: YuNet (MIT) + SFace (Apache-2.0), both OpenCV Zoo.
const YUNET_SFACE: ModelPair = ModelPair {
    detector_repo: "opencv/face_detection_yunet",
    detector_file: "face_detection_yunet_2023mar.onnx",
    embedder_repo: "opencv/face_recognition_sface",
    embedder_file: "face_recognition_sface_2021dec.onnx",
    embed_dim: 128,
};

/// Opt-in: InsightFace SCRFD + ArcFace. Better accuracy, but the weights are
/// licensed for non-commercial research only — the user chooses this knowingly.
const BUFFALO_L: ModelPair = ModelPair {
    detector_repo: "immich-app/buffalo_l",
    detector_file: "detection/model.onnx",
    embedder_repo: "immich-app/buffalo_l",
    embedder_file: "recognition/model.onnx",
    embed_dim: 512,
};

fn pair_for(raw: Option<&str>) -> Result<&'static ModelPair> {
    match raw {
        None | Some("yunet") => Ok(&YUNET_SFACE),
        Some("buffalo_l") => Ok(&BUFFALO_L),
        Some(other) => Err(Error::ModelLoad(format!(
            "Unknown EIDETIC_FACE_MODEL={other:?}; valid values: yunet, buffalo_l"
        ))),
    }
}

/// A detected face plus its embedding.
#[derive(Debug, Clone)]
pub struct AnalyzedFace {
    pub detection: Detection,
    /// L2-normalised, so cosine similarity is a plain dot product.
    pub embedding: Vec<f32>,
}

/// Owns the detection and recognition sessions.
///
/// Synchronous, like [`crate::SiglipEmbedder`]: callers inside a Tokio runtime
/// should drive it from a blocking worker.
pub struct FaceAnalyzer {
    detector: Session,
    embedder: Session,
    pair: &'static ModelPair,
    score_threshold: f32,
    min_face_pixels: f32,
}

impl FaceAnalyzer {
    /// Load the active model pair, downloading on first use.
    ///
    /// Selected by `EIDETIC_FACE_MODEL` (`yunet` default, `buffalo_l` opt-in).
    /// Unknown values are rejected rather than silently falling back, matching
    /// `EIDETIC_MODEL` and `EIDETIC_ACCELERATOR`.
    pub fn load(models_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(models_dir)
            .map_err(|e| Error::ModelLoad(format!("cannot create models dir: {e}")))?;

        let pair = pair_for(std::env::var("EIDETIC_FACE_MODEL").ok().as_deref())?;

        if std::ptr::eq(pair, &BUFFALO_L) {
            tracing::warn!(
                "EIDETIC_FACE_MODEL=buffalo_l uses InsightFace weights, which upstream \
                 licenses for non-commercial research use only. You are downloading them \
                 yourself; eidetic neither bundles nor redistributes them. See \
                 docs/adr/0010-face-stack.md."
            );
        }

        let detector_path =
            crate::siglip::download(models_dir, pair.detector_repo, pair.detector_file)?;
        let embedder_path =
            crate::siglip::download(models_dir, pair.embedder_repo, pair.embedder_file)?;

        Ok(Self {
            detector: crate::siglip::session_for_current_accelerator(&detector_path, models_dir)?,
            embedder: crate::siglip::session_for_current_accelerator(&embedder_path, models_dir)?,
            pair,
            score_threshold: DEFAULT_SCORE_THRESHOLD,
            min_face_pixels: MIN_FACE_PIXELS,
        })
    }

    pub fn embed_dim(&self) -> usize {
        self.pair.embed_dim
    }

    /// Detect every face in `image`, align it, and embed it.
    ///
    /// Faces below the size floor are dropped: a 20px face produces an
    /// embedding that is mostly noise and would poison a cluster.
    pub fn analyze(&mut self, image: &DynamicImage) -> Result<Vec<AnalyzedFace>> {
        let detections = self.detect(image)?;

        let mut out = Vec::with_capacity(detections.len());
        for detection in detections {
            if detection.bbox.max_side() < self.min_face_pixels {
                continue;
            }
            let Some(aligned) = align_face(image, &detection.landmarks) else {
                // Degenerate landmarks; nothing sensible to embed.
                continue;
            };
            let embedding = self.embed_aligned(&aligned)?;
            out.push(AnalyzedFace {
                detection,
                embedding,
            });
        }
        Ok(out)
    }

    /// Run the detector and decode all three strides.
    fn detect(&mut self, image: &DynamicImage) -> Result<Vec<Detection>> {
        let (input, scale) = letterbox(image);
        let shape = [1usize, 3, INPUT_SIZE as usize, INPUT_SIZE as usize];
        let tensor = Tensor::<f32>::from_array((shape, input))
            .map_err(|e| Error::Inference(format!("create detector tensor: {e}")))?;

        let outputs = self
            .detector
            .run(ort::inputs!["input" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        // Pull all twelve planes first; decoding borrows them.
        let mut planes = Vec::with_capacity(STRIDES.len());
        for (stride, grid) in STRIDES {
            let get = |prefix: &str| -> Result<Vec<f32>> {
                let name = format!("{prefix}_{stride}");
                let (_shape, data) = outputs[name.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| Error::Inference(format!("{name}: {e}")))?;
                Ok(data.to_vec())
            };
            planes.push((
                stride,
                grid,
                get("cls")?,
                get("obj")?,
                get("bbox")?,
                get("kps")?,
            ));
        }

        let mut dets = Vec::new();
        for (stride, grid, cls, obj, bbox, kps) in &planes {
            detect::decode_stride(
                &StridePlanes {
                    stride: *stride,
                    grid: *grid,
                    cls,
                    obj,
                    bbox,
                    kps,
                },
                self.score_threshold,
                scale,
                // Top-left placement, so there is no offset to undo.
                0.0,
                0.0,
                &mut dets,
            );
        }

        Ok(detect::non_max_suppression(dets, DEFAULT_NMS_THRESHOLD))
    }

    /// Embed one aligned 112x112 crop.
    fn embed_aligned(&mut self, aligned: &RgbImage) -> Result<Vec<f32>> {
        let pixels = align::to_input_tensor(aligned);
        let size = align::ALIGNED_SIZE as usize;
        let tensor = Tensor::<f32>::from_array(([1usize, 3, size, size], pixels))
            .map_err(|e| Error::Inference(format!("create embedder tensor: {e}")))?;

        let outputs = self
            .embedder
            .run(ort::inputs!["data" => tensor])
            .map_err(|e: ort::Error| Error::Inference(e.to_string()))?;

        // SFace names its output `fc1`; take the sole output rather than
        // hard-coding a name that differs across the two model pairs. Bound the
        // borrow to a local so the temporary outlives the extract.
        let (_name, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| Error::Inference("embedder produced no output".into()))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Inference(e.to_string()))?;

        let mut vec = data.to_vec();
        if vec.len() != self.pair.embed_dim {
            return Err(Error::Inference(format!(
                "expected {}-dim face embedding, model produced {}",
                self.pair.embed_dim,
                vec.len()
            )));
        }
        l2_normalize(&mut vec);
        Ok(vec)
    }
}

/// Resize into a fixed 640x640 buffer, preserving aspect ratio and placing the
/// image top-left so decoding only has to undo `scale`.
///
/// Returns the CHW tensor and the scale factor applied.
///
/// **Channel order and value range are the unverified part of this pipeline.**
/// YuNet ships through OpenCV, whose images are BGR with raw 0-255 values and
/// no mean/std normalisation, so that is what we feed it. Getting this wrong
/// does not error — it just makes detection quietly poor — so ADR-0010 lists it
/// as needing confirmation against a real photo.
fn letterbox(image: &DynamicImage) -> (Vec<f32>, f32) {
    let (w, h) = (image.width().max(1), image.height().max(1));
    let scale = (INPUT_SIZE as f32 / w as f32).min(INPUT_SIZE as f32 / h as f32);
    let new_w = ((w as f32 * scale).round() as u32).clamp(1, INPUT_SIZE);
    let new_h = ((h as f32 * scale).round() as u32).clamp(1, INPUT_SIZE);

    let resized = image
        .resize_exact(new_w, new_h, FilterType::Triangle)
        .to_rgb8();

    let size = INPUT_SIZE as usize;
    let plane = size * size;
    let mut chw = vec![0.0f32; 3 * plane];

    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let px = resized.get_pixel(x as u32, y as u32);
            let idx = y * size + x;
            // BGR, matching OpenCV's channel order.
            chw[idx] = px.0[2] as f32;
            chw[plane + idx] = px.0[1] as f32;
            chw[2 * plane + idx] = px.0[0] as f32;
        }
    }

    (chw, scale)
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
    fn pair_for_known_values() {
        assert_eq!(pair_for(None).unwrap().embed_dim, 128);
        assert_eq!(pair_for(Some("yunet")).unwrap().embed_dim, 128);
        assert_eq!(pair_for(Some("buffalo_l")).unwrap().embed_dim, 512);
    }

    #[test]
    fn pair_for_unknown_value_errors() {
        let err = pair_for(Some("scrfd")).unwrap_err().to_string();
        assert!(err.contains("scrfd"), "should echo the bad value: {err}");
        assert!(err.contains("buffalo_l"), "should list valid values: {err}");
    }

    #[test]
    fn letterbox_preserves_aspect_and_fills_target() {
        // 1280x640 halves to 640x320, so scale is 0.5.
        let img = DynamicImage::new_rgb8(1280, 640);
        let (tensor, scale) = letterbox(&img);
        assert!((scale - 0.5).abs() < 1e-6, "scale was {scale}");
        assert_eq!(tensor.len(), 3 * (INPUT_SIZE as usize).pow(2));
    }

    #[test]
    fn letterbox_upscales_small_images() {
        let img = DynamicImage::new_rgb8(64, 64);
        let (_t, scale) = letterbox(&img);
        assert!((scale - 10.0).abs() < 1e-6, "scale was {scale}");
    }

    #[test]
    fn letterbox_channel_order_is_bgr() {
        // Solid red image: R=255. In BGR the FIRST plane is blue (0) and the
        // THIRD is red (255).
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(
            INPUT_SIZE,
            INPUT_SIZE,
            image::Rgb([255, 0, 0]),
        ));
        let (t, _) = letterbox(&img);
        let plane = (INPUT_SIZE as usize).pow(2);
        assert_eq!(t[0], 0.0, "first plane should be blue");
        assert_eq!(t[2 * plane], 255.0, "third plane should be red");
    }

    #[test]
    fn l2_normalize_produces_unit_vector() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_leaves_zero_vector_alone() {
        let mut v = vec![0.0f32; 128];
        l2_normalize(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
    }
}
