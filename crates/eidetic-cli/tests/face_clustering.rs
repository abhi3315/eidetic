//! Incremental face clustering through the real entry point:
//! `eidetic faces --cluster-only` over a seeded SQLite database.
//!
//! Hermetic: every Eidetic path and HOME point into a per-test temp dir.
//! `--cluster-only` skips detection and the active face model is resolved by
//! name only, so no model is loaded or downloaded.

use eidetic_core::{AssetId, FaceId, PersonId};
use eidetic_db::{FacesRepo, NewFace};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

const DETECTOR: &str = "yunet-2023mar";
const EMBEDDER: &str = "auraface-v1";
/// A second recognition model, so a person can hold members clustering never
/// sees as vectors.
const OLD_EMBEDDER: &str = "sface-2021dec";

/// Point every path the binary could touch into `home` — its temp dir
/// included, so tests can see what it leaves behind.
fn hermetic(cmd: &mut Command, home: &Path) {
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).expect("create the child's TMPDIR");
    cmd.env("HOME", home)
        .env("TMPDIR", tmp)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("EIDETIC_DATABASE_PATH", home.join("eidetic.db"))
        .env("EIDETIC_LIBRARY_DIR", home.join("library"))
        .env("EIDETIC_MODELS_CACHE", home.join("models"))
        .env("EIDETIC_MODEL", "base")
        .env("EIDETIC_FACE_MODEL", "auraface")
        .env("EIDETIC_LOG", "error");
}

fn config(home: &Path) -> eidetic_core::Config {
    eidetic_core::Config {
        database_path: home.join("eidetic.db"),
        paths: eidetic_core::Paths {
            library_dir: home.join("library"),
            models_cache: home.join("models"),
        },
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn new_asset(home: &Path, n: u8, mime: &str) -> eidetic_db::NewAsset {
    let name = format!("asset{n}");
    eidetic_db::NewAsset {
        hash: format!("{n:02x}").repeat(32),
        original_filename: name.clone(),
        storage_path: home.join("library").join(name),
        file_size: 1,
        mime_type: Some(mime.to_string()),
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
        thumbnails_generated: true,
        duration_secs: None,
        video_codec: None,
        pixel_width: None,
        pixel_height: None,
    }
}

fn normalize(v: [f32; 3]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / n).collect()
}

fn new_face(asset_id: AssetId, v: [f32; 3], ts_secs: Option<f64>) -> NewFace {
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
        embedding: normalize(v),
        ts_secs,
    }
}

/// Seeds one asset per face so every face gets its own detection run.
struct Seeder<'a> {
    home: &'a Path,
    assets: eidetic_db::AssetsRepo,
    faces: FacesRepo,
    next: u8,
}

impl Seeder<'_> {
    async fn face(&mut self, v: [f32; 3], video: bool, embedder: &str) -> FaceId {
        self.next += 1;
        let mime = if video { "video/mp4" } else { "image/jpeg" };
        let asset = match self
            .assets
            .insert_asset(new_asset(self.home, self.next, mime))
            .await
            .expect("insert asset")
        {
            eidetic_db::InsertOutcome::Inserted(id) => id,
            eidetic_db::InsertOutcome::Existing(_) => panic!("fixture hashes are unique"),
        };
        let ts = video.then_some(2.0);
        self.faces
            .record_detection(asset, &[new_face(asset, v, ts)], DETECTOR, embedder)
            .await
            .expect("record detection")[0]
    }
}

/// Who each face belongs to, read back after the run.
fn memberships(home: &Path) -> HashMap<FaceId, PersonId> {
    runtime().block_on(async {
        let pool = eidetic_db::connect(&config(home)).await.expect("reconnect");
        let got = FacesRepo::new(pool.clone())
            .fetch_face_persons()
            .await
            .expect("memberships");
        pool.close().await;
        got.into_iter().collect()
    })
}

#[test]
fn cluster_only_keeps_split_faces_out_of_their_partners_person() {
    let home = tempfile::tempdir().expect("tempdir");
    let home = home.path();

    struct Seeded {
        p: PersonId,
        q: PersonId,
        r: PersonId,
        pinned: FaceId,
        b: FaceId,
        h: FaceId,
        c1: FaceId,
        c2: FaceId,
        d: FaceId,
        j: FaceId,
        g: FaceId,
    }

    let s = runtime().block_on(async {
        let pool = eidetic_db::connect(&config(home)).await.expect("connect");
        let faces = FacesRepo::new(pool.clone());
        let mut seed = Seeder {
            home,
            assets: eidetic_db::AssetsRepo::new(pool.clone()),
            faces: FacesRepo::new(pool.clone()),
            next: 0,
        };
        let near_p = |y: f32| [1.0, y, 0.0];

        // P: photo exemplar E, a user-pinned photo face, a video member and a
        // member embedded by another model. Neither of the last two can ever
        // be an exemplar.
        let p = faces.create_person().await.expect("p");
        let e = seed.face(near_p(0.0), false, EMBEDDER).await;
        let pinned = seed.face(near_p(0.0), false, EMBEDDER).await;
        let a_video = seed.face(near_p(0.0), true, EMBEDDER).await;
        let a_old = seed.face(near_p(0.0), false, OLD_EMBEDDER).await;
        for f in [e, a_video, a_old] {
            faces.assign_face(f, p, false).await.expect("assign to p");
        }
        faces.assign_face(pinned, p, true).await.expect("pin to p");

        // Q: further from every candidate than P, but within attach distance.
        let q = faces.create_person().await.expect("q");
        let q1 = seed.face([1.0, 1.0, 0.0], false, EMBEDDER).await;
        faces.assign_face(q1, q, false).await.expect("assign to q");

        // R: a separate direction entirely.
        let r = faces.create_person().await.expect("r");
        let r1 = seed.face([0.0, 0.0, 1.0], false, EMBEDDER).await;
        faces.assign_face(r1, r, false).await.expect("assign to r");

        // Unassigned candidates, all nearest P unless noted.
        let b = seed.face(near_p(0.05), false, EMBEDDER).await;
        let h = seed.face(near_p(0.05), false, EMBEDDER).await;
        let c1 = seed.face(near_p(0.04), false, EMBEDDER).await;
        let c2 = seed.face(near_p(0.03), false, EMBEDDER).await;
        let d = seed.face(near_p(0.02), false, EMBEDDER).await;
        let j = seed.face(near_p(0.05), false, EMBEDDER).await;
        let g = seed.face([0.0, 0.05, 1.0], false, EMBEDDER).await;

        faces.set_link(b, a_video, false).await.expect("split b");
        faces.set_link(h, a_old, false).await.expect("split h");
        faces.set_link(c1, c2, false).await.expect("split c1/c2");
        faces.set_link(g, r1, false).await.expect("split g");
        faces.reject_face(j, p).await.expect("reject j");

        pool.close().await;
        Seeded {
            p,
            q,
            r,
            pinned,
            b,
            h,
            c1,
            c2,
            d,
            j,
            g,
        }
    });

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
    cmd.args(["faces", "--cluster-only"]);
    hermetic(&mut cmd, home);
    let out = cmd.output().expect("run eidetic faces");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let m = memberships(home);
    let person = |f: FaceId| m.get(&f).copied();

    assert_eq!(person(s.b), Some(s.q), "B is split from P's video member");
    assert_eq!(
        person(s.h),
        Some(s.q),
        "H is split from P's member embedded by another model"
    );
    let mut pair = [person(s.c1), person(s.c2)];
    pair.sort_by_key(|p| p.map(|p| p.as_uuid()));
    let mut want = [Some(s.p), Some(s.q)];
    want.sort_by_key(|p| p.map(|p| p.as_uuid()));
    assert_eq!(pair, want, "C1 and C2 must end up in different people");
    assert_eq!(person(s.d), Some(s.p), "an unconstrained face joins P");
    assert_eq!(person(s.j), Some(s.q), "a face rejected for P joins Q");
    assert_eq!(person(s.g), None, "G's only nearby person holds R1");
    assert_eq!(person(s.pinned), Some(s.p), "the pinned face stays put");
    assert!(
        !m.values().any(|p| ![s.p, s.q, s.r].contains(p)),
        "no new person is proposed from the leftovers"
    );
}
