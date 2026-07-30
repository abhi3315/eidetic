//! Face detection, alignment and embedding (ADR-0010).
//!
//! Default stack is YuNet (MIT) for detection and SFace (Apache-2.0) for
//! embedding, both from OpenCV Zoo. InsightFace SCRFD + ArcFace is an opt-in
//! because its weights are licensed for non-commercial research only — see
//! the ADR for why that rules them out as a default here.

mod align;
mod analyzer;
mod detect;

pub use align::{ALIGNED_SIZE, align_face};
pub use analyzer::{AnalyzedFace, FaceAnalyzer};
pub use detect::Detection;

/// A face box in original-image pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl BoundingBox {
    /// Longest side in pixels — the cheap quality signal. ADR-0010 calls for
    /// filtering tiny faces out before they can seed a cluster.
    pub fn max_side(&self) -> f32 {
        self.width.max(self.height)
    }
}

/// The detector's five keypoints, stored by anatomical meaning rather than by
/// wire order.
///
/// This matters: YuNet emits **right** eye first, while the ArcFace alignment
/// template expects **left** eye first. Naming the fields means the swap
/// happens exactly once, at [`Landmarks::from_yunet_order`], instead of being
/// an index convention every call site has to remember. Getting it wrong
/// mirrors every aligned crop, which silently degrades embeddings without
/// raising any error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landmarks {
    pub left_eye: (f32, f32),
    pub right_eye: (f32, f32),
    pub nose: (f32, f32),
    pub left_mouth: (f32, f32),
    pub right_mouth: (f32, f32),
}

impl Landmarks {
    /// Reorder YuNet's raw head output, which is
    /// `[right_eye, left_eye, nose, right_mouth, left_mouth]`.
    ///
    /// That order is documented for OpenCV's `FaceDetectorYN` wrapper rather
    /// than for the raw tensors, so ADR-0010 lists it as needing confirmation
    /// against a real face.
    pub fn from_yunet_order(points: [(f32, f32); 5]) -> Self {
        Self {
            right_eye: points[0],
            left_eye: points[1],
            nose: points[2],
            right_mouth: points[3],
            left_mouth: points[4],
        }
    }

    /// In the order the ArcFace/SFace alignment template expects.
    pub fn as_template_order(&self) -> [(f32, f32); 5] {
        [
            self.left_eye,
            self.right_eye,
            self.nose,
            self.left_mouth,
            self.right_mouth,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yunet_order_is_swapped_into_template_order() {
        let raw = [
            (1.0, 1.0), // right eye on the wire
            (2.0, 2.0), // left eye on the wire
            (3.0, 3.0), // nose
            (4.0, 4.0), // right mouth on the wire
            (5.0, 5.0), // left mouth on the wire
        ];
        let lm = Landmarks::from_yunet_order(raw);

        assert_eq!(lm.right_eye, (1.0, 1.0));
        assert_eq!(lm.left_eye, (2.0, 2.0));

        // Template order must lead with the LEFT eye, i.e. the second wire point.
        let t = lm.as_template_order();
        assert_eq!(t[0], (2.0, 2.0), "template must start with left eye");
        assert_eq!(t[1], (1.0, 1.0));
        assert_eq!(t[2], (3.0, 3.0));
        assert_eq!(t[3], (5.0, 5.0), "left mouth before right");
        assert_eq!(t[4], (4.0, 4.0));
    }

    #[test]
    fn max_side_picks_the_longer_edge() {
        let b = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 45.0,
        };
        assert_eq!(b.max_side(), 45.0);
    }
}
