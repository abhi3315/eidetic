use chrono::{DateTime, Utc};
use eidetic_core::AssetId;
use sqlx::PgPool;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub aperture: Option<f32>,
    pub shutter: Option<String>,
    pub iso: Option<i32>,
    pub orientation: Option<i16>,
    pub altitude: Option<f64>,
    pub gps_direction: Option<f64>,
    pub exif_raw: Option<serde_json::Value>,
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub admin1: Option<String>,
    pub place: Option<String>,
    pub place_distance_m: Option<f32>,
    pub thumbnails_generated: bool,
}

#[derive(Debug)]
pub enum InsertOutcome {
    Inserted(AssetId),
    Existing(AssetId),
}

pub struct PgAssetsRepo {
    pool: PgPool,
}

pub struct LibraryStats {
    pub total: i64,
    pub images: i64,
    pub videos: i64,
    pub embedded: i64,
    pub needs_embed: i64,
    pub thumbnails_pending: i64,
    pub total_bytes: i64,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}

pub struct SearchResult {
    pub id: AssetId,
    pub storage_path: PathBuf,
    pub score: f32,
    pub mime_type: Option<String>,
    pub file_size: i64,
    pub thumbnails_generated: bool,
    pub date_taken: Option<DateTime<Utc>>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

pub struct RecentAsset {
    pub id: AssetId,
    pub hash: eidetic_core::Sha256,
    pub original_filename: String,
    pub mime_type: String,
    pub thumbnails_generated: bool,
    pub file_size: i64,
    pub imported_at: chrono::DateTime<chrono::Utc>,
}

pub struct AssetDetail {
    pub id: AssetId,
    pub hash: eidetic_core::Sha256,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: i64,
    pub mime_type: Option<String>,
    pub imported_at: chrono::DateTime<chrono::Utc>,
    pub date_taken: Option<chrono::DateTime<chrono::Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub aperture: Option<f32>,
    pub shutter: Option<String>,
    pub iso: Option<i32>,
    pub orientation: Option<i16>,
    pub altitude: Option<f64>,
    pub gps_direction: Option<f64>,
    pub exif_raw: Option<serde_json::Value>,
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub admin1: Option<String>,
    pub place: Option<String>,
    pub place_distance_m: Option<f32>,
    pub thumbnails_generated: bool,
}

impl PgAssetsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn fetch_stats(&self) -> crate::Result<LibraryStats> {
        #[derive(sqlx::FromRow)]
        struct StatsRow {
            total: i64,
            images: i64,
            videos: i64,
            embedded: i64,
            needs_embed: i64,
            thumbnails_pending: i64,
            total_bytes: i64,
            earliest: Option<DateTime<Utc>>,
            latest: Option<DateTime<Utc>>,
        }

        let row: StatsRow = sqlx::query_as(
            "SELECT \
               COUNT(*)                                                   AS total, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'image/%')           AS images, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'video/%')           AS videos, \
               COUNT(*) FILTER (WHERE embedding IS NOT NULL)              AS embedded, \
               COUNT(*) FILTER (WHERE embedding IS NULL \
                                  AND mime_type LIKE 'image/%')           AS needs_embed, \
               COUNT(*) FILTER (WHERE thumbnails_generated = FALSE \
                                  AND mime_type LIKE 'image/%')           AS thumbnails_pending, \
               COALESCE(SUM(file_size), 0)::bigint                        AS total_bytes, \
               MIN(date_taken)                                            AS earliest, \
               MAX(date_taken)                                            AS latest \
             FROM assets",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(LibraryStats {
            total: row.total,
            images: row.images,
            videos: row.videos,
            embedded: row.embedded,
            needs_embed: row.needs_embed,
            thumbnails_pending: row.thumbnails_pending,
            total_bytes: row.total_bytes,
            earliest: row.earliest,
            latest: row.latest,
        })
    }

    pub async fn fetch_unembedded(&self) -> crate::Result<Vec<(AssetId, PathBuf)>> {
        let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT id, storage_path FROM assets \
             WHERE embedding IS NULL AND mime_type LIKE 'image/%' \
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(uuid, path)| (AssetId::from(uuid), PathBuf::from(path)))
            .collect())
    }

    /// Ordered by `id` for stable resumption across runs.
    pub async fn fetch_unthumbnailed(
        &self,
    ) -> crate::Result<Vec<(AssetId, eidetic_core::Sha256, PathBuf)>> {
        let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT id, hash, storage_path FROM assets \
             WHERE thumbnails_generated = FALSE \
               AND mime_type LIKE 'image/%' \
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(uuid, hash_hex, path)| {
                let hash = eidetic_core::Sha256::from_hex(&hash_hex)
                    .expect("hash column is CHAR(64) of lowercase hex written by our own code");
                (AssetId::from(uuid), hash, PathBuf::from(path))
            })
            .collect())
    }

    pub async fn mark_thumbnailed(&self, id: AssetId) -> crate::Result<()> {
        sqlx::query("UPDATE assets SET thumbnails_generated = TRUE WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }

    pub async fn fetch_recent(&self, limit: u32) -> crate::Result<Vec<RecentAsset>> {
        type Row = (
            uuid::Uuid,
            String,
            String,
            String,
            bool,
            i64,
            chrono::DateTime<chrono::Utc>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, hash, original_filename, mime_type, thumbnails_generated, file_size, imported_at \
             FROM assets \
             WHERE mime_type IS NOT NULL \
             ORDER BY imported_at DESC \
             LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    uuid,
                    hash_hex,
                    original_filename,
                    mime_type,
                    thumbnails_generated,
                    file_size,
                    imported_at,
                )| {
                    let hash = eidetic_core::Sha256::from_hex(&hash_hex)
                        .expect("hash column is CHAR(64) of lowercase hex");
                    RecentAsset {
                        id: AssetId::from(uuid),
                        hash,
                        original_filename,
                        mime_type,
                        thumbnails_generated,
                        file_size,
                        imported_at,
                    }
                },
            )
            .collect())
    }

    pub async fn fetch_by_id(&self, id: AssetId) -> crate::Result<Option<AssetDetail>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: uuid::Uuid,
            hash: String,
            original_filename: String,
            storage_path: String,
            file_size: i64,
            mime_type: Option<String>,
            imported_at: chrono::DateTime<chrono::Utc>,
            date_taken: Option<chrono::DateTime<chrono::Utc>>,
            latitude: Option<f64>,
            longitude: Option<f64>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            lens_make: Option<String>,
            lens_model: Option<String>,
            focal_length: Option<f32>,
            focal_length_35mm: Option<f32>,
            aperture: Option<f32>,
            shutter: Option<String>,
            iso: Option<i32>,
            orientation: Option<i16>,
            altitude: Option<f64>,
            gps_direction: Option<f64>,
            exif_raw: Option<serde_json::Value>,
            country_code: Option<String>,
            country_name: Option<String>,
            admin1: Option<String>,
            place: Option<String>,
            place_distance_m: Option<f32>,
            thumbnails_generated: bool,
        }

        let row: Option<Row> = sqlx::query_as(
            "SELECT id, hash, original_filename, storage_path, file_size, mime_type, \
                    imported_at, date_taken, latitude, longitude, camera_make, camera_model, \
                    lens_make, lens_model, focal_length, focal_length_35mm, aperture, \
                    shutter, iso, orientation, altitude, gps_direction, exif_raw, \
                    country_code, country_name, admin1, place, place_distance_m, \
                    thumbnails_generated \
             FROM assets WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(row.map(|r| AssetDetail {
            id: AssetId::from(r.id),
            hash: eidetic_core::Sha256::from_hex(&r.hash)
                .expect("hash column is CHAR(64) of lowercase hex"),
            original_filename: r.original_filename,
            storage_path: PathBuf::from(r.storage_path),
            file_size: r.file_size,
            mime_type: r.mime_type,
            imported_at: r.imported_at,
            date_taken: r.date_taken,
            latitude: r.latitude,
            longitude: r.longitude,
            camera_make: r.camera_make,
            camera_model: r.camera_model,
            lens_make: r.lens_make,
            lens_model: r.lens_model,
            focal_length: r.focal_length,
            focal_length_35mm: r.focal_length_35mm,
            aperture: r.aperture,
            shutter: r.shutter,
            iso: r.iso,
            orientation: r.orientation,
            altitude: r.altitude,
            gps_direction: r.gps_direction,
            exif_raw: r.exif_raw,
            country_code: r.country_code,
            country_name: r.country_name,
            admin1: r.admin1,
            place: r.place,
            place_distance_m: r.place_distance_m,
            thumbnails_generated: r.thumbnails_generated,
        }))
    }

    pub async fn store_embedding(&self, id: AssetId, embedding: &[f32]) -> crate::Result<()> {
        let vec = pgvector::Vector::from(embedding.to_vec());
        sqlx::query("UPDATE assets SET embedding = $1 WHERE id = $2")
            .bind(vec)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }

    pub async fn search_similar(
        &self,
        query_vec: &[f32],
        limit: u32,
    ) -> crate::Result<Vec<SearchResult>> {
        #[derive(sqlx::FromRow)]
        struct SearchRow {
            id: uuid::Uuid,
            storage_path: String,
            score: f32,
            mime_type: Option<String>,
            file_size: i64,
            thumbnails_generated: bool,
            date_taken: Option<DateTime<Utc>>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<SearchRow> = sqlx::query_as(
            "SELECT id, storage_path, mime_type, file_size, thumbnails_generated, date_taken, \
                    camera_make, camera_model, latitude, longitude, \
                    (1.0 - (embedding <=> $1))::real AS score \
             FROM assets \
             WHERE embedding IS NOT NULL \
             ORDER BY embedding <=> $1 \
             LIMIT $2",
        )
        .bind(vec)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|r| SearchResult {
                id: AssetId::from(r.id),
                storage_path: PathBuf::from(r.storage_path),
                score: r.score,
                mime_type: r.mime_type,
                file_size: r.file_size,
                thumbnails_generated: r.thumbnails_generated,
                date_taken: r.date_taken,
                camera_make: r.camera_make,
                camera_model: r.camera_model,
                latitude: r.latitude,
                longitude: r.longitude,
            })
            .collect())
    }

    pub async fn find_by_hash(&self, hash: &str) -> crate::Result<Option<AssetId>> {
        let row: Option<(uuid::Uuid,)> = sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::Error::Query)?;

        Ok(row.map(|(uuid,)| AssetId::from(uuid)))
    }

    pub async fn insert_asset(&self, asset: NewAsset) -> crate::Result<InsertOutcome> {
        let new_id = AssetId::new();
        // Backfill semantic: an asset that parsed-but-had-no-EXIF still gets
        // an empty-object sentinel so `WHERE exif_raw IS NULL` distinguishes
        // "never processed" from "processed, nothing there."
        let exif_raw = asset
            .exif_raw
            .clone()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type, \
              date_taken, latitude, longitude, camera_make, camera_model, \
              lens_make, lens_model, focal_length, focal_length_35mm, aperture, \
              shutter, iso, orientation, altitude, gps_direction, exif_raw, \
              country_code, country_name, admin1, place, place_distance_m, \
              thumbnails_generated) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
                     $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, \
                     $23, $24, $25, $26, $27, $28) \
             ON CONFLICT (hash) DO NOTHING \
             RETURNING id",
        )
        .bind(new_id.as_uuid())
        .bind(&asset.hash)
        .bind(&asset.original_filename)
        .bind(asset.storage_path.to_string_lossy().as_ref())
        .bind(asset.file_size as i64)
        .bind(asset.mime_type.as_deref())
        .bind(asset.date_taken)
        .bind(asset.latitude)
        .bind(asset.longitude)
        .bind(asset.camera_make.as_deref())
        .bind(asset.camera_model.as_deref())
        .bind(asset.lens_make.as_deref())
        .bind(asset.lens_model.as_deref())
        .bind(asset.focal_length)
        .bind(asset.focal_length_35mm)
        .bind(asset.aperture)
        .bind(asset.shutter.as_deref())
        .bind(asset.iso)
        .bind(asset.orientation)
        .bind(asset.altitude)
        .bind(asset.gps_direction)
        .bind(exif_raw)
        .bind(asset.country_code.as_deref())
        .bind(asset.country_name.as_deref())
        .bind(asset.admin1.as_deref())
        .bind(asset.place.as_deref())
        .bind(asset.place_distance_m)
        .bind(asset.thumbnails_generated)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        match row {
            Some((uuid,)) => Ok(InsertOutcome::Inserted(AssetId::from(uuid))),
            None => {
                let (uuid,): (uuid::Uuid,) =
                    sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
                        .bind(&asset.hash)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(crate::Error::Query)?;
                Ok(InsertOutcome::Existing(AssetId::from(uuid)))
            }
        }
    }
}
