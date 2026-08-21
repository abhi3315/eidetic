use crate::vector::{self, BruteForce, VectorIndex};
use chrono::{DateTime, Utc};
use eidetic_core::AssetId;
use sqlx::SqlitePool;
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

/// Asset CRUD + search over the embedded SQLite database.
///
/// Vector ranking is delegated to a [`VectorIndex`] so the strategy can change
/// without touching callers (ADR-0005).
pub struct AssetsRepo {
    pool: SqlitePool,
    index: Box<dyn VectorIndex>,
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

/// UUIDs are stored as lowercase hyphenated TEXT so the database stays
/// readable from the `sqlite3` CLI.
fn id_text(id: AssetId) -> String {
    id.as_uuid().to_string()
}

fn parse_id(raw: &str) -> AssetId {
    let uuid = uuid::Uuid::parse_str(raw)
        .expect("id column holds hyphenated UUID text written by id_text()");
    AssetId::from(uuid)
}

fn parse_hash(raw: &str) -> eidetic_core::Sha256 {
    eidetic_core::Sha256::from_hex(raw)
        .expect("hash column is 64 lowercase hex chars written by our own code")
}

impl AssetsRepo {
    /// Exact brute-force vector search (ADR-0005 default).
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            index: Box::new(BruteForce),
        }
    }

    /// Override the ranking strategy — the seam for an approximate index.
    pub fn with_index(pool: SqlitePool, index: Box<dyn VectorIndex>) -> Self {
        Self { pool, index }
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
               COUNT(*)                                                     AS total, \
               COUNT(*) FILTER (WHERE a.mime_type LIKE 'image/%')           AS images, \
               COUNT(*) FILTER (WHERE a.mime_type LIKE 'video/%')           AS videos, \
               COUNT(*) FILTER (WHERE e.asset_id IS NOT NULL)               AS embedded, \
               COUNT(*) FILTER (WHERE e.asset_id IS NULL \
                                  AND a.mime_type LIKE 'image/%')           AS needs_embed, \
               COUNT(*) FILTER (WHERE a.thumbnails_generated = 0 \
                                  AND a.mime_type LIKE 'image/%')           AS thumbnails_pending, \
               COALESCE(SUM(a.file_size), 0)                                AS total_bytes, \
               MIN(a.date_taken)                                            AS earliest, \
               MAX(a.date_taken)                                            AS latest \
             FROM assets a \
             LEFT JOIN embeddings e ON e.asset_id = a.id",
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
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.id, a.storage_path FROM assets a \
             LEFT JOIN embeddings e ON e.asset_id = a.id \
             WHERE e.asset_id IS NULL AND a.mime_type LIKE 'image/%' \
             ORDER BY a.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(id, path)| (parse_id(&id), PathBuf::from(path)))
            .collect())
    }

    /// Ordered by `id` for stable resumption across runs.
    pub async fn fetch_unthumbnailed(
        &self,
    ) -> crate::Result<Vec<(AssetId, eidetic_core::Sha256, PathBuf)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, hash, storage_path FROM assets \
             WHERE thumbnails_generated = 0 \
               AND mime_type LIKE 'image/%' \
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(id, hash_hex, path)| (parse_id(&id), parse_hash(&hash_hex), PathBuf::from(path)))
            .collect())
    }

    pub async fn update_place_columns(
        &self,
        id: AssetId,
        place: &eidetic_core::geocoder::Place,
    ) -> crate::Result<()> {
        sqlx::query(
            "UPDATE assets SET \
               country_code = ?, country_name = ?, admin1 = ?, \
               place = ?, place_distance_m = ? \
             WHERE id = ?",
        )
        .bind(&place.country_code)
        .bind(&place.country_name)
        .bind(&place.admin1)
        .bind(&place.place)
        .bind(place.distance_m)
        .bind(id_text(id))
        .execute(&self.pool)
        .await
        .map_err(crate::Error::Query)?;
        Ok(())
    }

    pub async fn mark_thumbnailed(&self, id: AssetId) -> crate::Result<()> {
        sqlx::query("UPDATE assets SET thumbnails_generated = 1 WHERE id = ?")
            .bind(id_text(id))
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }

    pub async fn fetch_recent(&self, limit: u32) -> crate::Result<Vec<RecentAsset>> {
        type Row = (
            String,
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
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    hash_hex,
                    original_filename,
                    mime_type,
                    thumbnails_generated,
                    file_size,
                    imported_at,
                )| RecentAsset {
                    id: parse_id(&id),
                    hash: parse_hash(&hash_hex),
                    original_filename,
                    mime_type,
                    thumbnails_generated,
                    file_size,
                    imported_at,
                },
            )
            .collect())
    }

    pub async fn fetch_by_id(&self, id: AssetId) -> crate::Result<Option<AssetDetail>> {
        let row: Option<DetailRow> = sqlx::query_as(DETAIL_SELECT)
            .bind(id_text(id))
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::Error::Query)?;

        Ok(row.map(DetailRow::into_detail))
    }

    pub async fn store_embedding(&self, id: AssetId, embedding: &[f32]) -> crate::Result<()> {
        sqlx::query(
            "INSERT INTO embeddings (asset_id, dim, vector) VALUES (?, ?, ?) \
             ON CONFLICT(asset_id) DO UPDATE SET dim = excluded.dim, vector = excluded.vector",
        )
        .bind(id_text(id))
        .bind(embedding.len() as i64)
        .bind(vector::encode(embedding))
        .execute(&self.pool)
        .await
        .map_err(crate::Error::Query)?;
        Ok(())
    }

    /// Rank every stored embedding against `query_vec` and hydrate the top hits.
    ///
    /// Two round trips: load the vectors, then fetch metadata for the winners.
    /// Ranking itself is exact and happens in-process (ADR-0005).
    pub async fn search_similar(
        &self,
        query_vec: &[f32],
        limit: u32,
    ) -> crate::Result<Vec<SearchResult>> {
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT asset_id, vector FROM embeddings")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::Error::Query)?;

        let mut stored = Vec::with_capacity(rows.len());
        for (id, blob) in rows {
            let decoded = vector::decode(&blob).ok_or_else(|| crate::Error::CorruptRow {
                table: "embeddings",
                column: "vector",
                detail: format!("asset {id}: blob of {} bytes is not whole f32s", blob.len()),
            })?;
            stored.push((parse_id(&id), decoded));
        }

        let ranked = self.index.top_k(query_vec, &stored, limit as usize);
        if ranked.is_empty() {
            return Ok(Vec::new());
        }

        // Hydrate metadata for the winners in one query, then restore the
        // ranked order (SQL makes no ordering promise for an IN filter).
        let placeholders = std::iter::repeat_n("?", ranked.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, storage_path, mime_type, file_size, thumbnails_generated, date_taken, \
                    camera_make, camera_model, latitude, longitude \
             FROM assets WHERE id IN ({placeholders})"
        );

        #[derive(sqlx::FromRow)]
        struct MetaRow {
            id: String,
            storage_path: String,
            mime_type: Option<String>,
            file_size: i64,
            thumbnails_generated: bool,
            date_taken: Option<DateTime<Utc>>,
            camera_make: Option<String>,
            camera_model: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let mut q = sqlx::query_as::<_, MetaRow>(&sql);
        for (id, _) in &ranked {
            q = q.bind(id_text(*id));
        }
        let metas = q.fetch_all(&self.pool).await.map_err(crate::Error::Query)?;

        let mut by_id: std::collections::HashMap<AssetId, MetaRow> =
            metas.into_iter().map(|m| (parse_id(&m.id), m)).collect();

        Ok(ranked
            .into_iter()
            .filter_map(|(id, score)| {
                let m = by_id.remove(&id)?;
                Some(SearchResult {
                    id,
                    storage_path: PathBuf::from(m.storage_path),
                    score,
                    mime_type: m.mime_type,
                    file_size: m.file_size,
                    thumbnails_generated: m.thumbnails_generated,
                    date_taken: m.date_taken,
                    camera_make: m.camera_make,
                    camera_model: m.camera_model,
                    latitude: m.latitude,
                    longitude: m.longitude,
                })
            })
            .collect())
    }

    pub async fn find_by_hash(&self, hash: &str) -> crate::Result<Option<AssetId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT id FROM assets WHERE hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::Error::Query)?;

        Ok(row.map(|(id,)| parse_id(&id)))
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
        let row: Option<(String,)> = sqlx::query_as(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type, \
              date_taken, latitude, longitude, camera_make, camera_model, \
              lens_make, lens_model, focal_length, focal_length_35mm, aperture, \
              shutter, iso, orientation, altitude, gps_direction, exif_raw, \
              country_code, country_name, admin1, place, place_distance_m, \
              thumbnails_generated) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                     ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                     ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (hash) DO NOTHING \
             RETURNING id",
        )
        .bind(id_text(new_id))
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
            Some((id,)) => Ok(InsertOutcome::Inserted(parse_id(&id))),
            None => {
                let (id,): (String,) = sqlx::query_as("SELECT id FROM assets WHERE hash = ?")
                    .bind(&asset.hash)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(crate::Error::Query)?;
                Ok(InsertOutcome::Existing(parse_id(&id)))
            }
        }
    }
}

const DETAIL_SELECT: &str = "SELECT id, hash, original_filename, storage_path, file_size, mime_type, \
            imported_at, date_taken, latitude, longitude, camera_make, camera_model, \
            lens_make, lens_model, focal_length, focal_length_35mm, aperture, \
            shutter, iso, orientation, altitude, gps_direction, exif_raw, \
            country_code, country_name, admin1, place, place_distance_m, \
            thumbnails_generated \
     FROM assets WHERE id = ?";

#[derive(sqlx::FromRow)]
struct DetailRow {
    id: String,
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

impl DetailRow {
    fn into_detail(self) -> AssetDetail {
        AssetDetail {
            id: parse_id(&self.id),
            hash: parse_hash(&self.hash),
            original_filename: self.original_filename,
            storage_path: PathBuf::from(self.storage_path),
            file_size: self.file_size,
            mime_type: self.mime_type,
            imported_at: self.imported_at,
            date_taken: self.date_taken,
            latitude: self.latitude,
            longitude: self.longitude,
            camera_make: self.camera_make,
            camera_model: self.camera_model,
            lens_make: self.lens_make,
            lens_model: self.lens_model,
            focal_length: self.focal_length,
            focal_length_35mm: self.focal_length_35mm,
            aperture: self.aperture,
            shutter: self.shutter,
            iso: self.iso,
            orientation: self.orientation,
            altitude: self.altitude,
            gps_direction: self.gps_direction,
            exif_raw: self.exif_raw,
            country_code: self.country_code,
            country_name: self.country_name,
            admin1: self.admin1,
            place: self.place,
            place_distance_m: self.place_distance_m,
            thumbnails_generated: self.thumbnails_generated,
        }
    }
}
