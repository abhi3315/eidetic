//! Face alignment: 5-point similarity transform onto the ArcFace template.
//!
//! SFace, ArcFace, AdaFace and friends all expect a *similarity-transformed*
//! 112x112 crop, not a raw bbox crop. Feeding them an unaligned crop costs
//! several points of verification accuracy, so this step is mandatory rather
//! than an optimisation (ADR-0010).
//!
//! The transform is the closed-form 2D Procrustes / Umeyama estimate, giving
//! rotation, uniform scale and translation (4 degrees of freedom) as a
//! least-squares fit over the five point pairs. Deliberately *not* a projective
//! homography: fitting one of those (e.g. `imageproc`'s `from_control_points`)
//! allows shear, which distorts faces.
//!
//! Hand-rolled rather than pulling in `imageproc`: the maths is a dozen lines
//! and the warp is inverse-mapped bilinear sampling, so the dependency would
//! not be earning its place.

use super::Landmarks;
use image::{DynamicImage, Rgb, RgbImage};

/// Side length the recognition models expect.
pub const ALIGNED_SIZE: u32 = 112;

/// Canonical ArcFace 5-point destination template, in 112x112 space.
///
/// Point order is named in **image space**: `[image-left eye, image-right
/// eye, nose, image-left mouth corner, image-right mouth corner]`. For a
/// frontal face the image-left eye is the subject's anatomical *right* eye —
/// see [`super::Landmarks::as_template_order`].
///
/// These constants are the de-facto standard shared by the whole ArcFace
/// lineage; SFace uses the same convention.
const TEMPLATE: [(f32, f32); 5] = [
    (38.2946, 51.6963),
    (73.5318, 51.5014),
    (56.0252, 71.7366),
    (41.5493, 92.3655),
    (70.7299, 92.2041),
];

/// A 2D similarity transform: `q = M * p + t` with `M = [[sc, -ss], [ss, sc]]`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Similarity {
    sc: f32,
    ss: f32,
    tx: f32,
    ty: f32,
}

impl Similarity {
    /// Map a source point forward into destination space.
    fn apply(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            self.sc * x - self.ss * y + self.tx,
            self.ss * x + self.sc * y + self.ty,
        )
    }

    /// Map a destination point back into source space.
    ///
    /// `None` when the transform is degenerate (all landmarks coincident),
    /// which would otherwise divide by zero.
    fn invert(&self) -> Option<Self> {
        let det = self.sc * self.sc + self.ss * self.ss;
        if det <= f32::EPSILON {
            return None;
        }
        // R^-1 for a similarity is the transpose scaled by 1/det.
        let inv_sc = self.sc / det;
        let inv_ss = -self.ss / det;
        Some(Self {
            sc: inv_sc,
            ss: inv_ss,
            tx: -(inv_sc * self.tx - inv_ss * self.ty),
            ty: -(inv_ss * self.tx + inv_sc * self.ty),
        })
    }
}

/// Least-squares similarity transform taking `src` onto `dst`.
///
/// Closed form: with both point sets centred, `s*cos` is the summed dot
/// product over the source variance and `s*sin` the summed cross product.
fn estimate_similarity(src: &[(f32, f32); 5], dst: &[(f32, f32); 5]) -> Option<Similarity> {
    let n = src.len() as f32;

    let (mut sx, mut sy, mut dx, mut dy) = (0.0f32, 0.0, 0.0, 0.0);
    for i in 0..src.len() {
        sx += src[i].0;
        sy += src[i].1;
        dx += dst[i].0;
        dy += dst[i].1;
    }
    let (mu_sx, mu_sy, mu_dx, mu_dy) = (sx / n, sy / n, dx / n, dy / n);

    let mut var_src = 0.0f32;
    let mut dot = 0.0f32;
    let mut cross = 0.0f32;
    for i in 0..src.len() {
        let (px, py) = (src[i].0 - mu_sx, src[i].1 - mu_sy);
        let (qx, qy) = (dst[i].0 - mu_dx, dst[i].1 - mu_dy);
        var_src += px * px + py * py;
        dot += px * qx + py * qy;
        cross += px * qy - py * qx;
    }

    // Degenerate: every landmark landed on the same pixel. Happens on absurd
    // detector output; better to skip the face than to divide by zero.
    if var_src <= f32::EPSILON {
        return None;
    }

    let sc = dot / var_src;
    let ss = cross / var_src;

    Some(Similarity {
        sc,
        ss,
        tx: mu_dx - (sc * mu_sx - ss * mu_sy),
        ty: mu_dy - (ss * mu_sx + sc * mu_sy),
    })
}

/// Bilinear sample with edge clamping, so landmarks near the frame border
/// don't sample out of bounds.
fn sample_bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w == 0 || h == 0 {
        return Rgb([0, 0, 0]);
    }

    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let at = |ix: i64, iy: i64| -> [f32; 3] {
        let cx = ix.clamp(0, w - 1) as u32;
        let cy = iy.clamp(0, h - 1) as u32;
        let p = img.get_pixel(cx, cy);
        [p.0[0] as f32, p.0[1] as f32, p.0[2] as f32]
    };

    let (p00, p10, p01, p11) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );

    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
        out[c] = (top + (bottom - top) * fy).clamp(0.0, 255.0).round() as u8;
    }
    Rgb(out)
}

/// Produce the aligned 112x112 RGB crop for one detected face.
///
/// Returns `None` only when the landmarks are degenerate.
pub fn align_face(image: &DynamicImage, landmarks: &Landmarks) -> Option<RgbImage> {
    let rgb = image.to_rgb8();
    let src = landmarks.as_template_order();
    let forward = estimate_similarity(&src, &TEMPLATE)?;
    // Sampling walks the destination and pulls from the source, so we need the
    // inverse map.
    let inverse = forward.invert()?;

    let mut out = RgbImage::new(ALIGNED_SIZE, ALIGNED_SIZE);
    for dy in 0..ALIGNED_SIZE {
        for dx in 0..ALIGNED_SIZE {
            let (sx, sy) = inverse.apply((dx as f32 + 0.5, dy as f32 + 0.5));
            out.put_pixel(dx, dy, sample_bilinear(&rgb, sx - 0.5, sy - 0.5));
        }
    }
    Some(out)
}

/// Convert an aligned crop into SFace's input tensor layout: CHW,
/// `(px - 127.5) / 128.0`.
pub(super) fn to_input_tensor(aligned: &RgbImage) -> Vec<f32> {
    let size = ALIGNED_SIZE as usize;
    let plane = size * size;
    let raw = aligned.as_raw();
    debug_assert_eq!(raw.len(), 3 * plane);

    let mut chw = vec![0.0f32; 3 * plane];
    for i in 0..plane {
        chw[i] = (raw[3 * i] as f32 - 127.5) / 128.0;
        chw[plane + i] = (raw[3 * i + 1] as f32 - 127.5) / 128.0;
        chw[2 * plane + i] = (raw[3 * i + 2] as f32 - 127.5) / 128.0;
    }
    chw
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Landmarks that already sit exactly on the template. TEMPLATE[0] is the
    /// image-left point, i.e. the subject's anatomical RIGHT eye.
    fn template_landmarks() -> Landmarks {
        Landmarks {
            right_eye: TEMPLATE[0],
            left_eye: TEMPLATE[1],
            nose: TEMPLATE[2],
            right_mouth: TEMPLATE[3],
            left_mouth: TEMPLATE[4],
        }
    }

    #[test]
    fn identity_when_landmarks_match_template() {
        let lm = template_landmarks();
        let t = estimate_similarity(&lm.as_template_order(), &TEMPLATE).expect("non-degenerate");
        assert!(
            (t.sc - 1.0).abs() < 1e-3,
            "scale*cos should be 1, got {}",
            t.sc
        );
        assert!(t.ss.abs() < 1e-3, "scale*sin should be 0, got {}", t.ss);
        assert!(t.tx.abs() < 1e-2 && t.ty.abs() < 1e-2);
    }

    #[test]
    fn recovers_a_known_scale_and_translation() {
        // Source = template scaled 2x then shifted by (30, 40).
        let src: [(f32, f32); 5] =
            std::array::from_fn(|i| (TEMPLATE[i].0 * 2.0 + 30.0, TEMPLATE[i].1 * 2.0 + 40.0));
        let t = estimate_similarity(&src, &TEMPLATE).expect("non-degenerate");

        // Mapping source -> template must halve.
        assert!(
            (t.sc - 0.5).abs() < 1e-3,
            "expected 0.5 scale, got {}",
            t.sc
        );
        assert!(t.ss.abs() < 1e-3, "no rotation expected, got {}", t.ss);

        // Every source point should land on its template counterpart.
        for i in 0..5 {
            let (mx, my) = t.apply(src[i]);
            assert!(
                (mx - TEMPLATE[i].0).abs() < 1e-2 && (my - TEMPLATE[i].1).abs() < 1e-2,
                "point {i} mapped to ({mx}, {my}), wanted {:?}",
                TEMPLATE[i]
            );
        }
    }

    #[test]
    fn recovers_a_rotation() {
        // Rotate the template by 90 degrees about the origin: (x,y) -> (-y,x).
        let src: [(f32, f32); 5] = std::array::from_fn(|i| (-TEMPLATE[i].1, TEMPLATE[i].0));
        let t = estimate_similarity(&src, &TEMPLATE).expect("non-degenerate");

        for i in 0..5 {
            let (mx, my) = t.apply(src[i]);
            assert!(
                (mx - TEMPLATE[i].0).abs() < 1e-2 && (my - TEMPLATE[i].1).abs() < 1e-2,
                "rotated point {i} mapped to ({mx}, {my})"
            );
        }
    }

    #[test]
    fn degenerate_landmarks_are_rejected() {
        let same = [(10.0f32, 10.0f32); 5];
        assert!(estimate_similarity(&same, &TEMPLATE).is_none());
    }

    #[test]
    fn invert_round_trips() {
        let src: [(f32, f32); 5] =
            std::array::from_fn(|i| (TEMPLATE[i].0 * 1.7 + 12.0, TEMPLATE[i].1 * 1.7 - 5.0));
        let fwd = estimate_similarity(&src, &TEMPLATE).expect("non-degenerate");
        let inv = fwd.invert().expect("invertible");

        let p = (42.0f32, 77.0f32);
        let (rx, ry) = inv.apply(fwd.apply(p));
        assert!(
            (rx - p.0).abs() < 1e-2 && (ry - p.1).abs() < 1e-2,
            "round trip gave ({rx}, {ry})"
        );
    }

    #[test]
    fn align_produces_correct_dimensions() {
        let img = DynamicImage::new_rgb8(300, 200);
        let out = align_face(&img, &template_landmarks()).expect("aligns");
        assert_eq!(out.width(), ALIGNED_SIZE);
        assert_eq!(out.height(), ALIGNED_SIZE);
    }

    #[test]
    fn align_rejects_degenerate_landmarks() {
        let img = DynamicImage::new_rgb8(300, 200);
        let lm = Landmarks {
            left_eye: (5.0, 5.0),
            right_eye: (5.0, 5.0),
            nose: (5.0, 5.0),
            left_mouth: (5.0, 5.0),
            right_mouth: (5.0, 5.0),
        };
        assert!(align_face(&img, &lm).is_none());
    }

    #[test]
    fn align_samples_the_right_region() {
        // Paint a white square where the aligned face should come from, black
        // elsewhere, then check the crop is predominantly white. Guards against
        // an inverted or mirrored transform silently sampling the wrong area.
        let mut img = RgbImage::from_pixel(400, 400, Rgb([0, 0, 0]));
        for y in 100..300 {
            for x in 100..300 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        // Landmarks = template shifted into that square (scale 1, offset +120).
        let lm = Landmarks {
            right_eye: (TEMPLATE[0].0 + 120.0, TEMPLATE[0].1 + 120.0),
            left_eye: (TEMPLATE[1].0 + 120.0, TEMPLATE[1].1 + 120.0),
            nose: (TEMPLATE[2].0 + 120.0, TEMPLATE[2].1 + 120.0),
            right_mouth: (TEMPLATE[3].0 + 120.0, TEMPLATE[3].1 + 120.0),
            left_mouth: (TEMPLATE[4].0 + 120.0, TEMPLATE[4].1 + 120.0),
        };

        let out = align_face(&DynamicImage::ImageRgb8(img), &lm).expect("aligns");
        let white = out.pixels().filter(|p| p.0[0] > 200).count();
        let total = (ALIGNED_SIZE * ALIGNED_SIZE) as usize;
        assert!(
            white * 10 > total * 9,
            "expected mostly white, got {white}/{total}"
        );
    }

    #[test]
    fn input_tensor_is_chw_and_normalised() {
        let mut img = RgbImage::new(ALIGNED_SIZE, ALIGNED_SIZE);
        // Distinct per-channel constants so plane ordering is checkable.
        for p in img.pixels_mut() {
            *p = Rgb([255, 127, 0]);
        }
        let t = to_input_tensor(&img);
        let plane = (ALIGNED_SIZE * ALIGNED_SIZE) as usize;
        assert_eq!(t.len(), 3 * plane);

        assert!((t[0] - (255.0 - 127.5) / 128.0).abs() < 1e-6, "R plane");
        assert!((t[plane] - (127.0 - 127.5) / 128.0).abs() < 1e-6, "G plane");
        assert!(
            (t[2 * plane] - (0.0 - 127.5) / 128.0).abs() < 1e-6,
            "B plane"
        );
    }
}
