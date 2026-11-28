//! Real entry points (edit loop and stats): the `eidetic mcp` stdio server
//! (edit_reel, render, library_stats), `eidetic reel edit` and
//! `eidetic stats`.
//!
//! Hermetic by construction: every Eidetic path (database, library, model
//! cache) and HOME point into a per-test temp dir, projects are written by
//! hand, and no op here reaches the embedder — so no personal library is
//! read and no model is downloaded. Render tests need ffmpeg (CI installs
//! it) and skip without it; the media they cut is synthesised by ffmpeg.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A per-test home under the resolved temp dir. MCP tools refuse to write
/// below a symbolic link, which the temp root itself is on some hosts
/// (macOS's /var), so every test works in real directories.
fn scratch() -> tempfile::TempDir {
    let root = std::fs::canonicalize(std::env::temp_dir()).expect("the temp dir resolves");
    tempfile::tempdir_in(root).expect("create a test home")
}

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
        .env("EIDETIC_LOG", "error");
}

/// A live `eidetic mcp` session speaking newline-delimited JSON-RPC.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(home: &Path) -> Mcp {
        Mcp::start_with(home, &[])
    }

    fn start_with(home: &Path, args: &[&str]) -> Mcp {
        Mcp::start_env(home, args, &[])
    }

    fn start_env(home: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
        cmd.arg("mcp").args(args);
        hermetic(&mut cmd, home);
        cmd.envs(envs.iter().copied());
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn eidetic mcp");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "edit-workflow-test", "version": "0"},
            }),
        );
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").expect("write to eidetic mcp");
        self.stdin.flush().expect("flush eidetic mcp stdin");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read eidetic mcp");
            assert!(n > 0, "eidetic mcp exited before answering {method}");
            let msg: Value = serde_json::from_str(&line).expect("server speaks JSON");
            if msg["id"] == json!(id) {
                assert!(msg["error"].is_null(), "{method}: {msg}");
                return msg["result"].clone();
            }
        }
    }

    /// Call a tool: Ok(its JSON result) or Err(the error text it reported).
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let result = self.call_raw(tool, args);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool results are one text block")
            .to_string();
        if result["isError"] == json!(true) {
            Err(text)
        } else {
            Ok(serde_json::from_str(&text).expect("tool results are JSON"))
        }
    }

    /// Call a tool and return the whole `CallToolResult`, every content
    /// block included.
    fn call_raw(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": args}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct SlotSpec {
    source: PathBuf,
    kind: &'static str,
    start: f64,
    duration: f64,
    transition_in: Value,
}

fn photo(source: &Path, duration: f64) -> SlotSpec {
    SlotSpec {
        source: source.to_path_buf(),
        kind: "photo",
        start: 0.0,
        duration,
        transition_in: json!("cut"),
    }
}

/// Write a project file by hand — what `reel` / `create_reel` would have
/// planned, project id included — and return its path.
fn write_project(dir: &Path, (w, h): (u32, u32), frame: &str, slots: &[SlotSpec]) -> PathBuf {
    let slots: Vec<Value> = slots
        .iter()
        .map(|s| {
            json!({
                "asset": uuid::Uuid::new_v4().to_string(),
                "source": s.source,
                "kind": s.kind,
                "start": s.start,
                "duration": s.duration,
                "score": 0.3,
                "pinned": false,
                "transition_in": s.transition_in,
            })
        })
        .collect();
    let now = chrono::Utc::now();
    let project = json!({
        "version": 1,
        "id": uuid::Uuid::new_v4().to_string(),
        "created": now,
        "modified": now,
        "prompt": "synthetic",
        "width": w,
        "height": h,
        "frame": frame,
        "output": dir.join("reel.mp4"),
        "slots": slots,
    });
    let path = dir.join("reel.eidetic.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&project).unwrap()).unwrap();
    path
}

fn four_photos(dir: &Path) -> PathBuf {
    let slots: Vec<SlotSpec> = (0..4)
        .map(|i| photo(&dir.join(format!("p{i}.png")), 1.0 + i as f64))
        .collect();
    write_project(dir, (320, 180), "auto", &slots)
}

fn saved(project: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(project).unwrap()).unwrap()
}

fn assets(slots: &Value) -> Vec<String> {
    slots
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["asset"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn edit_reel_descriptions_state_the_contract_the_tests_check() {
    let home = scratch();
    let mut mcp = Mcp::start(home.path());
    let tools = mcp.request("tools/list", json!({}));
    let describe = |name: &str| -> String {
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no {name} tool"))["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let edit = describe("edit_reel");
    for claim in [
        "BEFORE the call",
        "a drop does not renumber later ops",
        "pin/unpin, retime, transition, swap, drop, reorder",
        "moves with it on reorder",
        "use two calls",
        "changes nothing on disk",
    ] {
        assert!(edit.contains(claim), "edit_reel description lost {claim:?}");
    }
    assert!(describe("render").contains("reel.mp4 → reel.preview.mp4"));
    // Every tool that writes for a project states the write rule.
    for tool in ["create_reel", "edit_reel", "render", "export_otio"] {
        let d = describe(tool);
        assert!(d.contains("symbolic link"), "{tool}: {d}");
        assert!(d.contains("library directory"), "{tool}: {d}");
    }
    for tool in ["render", "export_otio"] {
        assert!(describe(tool).contains("ops: [] adopts"), "{tool}");
    }
}

#[test]
fn edit_reel_applies_batches_against_the_original_numbering_and_persists() {
    let home = scratch();
    let dir = home.path();
    let project = four_photos(dir);
    let a = assets(&saved(&project)["slots"]);
    let mut mcp = Mcp::start(dir);
    let edit = |mcp: &mut Mcp, ops: Value| {
        mcp.call(
            "edit_reel",
            json!({"project": project.to_str().unwrap(), "ops": ops}),
        )
    };

    // A mixed batch, listed out of application order. Every number names
    // the list as it was before the call.
    let out = edit(
        &mut mcp,
        json!([
            {"op": "reorder", "order": [4, 1, 3]},
            {"op": "drop", "slot": 2},
            {"op": "transition", "slot": 1, "style": "crossfade:0.4"},
            {"op": "retime", "slot": 4, "secs": 1.5},
            {"op": "pin", "slot": 3},
        ]),
    )
    .unwrap();
    let slots = &out["slots"];
    assert_eq!(
        assets(slots),
        vec![a[3].clone(), a[0].clone(), a[2].clone()]
    );
    assert_eq!(slots[0]["secs"], json!(1.5));
    assert_eq!(slots[1]["transition_in"], json!("crossfade:0.4"));
    assert_eq!(slots[2]["pinned"], json!(true));
    assert_eq!(slots[0]["n"], json!(1));

    // Persisted: the file on disk is the cut list the tool returned.
    let disk = saved(&project);
    assert_eq!(assets(&disk["slots"]), assets(slots));
    assert_eq!(disk["slots"][2]["pinned"], json!(true));
    assert_eq!(disk["output"], json!(dir.join("reel.mp4")));

    // Refusals change nothing on disk, byte for byte.
    let before = std::fs::read(&project).unwrap();
    for (ops, want) in [
        (json!([{"op": "drop", "slot": 9}]), "no slot 9"),
        (json!([{"op": "drop", "slot": 3}]), "is pinned"),
        (
            json!([{"op": "swap", "slot": 3, "query": "anything"}]),
            "is pinned",
        ),
        // Pins apply first, so pinning in the same call already protects.
        (
            json!([{"op": "drop", "slot": 1}, {"op": "pin", "slot": 1}]),
            "is pinned",
        ),
        // Valid ops before an invalid one do not land either.
        (
            json!([{"op": "retime", "slot": 1, "secs": 4.0}, {"op": "reorder", "order": [1, 2]}]),
            "exactly once",
        ),
    ] {
        let err = edit(&mut mcp, ops.clone()).unwrap_err();
        assert!(err.contains(want), "{ops}: {err}");
        assert_eq!(std::fs::read(&project).unwrap(), before, "{ops} wrote");
    }

    // The documented two-call recipe: reorder, then transition against the
    // numbering the reorder returned.
    let out = edit(&mut mcp, json!([{"op": "reorder", "order": [3, 1, 2]}])).unwrap();
    assert_eq!(
        assets(&out["slots"]),
        vec![a[2].clone(), a[3].clone(), a[0].clone()]
    );
    let out = edit(
        &mut mcp,
        json!([{"op": "transition", "slot": 2, "style": "whip"}]),
    )
    .unwrap();
    assert_eq!(out["slots"][1]["asset"], json!(a[3]));
    assert_eq!(out["slots"][1]["transition_in"], json!("whip:0.2"));
    // Original slot 1's crossfade travelled with its shot to position 3.
    assert_eq!(out["slots"][2]["transition_in"], json!("crossfade:0.4"));
    assert_eq!(
        saved(&project)["slots"][1]["transition_in"]["whip"]["secs"],
        json!(0.2)
    );
}

#[test]
fn reel_edit_cli_shares_the_numbering_and_refuses_without_writing() {
    let home = scratch();
    let dir = home.path();
    let project = four_photos(dir);
    let a = assets(&saved(&project)["slots"]);
    let reel_edit = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
        cmd.args(["reel", "edit"]).arg(&project).args(args);
        hermetic(&mut cmd, dir);
        cmd.output().expect("run eidetic reel edit")
    };

    let before = std::fs::read(&project).unwrap();
    let out = reel_edit(&["--pin", "2", "--drop", "2", "--dry-run"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is pinned"));
    assert_eq!(std::fs::read(&project).unwrap(), before);

    let out = reel_edit(&["--drop", "2", "--reorder", "4,1,3", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        assets(&saved(&project)["slots"]),
        vec![a[3].clone(), a[0].clone(), a[2].clone()]
    );
    assert!(!dir.join("reel.mp4").exists(), "--dry-run rendered");
}

#[test]
fn reel_edit_cli_repairs_a_project_the_mcp_tools_refuse() {
    let home = scratch();
    let dir = home.path();
    let project = four_photos(dir);
    let mut broken = saved(&project);
    broken["slots"][0]["duration"] = json!(-1.0);
    std::fs::write(&project, serde_json::to_vec_pretty(&broken).unwrap()).unwrap();
    let arg = project.to_str().unwrap();
    let mut mcp = Mcp::start(dir);

    let err = mcp
        .call("export_otio", json!({"project": arg}))
        .unwrap_err();
    assert!(err.contains("unusable duration (-1)"), "{err}");
    assert!(err.contains("eidetic reel edit"), "{err}");

    // The CLI reads the operator's own file without range checks, so the
    // route the refusal names can retime the bad slot.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
    cmd.args(["reel", "edit"])
        .arg(&project)
        .args(["--retime", "1", "2", "--dry-run"]);
    hermetic(&mut cmd, dir);
    let out = cmd.output().expect("run eidetic reel edit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(saved(&project)["slots"][0]["duration"], json!(2.0));

    mcp.call("export_otio", json!({"project": arg})).unwrap();
    assert!(dir.join("reel.otio").is_file());
}

// ---------------------------------------------------------------------------
// Stats readiness
// ---------------------------------------------------------------------------

/// The id `EIDETIC_MODEL=base` resolves to (ADR-0013 pins the revision).
const PINNED_BASE: &str = "siglip2-base-patch16-256@d1114256522a37ffa257a0a58017348ab0058db2";
/// The same variant as migration 009 recorded it, before the pin.
const LEGACY_BASE: &str = "siglip2-base-patch16-256";

/// Seed `home`'s database: a photo embedded under the legacy id, one under
/// the pinned id, and a video with no frames at all.
fn seed_mixed_library(home: &Path) {
    let config = eidetic_core::Config {
        database_path: home.join("eidetic.db"),
        paths: eidetic_core::Paths {
            library_dir: home.join("library"),
            models_cache: home.join("models"),
        },
    };
    let asset = |byte: char, name: &str, mime: &str| eidetic_db::NewAsset {
        hash: byte.to_string().repeat(64),
        original_filename: name.to_string(),
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
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = eidetic_db::connect(&config).await.unwrap();
        let repo = eidetic_db::AssetsRepo::new(pool.clone());
        let insert = |a| async {
            match repo.insert_asset(a).await.unwrap() {
                eidetic_db::InsertOutcome::Inserted(id) => id,
                eidetic_db::InsertOutcome::Existing(_) => panic!("fixture hashes are unique"),
            }
        };
        let legacy = insert(asset('a', "legacy.jpg", "image/jpeg")).await;
        let pinned = insert(asset('b', "pinned.jpg", "image/jpeg")).await;
        insert(asset('c', "clip.mp4", "video/mp4")).await;
        let v = vec![0.0f32; 768];
        repo.store_embedding(legacy, &v, LEGACY_BASE).await.unwrap();
        repo.store_embedding(pinned, &v, PINNED_BASE).await.unwrap();
        pool.close().await;
    });
}

/// `SiglipEmbedder::load` creates the models dir before it downloads
/// anything, so its absence proves no load was attempted.
fn assert_no_model_touched(home: &Path) {
    assert!(!home.join("models").exists(), "a model load was attempted");
    assert!(
        !home.join("cache").join("huggingface").exists(),
        "something reached the HuggingFace cache"
    );
}

#[test]
fn stats_cli_reports_readiness_for_the_active_model_without_loading_it() {
    let home = scratch();
    seed_mixed_library(home.path());

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
    cmd.arg("stats");
    hermetic(&mut cmd, home.path());
    let out = cmd.output().expect("run eidetic stats");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let line = |label: &str| -> String {
        stdout
            .lines()
            .find(|l| l.trim_start().starts_with(label))
            .unwrap_or_else(|| panic!("no {label:?} line in:\n{stdout}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(
        line("Stored"),
        "Stored 2 (assets with an embedding from any model)"
    );
    assert_eq!(line("Model"), format!("Model {PINNED_BASE}"));
    assert_eq!(line("ready"), "ready 1");
    assert_eq!(line("pending"), "pending 1 images, 1 videos");
    assert!(stdout.contains("run `eidetic embed`"), "{stdout}");
    assert_no_model_touched(home.path());
}

#[test]
fn library_stats_reports_readiness_for_the_active_model_without_loading_it() {
    let home = scratch();
    seed_mixed_library(home.path());

    let mut mcp = Mcp::start(home.path());
    let s = mcp.call("library_stats", json!({})).unwrap();
    assert_eq!(s["assets"], json!(3));
    assert_eq!(s["embedded"], json!(2), "stored vectors from any model");
    assert_eq!(s["model"], json!(PINNED_BASE));
    assert_eq!(s["ready"], json!(1));
    assert_eq!(s["pending"], json!({"images": 1, "videos": 1}));
    assert!(s.get("needs_embed").is_none(), "{s}");
    drop(mcp);
    assert_no_model_touched(home.path());
}

// ---------------------------------------------------------------------------
// Rendering (needs ffmpeg)
// ---------------------------------------------------------------------------

fn ffmpeg(args: &[&str]) {
    let ffmpeg = eidetic_ingest::video::ffmpeg().expect("caller checked for ffmpeg");
    let ok = Command::new(ffmpeg)
        .args(["-y", "-v", "error"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg {args:?} failed");
}

fn size_of(video: &Path) -> (i64, i64) {
    let p = eidetic_ingest::video::probe(video)
        .unwrap()
        .expect("ffprobe ships with ffmpeg");
    (p.width.unwrap(), p.height.unwrap())
}

fn render_litter(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".eidetic-render-"))
        .collect()
}

#[test]
fn previews_and_failed_renders_leave_the_full_render_alone() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let clip = dir.join("clip.mp4");
    let still = dir.join("still.png");
    let clip_arg = clip.to_str().unwrap();
    let still_arg = still.to_str().unwrap();
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=256x144:rate=30",
        "-t",
        "3",
        "-pix_fmt",
        "yuv420p",
        clip_arg,
    ]);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=256x144",
        "-frames:v",
        "1",
        still_arg,
    ]);

    let project = write_project(
        dir,
        (256, 144),
        "cover",
        &[
            SlotSpec {
                source: clip.clone(),
                kind: "video",
                start: 0.5,
                duration: 1.0,
                transition_in: json!("cut"),
            },
            SlotSpec {
                transition_in: json!({"crossfade": {"secs": 0.3}}),
                ..photo(&still, 1.0)
            },
        ],
    );
    let output = dir.join("reel.mp4");
    let preview = dir.join("reel.preview.mp4");
    let mut mcp = Mcp::start(dir);
    let render = |mcp: &mut Mcp, preview: bool| {
        mcp.call(
            "render",
            json!({"project": project.to_str().unwrap(), "preview": preview}),
        )
    };

    let full = render(&mut mcp, false).unwrap();
    assert_eq!(full["output"], json!(output));
    assert_eq!(full["size"], json!("256x144"));
    assert_eq!(size_of(&output), (256, 144));
    let full_bytes = std::fs::read(&output).unwrap();
    let project_bytes = std::fs::read(&project).unwrap();

    // A preview reports where it actually went, and that is not the output.
    let out = render(&mut mcp, true).unwrap();
    assert_eq!(out["output"], json!(preview));
    assert_eq!(out["size"], json!("128x72"));
    assert_eq!(out["preview"], json!(true));
    assert_eq!(size_of(&preview), (128, 72));
    assert_eq!(
        std::fs::read(&output).unwrap(),
        full_bytes,
        "preview clobbered the reel"
    );
    assert_eq!(
        std::fs::read(&project).unwrap(),
        project_bytes,
        "preview edited the project"
    );
    let preview_bytes = std::fs::read(&preview).unwrap();

    // Break the cut (its photo vanished) and render both ways: each fails,
    // each earlier file survives, and no staging directory is left behind.
    std::fs::remove_file(&still).unwrap();
    for as_preview in [false, true] {
        let err = render(&mut mcp, as_preview).unwrap_err();
        assert!(err.contains("segment 2"), "{err}");
    }
    assert_eq!(std::fs::read(&output).unwrap(), full_bytes);
    assert_eq!(std::fs::read(&preview).unwrap(), preview_bytes);
    assert_eq!(std::fs::read(&project).unwrap(), project_bytes);
    assert_eq!(render_litter(dir), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Preview sheets (goals-v0.9.md phase 1)
// ---------------------------------------------------------------------------

/// Every image block anywhere in a tool result.
fn image_blocks(result: &Value) -> Vec<Value> {
    result["content"]
        .as_array()
        .map(|c| c.iter().filter(|b| b["type"] == "image").cloned().collect())
        .unwrap_or_default()
}

fn tool_names(mcp: &mut Mcp) -> Vec<String> {
    mcp.request("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn preview_sheet_is_hidden_and_refused_without_the_opt_in() {
    let home = scratch();
    let dir = home.path();
    let project = four_photos(dir);
    let before = std::fs::read(&project).unwrap();
    let mut mcp = Mcp::start(dir);

    assert!(!tool_names(&mut mcp).contains(&"preview_sheet".to_string()));
    // Called anyway (a client can name a tool it was never offered): an
    // actionable refusal, and no pixels in any block.
    for project in [project.clone(), dir.join("does-not-exist.eidetic.json")] {
        let result = mcp.call_raw(
            "preview_sheet",
            json!({"project": project.to_str().unwrap()}),
        );
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(image_blocks(&result).is_empty(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("--allow-previews"), "{text}");
        // Refused before the project is read: a missing file is not reported.
        assert!(!text.contains("failed to read"), "{text}");
    }
    assert_eq!(std::fs::read(&project).unwrap(), before);
    assert!(listing(&dir.join("tmp")).is_empty());

    // With the opt-in the tool is offered.
    let mut mcp = Mcp::start_with(dir, &["--allow-previews"]);
    assert!(tool_names(&mut mcp).contains(&"preview_sheet".to_string()));
    let help = Command::new(env!("CARGO_BIN_EXE_eidetic"))
        .args(["mcp", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--allow-previews"), "{help}");
    assert!(help.contains("Off by default"), "{help}");
}

/// Decode the sheet a successful preview_sheet returned, with its metadata.
fn sheet(result: &Value) -> (Value, image::RgbImage) {
    use base64::Engine;
    assert_ne!(result["isError"], json!(true), "{result}");
    let meta: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    let images = image_blocks(result);
    assert_eq!(images.len(), 1, "{result}");
    assert_eq!(images[0]["mimeType"], json!("image/jpeg"));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(images[0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::Jpeg
    );
    let img = image::load_from_memory(&bytes).unwrap().into_rgb8();
    assert_eq!(
        meta["size"],
        json!(format!("{}x{}", img.width(), img.height()))
    );
    (meta, img)
}

/// Average colour of a 9x9 patch centred at (x, y).
fn patch(img: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    let mut sum = [0u32; 3];
    for dy in 0..9 {
        for dx in 0..9 {
            let p = img.get_pixel(x + dx - 4, y + dy - 4).0;
            for c in 0..3 {
                sum[c] += p[c] as u32;
            }
        }
    }
    sum.map(|s| (s / 81) as u8)
}

fn is(rgb: [u8; 3], want: [bool; 3]) -> bool {
    rgb.iter()
        .zip(want)
        .all(|(&v, on)| if on { v > 170 } else { v < 90 })
}

const RED: [bool; 3] = [true, false, false];
const GREEN: [bool; 3] = [false, true, false];
const BLUE: [bool; 3] = [false, false, true];
const YELLOW: [bool; 3] = [true, true, false];
const BLACK: [bool; 3] = [false, false, false];

/// The library directory [`hermetic`] points the server at, created.
/// preview_sheet only decodes sources inside it.
fn library(home: &Path) -> PathBuf {
    let lib = home.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    lib
}

/// A stand-in for ffmpeg and ffprobe, and the marker any run of it leaves:
/// every run appends its arguments to it, one per line.
#[cfg(unix)]
fn fake_ffmpeg(home: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = home.join("subprocess-ran");
    let fake = home.join("fake-ffmpeg.sh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    (fake, marker)
}

/// Top-left corner of tile `k` on a sheet of 320x180 tiles, two columns.
fn tile_origin(k: u32) -> (u32, u32) {
    (4 + (k % 2) * 324, 4 + (k / 2) * 224)
}

#[test]
fn preview_sheet_shows_each_slot_as_the_render_frames_it() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let arg = |p: &Path| p.to_str().unwrap().to_string();
    let lib = library(dir);
    // Red for the first second, green for the second, blue for the third:
    // the colour of a tile says which moment it was taken at.
    let clip = lib.join("rgb.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=160x90:r=30:d=1",
        "-f",
        "lavfi",
        "-i",
        "color=c=lime:s=160x90:r=30:d=1",
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=160x90:r=30:d=1",
        "-filter_complex",
        "[0][1][2]concat=n=3:v=1",
        "-pix_fmt",
        "yuv420p",
        &arg(&clip),
    ]);
    let still = lib.join("yellow.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=yellow:s=160x90",
        "-frames:v",
        "1",
        &arg(&still),
    ]);
    // Landscape pixels (red left, blue right) a phone flagged as rotated:
    // the render shows it upright, i.e. portrait with red and blue stacked.
    let flat = lib.join("flat.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=80x40:r=30:d=1",
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=80x40:r=30:d=1",
        "-filter_complex",
        "[0][1]hstack",
        "-pix_fmt",
        "yuv420p",
        &arg(&flat),
    ]);
    let rotated = lib.join("rotated.mp4");
    ffmpeg(&[
        "-display_rotation",
        "90",
        "-i",
        &arg(&flat),
        "-c",
        "copy",
        &arg(&rotated),
    ]);

    let video = |source: &Path, start: f64, duration: f64, transition_in: Value| SlotSpec {
        source: source.to_path_buf(),
        kind: "video",
        start,
        duration,
        transition_in,
    };
    let project = write_project(
        dir,
        (320, 180),
        "pad",
        &[
            video(&clip, 1.0, 1.0, json!("cut")),
            SlotSpec {
                transition_in: json!({"crossfade": {"secs": 0.5}}),
                ..photo(&still, 2.0)
            },
            video(&clip, 2.0, 0.8, json!({"whip": {"secs": 0.2}})),
            video(&rotated, 0.0, 1.0, json!("cut")),
        ],
    );
    let output = dir.join("reel.mp4");
    std::fs::write(&output, b"the final reel").unwrap();
    let project_bytes = std::fs::read(&project).unwrap();

    let mut mcp = Mcp::start_with(dir, &["--allow-previews"]);
    // After startup, which opens (creates) the database here.
    let files_before = listing(dir);
    let result = mcp.call_raw("preview_sheet", json!({"project": arg(&project)}));
    let (meta, img) = sheet(&result);

    // Midpoints of the played spans; photos have no timestamp to pick.
    let at: Vec<f64> = meta["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["at_secs"].as_f64().unwrap())
        .collect();
    assert_eq!(at, vec![1.5, 0.0, 2.4, 0.5]);
    assert_eq!(meta["slots"], json!("1-4 of 4"));
    assert_eq!(meta["next_first"], Value::Null);
    assert_eq!(meta["tiles"][1]["transition_in"], json!("crossfade:0.5"));
    // Four 320x180 tiles with 40px label strips, two by two, 4px gaps.
    assert_eq!(img.dimensions(), (652, 452));

    let centre = |k: u32| {
        let (x, y) = tile_origin(k);
        patch(&img, x + 160, y + 90)
    };
    assert!(is(centre(0), GREEN), "slot 1 at 1.5s: {:?}", centre(0));
    assert!(is(centre(1), YELLOW), "slot 2 photo: {:?}", centre(1));
    assert!(is(centre(2), BLUE), "slot 3 at 2.4s: {:?}", centre(2));

    // Rotated clip: upright means portrait, so the pad frame pillarboxes
    // it (black at the sides) and its two colours are stacked vertically.
    let (x, y) = tile_origin(3);
    assert!(is(patch(&img, x + 20, y + 90), BLACK));
    let (top, bottom) = (patch(&img, x + 160, y + 30), patch(&img, x + 160, y + 150));
    assert!(
        (is(top, RED) && is(bottom, BLUE)) || (is(top, BLUE) && is(bottom, RED)),
        "rotated tile not upright: top {top:?} bottom {bottom:?}"
    );

    // Every tile has a label drawn in its strip.
    for k in 0..4 {
        let (x, y) = tile_origin(k);
        let lit = (x..x + 320)
            .flat_map(|px| (y + 180..y + 220).map(move |py| (px, py)))
            .filter(|&(px, py)| img.get_pixel(px, py).0.iter().all(|&v| v > 200))
            .count();
        assert!(lit > 50, "tile {k} label has {lit} lit pixels");
    }

    // Read-only: the project, the render and the directory are as they
    // were, and the work dir is gone.
    assert_eq!(std::fs::read(&project).unwrap(), project_bytes);
    assert_eq!(std::fs::read(&output).unwrap(), b"the final reel");
    assert_eq!(listing(dir), files_before);
    assert!(listing(&dir.join("tmp")).is_empty());
}

#[test]
fn preview_sheet_pages_long_reels_and_explains_bad_media() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let lib = library(dir);
    let still = lib.join("still.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=gray:s=64x36",
        "-frames:v",
        "1",
        still.to_str().unwrap(),
    ]);
    let clip = lib.join("short.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x36:rate=30",
        "-t",
        "1",
        "-pix_fmt",
        "yuv420p",
        clip.to_str().unwrap(),
    ]);
    let mut mcp = Mcp::start_with(dir, &["--allow-previews"]);
    let preview = |mcp: &mut Mcp, project: &Path, first: Option<u32>| {
        let mut args = json!({"project": project.to_str().unwrap()});
        if let Some(f) = first {
            args["first"] = json!(f);
        }
        mcp.call_raw("preview_sheet", args)
    };
    let error_text = |result: &Value| -> String {
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(image_blocks(result).is_empty(), "{result}");
        result["content"][0]["text"].as_str().unwrap().to_string()
    };

    // 30 slots: a sheet holds 24, and says where the next page starts.
    let slots: Vec<SlotSpec> = (0..30).map(|_| photo(&still, 1.0)).collect();
    let long = write_project(dir, (1080, 1920), "auto", &slots);
    let (meta, img) = sheet(&preview(&mut mcp, &long, None));
    assert_eq!(meta["tiles"].as_array().unwrap().len(), 24);
    assert_eq!(meta["slots"], json!("1-24 of 30"));
    assert_eq!(meta["next_first"], json!(25));
    assert!(
        img.width() <= 2048 && img.height() <= 2048,
        "{:?}",
        img.dimensions()
    );
    let (meta, _) = sheet(&preview(&mut mcp, &long, Some(25)));
    assert_eq!(meta["slots"], json!("25-30 of 30"));
    assert_eq!(meta["tiles"][0]["n"], json!(25));
    for first in [0, 31] {
        let err = error_text(&preview(&mut mcp, &long, Some(first)));
        assert!(err.contains("outside the cut list"), "{err}");
    }

    // Each broken slot is named, with what to do about it.
    let garbage = lib.join("corrupt.png");
    std::fs::write(&garbage, b"not an image at all").unwrap();
    for (slot, want) in [
        (
            photo(&lib.join("gone.png"), 1.0),
            "slot 2's source is missing",
        ),
        (photo(&garbage, 1.0), "ffmpeg could not decode slot 2"),
        (
            SlotSpec {
                source: clip.clone(),
                kind: "video",
                start: 5.0,
                duration: 1.0,
                transition_in: json!("cut"),
            },
            "runs past the end",
        ),
        (photo(&still, -1.0), "unusable duration"),
    ] {
        let project = write_project(dir, (320, 180), "auto", &[photo(&still, 1.0), slot]);
        let before = std::fs::read(&project).unwrap();
        let err = error_text(&preview(&mut mcp, &project, None));
        assert!(err.contains(want), "want {want:?}: {err}");
        assert_eq!(std::fs::read(&project).unwrap(), before);
    }
    assert!(listing(&dir.join("tmp")).is_empty());
}

#[test]
fn preview_sheet_frames_extreme_aspects_within_its_memory_budget() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let arg = |p: &Path| p.to_str().unwrap().to_string();
    let lib = library(dir);
    // A 1 MP sliver. Scaling it to cover a 180x320 tile BEFORE cropping
    // needs a 1280000x320 intermediate (410 MB of luma alone), which the
    // per-grab -max_alloc budget refuses; cropping first needs 9x16.
    let sliver = lib.join("sliver.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64000x16",
        "-frames:v",
        "1",
        &arg(&sliver),
    ]);
    let clip = lib.join("clip.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=160x90:r=30:d=1",
        "-pix_fmt",
        "yuv420p",
        &arg(&clip),
    ]);
    let scenic = |source: &Path| SlotSpec {
        kind: "photo_scenic",
        ..photo(source, 1.0)
    };
    let video = SlotSpec {
        source: clip.clone(),
        kind: "video",
        start: 0.0,
        duration: 1.0,
        transition_in: json!("cut"),
    };
    let mut mcp = Mcp::start_with(dir, &["--allow-previews"]);

    // Portrait reel (180x320 tiles): cover (photo), blur-fill (scenic).
    let project = write_project(
        dir,
        (1080, 1920),
        "auto",
        &[photo(&sliver, 1.0), scenic(&sliver)],
    );
    let (meta, img) = sheet(&mcp.call_raw("preview_sheet", json!({"project": arg(&project)})));
    assert_eq!(meta["tiles"].as_array().unwrap().len(), 2);
    // The photo tile (first, at 4,4) is the sliver's centre, cover-cropped:
    // all red.
    let c = patch(&img, 4 + 90, 4 + 160);
    assert!(is(c, RED), "cover tile of the sliver: {c:?}");

    // A 10:1 reel makes 320x32 tiles, too small for the render's blur
    // radius of 20: blur-fill must still work for videos and scenic photos.
    let project = write_project(
        dir,
        (3200, 320),
        "blur",
        &[video, scenic(&sliver), photo(&sliver, 1.0)],
    );
    let (meta, img) = sheet(&mcp.call_raw("preview_sheet", json!({"project": arg(&project)})));
    assert_eq!(meta["tiles"].as_array().unwrap().len(), 3);
    assert!(img.width() <= 2048 && img.height() <= 2048);
    assert!(listing(&dir.join("tmp")).is_empty());
}

#[cfg(unix)]
#[test]
fn preview_sheet_refuses_sources_outside_the_library() {
    use std::os::unix::fs::symlink;
    let home = scratch();
    let dir = home.path();
    let (fake, marker) = fake_ffmpeg(dir);
    let mut mcp = Mcp::start_env(
        dir,
        &["--allow-previews"],
        &[
            ("EIDETIC_FFMPEG_PATH", fake.as_path()),
            ("EIDETIC_FFPROBE_PATH", fake.as_path()),
        ],
    );
    let lib = library(dir);
    let outside = dir.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let secret = outside.join("secret.png");
    std::fs::write(&secret, b"TOP-SECRET-PIXELS").unwrap();
    let ok = lib.join("ok.png");
    std::fs::write(&ok, b"not decoded").unwrap();
    symlink(&secret, lib.join("link.png")).unwrap();
    symlink(&outside, lib.join("sub")).unwrap();

    let refuse = |mcp: &mut Mcp, slots: &[SlotSpec], want: &str| {
        let project = write_project(dir, (320, 180), "auto", slots);
        let before = std::fs::read(&project).unwrap();
        let result = mcp.call_raw(
            "preview_sheet",
            json!({"project": project.to_str().unwrap()}),
        );
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(image_blocks(&result).is_empty(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(want), "want {want:?}: {text}");
        assert!(!text.contains("TOP-SECRET"), "{text}");
        assert!(!marker.exists(), "ffmpeg ran before refusing: {text}");
        assert_eq!(std::fs::read(&project).unwrap(), before);
        assert!(listing(&dir.join("tmp")).is_empty());
    };
    for source in [
        secret.clone(),
        lib.join("..").join("outside").join("secret.png"),
        lib.join("link.png"),
        lib.join("sub").join("secret.png"),
    ] {
        refuse(
            &mut mcp,
            &[photo(&ok, 1.0), photo(&source, 1.0)],
            "slot 2's source is outside the library",
        );
    }
    refuse(
        &mut mcp,
        &[photo(&ok, 1.0), photo(&outside.join("nope.png"), 1.0)],
        "slot 2's source is missing",
    );
    refuse(
        &mut mcp,
        &[photo(&ok, 1.0), photo(&lib, 1.0)],
        "slot 2's source is not a regular file",
    );
    // The whole project is checked, not only the page the sheet shows.
    let mut long: Vec<SlotSpec> = (0..29).map(|_| photo(&ok, 1.0)).collect();
    long.push(photo(&secret, 1.0));
    refuse(&mut mcp, &long, "slot 30's source is outside the library");

    // The marker is live: an all-library project does reach ffmpeg, which
    // opens the resolved file, not the link the project names.
    let alias = lib.join("alias.png");
    symlink(&ok, &alias).unwrap();
    let project = write_project(dir, (320, 180), "auto", &[photo(&alias, 1.0)]);
    let err = mcp
        .call(
            "preview_sheet",
            json!({"project": project.to_str().unwrap()}),
        )
        .unwrap_err();
    assert!(err.contains("ffmpeg could not decode slot 1"), "{err}");
    let args = std::fs::read_to_string(&marker).expect("the fake ffmpeg never ran");
    let args: Vec<&str> = args.lines().collect();
    let input = args.iter().position(|a| *a == "-i").map(|i| args[i + 1]);
    let real = std::fs::canonicalize(&ok).unwrap();
    assert_eq!(input, Some(real.to_str().unwrap()), "{args:?}");
}

#[cfg(unix)]
#[test]
fn preview_sheet_shows_library_media_reached_through_a_symlink() {
    use eidetic_ingest::thumbnail::{ThumbSize, thumbnail_path};
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let real = dir.join("real-library");
    let link = dir.join("library-link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // Laid out the way ingest stores media: content-addressed originals,
    // medium thumbnails under .thumbs.
    let stored = |byte: u8, ext: &str| {
        let hash = eidetic_core::Sha256::from_hex(&format!("{byte:02x}").repeat(32)).unwrap();
        let hex = hash.to_string();
        let path = link
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(format!("{hex}.{ext}"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        (path, hash)
    };
    let still = |path: &Path, colour: &str| {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={colour}:s=160x90"),
            "-frames:v",
            "1",
            path.to_str().unwrap(),
        ]);
    };
    let (clip, _) = stored(0xab, "mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x90:rate=30",
        "-t",
        "2",
        "-pix_fmt",
        "yuv420p",
        clip.to_str().unwrap(),
    ]);
    let (original, hash) = stored(0xcd, "png");
    still(&original, "red");
    let thumb = thumbnail_path(&link, &hash, ThumbSize::Medium);
    std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
    still(&thumb, "red");
    let (plain, _) = stored(0xef, "png");
    still(&plain, "blue");
    let via_real = real.join(plain.strip_prefix(&link).unwrap());

    let project = write_project(
        dir,
        (320, 180),
        "auto",
        &[
            SlotSpec {
                source: clip,
                kind: "video",
                start: 0.5,
                duration: 1.0,
                transition_in: json!("cut"),
            },
            photo(&thumb, 1.0),
            photo(&plain, 1.0),
            photo(&via_real, 1.0),
        ],
    );
    let mut mcp = Mcp::start_env(
        dir,
        &["--allow-previews"],
        &[("EIDETIC_LIBRARY_DIR", link.as_path())],
    );
    let (meta, _) = sheet(&mcp.call_raw(
        "preview_sheet",
        json!({"project": project.to_str().unwrap()}),
    ));
    assert_eq!(meta["tiles"].as_array().unwrap().len(), 4);
    assert!(listing(&dir.join("tmp")).is_empty());
}

#[test]
fn preview_sheet_refuses_project_paths_it_cannot_read_boundedly() {
    let home = scratch();
    let dir = home.path();
    let mut mcp = Mcp::start_with(dir, &["--allow-previews"]);
    let mut refuse = |path: &Path, want: &str| {
        let result = mcp.call_raw("preview_sheet", json!({"project": path.to_str().unwrap()}));
        assert_eq!(result["isError"], json!(true), "{result}");
        assert!(image_blocks(&result).is_empty());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(want), "want {want:?}: {text}");
    };

    // Bigger than any project: refused after reading at most the cap.
    let huge = dir.join("huge.eidetic.json");
    std::fs::write(&huge, vec![b' '; 9 << 20]).unwrap();
    refuse(&huge, "larger than");
    // Not a regular file.
    refuse(dir, "not a regular file");
    // A FIFO with no writer: opening it would block forever.
    let fifo = dir.join("fifo.eidetic.json");
    if Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success())
    {
        refuse(&fifo, "not a regular file");
    } else {
        eprintln!("skipping the FIFO case: mkfifo unavailable");
    }
}

// ---------------------------------------------------------------------------
// Project paths from the agent: bounded reads, range checks
// ---------------------------------------------------------------------------

/// Every MCP tool that reads a project, called on `project`. edit_reel
/// retimes slot 1, which probes the source when that slot is a video.
fn project_tool_calls(project: &Path) -> Vec<(&'static str, Value)> {
    let p = project.to_str().unwrap();
    vec![
        (
            "edit_reel",
            json!({"project": p, "ops": [{"op": "retime", "slot": 1, "secs": 1.0}]}),
        ),
        ("render", json!({"project": p, "preview": false})),
        ("render", json!({"project": p, "preview": true})),
        ("export_otio", json!({"project": p})),
        ("preview_sheet", json!({"project": p})),
    ]
}

/// What a path holds, comparable across calls: a directory's listing, a
/// FIFO's kind, or a file's bytes.
#[cfg(unix)]
fn contents(path: &Path) -> Vec<u8> {
    use std::os::unix::fs::FileTypeExt;
    let meta = std::fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        listing(path).join("\n").into_bytes()
    } else if meta.file_type().is_fifo() {
        b"<fifo>".to_vec()
    } else {
        std::fs::read(path).unwrap()
    }
}

/// Outputs a project in `dir` could write, with recognisable bytes.
fn write_sentinels(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    ["reel.mp4", "reel.preview.mp4", "reel.otio"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            let bytes = format!("sentinel {name}").into_bytes();
            std::fs::write(&path, &bytes).unwrap();
            (path, bytes)
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn every_project_tool_refuses_unreadable_projects_without_writing() {
    let home = scratch();
    let dir = home.path().join("work");
    std::fs::create_dir(&dir).unwrap();
    let sentinels = write_sentinels(&dir);

    let huge = dir.join("huge.eidetic.json");
    std::fs::write(&huge, vec![b' '; 9 << 20]).unwrap();
    let subdir = dir.join("a-directory.eidetic.json");
    std::fs::create_dir(&subdir).unwrap();
    let not_a_project = dir.join("hello.eidetic.json");
    std::fs::write(&not_a_project, br#"{"hello":1}"#).unwrap();
    let truncated = dir.join("truncated.eidetic.json");
    let whole = std::fs::read(four_photos(&dir)).unwrap();
    std::fs::write(&truncated, &whole[..whole.len() / 2]).unwrap();
    let mut cases = vec![
        (huge, "larger than"),
        (subdir, "not a regular file"),
        (not_a_project, "not a reel project file"),
        (truncated, "not a reel project file"),
    ];
    // A FIFO with no writer: opening it for reading would block forever.
    let fifo = dir.join("fifo.eidetic.json");
    if Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success())
    {
        cases.push((fifo, "not a regular file"));
    } else {
        eprintln!("skipping the FIFO case: mkfifo unavailable");
    }

    let mut mcp = Mcp::start_with(home.path(), &["--allow-previews"]);
    let files_before = listing(&dir);
    for (project, want) in &cases {
        let before = contents(project);
        for (tool, args) in project_tool_calls(project) {
            let err = mcp.call(tool, args.clone()).unwrap_err();
            assert!(err.contains(want), "{tool} {args}: want {want:?}: {err}");
            assert_eq!(contents(project), before, "{tool} changed {project:?}");
            for (path, bytes) in &sentinels {
                assert_eq!(
                    &std::fs::read(path).unwrap(),
                    bytes,
                    "{tool} wrote {path:?}"
                );
            }
            assert_eq!(listing(&dir), files_before, "{tool} {args}");
            assert!(listing(&home.path().join("tmp")).is_empty(), "{tool}");
        }
    }
}

#[cfg(unix)]
#[test]
fn every_project_tool_refuses_out_of_range_projects_before_any_subprocess() {
    let home = scratch();
    let dir = home.path().join("work");
    std::fs::create_dir(&dir).unwrap();
    let (fake, marker) = fake_ffmpeg(home.path());
    let mut mcp = Mcp::start_env(
        home.path(),
        &["--allow-previews"],
        &[
            ("EIDETIC_FFMPEG_PATH", fake.as_path()),
            ("EIDETIC_FFPROBE_PATH", fake.as_path()),
        ],
    );

    let clip = dir.join("clip.mp4");
    let still = dir.join("still.png");
    std::fs::write(&clip, b"not decoded").unwrap();
    std::fs::write(&still, b"not decoded").unwrap();
    // Slot 1 is a video, so a retime or an OTIO export would probe it.
    let valid = || {
        let path = write_project(
            &dir,
            (320, 180),
            "auto",
            &[
                SlotSpec {
                    source: clip.clone(),
                    kind: "video",
                    start: 0.5,
                    duration: 1.0,
                    transition_in: json!("cut"),
                },
                photo(&still, 1.0),
            ],
        );
        let project = saved(&path);
        (path, project)
    };
    let sentinels = write_sentinels(&dir);

    let many = vec![valid().1["slots"][1].clone(); 2001];
    type Breaker = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Breaker)> = vec![
        (
            "frame 0x180 is outside",
            Box::new(|p| p["width"] = json!(0)),
        ),
        (
            "frame 100000x180 is outside",
            Box::new(|p| p["width"] = json!(100000)),
        ),
        ("has no slots", Box::new(|p| p["slots"] = json!([]))),
        (
            "2001 slots; at most 2000",
            Box::new(move |p| p["slots"] = json!(many)),
        ),
        (
            "slot 1 has an unusable duration (-1)",
            Box::new(|p| p["slots"][0]["duration"] = json!(-1.0)),
        ),
        // Finite and positive, so only the total refuses it.
        (
            "the reel runs",
            Box::new(|p| p["slots"][0]["duration"] = json!(1e308)),
        ),
        (
            "slot 1 has an unusable start (-2)",
            Box::new(|p| p["slots"][0]["start"] = json!(-2.0)),
        ),
        (
            "slot 2's transition lasts 99s",
            Box::new(|p| p["slots"][1]["transition_in"] = json!({"crossfade": {"secs": 99.0}})),
        ),
    ];
    for (want, break_it) in &cases {
        let (path, mut project) = valid();
        break_it(&mut project);
        std::fs::write(&path, serde_json::to_vec_pretty(&project).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        for (tool, args) in project_tool_calls(&path) {
            let err = mcp.call(tool, args.clone()).unwrap_err();
            assert!(err.contains("is out of range"), "{tool}: {err}");
            assert!(err.contains(want), "{tool}: want {want:?}: {err}");
            assert!(!marker.exists(), "{tool} started a subprocess for {want:?}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{tool} wrote");
            for (out, bytes) in &sentinels {
                assert_eq!(&std::fs::read(out).unwrap(), bytes, "{tool} wrote {out:?}");
            }
        }
    }

    // An edit that would leave a valid project out of range saves nothing.
    for op in [
        json!({"op": "retime", "slot": 2, "secs": 100000.0}),
        json!({"op": "transition", "slot": 2, "style": "crossfade:nan"}),
    ] {
        let (path, _) = valid();
        let before = std::fs::read(&path).unwrap();
        let err = mcp
            .call(
                "edit_reel",
                json!({"project": path.to_str().unwrap(), "ops": [op]}),
            )
            .unwrap_err();
        assert!(
            err.contains("out of range; nothing was saved"),
            "{op}: {err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{op} wrote");
    }
    assert!(!marker.exists());

    // The marker is live: a valid project's export does probe its video.
    // The sentinel is no export of this project, so it would be refused.
    std::fs::remove_file(dir.join("reel.otio")).unwrap();
    let (path, _) = valid();
    mcp.call("export_otio", json!({"project": path.to_str().unwrap()}))
        .unwrap();
    assert!(marker.exists(), "the fake ffprobe never ran");
}

#[test]
fn legacy_projects_still_load_and_edit_through_mcp() {
    let home = scratch();
    let dir = home.path();
    let (p0, p1) = (dir.join("p0.png"), dir.join("p1.png"));
    let track = dir.join("track.wav");
    // The v0.8 shape: a grid without tracker, model or fallback; photo
    // slots without start, score, pinned, transition_in, motion, geometry
    // or near_duplicates.
    let legacy = json!({
        "version": 1,
        "created": "2026-08-01T10:00:00Z",
        "modified": "2026-08-01T10:00:00Z",
        "prompt": "rafting",
        "width": 320, "height": 180, "frame": "auto",
        "audio": track,
        "grid": {"bpm": 120.0, "beats": [0.0, 0.5, 1.0, 1.5, 2.0],
                 "downbeats": [0, 4], "high_energy": [[1.0, 2.0]]},
        "output": dir.join("reel.mp4"),
        "slots": [
            {"asset": uuid::Uuid::new_v4().to_string(), "source": p0,
             "kind": "photo", "duration": 1.0},
            {"asset": uuid::Uuid::new_v4().to_string(), "source": p1,
             "kind": "photo", "duration": 1.0},
        ],
    });
    let project = dir.join("reel.eidetic.json");
    std::fs::write(&project, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let arg = project.to_str().unwrap();
    let mut mcp = Mcp::start(dir);

    // No id yet: writing tools say how to adopt it, and write nothing.
    let before = listing(dir);
    for (tool, args) in [
        ("render", json!({"project": arg, "preview": true})),
        ("export_otio", json!({"project": arg})),
    ] {
        let err = mcp.call(tool, args).unwrap_err();
        assert!(err.contains("has no project id"), "{tool}: {err}");
        assert!(err.contains(r#""ops":[]"#), "{tool}: {err}");
        assert!(err.contains(&format!("call {tool} again")), "{tool}: {err}");
        assert_eq!(listing(dir), before, "{tool} wrote");
    }

    let ops = json!([
        {"op": "pin", "slot": 1},
        {"op": "transition", "slot": 2, "style": "crossfade:0.4"},
    ]);
    let out = mcp
        .call("edit_reel", json!({"project": arg, "ops": ops}))
        .unwrap();
    assert_eq!(out["slots"][0]["pinned"], json!(true));
    assert_eq!(out["adopted"], json!(true));
    let disk = saved(&project);
    assert!(disk["id"].is_string(), "edit_reel adopted it: {disk}");
    assert_eq!(disk["slots"][0]["pinned"], json!(true));
    assert_eq!(
        disk["slots"][1]["transition_in"]["crossfade"]["secs"],
        json!(0.4)
    );
    assert_eq!(disk["grid"]["bpm"], json!(120.0));

    let out = mcp.call("export_otio", json!({"project": arg})).unwrap();
    assert_eq!(out["otio"], json!(dir.join("reel.otio")));
    assert!(dir.join("reel.otio").is_file());

    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping the legacy render: ffmpeg not installed");
        return;
    }
    for (path, colour) in [(&p0, "red"), (&p1, "blue")] {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={colour}:s=320x180"),
            "-frames:v",
            "1",
            path.to_str().unwrap(),
        ]);
    }
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=3",
        track.to_str().unwrap(),
    ]);
    let out = mcp
        .call("render", json!({"project": arg, "preview": true}))
        .unwrap();
    assert_eq!(out["output"], json!(dir.join("reel.preview.mp4")));
    assert_eq!(size_of(&dir.join("reel.preview.mp4")), (160, 90));
}

// ---------------------------------------------------------------------------
// Output ownership: what MCP tools may replace
// ---------------------------------------------------------------------------

/// The three writes a loaded project asks for, by the path each targets.
fn write_calls(project: &Path) -> Vec<(&'static str, Value, &'static str)> {
    let p = project.to_str().unwrap();
    vec![
        (
            "render",
            json!({"project": p, "preview": false}),
            "reel.mp4",
        ),
        (
            "render",
            json!({"project": p, "preview": true}),
            "reel.preview.mp4",
        ),
        ("export_otio", json!({"project": p}), "reel.otio"),
    ]
}

/// Staging leftovers of a render or an export.
fn write_litter(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|n| n.starts_with(".eidetic-render-") || n.starts_with(".eidetic-otio-"))
        .collect()
}

/// The comment tag of a video, as ffprobe reads it.
fn comment_tag(video: &Path) -> String {
    let ffprobe = eidetic_ingest::video::ffprobe().expect("ffprobe ships with ffmpeg");
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-show_entries", "format_tags=comment"])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(video)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Two synthetic stills and a project cutting them, output `dir/reel.mp4`.
fn two_photo_project(dir: &Path) -> PathBuf {
    let (p0, p1) = (dir.join("p0.png"), dir.join("p1.png"));
    for (path, colour) in [(&p0, "red"), (&p1, "blue")] {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={colour}:s=160x90"),
            "-frames:v",
            "1",
            path.to_str().unwrap(),
        ]);
    }
    write_project(dir, (160, 90), "auto", &[photo(&p0, 0.6), photo(&p1, 0.6)])
}

/// A short real mp4 at `path`, with `comment` as its tag when given.
fn video_at(path: &Path, comment: Option<&str>) {
    let tag = comment.map(|c| format!("comment={c}"));
    let mut args = vec![
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x36:rate=30",
        "-t",
        "0.5",
        "-pix_fmt",
        "yuv420p",
    ];
    if let Some(tag) = &tag {
        args.extend(["-metadata", tag.as_str()]);
    }
    args.push(path.to_str().unwrap());
    ffmpeg(&args);
}

#[test]
fn foreign_files_at_every_destination_are_refused_byte_identical() {
    let home = scratch();
    let dir = home.path();
    let project = four_photos(dir);
    let sentinels = write_sentinels(dir);
    let mut mcp = Mcp::start(dir);

    for (tool, args, name) in write_calls(&project) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        let dest = dir.join(name);
        assert!(
            err.contains(&dest.display().to_string()),
            "{tool} {args}: {err}"
        );
        assert!(err.contains("nothing was written"), "{tool} {args}: {err}");
        if eidetic_ingest::video::ffprobe().is_some() {
            assert!(
                err.contains("was not written by Eidetic for this project"),
                "{tool} {args}: {err}"
            );
            assert!(err.contains("move or delete it"), "{tool} {args}: {err}");
        }
        for (path, bytes) in &sentinels {
            assert_eq!(
                &std::fs::read(path).unwrap(),
                bytes,
                "{tool} wrote {path:?}"
            );
        }
        assert_eq!(write_litter(dir), Vec::<String>::new(), "{tool}");
    }

    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping the real-media cases: ffmpeg not installed");
        return;
    }
    // Real media that is not this project's: an untagged video, a video
    // tagged for another project, an export of another project.
    let other = uuid::Uuid::new_v4();
    video_at(&dir.join("reel.mp4"), None);
    video_at(
        &dir.join("reel.preview.mp4"),
        Some(&format!("eidetic-project:{other}")),
    );
    let mut foreign_otio =
        json!({"OTIO_SCHEMA": "Timeline.1", "metadata": {"eidetic_project": other}});
    std::fs::write(dir.join("reel.otio"), foreign_otio.to_string()).unwrap();
    let before: Vec<Vec<u8>> = ["reel.mp4", "reel.preview.mp4", "reel.otio"]
        .iter()
        .map(|n| std::fs::read(dir.join(n)).unwrap())
        .collect();
    for (tool, args, _) in write_calls(&project) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        assert!(
            err.contains("was not written by Eidetic for this project"),
            "{tool} {args}: {err}"
        );
    }
    for (name, bytes) in ["reel.mp4", "reel.preview.mp4", "reel.otio"]
        .iter()
        .zip(&before)
    {
        assert_eq!(
            &std::fs::read(dir.join(name)).unwrap(),
            bytes,
            "{name} changed"
        );
    }
    assert_eq!(write_litter(dir), Vec::<String>::new());
    // The id alone, with no "metadata" around it, proves nothing either.
    foreign_otio = json!({"eidetic_project": saved(&project)["id"]});
    std::fs::write(dir.join("reel.otio"), foreign_otio.to_string()).unwrap();
    let err = mcp
        .call("export_otio", json!({"project": project.to_str().unwrap()}))
        .unwrap_err();
    assert!(err.contains("was not written by Eidetic"), "{err}");
}

/// A stand-in ffprobe that replaces `victim` with `replacement` when run,
/// as if someone else rewrote the file mid-call, then fails.
#[cfg(unix)]
fn replacing_ffprobe(home: &Path, victim: &Path, replacement: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let fake = home.join("replacing-ffprobe.sh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncp '{}' '{}'\nexit 1\n",
            replacement.display(),
            victim.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

#[cfg(unix)]
#[test]
fn edit_reel_never_saves_over_a_file_that_replaced_its_project() {
    let home = scratch();
    let dir = home.path();
    let project = write_project(
        dir,
        (320, 180),
        "auto",
        &[SlotSpec {
            source: dir.join("clip.mp4"),
            kind: "video",
            start: 0.0,
            duration: 1.0,
            transition_in: json!("cut"),
        }],
    );
    // Loaded without an id, so this edit also adopts it. Its replacement is
    // either another id-less project or not a project at all.
    let mut legacy = saved(&project);
    legacy.as_object_mut().unwrap().remove("id");
    let loaded = serde_json::to_vec_pretty(&legacy).unwrap();
    let mut other = legacy.clone();
    other["prompt"] = json!("someone else's reel");
    let replacements = [
        (
            "other-project.json",
            serde_json::to_vec_pretty(&other).unwrap(),
        ),
        ("not-a-project.json", br#"{"not": "this project"}"#.to_vec()),
    ];
    for (name, bytes) in replacements {
        std::fs::write(&project, &loaded).unwrap();
        let replacement = dir.join(name);
        std::fs::write(&replacement, &bytes).unwrap();
        // Retiming a video probes it, which is when the file gets replaced.
        let fake = replacing_ffprobe(dir, &project, &replacement);
        let mut mcp = Mcp::start_env(dir, &[], &[("EIDETIC_FFPROBE_PATH", fake.as_path())]);
        let err = mcp
            .call(
                "edit_reel",
                json!({"project": project.to_str().unwrap(),
                       "ops": [{"op": "retime", "slot": 1, "secs": 0.8}]}),
            )
            .unwrap_err();
        assert!(
            err.contains("no longer the project this call loaded"),
            "{name}: {err}"
        );
        assert!(err.contains("nothing was saved"), "{name}: {err}");
        assert!(
            err.contains(&project.display().to_string()),
            "{name}: {err}"
        );
        assert_eq!(std::fs::read(&project).unwrap(), bytes, "{name}");
        assert!(
            !listing(dir)
                .iter()
                .any(|n| n.starts_with(".eidetic-project-")),
            "{name}: {:?}",
            listing(dir)
        );
    }
}

/// A stand-in ffmpeg that puts a foreign file at `victim` when it is not
/// there yet, as if someone else created it mid-render, then runs the real
/// ffmpeg.
#[cfg(unix)]
fn intruding_ffmpeg(home: &Path, victim: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = eidetic_ingest::video::ffmpeg().expect("the caller checked for ffmpeg");
    let fake = home.join("intruding-ffmpeg.sh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ -e '{v}' ] || printf 'someone else' > '{v}'\nexec '{}' \"$@\"\n",
            real.display(),
            v = victim.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

#[cfg(unix)]
#[test]
fn render_never_replaces_a_file_that_appeared_while_it_ran() {
    if eidetic_ingest::video::ffmpeg().is_none() || eidetic_ingest::video::ffprobe().is_none() {
        eprintln!("skipping: ffmpeg or ffprobe not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let project = two_photo_project(dir);
    for (preview, name) in [(false, "reel.mp4"), (true, "reel.preview.mp4")] {
        let victim = dir.join(name);
        let fake = intruding_ffmpeg(dir, &victim);
        let mut mcp = Mcp::start_env(dir, &[], &[("EIDETIC_FFMPEG_PATH", fake.as_path())]);
        let err = mcp
            .call(
                "render",
                json!({"project": project.to_str().unwrap(), "preview": preview}),
            )
            .unwrap_err();
        assert!(
            err.contains("was not written by Eidetic for this project"),
            "{name}: {err}"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"someone else", "{name}");
        assert_eq!(write_litter(dir), Vec::<String>::new(), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn export_otio_never_replaces_a_file_that_appeared_while_it_probed() {
    let home = scratch();
    let dir = home.path();
    let project = write_project(
        dir,
        (320, 180),
        "auto",
        &[SlotSpec {
            source: dir.join("clip.mp4"),
            kind: "video",
            start: 0.0,
            duration: 1.0,
            transition_in: json!("cut"),
        }],
    );
    let victim = dir.join("reel.otio");
    let foreign = dir.join("foreign.otio");
    std::fs::write(&foreign, b"someone else's timeline").unwrap();
    // Building the timeline probes the video slot, which is when the file
    // appears.
    let fake = replacing_ffprobe(dir, &victim, &foreign);
    let mut mcp = Mcp::start_env(dir, &[], &[("EIDETIC_FFPROBE_PATH", fake.as_path())]);
    let err = mcp
        .call("export_otio", json!({"project": project.to_str().unwrap()}))
        .unwrap_err();
    assert!(
        err.contains("was not written by Eidetic for this project"),
        "{err}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"someone else's timeline");
    assert_eq!(write_litter(dir), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn symlinked_destinations_are_refused_without_touching_targets() {
    use std::os::unix::fs::symlink;
    let home = scratch();
    let work = home.path().join("work");
    let victims = home.path().join("victims");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&victims).unwrap();
    let project = four_photos(&work);
    let mut mcp = Mcp::start(home.path());

    // Each destination is a link: two to files that must not change, one
    // dangling into a directory where nothing may appear.
    for name in ["reel.mp4", "reel.preview.mp4"] {
        let victim = victims.join(name);
        std::fs::write(&victim, format!("victim {name}")).unwrap();
        symlink(&victim, work.join(name)).unwrap();
    }
    symlink(victims.join("new.otio"), work.join("reel.otio")).unwrap();
    let victims_before = listing(&victims);
    let check_untouched = |what: &str| {
        assert_eq!(listing(&victims), victims_before, "{what}");
        for name in ["reel.mp4", "reel.preview.mp4"] {
            assert_eq!(
                std::fs::read(victims.join(name)).unwrap(),
                format!("victim {name}").into_bytes(),
                "{what}"
            );
        }
    };
    for (tool, args, name) in write_calls(&project) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        let link = work.join(name);
        assert!(
            err.contains(&format!("{} is a symbolic link", link.display())),
            "{tool} {args}: {err}"
        );
        assert!(err.contains("nothing was written"), "{tool} {args}: {err}");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        check_untouched(tool);
    }
    assert_eq!(write_litter(&work), Vec::<String>::new());

    // The output's directory is reached through a link.
    let linked_dir = home.path().join("linked");
    symlink(&victims, &linked_dir).unwrap();
    let mut through = saved(&project);
    through["output"] = json!(linked_dir.join("other.mp4"));
    let through_path = work.join("through.eidetic.json");
    std::fs::write(&through_path, through.to_string()).unwrap();
    for (tool, args, _) in write_calls(&through_path) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        assert!(
            err.contains(&format!("{} is a symbolic link", linked_dir.display())),
            "{tool} {args}: {err}"
        );
        assert!(
            err.contains(&format!("resolves to {}", victims.display())),
            "{err}"
        );
        check_untouched(tool);
    }

    // The project file itself is a link: edit_reel will not save through it.
    let real_project = victims.join("real.eidetic.json");
    std::fs::copy(&project, &real_project).unwrap();
    let link = work.join("link.eidetic.json");
    symlink(&real_project, &link).unwrap();
    let before = std::fs::read(&real_project).unwrap();
    let err = mcp
        .call(
            "edit_reel",
            json!({"project": link.to_str().unwrap(), "ops": [{"op": "pin", "slot": 1}]}),
        )
        .unwrap_err();
    assert!(err.contains("is a symbolic link"), "{err}");
    assert_eq!(std::fs::read(&real_project).unwrap(), before);
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
}

#[test]
fn nothing_is_written_into_the_library_directory() {
    let home = scratch();
    let dir = home.path();
    let lib = library(dir);
    let project = four_photos(dir);
    let mut inside = saved(&project);
    inside["output"] = json!(lib.join("reel.mp4"));
    let inside_path = dir.join("inside.eidetic.json");
    std::fs::write(&inside_path, inside.to_string()).unwrap();
    // An export carrying this very project's id is still not replaced
    // there: an imported file keeps its tag, so a tag proves nothing about
    // a library file.
    let owned_otio =
        json!({"OTIO_SCHEMA": "Timeline.1", "metadata": {"eidetic_project": inside["id"]}});
    std::fs::write(lib.join("reel.otio"), owned_otio.to_string()).unwrap();
    let lib_before = listing(&lib);
    let mut mcp = Mcp::start(dir);
    for (tool, args, _) in write_calls(&inside_path) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        assert!(
            err.contains("inside the library directory"),
            "{tool} {args}: {err}"
        );
        assert!(err.contains("nothing was written"), "{tool} {args}: {err}");
        assert_eq!(listing(&lib), lib_before, "{tool}");
    }
    assert_eq!(
        std::fs::read(lib.join("reel.otio")).unwrap(),
        owned_otio.to_string().into_bytes()
    );

    // A project file inside the library is not saved back either.
    let lib_project = lib.join("reel.eidetic.json");
    std::fs::copy(&project, &lib_project).unwrap();
    let before = std::fs::read(&lib_project).unwrap();
    let err = mcp
        .call(
            "edit_reel",
            json!({"project": lib_project.to_str().unwrap(), "ops": [{"op": "pin", "slot": 1}]}),
        )
        .unwrap_err();
    assert!(err.contains("inside the library directory"), "{err}");
    assert_eq!(std::fs::read(&lib_project).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn the_library_is_recognised_through_a_symlinked_library_setting() {
    let home = scratch();
    let dir = home.path();
    let real = dir.join("real-library");
    std::fs::create_dir(&real).unwrap();
    let link = dir.join("library-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let project = four_photos(dir);
    let mut inside = saved(&project);
    inside["output"] = json!(real.join("reel.mp4"));
    std::fs::write(&project, inside.to_string()).unwrap();
    let mut mcp = Mcp::start_env(dir, &[], &[("EIDETIC_LIBRARY_DIR", link.as_path())]);
    for (tool, args, _) in write_calls(&project) {
        let err = mcp.call(tool, args.clone()).unwrap_err();
        assert!(
            err.contains("inside the library directory"),
            "{tool} {args}: {err}"
        );
        assert!(listing(&real).is_empty(), "{tool}");
    }
}

#[test]
fn create_reel_refuses_unwritable_outputs_before_planning() {
    let home = scratch();
    let dir = home.path();
    let lib = library(dir);
    std::fs::write(dir.join("taken.mp4"), b"someone's video").unwrap();
    std::fs::write(dir.join("kept.eidetic.json"), b"{}").unwrap();
    std::fs::write(dir.join("many.v2.mp4"), b"someone's video").unwrap();
    let mut cases = vec![
        (
            lib.join("reel.mp4"),
            None,
            "inside the library directory".to_string(),
        ),
        (
            dir.join("taken.mp4"),
            None,
            format!(
                "{} already exists; pick a different output",
                dir.join("taken.mp4").display()
            ),
        ),
        (
            dir.join("kept.mp4"),
            None,
            "kept.eidetic.json already exists; edit it with edit_reel".to_string(),
        ),
        (
            dir.join("many.mp4"),
            Some(2),
            "many.v2.mp4 already exists".to_string(),
        ),
        (
            dir.join("sub").join("..").join("up.mp4"),
            None,
            "contains `..`".to_string(),
        ),
    ];
    #[cfg(unix)]
    {
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(dir.join("tmp"), &linked).unwrap();
        cases.push((
            linked.join("reel.mp4"),
            None,
            format!("{} is a symbolic link", linked.display()),
        ));
    }
    let mut mcp = Mcp::start(dir);
    let files_before = listing(dir);
    for (output, variations, want) in cases {
        let mut args = json!({"prompt": "anything", "output": output});
        if let Some(n) = variations {
            args["variations"] = json!(n);
        }
        let err = mcp.call("create_reel", args).unwrap_err();
        assert!(err.contains(&want), "{output:?}: want {want:?}: {err}");
        assert_eq!(listing(dir), files_before, "{output:?}");
    }
    assert!(listing(&lib).is_empty());
    drop(mcp);
    assert_no_model_touched(dir);
}

#[test]
fn owned_outputs_rerender_and_reexport_repeatedly() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let project = two_photo_project(dir);
    let id = saved(&project)["id"].as_str().unwrap().to_string();
    let tag = format!("eidetic-project:{id}");
    let mut mcp = Mcp::start(dir);
    let round = |mcp: &mut Mcp| {
        for (tool, args, name) in write_calls(&project) {
            mcp.call(tool, args.clone())
                .unwrap_or_else(|e| panic!("{tool} {args}: {e}"));
            assert!(dir.join(name).is_file(), "{name}");
        }
        assert_eq!(comment_tag(&dir.join("reel.mp4")), tag);
        assert_eq!(comment_tag(&dir.join("reel.preview.mp4")), tag);
        let otio: Value =
            serde_json::from_slice(&std::fs::read(dir.join("reel.otio")).unwrap()).unwrap();
        assert_eq!(otio["metadata"]["eidetic_project"], json!(id));
        assert_eq!(write_litter(dir), Vec::<String>::new());
    };
    round(&mut mcp);
    round(&mut mcp);
    let out = mcp
        .call(
            "edit_reel",
            json!({"project": project.to_str().unwrap(),
                   "ops": [{"op": "retime", "slot": 1, "secs": 0.9}]}),
        )
        .unwrap();
    assert!(out.get("adopted").is_none(), "{out}");
    assert_eq!(saved(&project)["id"], json!(id), "an edit keeps the id");
    round(&mut mcp);
}

#[test]
fn legacy_outputs_are_reclaimed_by_adoption_and_a_cli_re_render() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let project = two_photo_project(dir);
    let arg = project.to_str().unwrap();
    // As an earlier version left it: no id in the project, and an untagged
    // render beside it.
    let mut legacy = saved(&project);
    legacy.as_object_mut().unwrap().remove("id");
    std::fs::write(&project, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let output = dir.join("reel.mp4");
    video_at(&output, None);
    let old_render = std::fs::read(&output).unwrap();
    let mut mcp = Mcp::start(dir);
    let render = |mcp: &mut Mcp| mcp.call("render", json!({"project": arg, "preview": false}));

    let err = render(&mut mcp).unwrap_err();
    assert!(err.contains("has no project id"), "{err}");
    assert!(err.contains("edit_reel"), "{err}");

    // Adoption: an empty batch records an id and changes nothing else.
    let out = mcp
        .call("edit_reel", json!({"project": arg, "ops": []}))
        .unwrap();
    assert_eq!(out["adopted"], json!(true));
    let id = saved(&project)["id"].as_str().unwrap().to_string();
    assert_eq!(assets(&saved(&project)["slots"]), assets(&legacy["slots"]));

    // The old render still cannot be proved to be this project's.
    let err = render(&mut mcp).unwrap_err();
    assert!(
        err.contains("was not written by Eidetic for this project"),
        "{err}"
    );
    assert!(err.contains(&format!("eidetic reel edit {arg}")), "{err}");
    assert_eq!(std::fs::read(&output).unwrap(), old_render);

    // The operator's CLI re-renders it as before, and marks it.
    let reel_edit = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
        cmd.args(["reel", "edit"]).arg(&project).args(extra);
        hermetic(&mut cmd, dir);
        let cli = cmd.output().expect("run eidetic reel edit");
        assert!(
            cli.status.success(),
            "{}",
            String::from_utf8_lossy(&cli.stderr)
        );
    };
    reel_edit(&[]);
    assert_eq!(comment_tag(&output), format!("eidetic-project:{id}"));

    // From here on the agent re-renders it like any planned project.
    render(&mut mcp).unwrap();
    assert_eq!(comment_tag(&output), format!("eidetic-project:{id}"));

    // An export from before exports were marked: refused, until the
    // re-export the refusal names marks it as this project's.
    let otio = dir.join("reel.otio");
    let old_export = json!({"OTIO_SCHEMA": "Timeline.1", "metadata": {}}).to_string();
    std::fs::write(&otio, &old_export).unwrap();
    let export = |mcp: &mut Mcp| mcp.call("export_otio", json!({"project": arg}));
    let err = export(&mut mcp).unwrap_err();
    let remedy = format!("eidetic reel edit {arg} --export otio --dry-run");
    assert!(err.contains(&remedy), "{err}");
    assert_eq!(std::fs::read_to_string(&otio).unwrap(), old_export);
    reel_edit(&["--export", "otio", "--dry-run"]);
    let exported: Value = serde_json::from_slice(&std::fs::read(&otio).unwrap()).unwrap();
    assert_eq!(exported["metadata"]["eidetic_project"], json!(id));
    assert_eq!(saved(&project)["id"], json!(id));
    export(&mut mcp).unwrap();
}

#[test]
fn reel_edit_cli_still_overwrites_its_own_untagged_output() {
    if eidetic_ingest::video::ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let home = scratch();
    let dir = home.path();
    let project = two_photo_project(dir);
    let output = dir.join("reel.mp4");
    std::fs::write(&output, b"an earlier render").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
    cmd.args(["reel", "edit"])
        .arg(&project)
        .args(["--export", "otio"]);
    hermetic(&mut cmd, dir);
    let out = cmd.output().expect("run eidetic reel edit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(size_of(&output), (160, 90));
    assert!(dir.join("reel.otio").is_file());
}
