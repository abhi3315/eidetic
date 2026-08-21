//! YuNet face detection (ADR-0010).
//!
//! The raw ONNX exposes 12 output tensors — `cls_`, `obj_`, `bbox_` and `kps_`
//! at strides 8/16/32 — because we deliberately bypass OpenCV's
//! `FaceDetectorYN` wrapper. Decoding and NMS therefore live here.
//!
//! Signatures were measured by loading the real file, not read off the model
//! card (the card is inconsistent about input size). Input is a fixed
//! `[1,3,640,640]`; each stride has one anchor per cell with the prior at the
//! cell centre, so the row counts are 80²=6400, 40²=1600 and 20²=400.

use super::{BoundingBox, Landmarks};

/// Strides the model emits, paired with their grid side length at 640x640.
pub(super) const STRIDES: [(u32, usize); 3] = [(8, 80), (16, 40), (32, 20)];

/// The size YuNet's input tensor is fixed at.
pub(super) const INPUT_SIZE: u32 = 640;

/// A face as the detector sees it, in the coordinate space of the original
/// image (decoding scales back out of the 640x640 letterbox).
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub bbox: BoundingBox,
    pub landmarks: Landmarks,
    /// Detector confidence in `[0.0, 1.0]`.
    pub score: f32,
}

/// One stride's four output planes, already flattened.
pub(super) struct StridePlanes<'a> {
    pub stride: u32,
    pub grid: usize,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
    pub kps: &'a [f32],
}

/// Decode one stride's planes into detections above `score_threshold`.
///
/// Mirrors OpenCV's `FaceDetectorYNImpl::postProcess`: centres are offset from
/// the cell index, extents are exponential, and the score is the geometric
/// mean of the classification and objectness heads.
///
/// `scale` converts 640x640 letterbox coordinates back to original-image
/// pixels; `(pad_x, pad_y)` is the letterbox offset to subtract first.
pub(super) fn decode_stride(
    planes: &StridePlanes<'_>,
    score_threshold: f32,
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    out: &mut Vec<Detection>,
) {
    let grid = planes.grid;
    let stride = planes.stride as f32;

    for row in 0..grid {
        for col in 0..grid {
            let idx = row * grid + col;

            // Geometric mean of the two heads, each clamped first — the raw
            // logits can sit marginally outside [0,1] and a negative product
            // would make sqrt() produce NaN.
            let cls = planes.cls[idx].clamp(0.0, 1.0);
            let obj = planes.obj[idx].clamp(0.0, 1.0);
            let score = (cls * obj).sqrt();
            if score < score_threshold {
                continue;
            }

            let b = &planes.bbox[idx * 4..idx * 4 + 4];
            let cx = (col as f32 + b[0]) * stride;
            let cy = (row as f32 + b[1]) * stride;
            let w = b[2].exp() * stride;
            let h = b[3].exp() * stride;

            let to_orig = |x: f32, y: f32| ((x - pad_x) / scale, (y - pad_y) / scale);
            let (x, y) = to_orig(cx - w / 2.0, cy - h / 2.0);

            let mut points = [(0.0f32, 0.0f32); 5];
            for (i, point) in points.iter_mut().enumerate() {
                let kx = (col as f32 + planes.kps[idx * 10 + 2 * i]) * stride;
                let ky = (row as f32 + planes.kps[idx * 10 + 2 * i + 1]) * stride;
                *point = to_orig(kx, ky);
            }

            out.push(Detection {
                bbox: BoundingBox {
                    x,
                    y,
                    width: w / scale,
                    height: h / scale,
                },
                landmarks: Landmarks::from_yunet_order(points),
                score,
            });
        }
    }
}

/// Greedy non-maximum suppression by descending score.
///
/// Detections are few after thresholding (a photo has a handful of faces, not
/// thousands), so the quadratic pass is not worth optimising.
pub(super) fn non_max_suppression(mut dets: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    dets.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));

    let mut kept: Vec<Detection> = Vec::new();
    for det in dets {
        if kept.iter().any(|k| iou(&k.bbox, &det.bbox) > iou_threshold) {
            continue;
        }
        kept.push(det);
    }
    kept
}

/// Intersection over union of two boxes. Returns 0.0 when they don't overlap.
fn iou(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width).min(b.x + b.width);
    let y2 = (a.y + a.height).min(b.y + b.height);

    let inter_w = (x2 - x1).max(0.0);
    let inter_h = (y2 - y1).max(0.0);
    let inter = inter_w * inter_h;
    if inter <= 0.0 {
        return 0.0;
    }

    let union = a.width * a.height + b.width * b.height - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(x: f32, y: f32, w: f32, h: f32) -> BoundingBox {
        BoundingBox {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn det(x: f32, y: f32, w: f32, h: f32, score: f32) -> Detection {
        Detection {
            bbox: bbox(x, y, w, h),
            landmarks: Landmarks::from_yunet_order([(0.0, 0.0); 5]),
            score,
        }
    }

    #[test]
    fn iou_identical_boxes_is_one() {
        let a = bbox(10.0, 10.0, 20.0, 20.0);
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_boxes_is_zero() {
        let a = bbox(0.0, 0.0, 10.0, 10.0);
        let b = bbox(100.0, 100.0, 10.0, 10.0);
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn iou_half_overlap() {
        // Two 10x10 boxes sharing a 5x10 strip: inter 50, union 150.
        let a = bbox(0.0, 0.0, 10.0, 10.0);
        let b = bbox(5.0, 0.0, 10.0, 10.0);
        assert!((iou(&a, &b) - 50.0 / 150.0).abs() < 1e-6);
    }

    #[test]
    fn nms_keeps_highest_score_of_an_overlapping_pair() {
        let dets = vec![
            det(0.0, 0.0, 10.0, 10.0, 0.7),
            det(1.0, 1.0, 10.0, 10.0, 0.9), // heavily overlaps, better score
        ];
        let kept = non_max_suppression(dets, 0.3);
        assert_eq!(kept.len(), 1);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn nms_keeps_distinct_faces() {
        let dets = vec![
            det(0.0, 0.0, 10.0, 10.0, 0.9),
            det(500.0, 500.0, 10.0, 10.0, 0.8),
        ];
        assert_eq!(non_max_suppression(dets, 0.3).len(), 2);
    }

    /// Build single-cell planes so the decode arithmetic is checkable by hand.
    #[test]
    fn decode_stride_recovers_known_box() {
        // 1x1 grid at stride 32. Centre offset 0.5 in both axes, extents
        // exp(0) = 1 cell.
        let cls = [1.0f32];
        let obj = [1.0f32];
        let bbox_v = [0.5f32, 0.5, 0.0, 0.0];
        let kps_v = [0.5f32; 10];

        let planes = StridePlanes {
            stride: 32,
            grid: 1,
            cls: &cls,
            obj: &obj,
            bbox: &bbox_v,
            kps: &kps_v,
        };

        let mut out = Vec::new();
        decode_stride(&planes, 0.5, 1.0, 0.0, 0.0, &mut out);

        assert_eq!(out.len(), 1);
        let d = &out[0];
        // centre = (0 + 0.5) * 32 = 16; w = h = exp(0) * 32 = 32
        // so top-left = 16 - 16 = 0
        assert!((d.bbox.x - 0.0).abs() < 1e-4, "x was {}", d.bbox.x);
        assert!((d.bbox.y - 0.0).abs() < 1e-4, "y was {}", d.bbox.y);
        assert!((d.bbox.width - 32.0).abs() < 1e-4);
        assert!((d.bbox.height - 32.0).abs() < 1e-4);
        assert!((d.score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decode_stride_applies_letterbox_inverse() {
        let cls = [1.0f32];
        let obj = [1.0f32];
        let bbox_v = [0.5f32, 0.5, 0.0, 0.0];
        let kps_v = [0.5f32; 10];
        let planes = StridePlanes {
            stride: 32,
            grid: 1,
            cls: &cls,
            obj: &obj,
            bbox: &bbox_v,
            kps: &kps_v,
        };

        let mut out = Vec::new();
        // Half scale with a 10px horizontal pad: x_orig = (0 - 10) / 0.5
        decode_stride(&planes, 0.5, 0.5, 10.0, 0.0, &mut out);

        let d = &out[0];
        assert!((d.bbox.x - (-20.0)).abs() < 1e-4, "x was {}", d.bbox.x);
        assert!((d.bbox.width - 64.0).abs() < 1e-4, "w was {}", d.bbox.width);
    }

    #[test]
    fn decode_stride_drops_low_scores() {
        let cls = [0.1f32];
        let obj = [0.1f32];
        let bbox_v = [0.5f32, 0.5, 0.0, 0.0];
        let kps_v = [0.5f32; 10];
        let planes = StridePlanes {
            stride: 32,
            grid: 1,
            cls: &cls,
            obj: &obj,
            bbox: &bbox_v,
            kps: &kps_v,
        };

        let mut out = Vec::new();
        decode_stride(&planes, 0.5, 1.0, 0.0, 0.0, &mut out);
        assert!(out.is_empty(), "score 0.1 should fall below threshold 0.5");
    }

    #[test]
    fn decode_stride_clamps_out_of_range_logits() {
        // Slightly-negative logits must not produce NaN through sqrt().
        let cls = [-0.01f32];
        let obj = [1.2f32];
        let bbox_v = [0.5f32, 0.5, 0.0, 0.0];
        let kps_v = [0.5f32; 10];
        let planes = StridePlanes {
            stride: 32,
            grid: 1,
            cls: &cls,
            obj: &obj,
            bbox: &bbox_v,
            kps: &kps_v,
        };

        let mut out = Vec::new();
        decode_stride(&planes, 0.0, 1.0, 0.0, 0.0, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].score.is_finite(), "score must not be NaN");
        assert_eq!(out[0].score, 0.0);
    }
}
