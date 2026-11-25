//! The edit loop through its real entry points: the `eidetic mcp` stdio
//! server (edit_reel, render) and `eidetic reel edit`.
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

/// Point every path the binary could touch into `home`.
fn hermetic(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("EIDETIC_DATABASE_PATH", home.join("eidetic.db"))
        .env("EIDETIC_LIBRARY_DIR", home.join("library"))
        .env("EIDETIC_MODELS_CACHE", home.join("models"))
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_eidetic"));
        cmd.arg("mcp");
        hermetic(&mut cmd, home);
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
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}));
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
/// planned — and return its path.
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
    let home = tempfile::tempdir().unwrap();
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
    assert!(describe("render").contains("<output>.preview.mp4"));
}

#[test]
fn edit_reel_applies_batches_against_the_original_numbering_and_persists() {
    let home = tempfile::tempdir().unwrap();
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
    let home = tempfile::tempdir().unwrap();
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
    let home = tempfile::tempdir().unwrap();
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
