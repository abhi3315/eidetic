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
/// Naming traps, learned the hard way: YuNet's wire order leads with the
/// **subject's** right eye, while the ArcFace template's first point
/// `(38.29, 51.69)` is the **image-left** eye — which is the *same* eye,
/// because a frontal subject's right eye appears on the viewer's left. The
/// InsightFace convention names template points in image space, so its
/// "left eye first" and YuNet's "right eye first" agree, and the two orders
/// compose to the identity. An earlier version swapped eyes and mouths here;
/// a similarity transform cannot represent that mirrored correspondence, so
/// every crop came out scale-collapsed (~0.24x instead of ~0.85x on a real
/// portrait) and same-person cosine similarity dropped from ~0.6 to ~0.39 —
/// no error raised anywhere. Verified against LFW: see the
/// `lfw_same_person_similarity_beats_different_person` test.
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
    ///
    /// The template is named in **image space**: its first point sits at
    /// image-left, which for a frontal face is the subject's *right* eye.
    /// Anatomically-named fields therefore emit right-before-left here.
    pub fn as_template_order(&self) -> [(f32, f32); 5] {
        [
            self.right_eye,
            self.left_eye,
            self.nose,
            self.right_mouth,
            self.left_mouth,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yunet_wire_order_passes_through_to_template_order() {
        let raw = [
            (1.0, 1.0), // subject's right eye on the wire — image-left
            (2.0, 2.0), // subject's left eye on the wire — image-right
            (3.0, 3.0), // nose
            (4.0, 4.0), // subject's right mouth corner — image-left
            (5.0, 5.0), // subject's left mouth corner — image-right
        ];
        let lm = Landmarks::from_yunet_order(raw);

        assert_eq!(lm.right_eye, (1.0, 1.0));
        assert_eq!(lm.left_eye, (2.0, 2.0));

        // Both the wire and the template lead with the image-left point (the
        // subject's RIGHT eye), so composing the two mappings is the identity.
        // Swapping here mirrors the correspondence, which a similarity
        // transform cannot fit — crops come out scale-collapsed.
        let t = lm.as_template_order();
        assert_eq!(t[0], (1.0, 1.0), "template leads with the image-left eye");
        assert_eq!(t[1], (2.0, 2.0));
        assert_eq!(t[2], (3.0, 3.0));
        assert_eq!(t[3], (4.0, 4.0), "image-left mouth corner before right");
        assert_eq!(t[4], (5.0, 5.0));
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
