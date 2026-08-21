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
}

#[derive(Debug, Clone)]
pub struct Person {
    pub id: PersonId,
    pub name: Option<String>,
    pub face_count: i64,
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
                   lm_right_mouth_x, lm_right_mouth_y, score, embedding, dim\
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

    /// Load faces for clustering, optionally only the unassigned ones.
    pub async fn fetch_embeddings(
        &self,
        unassigned_only: bool,
    ) -> crate::Result<Vec<FaceEmbedding>> {
        let sql = if unassigned_only {
            "SELECT id, asset_id, person_id, score, embedding, assignment_source \
             FROM faces WHERE embedding IS NOT NULL AND person_id IS NULL ORDER BY id"
        } else {
            "SELECT id, asset_id, person_id, score, embedding, assignment_source \
             FROM faces WHERE embedding IS NOT NULL ORDER BY id"
        };

        type Row = (String, String, Option<String>, f32, Vec<u8>, String);
        let rows: Vec<Row> = sqlx::query_as(sql)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::Error::Query)?;

        rows.into_iter()
            .map(|(id, asset_id, person_id, score, blob, source)| {
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
        let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
            "SELECT p.id, p.name, COUNT(f.id) AS face_count \
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
            .map(|(id, name, face_count)| Person {
                id: PersonId::from(parse_uuid(&id)),
                name,
                face_count,
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
