use eidetic_core::Config;
use eidetic_db::PgAssetsRepo;
use eidetic_ingest::{AssetIndex, NewAsset};
use std::path::PathBuf;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

async fn start_db() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("tensorchord/vchord-postgres", "pg17-v0.4.3")
        .with_wait_for(WaitFor::message_on_stderr("ready to accept connections"))
        .with_env_var("POSTGRES_USER", "eidetic")
        .with_env_var("POSTGRES_PASSWORD", "eidetic")
        .with_env_var("POSTGRES_DB", "eidetic")
        .start()
        .await
        .expect("failed to start postgres container");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://eidetic:eidetic@127.0.0.1:{port}/eidetic");
    (container, url)
}

#[tokio::test]
async fn insert_asset_then_find_by_hash() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let hash = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222";
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "test.jpg".to_string(),
        storage_path: PathBuf::from("/library/aa/aa/aaaa1111.jpg"),
        file_size: 2048,
        mime_type: None,
    };

    let id = repo.insert_asset(asset).await.expect("insert");
    let found = repo.find_by_hash(hash).await.expect("find");
    assert_eq!(found, Some(id));
}

#[tokio::test]
async fn find_by_hash_returns_none_for_unknown() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };

    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let found = repo
        .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("find");
    assert!(found.is_none());
}
