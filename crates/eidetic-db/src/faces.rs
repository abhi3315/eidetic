//! Face and person storage (ADR-0010).
//!
//! Mirrors [`crate::AssetsRepo`]: one type owns every query, callers never see
//! `sqlx`. Face embeddings use the same little-endian f32 blob convention as
//! image embeddings, so [`crate::vector`] is shared rather than duplicated.

use crate::vector;
use eidetic_core::{AssetId, FaceId, PersonId};
use sqlx::SqlitePool;

/// A face to insert, as produced by the ML pipeline.
#[derive(Debug, Clone)]
pub struct NewFace {
    pub asset_id: AssetId,
    /// `(x, y, width, height)` in original-image pixels.
    pub bbox: (f32, f32, f32, f32),
    /// Landmarks in template order: left eye, right eye, nose, left mouth,
    /// right mouth.
    pub landmarks: [(f32, f32); 5],
    pub score: f32,
    pub embedding: Vec<f32>,
    /// For faces found in video frames: the sampled timestamp (seconds).
    /// None for photos.
    pub ts_secs: Option<f64>,
}

/// A stored face with just what clustering needs.
#[derive(Debug, Clone)]
pub struct FaceEmbedding {
    pub id: FaceId,
    pub asset_id: AssetId,
    pub person_id: Option<PersonId>,
    pub score: f32,
    pub embedding: Vec<f32>,
    /// True when the user set this attribution by hand. Such faces are frozen:
    /// clustering must never reassign them.
    pub pinned: bool,
    /// Video-frame timestamp; None for photo faces. Video faces may match
    /// existing people but never seed clusters or act as exemplars.
    pub ts_secs: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Person {
    pub id: PersonId,
    pub name: Option<String>,
    pub face_count: i64,
    /// The person's highest-score face, used as the cover crop in the web UI.
    /// `None` only when the person currently has no faces at all.
    pub cover_face: Option<FaceId>,
}

/// One stored face with everything a viewer needs to crop it back out of its
/// photo. The asset's storage path is joined in so callers don't pay one
/// asset lookup per face.
#[derive(Debug, Clone)]
pub struct PersonFace {
    pub face_id: FaceId,
    pub asset_id: AssetId,
    pub storage_path: std::path::PathBuf,
    pub score: f32,
    /// `(x, y, width, height)` in original-image pixels.
    pub bbox: (f32, f32, f32, f32),
    /// Landmarks exactly as stored: template order (image-left eye,
    /// image-right eye, nose, image-left mouth corner, image-right mouth
    /// corner) — the order `eidetic_ml::Landmarks::as_template_order`
    /// produces at the write site.
    pub landmarks: [(f32, f32); 5],
    /// True when the user set the person attribution by hand.
    pub pinned: bool,
    /// Video-frame timestamp when this face came from a video; None for
    /// photos. Drives the person page's deep link into the moment.
    pub ts_secs: Option<f64>,
}

/// A face in one asset together with the person it is attributed to. Faces
/// with no person are filtered out at the query: "who is in this photo?"
/// callers have no use for them.
#[derive(Debug, Clone)]
pub struct AssetFace {
    pub face_id: FaceId,
    pub person_id: PersonId,
    pub person_name: Option<String>,
}

/// Shared SELECT for [`PersonFace`]. 19 columns is past sqlx's tuple
/// implementations, hence the derived row struct below.
const PERSON_FACE_SELECT: &str = "SELECT f.id, f.asset_id, a.storage_path, f.score, \
     f.bbox_x, f.bbox_y, f.bbox_w, f.bbox_h, \
     f.lm_left_eye_x, f.lm_left_eye_y, f.lm_right_eye_x, f.lm_right_eye_y, \
     f.lm_nose_x, f.lm_nose_y, f.lm_left_mouth_x, f.lm_left_mouth_y, \
     f.lm_right_mouth_x, f.lm_right_mouth_y, f.assignment_source, f.ts_secs \
     FROM faces f JOIN assets a ON a.id = f.asset_id";

#[derive(sqlx::FromRow)]
struct PersonFaceRow {
    id: String,
    asset_id: String,
    storage_path: String,
    score: f32,
    bbox_x: f32,
    bbox_y: f32,
    bbox_w: f32,
    bbox_h: f32,
    lm_left_eye_x: f32,
    lm_left_eye_y: f32,
    lm_right_eye_x: f32,
    lm_right_eye_y: f32,
    lm_nose_x: f32,
    lm_nose_y: f32,
    lm_left_mouth_x: f32,
    lm_left_mouth_y: f32,
    lm_right_mouth_x: f32,
    lm_right_mouth_y: f32,
    assignment_source: String,
    ts_secs: Option<f64>,
}

impl From<PersonFaceRow> for PersonFace {
    fn from(r: PersonFaceRow) -> Self {
        PersonFace {
            face_id: FaceId::from(parse_uuid(&r.id)),
            asset_id: AssetId::from(parse_uuid(&r.asset_id)),
            storage_path: std::path::PathBuf::from(r.storage_path),
            score: r.score,
            bbox: (r.bbox_x, r.bbox_y, r.bbox_w, r.bbox_h),
            landmarks: [
                (r.lm_left_eye_x, r.lm_left_eye_y),
                (r.lm_right_eye_x, r.lm_right_eye_y),
                (r.lm_nose_x, r.lm_nose_y),
                (r.lm_left_mouth_x, r.lm_left_mouth_y),
                (r.lm_right_mouth_x, r.lm_right_mouth_y),
            ],
            pinned: r.assignment_source == "user",
            ts_secs: r.ts_secs,
        }
    }
}

pub struct FacesRepo {
    pool: SqlitePool,
}

fn id_text<T: std::fmt::Display>(id: T) -> String {
    id.to_string()
}

fn parse_uuid(raw: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(raw).expect("id column holds hyphenated UUID text written by id_text()")
}

impl FacesRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record a detection pass over one asset, storing every face found.
    ///
    /// The run row is written even when `faces` is empty, so a photo with no
    /// faces is not re-scanned forever. Both writes share a transaction: a
    /// crash must not leave a "scanned" marker with no faces behind it.
    pub async fn record_detection(
        &self,
        asset_id: AssetId,
        faces: &[NewFace],
    ) -> crate::Result<Vec<FaceId>> {
        let mut tx = self.pool.begin().await.map_err(crate::Error::Query)?;

        let mut ids = Vec::with_capacity(faces.len());
        for face in faces {
            let id = FaceId::new();
            let lm = face.landmarks;
            sqlx::query(
                "INSERT INTO faces (\
                   id, asset_id, bbox_x, bbox_y, bbox_w, bbox_h, \
                   lm_left_eye_x, lm_left_eye_y, lm_right_eye_x, lm_right_eye_y, \
                   lm_nose_x, lm_nose_y, lm_left_mouth_x, lm_left_mouth_y, \
                   lm_right_mouth_x, lm_right_mouth_y, score, embedding, dim, ts_secs\
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id_text(id))
            .bind(id_text(face.asset_id))
            .bind(face.bbox.0)
            .bind(face.bbox.1)
            .bind(face.bbox.2)
            .bind(face.bbox.3)
            .bind(lm[0].0)
            .bind(lm[0].1)
            .bind(lm[1].0)
            .bind(lm[1].1)
            .bind(lm[2].0)
            .bind(lm[2].1)
            .bind(lm[3].0)
            .bind(lm[3].1)
            .bind(lm[4].0)
            .bind(lm[4].1)
            .bind(face.score)
            .bind(vector::encode(&face.embedding))
            .bind(face.embedding.len() as i64)
            .bind(face.ts_secs)
            .execute(&mut *tx)
            .await
            .map_err(crate::Error::Query)?;
            ids.push(id);
        }

        sqlx::query(
            "INSERT INTO face_detection_runs (asset_id, face_count) VALUES (?, ?) \
             ON CONFLICT(asset_id) DO UPDATE SET \
               face_count = excluded.face_count, detected_at = excluded.detected_at",
        )
        .bind(id_text(asset_id))
        .bind(faces.len() as i64)
        .execute(&mut *tx)
        .await
        .map_err(crate::Error::Query)?;

        tx.commit().await.map_err(crate::Error::Query)?;
        Ok(ids)
    }

    /// Images that have never been through face detection.
    ///
    /// An anti-join on `face_detection_runs`, not on `faces` — otherwise every
    /// photo containing no people would be rescanned on every run.
    pub async fn fetch_undetected(&self) -> crate::Result<Vec<(AssetId, std::path::PathBuf)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.id, a.storage_path FROM assets a \
             LEFT JOIN face_detection_runs r ON r.asset_id = a.id \
             WHERE r.asset_id IS NULL AND a.mime_type LIKE 'image/%' \
             ORDER BY a.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(id, path)| {
                (
                    AssetId::from(parse_uuid(&id)),
                    std::path::PathBuf::from(path),
                )
            })
            .collect())
    }

    /// Videos never scanned for faces (anti-join on `face_detection_runs`,
    /// same idempotency contract as photos). Duration rides along for frame
    /// sampling; None means imported without ffprobe.
    pub async fn fetch_videos_undetected(
        &self,
    ) -> crate::Result<Vec<(AssetId, std::path::PathBuf, Option<f64>)>> {
        let rows: Vec<(String, String, Option<f64>)> = sqlx::query_as(
            "SELECT a.id, a.storage_path, a.duration_secs FROM assets a \
             LEFT JOIN face_detection_runs r ON r.asset_id = a.id \
             WHERE r.asset_id IS NULL AND a.mime_type LIKE 'video/%' \
             ORDER BY a.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(id, path, dur)| {
                (
                    AssetId::from(parse_uuid(&id)),
                    std::path::PathBuf::from(path),
                    dur,
                )
            })
            .collect())
    }

    /// Load faces for clustering, optionally only the unassigned ones.
    pub async fn fetch_embeddings(
        &self,
        unassigned_only: bool,
    ) -> crate::Result<Vec<FaceEmbedding>> {
        let sql = if unassigned_only {
            "SELECT id, asset_id, person_id, score, embedding, assignment_source, ts_secs \
             FROM faces WHERE embedding IS NOT NULL AND person_id IS NULL ORDER BY id"
        } else {
            "SELECT id, asset_id, person_id, score, embedding, assignment_source, ts_secs \
             FROM faces WHERE embedding IS NOT NULL ORDER BY id"
        };

        type Row = (
            String,
            String,
            Option<String>,
            f32,
            Vec<u8>,
            String,
            Option<f64>,
        );
        let rows: Vec<Row> = sqlx::query_as(sql)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::Error::Query)?;

        rows.into_iter()
            .map(|(id, asset_id, person_id, score, blob, source, ts_secs)| {
                let embedding = vector::decode(&blob).ok_or_else(|| crate::Error::CorruptRow {
                    table: "faces",
                    column: "embedding",
                    detail: format!("face {id}: {} bytes is not whole f32s", blob.len()),
                })?;
                Ok(FaceEmbedding {
                    id: FaceId::from(parse_uuid(&id)),
                    asset_id: AssetId::from(parse_uuid(&asset_id)),
                    person_id: person_id.as_deref().map(|p| PersonId::from(parse_uuid(p))),
                    score,
                    embedding,
                    pinned: source == "user",
                    ts_secs,
                })
            })
            .collect()
    }

    /// Create an unnamed person. Clustering produces candidates; naming is a
    /// separate, user-driven step.
    pub async fn create_person(&self) -> crate::Result<PersonId> {
        let id = PersonId::new();
        sqlx::query("INSERT INTO persons (id) VALUES (?)")
            .bind(id_text(id))
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(id)
    }

    pub async fn name_person(&self, id: PersonId, name: &str) -> crate::Result<()> {
        sqlx::query("UPDATE persons SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id_text(id))
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }

    /// Attach a face to a person.
    ///
    /// `by_user` marks the attribution as frozen. A pinned face is never
    /// overwritten by an automatic assignment — the `assignment_source = 'auto'`
    /// guard in the WHERE clause is what enforces that, so callers cannot
    /// forget to check.
    pub async fn assign_face(
        &self,
        face: FaceId,
        person: PersonId,
        by_user: bool,
    ) -> crate::Result<()> {
        let sql = if by_user {
            "UPDATE faces SET person_id = ?, assignment_source = 'user' WHERE id = ?"
        } else {
            "UPDATE faces SET person_id = ?, assignment_source = 'auto' \
             WHERE id = ? AND assignment_source = 'auto'"
        };
        sqlx::query(sql)
            .bind(id_text(person))
            .bind(id_text(face))
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }

    /// Record "this face is not this person", and clear the attribution if it
    /// is the one being rejected.
    pub async fn reject_face(&self, face: FaceId, person: PersonId) -> crate::Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::Error::Query)?;

        sqlx::query(
            "INSERT INTO face_person_rejections (face_id, person_id) VALUES (?, ?) \
             ON CONFLICT(face_id, person_id) DO NOTHING",
        )
        .bind(id_text(face))
        .bind(id_text(person))
        .execute(&mut *tx)
        .await
        .map_err(crate::Error::Query)?;

        sqlx::query(
            "UPDATE faces SET person_id = NULL, assignment_source = 'auto' \
             WHERE id = ? AND person_id = ?",
        )
        .bind(id_text(face))
        .bind(id_text(person))
        .execute(&mut *tx)
        .await
        .map_err(crate::Error::Query)?;

        tx.commit().await.map_err(crate::Error::Query)?;
        Ok(())
    }

    /// Every `(face, person)` pair the user has explicitly rejected.
    pub async fn fetch_rejections(&self) -> crate::Result<Vec<(FaceId, PersonId)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT face_id, person_id FROM face_person_rejections")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::Error::Query)?;
        Ok(rows
            .into_iter()
            .map(|(f, p)| (FaceId::from(parse_uuid(&f)), PersonId::from(parse_uuid(&p))))
            .collect())
    }

    /// Record a must-link (merge) or must-not-link (split) between two faces.
    ///
    /// The pair is ordered before writing so the same two faces cannot be
    /// stored twice in opposite orders.
    pub async fn set_link(&self, a: FaceId, b: FaceId, must_link: bool) -> crate::Result<()> {
        let (lo, hi) = order_pair(a, b);
        sqlx::query(
            "INSERT INTO face_links (face_a, face_b, must_link) VALUES (?, ?, ?) \
             ON CONFLICT(face_a, face_b) DO UPDATE SET must_link = excluded.must_link",
        )
        .bind(lo)
        .bind(hi)
        .bind(must_link as i64)
        .execute(&self.pool)
        .await
        .map_err(crate::Error::Query)?;
        Ok(())
    }

    /// All user links, split into `(must_link, must_not_link)`.
    pub async fn fetch_links(
        &self,
    ) -> crate::Result<(Vec<(FaceId, FaceId)>, Vec<(FaceId, FaceId)>)> {
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT face_a, face_b, must_link FROM face_links")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::Error::Query)?;

        let mut must = Vec::new();
        let mut must_not = Vec::new();
        for (a, b, link) in rows {
            let pair = (FaceId::from(parse_uuid(&a)), FaceId::from(parse_uuid(&b)));
            if link == 1 {
                must.push(pair)
            } else {
                must_not.push(pair)
            }
        }
        Ok((must, must_not))
    }

    /// People with at least one face, most faces first.
    pub async fn list_persons(&self) -> crate::Result<Vec<Person>> {
        let rows: Vec<(String, Option<String>, i64, Option<String>)> = sqlx::query_as(
            "SELECT p.id, p.name, COUNT(f.id) AS face_count, \
               (SELECT f2.id FROM faces f2 WHERE f2.person_id = p.id \
                  ORDER BY f2.score DESC, f2.id LIMIT 1) AS cover_face \
             FROM persons p LEFT JOIN faces f ON f.person_id = p.id \
             WHERE p.merged_into IS NULL \
             GROUP BY p.id \
             HAVING face_count > 0 \
             ORDER BY face_count DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(id, name, face_count, cover)| Person {
                id: PersonId::from(parse_uuid(&id)),
                name,
                face_count,
                cover_face: cover.as_deref().map(|c| FaceId::from(parse_uuid(c))),
            })
            .collect())
    }

    /// One person by id. `None` for unknown people and for people that were
    /// merged away — their faces live under the merge target now.
    pub async fn fetch_person(&self, id: PersonId) -> crate::Result<Option<Person>> {
        let row: Option<(String, Option<String>, i64, Option<String>)> = sqlx::query_as(
            "SELECT p.id, p.name, \
               (SELECT COUNT(*) FROM faces f WHERE f.person_id = p.id) AS face_count, \
               (SELECT f2.id FROM faces f2 WHERE f2.person_id = p.id \
                  ORDER BY f2.score DESC, f2.id LIMIT 1) AS cover_face \
             FROM persons p WHERE p.id = ? AND p.merged_into IS NULL",
        )
        .bind(id_text(id))
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(row.map(|(id, name, face_count, cover)| Person {
            id: PersonId::from(parse_uuid(&id)),
            name,
            face_count,
            cover_face: cover.as_deref().map(|c| FaceId::from(parse_uuid(c))),
        }))
    }

    /// Every face attributed to `person`, best (highest score) first.
    pub async fn fetch_faces_for_person(&self, person: PersonId) -> crate::Result<Vec<PersonFace>> {
        let sql = format!("{PERSON_FACE_SELECT} WHERE f.person_id = ? ORDER BY f.score DESC, f.id");
        let rows: Vec<PersonFaceRow> = sqlx::query_as(&sql)
            .bind(id_text(person))
            .fetch_all(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// One face by id, with the crop-relevant geometry and its asset's path.
    pub async fn fetch_face(&self, face: FaceId) -> crate::Result<Option<PersonFace>> {
        let sql = format!("{PERSON_FACE_SELECT} WHERE f.id = ?");
        let row: Option<PersonFaceRow> = sqlx::query_as(&sql)
            .bind(id_text(face))
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(row.map(Into::into))
    }

    /// Faces in `asset` that are attributed to a person, best first — the
    /// "people in this photo" query.
    pub async fn fetch_faces_for_asset(&self, asset: AssetId) -> crate::Result<Vec<AssetFace>> {
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT f.id, f.person_id, p.name FROM faces f \
             JOIN persons p ON p.id = f.person_id \
             WHERE f.asset_id = ? ORDER BY f.score DESC, f.id",
        )
        .bind(id_text(asset))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(face, person, name)| AssetFace {
                face_id: FaceId::from(parse_uuid(&face)),
                person_id: PersonId::from(parse_uuid(&person)),
                person_name: name,
            })
            .collect())
    }

    /// Merge `from` into `into`: move the faces, then leave a breadcrumb so a
    /// later re-cluster that re-splits them can be re-merged.
    pub async fn merge_persons(&self, from: PersonId, into: PersonId) -> crate::Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::Error::Query)?;

        sqlx::query("UPDATE faces SET person_id = ? WHERE person_id = ?")
            .bind(id_text(into))
            .bind(id_text(from))
            .execute(&mut *tx)
            .await
            .map_err(crate::Error::Query)?;

        sqlx::query("UPDATE persons SET merged_into = ? WHERE id = ?")
            .bind(id_text(into))
            .bind(id_text(from))
            .execute(&mut *tx)
            .await
            .map_err(crate::Error::Query)?;

        tx.commit().await.map_err(crate::Error::Query)?;
        Ok(())
    }
}

/// Order a face pair so `(a, b)` and `(b, a)` produce the same key, satisfying
/// the `face_a < face_b` check constraint.
fn order_pair(a: FaceId, b: FaceId) -> (String, String) {
    let (x, y) = (a.to_string(), b.to_string());
    if x <= y { (x, y) } else { (y, x) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_pair_is_symmetric() {
        let a = FaceId::new();
        let b = FaceId::new();
        assert_eq!(order_pair(a, b), order_pair(b, a));
        let (lo, hi) = order_pair(a, b);
        assert!(lo <= hi, "pair must be sorted to satisfy the CHECK");
    }
}
