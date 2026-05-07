use crate::Result;
use eidetic_core::AssetId;
use std::path::PathBuf;

#[allow(async_fn_in_trait)]
pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> Result<AssetId>;
}

pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(crate) struct MockAssetIndex {
        records: Mutex<HashMap<String, AssetId>>,
    }

    impl MockAssetIndex {
        pub(crate) fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
            }
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

        async fn insert_asset(&self, asset: NewAsset) -> crate::Result<AssetId> {
            let id = AssetId::new();
            self.records.lock().unwrap().insert(asset.hash.clone(), id);
            Ok(id)
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
        };
        let id = index.insert_asset(asset).await.unwrap();
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
