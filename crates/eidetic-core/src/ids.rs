use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Define a UUID newtype.
///
/// The three entity ids are byte-identical in behaviour and differ only in
/// which table they point at, so they share one definition rather than three
/// copies. The point of the newtypes is the type-level distinction: passing a
/// `PersonId` where a `FaceId` belongs must not compile.
macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_newtype! {
    /// Strongly-typed identifier for an asset (photo or video).
    AssetId
}

uuid_newtype! {
    /// One detected face in one asset (ADR-0010).
    FaceId
}

uuid_newtype! {
    /// A person: a named cluster of faces (ADR-0010).
    PersonId
}

/// A verified SHA-256 digest.
///
/// Only constructable via [`Sha256::from_bytes`] (from real SHA-256 output)
/// or [`Sha256::from_hex`] (validated parsing). The hex representation is
/// always exactly 64 lowercase chars, so CAS path slicing is always safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parse a 64-char lowercase hex string. Returns `None` for any input
    /// that is not exactly 64 valid hex characters.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let hi = char::from(chunk[0]).to_digit(16)? as u8;
            let lo = char::from(chunk[1]).to_digit(16)? as u8;
            bytes[i] = (hi << 4) | lo;
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Display for Sha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
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
    fn face_and_person_ids_are_unique_and_round_trip() {
        assert_ne!(FaceId::new(), FaceId::new());
        assert_ne!(PersonId::new(), PersonId::new());

        let f = FaceId::new();
        assert_eq!(FaceId::from(f.as_uuid()), f);
        assert_eq!(f.to_string(), f.as_uuid().to_string());
    }

    #[test]
    fn sha256_from_hex_valid() {
        let hex = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let hash = Sha256::from_hex(hex).unwrap();
        assert_eq!(hash.to_string(), hex);
    }

    #[test]
    fn sha256_from_hex_too_short_returns_none() {
        assert!(Sha256::from_hex("2cf24d").is_none());
    }

    #[test]
    fn sha256_from_hex_invalid_char_returns_none() {
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(Sha256::from_hex(bad).is_none());
    }

    #[test]
    fn sha256_round_trip_via_bytes() {
        let bytes = [0xABu8; 32];
        let hash = Sha256::from_bytes(bytes);
        let hex = hash.to_string();
        let restored = Sha256::from_hex(&hex).unwrap();
        assert_eq!(hash, restored);
    }
}
