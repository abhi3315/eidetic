//! The SQLite that `connect` links must not be one with a published
//! vulnerability in the paths Eidetic uses.

use eidetic_core::Config;

/// SQLite before 3.54.0 can return uninitialised page-cache memory when a
/// database is opened alongside a `-wal` file of a different page size
/// (CVE-2026-119375). Eidetic always opens its library in WAL mode.
const MIN_SQLITE: (u32, u32, u32) = (3, 54, 0);

#[tokio::test]
async fn linked_sqlite_has_the_wal_page_size_fix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config {
        database_path: dir.path().join("eidetic.db"),
        paths: eidetic_core::Paths {
            library_dir: dir.path().join("library"),
            models_cache: dir.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let (version,): (String,) = sqlx::query_as("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    let linked = (parts[0], parts[1], parts.get(2).copied().unwrap_or(0));
    assert!(
        linked >= MIN_SQLITE,
        "linked SQLite {version} is older than {}.{}.{}",
        MIN_SQLITE.0,
        MIN_SQLITE.1,
        MIN_SQLITE.2
    );
}
