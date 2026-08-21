use eidetic_core::Config;
use eidetic_core::geocoder::Place;
use eidetic_db::{AssetsRepo, InsertOutcome, NewAsset};
use std::path::PathBuf;

/// A throwaway SQLite database in its own temp dir. The `TempDir` guard must
/// outlive the test body, or the database file is deleted mid-test.
fn temp_db() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config {
        database_path: dir.path().join("eidetic.db"),
        ..Default::default()
    };
    (dir, config)
}

#[tokio::test]
async fn update_place_columns_writes_all_five_fields() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = AssetsRepo::new(pool);

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
