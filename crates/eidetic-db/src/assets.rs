use chrono::{DateTime, Utc};
use eidetic_core::AssetId;
use eidetic_core::{AssetIndex, InsertOutcome, NewAsset};
use sqlx::PgPool;
use std::path::PathBuf;

pub struct PgAssetsRepo {
    pool: PgPool,
}

pub struct LibraryStats {
    pub total: i64,
    pub images: i64,
    pub videos: i64,
    pub embedded: i64,
    pub needs_embed: i64,
    pub total_bytes: i64,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}

pub struct SearchResult {
    pub id: AssetId,
    pub storage_path: PathBuf,
    pub score: f32,
    pub mime_type: Option<String>,
    pub date_taken: Option<DateTime<Utc>>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
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
            date_taken: Option<DateTime<Utc>>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let vec = pgvector::Vector::from(query_vec.to_vec());
        let rows: Vec<SearchRow> = sqlx::query_as(
            "SELECT id, storage_path, mime_type, date_taken, \
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
                date_taken: r.date_taken,
                camera_make: r.camera_make,
                camera_model: r.camera_model,
                latitude: r.latitude,
                longitude: r.longitude,
            })
            .collect())
    }
}

#[allow(async_fn_in_trait)]
impl AssetIndex for PgAssetsRepo {
    async fn find_by_hash(&self, hash: &str) -> eidetic_core::IndexResult<Option<AssetId>> {
        let row: Option<(uuid::Uuid,)> = sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Box::new(e) as eidetic_core::IndexError)?;

        Ok(row.map(|(uuid,)| AssetId::from(uuid)))
    }

    async fn insert_asset(&self, asset: NewAsset) -> eidetic_core::IndexResult<InsertOutcome> {
        let new_id = AssetId::new();
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type, \
              date_taken, latitude, longitude, camera_make, camera_model) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
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
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Box::new(e) as eidetic_core::IndexError)?;

        match row {
            Some((uuid,)) => Ok(InsertOutcome::Inserted(AssetId::from(uuid))),
            None => {
                let (uuid,): (uuid::Uuid,) =
                    sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
                        .bind(&asset.hash)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(|e| Box::new(e) as eidetic_core::IndexError)?;
                Ok(InsertOutcome::Existing(AssetId::from(uuid)))
            }
        }
    }
}
