use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Strongly-typed identifier for an asset (photo or video).
///
/// Wrapping `Uuid` in a newtype makes it impossible to mix up an `AssetId`
/// with (eventually) a `PersonId` or `FaceId` at the type level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(Uuid);

impl AssetId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for AssetId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for AssetId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_assets_are_unique() {
        let a = AssetId::new();
        let b = AssetId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn round_trip_via_uuid() {
        let id = AssetId::new();
        let uuid = id.as_uuid();
        let restored = AssetId::from(uuid);
        assert_eq!(id, restored);
    }
}
