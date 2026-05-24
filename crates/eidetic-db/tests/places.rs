use eidetic_core::Config;
use eidetic_core::geocoder::Place;
use eidetic_db::{InsertOutcome, NewAsset, PgAssetsRepo};
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
async fn update_place_columns_writes_all_five_fields() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let id = match repo
        .insert_asset(NewAsset {
            hash: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            original_filename: "img.jpg".into(),
            storage_path: PathBuf::from("/lib/11/img.jpg"),
            file_size: 1024,
            mime_type: Some("image/jpeg".into()),
            date_taken: None,
            latitude: Some(32.539),
            longitude: Some(75.972),
            camera_make: None,
            camera_model: None,
            lens_make: None,
            lens_model: None,
            focal_length: None,
            focal_length_35mm: None,
            aperture: None,
            shutter: None,
            iso: None,
            orientation: None,
            altitude: None,
            gps_direction: None,
            exif_raw: None,
            country_code: None,
            country_name: None,
            admin1: None,
            place: None,
            place_distance_m: None,
            thumbnails_generated: false,
        })
        .await
        .unwrap()
    {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("fresh insert"),
    };

    let place = Place {
        country_code: "IN".into(),
        country_name: "India".into(),
        admin1: "Himachal Pradesh".into(),
        place: "Dalhousie".into(),
        distance_m: 2794.0,
    };
    repo.update_place_columns(id, &place).await.unwrap();

    let detail = repo.fetch_by_id(id).await.unwrap().unwrap();
    assert_eq!(detail.country_code.as_deref(), Some("IN"));
    assert_eq!(detail.country_name.as_deref(), Some("India"));
    assert_eq!(detail.admin1.as_deref(), Some("Himachal Pradesh"));
    assert_eq!(detail.place.as_deref(), Some("Dalhousie"));
    assert_eq!(detail.place_distance_m, Some(2794.0));
}
