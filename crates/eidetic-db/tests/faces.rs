//! Face and person storage against a real SQLite database (ADR-0010).

use eidetic_core::Config;
use eidetic_db::{AssetsRepo, FacesRepo, InsertOutcome, NewAsset, NewFace};
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

async fn setup() -> (tempfile::TempDir, AssetsRepo, FacesRepo) {
    let (tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    (tmp, AssetsRepo::new(pool.clone()), FacesRepo::new(pool))
}

/// Insert an image asset and return its id. `n` varies the hash so several
/// assets can coexist.
async fn insert_asset(repo: &AssetsRepo, n: u8) -> eidetic_core::AssetId {
    let hash = format!("{:02x}", n).repeat(32);
    let outcome = repo
        .insert_asset(NewAsset {
            hash,
            original_filename: format!("img{n}.jpg"),
            storage_path: PathBuf::from(format!("/lib/img{n}.jpg")),
            file_size: 4096,
            mime_type: Some("image/jpeg".into()),
            date_taken: None,
            latitude: None,
            longitude: None,
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
            duration_secs: None,
            video_codec: None,
            pixel_width: None,
            pixel_height: None,
        })
        .await
        .expect("insert asset");
    match outcome {
        InsertOutcome::Inserted(id) | InsertOutcome::Existing(id) => id,
    }
}

fn face(asset_id: eidetic_core::AssetId, embedding: Vec<f32>) -> NewFace {
    NewFace {
        asset_id,
        bbox: (10.0, 20.0, 100.0, 120.0),
        landmarks: [
            (30.0, 50.0),
            (70.0, 50.0),
            (50.0, 70.0),
            (35.0, 90.0),
            (65.0, 90.0),
        ],
        score: 0.95,
        embedding,
    }
}

#[tokio::test]
async fn record_detection_stores_faces_and_round_trips_embeddings() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 1).await;

    let embedding = vec![0.5f32, 0.5, 0.5, 0.5];
    let ids = faces
        .record_detection(asset, &[face(asset, embedding.clone())])
        .await
        .expect("record");
    assert_eq!(ids.len(), 1);

    let stored = faces.fetch_embeddings(false).await.expect("fetch");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, ids[0]);
    assert_eq!(stored[0].asset_id, asset);
    assert_eq!(
        stored[0].embedding, embedding,
        "blob must round-trip exactly"
    );
    assert!(stored[0].person_id.is_none());
    assert!(!stored[0].pinned);
}

/// The reason detection runs are tracked separately from faces: a photo with
/// nobody in it must not be rescanned forever.
#[tokio::test]
async fn asset_with_zero_faces_is_not_rescanned() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 2).await;

    assert_eq!(faces.fetch_undetected().await.expect("before").len(), 1);

    faces
        .record_detection(asset, &[])
        .await
        .expect("record empty run");

    assert!(
        faces.fetch_undetected().await.expect("after").is_empty(),
        "a scanned asset with no faces must not come back as undetected"
    );
}

#[tokio::test]
async fn fetch_undetected_only_returns_unscanned_assets() {
    let (_tmp, assets, faces) = setup().await;
    let a = insert_asset(&assets, 3).await;
    let _b = insert_asset(&assets, 4).await;

    faces
        .record_detection(a, &[face(a, vec![1.0, 0.0])])
        .await
        .expect("record");

    let pending = faces.fetch_undetected().await.expect("fetch");
    assert_eq!(pending.len(), 1, "only the unscanned asset should remain");
    assert_ne!(pending[0].0, a);
}

/// The load-bearing guarantee of ADR-0010: automatic clustering must never
/// overwrite an attribution the user made by hand.
#[tokio::test]
async fn auto_assignment_cannot_overwrite_a_user_pinned_face() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 5).await;
    let ids = faces
        .record_detection(asset, &[face(asset, vec![1.0, 0.0])])
        .await
        .expect("record");
    let face_id = ids[0];

    let alice = faces.create_person().await.expect("person");
    let bob = faces.create_person().await.expect("person");

    faces
        .assign_face(face_id, alice, true)
        .await
        .expect("user assign");
    // An automatic pass now tries to claim the same face for someone else.
    faces
        .assign_face(face_id, bob, false)
        .await
        .expect("auto assign is a no-op here");

    let stored = faces.fetch_embeddings(false).await.expect("fetch");
    assert_eq!(
        stored[0].person_id,
        Some(alice),
        "user attribution must survive an automatic reassignment"
    );
    assert!(stored[0].pinned);
}

#[tokio::test]
async fn auto_assignment_does_update_an_auto_face() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 6).await;
    let ids = faces
        .record_detection(asset, &[face(asset, vec![1.0, 0.0])])
        .await
        .expect("record");

    let first = faces.create_person().await.expect("person");
    let second = faces.create_person().await.expect("person");
    faces.assign_face(ids[0], first, false).await.expect("auto");
    faces
        .assign_face(ids[0], second, false)
        .await
        .expect("auto");

    let stored = faces.fetch_embeddings(false).await.expect("fetch");
    assert_eq!(stored[0].person_id, Some(second));
}

#[tokio::test]
async fn rejecting_a_face_clears_it_and_is_recorded() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 7).await;
    let ids = faces
        .record_detection(asset, &[face(asset, vec![1.0, 0.0])])
        .await
        .expect("record");
    let person = faces.create_person().await.expect("person");

    faces
        .assign_face(ids[0], person, false)
        .await
        .expect("assign");
    faces.reject_face(ids[0], person).await.expect("reject");

    let stored = faces.fetch_embeddings(false).await.expect("fetch");
    assert!(
        stored[0].person_id.is_none(),
        "rejection must clear the attribution"
    );

    let rejections = faces.fetch_rejections().await.expect("rejections");
    assert_eq!(rejections, vec![(ids[0], person)]);
}

#[tokio::test]
async fn unassigned_only_filter_excludes_assigned_faces() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 8).await;
    let ids = faces
        .record_detection(
            asset,
            &[face(asset, vec![1.0, 0.0]), face(asset, vec![0.0, 1.0])],
        )
        .await
        .expect("record");

    let person = faces.create_person().await.expect("person");
    faces
        .assign_face(ids[0], person, false)
        .await
        .expect("assign");

    assert_eq!(faces.fetch_embeddings(false).await.expect("all").len(), 2);
    let pending = faces.fetch_embeddings(true).await.expect("unassigned");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, ids[1]);
}

#[tokio::test]
async fn links_are_stored_symmetrically_and_split_by_kind() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 9).await;
    let ids = faces
        .record_detection(
            asset,
            &[
                face(asset, vec![1.0, 0.0]),
                face(asset, vec![0.0, 1.0]),
                face(asset, vec![0.5, 0.5]),
            ],
        )
        .await
        .expect("record");

    faces.set_link(ids[0], ids[1], true).await.expect("merge");
    faces.set_link(ids[2], ids[0], false).await.expect("split");

    let (must, must_not) = faces.fetch_links().await.expect("links");
    assert_eq!(must.len(), 1);
    assert_eq!(must_not.len(), 1);

    // Writing the same pair in the opposite order must update, not duplicate.
    faces.set_link(ids[1], ids[0], true).await.expect("re-link");
    let (must, _) = faces.fetch_links().await.expect("links");
    assert_eq!(must.len(), 1, "reversed pair must not create a second row");
}

#[tokio::test]
async fn changing_a_link_kind_updates_in_place() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 10).await;
    let ids = faces
        .record_detection(
            asset,
            &[face(asset, vec![1.0, 0.0]), face(asset, vec![0.0, 1.0])],
        )
        .await
        .expect("record");

    faces.set_link(ids[0], ids[1], true).await.expect("merge");
    faces
        .set_link(ids[0], ids[1], false)
        .await
        .expect("now split");

    let (must, must_not) = faces.fetch_links().await.expect("links");
    assert!(must.is_empty(), "must_link should have been overwritten");
    assert_eq!(must_not.len(), 1);
}

#[tokio::test]
async fn merging_persons_moves_faces_and_hides_the_source() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 11).await;
    let ids = faces
        .record_detection(
            asset,
            &[face(asset, vec![1.0, 0.0]), face(asset, vec![0.0, 1.0])],
        )
        .await
        .expect("record");

    let keep = faces.create_person().await.expect("person");
    let gone = faces.create_person().await.expect("person");
    faces
        .assign_face(ids[0], keep, false)
        .await
        .expect("assign");
    faces
        .assign_face(ids[1], gone, false)
        .await
        .expect("assign");

    faces.merge_persons(gone, keep).await.expect("merge");

    let people = faces.list_persons().await.expect("list");
    assert_eq!(people.len(), 1, "merged-away person must not be listed");
    assert_eq!(people[0].id, keep);
    assert_eq!(people[0].face_count, 2, "both faces moved across");
}

#[tokio::test]
async fn list_persons_names_and_counts() {
    let (_tmp, assets, faces) = setup().await;
    let asset = insert_asset(&assets, 12).await;
    let ids = faces
        .record_detection(
            asset,
            &[
                face(asset, vec![1.0, 0.0]),
                face(asset, vec![0.0, 1.0]),
                face(asset, vec![0.5, 0.5]),
            ],
        )
        .await
        .expect("record");

    let many = faces.create_person().await.expect("person");
    let few = faces.create_person().await.expect("person");
    let empty = faces.create_person().await.expect("person");

    faces.assign_face(ids[0], many, false).await.expect("a");
    faces.assign_face(ids[1], many, false).await.expect("b");
    faces.assign_face(ids[2], few, false).await.expect("c");
    faces.name_person(many, "Alice").await.expect("name");

    let people = faces.list_persons().await.expect("list");
    assert_eq!(people.len(), 2, "person with no faces is not listed");
    // Ordered by face count, descending.
    assert_eq!(people[0].id, many);
    assert_eq!(people[0].name.as_deref(), Some("Alice"));
    assert_eq!(people[0].face_count, 2);
    assert_eq!(people[1].id, few);
    assert!(people[1].name.is_none(), "unnamed candidate stays unnamed");
    let _ = empty;
}

/// Deleting an asset must take its faces with it, or the faces table
/// accumulates orphans pointing at files that no longer exist.
#[tokio::test]
async fn faces_are_removed_with_their_asset() {
    let (_tmp, config) = temp_db();
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let assets = AssetsRepo::new(pool.clone());
    let faces = FacesRepo::new(pool.clone());

    let asset = insert_asset(&assets, 13).await;
    faces
        .record_detection(asset, &[face(asset, vec![1.0, 0.0])])
        .await
        .expect("record");
    assert_eq!(
        faces.fetch_embeddings(false).await.expect("before").len(),
        1
    );

    sqlx::query("DELETE FROM assets WHERE id = ?")
        .bind(asset.to_string())
        .execute(&pool)
        .await
        .expect("delete asset");

    assert!(
        faces
            .fetch_embeddings(false)
            .await
            .expect("after")
            .is_empty(),
        "ON DELETE CASCADE should have removed the face"
    );
}
