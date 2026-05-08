use eidetic_core::AssetId;
use eidetic_ingest::{AssetIndex, NewAsset};
use sqlx::PgPool;

pub struct PgAssetsRepo {
    pool: PgPool,
}

impl PgAssetsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[allow(async_fn_in_trait)]
impl AssetIndex for PgAssetsRepo {
    async fn find_by_hash(&self, hash: &str) -> eidetic_ingest::Result<Option<AssetId>> {
        let row: Option<(uuid::Uuid,)> = sqlx::query_as("SELECT id FROM assets WHERE hash = $1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| eidetic_ingest::Error::Index(Box::new(e)))?;

        Ok(row.map(|(uuid,)| AssetId::from(uuid)))
    }

    async fn insert_asset(&self, asset: NewAsset) -> eidetic_ingest::Result<AssetId> {
        let id = AssetId::new();
        sqlx::query(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id.as_uuid())
        .bind(&asset.hash)
        .bind(&asset.original_filename)
        .bind(asset.storage_path.to_string_lossy().as_ref())
        .bind(asset.file_size as i64)
        .bind(asset.mime_type.as_deref())
        .execute(&self.pool)
        .await
        .map_err(|e| eidetic_ingest::Error::Index(Box::new(e)))?;

        Ok(id)
    }
}
