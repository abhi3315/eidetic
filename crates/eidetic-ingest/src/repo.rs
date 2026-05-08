use crate::Result;
use chrono::{DateTime, Utc};
use eidetic_core::AssetId;
use std::path::PathBuf;

pub enum InsertOutcome {
    Inserted(AssetId),
    Existing(AssetId),
}

#[allow(async_fn_in_trait)]
pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> Result<InsertOutcome>;
}

#[derive(Clone)]
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
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(crate) struct MockAssetIndex {
        records: Mutex<HashMap<String, AssetId>>,
        inserted: Mutex<Vec<NewAsset>>,
    }

    impl MockAssetIndex {
        pub(crate) fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                inserted: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn all_inserted(&self) -> Vec<NewAsset> {
            self.inserted.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        pub(crate) fn seed(&self, hash: &str, id: AssetId) {
            self.records.lock().unwrap().insert(hash.to_string(), id);
        }
    }

    impl AssetIndex for MockAssetIndex {
        async fn find_by_hash(&self, hash: &str) -> crate::Result<Option<AssetId>> {
            Ok(self.records.lock().unwrap().get(hash).copied())
        }

        async fn insert_asset(&self, asset: NewAsset) -> crate::Result<InsertOutcome> {
            let mut records = self.records.lock().unwrap();
            if let Some(&existing_id) = records.get(&asset.hash) {
                return Ok(InsertOutcome::Existing(existing_id));
            }
            let id = AssetId::new();
            records.insert(asset.hash.clone(), id);
            drop(records);
            self.inserted.lock().unwrap().push(asset);
            Ok(InsertOutcome::Inserted(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::MockAssetIndex;

    #[tokio::test]
    async fn mock_insert_then_find() {
        let index = MockAssetIndex::new();
        let asset = NewAsset {
            hash: "abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd".to_string(),
            original_filename: "photo.jpg".to_string(),
            storage_path: PathBuf::from("/library/ab/c1/abc123.jpg"),
            file_size: 1024,
            mime_type: None,
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
        };
        let outcome = index.insert_asset(asset).await.unwrap();
        let id = match outcome {
            InsertOutcome::Inserted(id) => id,
            InsertOutcome::Existing(_) => panic!("expected Inserted, got Existing"),
        };
        let found = index
            .find_by_hash("abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd")
            .await
            .unwrap();
        assert_eq!(found, Some(id));
    }

    #[tokio::test]
    async fn mock_unknown_hash_returns_none() {
        let index = MockAssetIndex::new();
        let found = index
            .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert!(found.is_none());
    }
}
