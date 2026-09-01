//! OpenTimelineIO export (goals-v0.7.md phase 3; the v0.8 "rough cut +
//! human polish" handoff).
//!
//! Plain JSON emitted with serde_json — the crates.io `opentimelineio`
//! crate is an empty placeholder and the OTIO core schema is small and
//! stable, so we write it directly. The shape matches what the reference
//! Python library (0.17) serializes: `Clip.2` with a `DEFAULT_MEDIA`
//! external reference, ranges as `RationalTime` at each source's real
//! frame rate. One video track of cuts, one audio track with the music.
//! Cuts only, by design: OTIO importers drop effects anyway, and dissolves
//! are precisely the polish the user will do in the editor (Kdenlive
//! 25.04+, Resolve 18.5+ both open this natively).

use crate::project::ReelProject;
use crate::reel::{Slot, SlotKind};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

/// Photos and audio have no intrinsic rate; the reel's own 30fps applies.
const TIMELINE_FPS: f64 = 30.0;

/// Write `project` as a `.otio` file at `path`.
pub fn export(project: &ReelProject, path: &Path) -> Result<()> {
    let clips: Vec<Value> = project.slots.iter().map(clip).collect();
    let mut tracks = vec![track("Video", "V1", clips)];
    if let Some(audio) = &project.audio {
        let music = json!({
            "OTIO_SCHEMA": "Clip.2",
            "metadata": {},
            "name": stem(audio),
            "source_range": time_range(0.0, project.total_secs(), TIMELINE_FPS),
            "effects": [],
            "markers": [],
            "enabled": true,
            "media_references": {
                "DEFAULT_MEDIA": external_reference(audio, None, TIMELINE_FPS),
            },
            "active_media_reference_key": "DEFAULT_MEDIA",
        });
        tracks.push(track("Audio", "A1", vec![music]));
    }

    let timeline = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "metadata": { "generator": concat!("eidetic ", env!("CARGO_PKG_VERSION")) },
        "name": project.prompt,
        "global_start_time": null,
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "metadata": {},
            "name": "tracks",
            "source_range": null,
            "effects": [],
            "markers": [],
            "enabled": true,
            "children": tracks,
        },
    });

    let bytes = serde_json::to_vec_pretty(&timeline).context("serialize OTIO timeline")?;
    std::fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn clip(slot: &Slot) -> Value {
    // A video clip's ranges are expressed at the source's real frame rate
    // so the editor lands on exact frames; photos have no rate of their
    // own and use the timeline's.
    let (rate, available) = match slot.kind {
        SlotKind::Video => match eidetic_ingest::video::probe(&slot.source) {
            Ok(Some(p)) => (p.frame_rate.unwrap_or(TIMELINE_FPS), p.duration_secs),
            _ => (TIMELINE_FPS, None),
        },
        SlotKind::Photo | SlotKind::PhotoScenic => (TIMELINE_FPS, None),
    };
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "metadata": {},
        "name": stem(&slot.source),
        "source_range": time_range(slot.start, slot.duration, rate),
        "effects": [],
        "markers": [],
        "enabled": true,
        "media_references": {
            "DEFAULT_MEDIA": external_reference(&slot.source, available, rate),
        },
        "active_media_reference_key": "DEFAULT_MEDIA",
    })
}

fn track(kind: &str, name: &str, children: Vec<Value>) -> Value {
    json!({
        "OTIO_SCHEMA": "Track.1",
        "metadata": {},
        "name": name,
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": true,
        "children": children,
        "kind": kind,
    })
}

fn external_reference(path: &Path, duration_secs: Option<f64>, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "ExternalReference.1",
        "metadata": {},
        "name": "",
        "available_range": duration_secs.map(|d| time_range(0.0, d, rate)),
        "available_image_bounds": null,
        "target_url": file_url(path),
    })
}

fn time_range(start_secs: f64, duration_secs: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational_time(start_secs, rate),
        "duration": rational_time(duration_secs, rate),
    })
}

fn rational_time(secs: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "rate": rate,
        "value": (secs * rate).round(),
    })
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A file:// URL with the handful of characters that break URL parsing
/// percent-encoded. Library paths are content-hashed hex so this is
/// belt-and-braces for user-supplied audio paths.
fn file_url(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut url = String::from("file://");
    for c in abs.display().to_string().chars() {
        match c {
            '%' => url.push_str("%25"),
            ' ' => url.push_str("%20"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            _ => url.push(c),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reel::Transition;
    use eidetic_core::AssetId;
    use std::path::PathBuf;

    fn project() -> ReelProject {
        ReelProject::new(
            "sunset at the beach".into(),
            (1080, 1920),
            "auto".into(),
            Some(PathBuf::from("/music/my track.m4a")),
            None,
            Path::new("/out/reel.mp4"),
            vec![
                Slot {
                    asset: AssetId::new(),
                    source: PathBuf::from("/lib/ab/cd/abcd.mp4"),
                    kind: SlotKind::Video,
                    start: 3.0,
                    duration: 2.0,
                    focus_x: None,
                    score: 0.4,
                    pinned: false,
                    transition_in: Transition::Cut,
                },
                Slot {
                    asset: AssetId::new(),
                    source: PathBuf::from("/lib/.thumbs/m/ee/ff/eeff.jpg"),
                    kind: SlotKind::Photo,
                    start: 0.0,
                    duration: 1.5,
                    focus_x: None,
                    score: 0.3,
                    pinned: false,
                    transition_in: Transition::Cut,
                },
            ],
        )
    }

    #[test]
    fn writes_a_two_track_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reel.otio");
        export(&project(), &path).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        assert_eq!(v["OTIO_SCHEMA"], "Timeline.1");
        assert_eq!(v["name"], "sunset at the beach");
        let tracks = v["tracks"]["children"].as_array().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0]["kind"], "Video");
        assert_eq!(tracks[1]["kind"], "Audio");

        let clips = tracks[0]["children"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        // Probe fails on the fake path, so the video falls back to 30fps:
        // 3s in = frame 90, 2s = 60 frames.
        assert_eq!(clips[0]["source_range"]["start_time"]["value"], 90.0);
        assert_eq!(clips[0]["source_range"]["duration"]["value"], 60.0);
        assert_eq!(
            clips[0]["media_references"]["DEFAULT_MEDIA"]["target_url"],
            "file:///lib/ab/cd/abcd.mp4"
        );

        // The photo holds 1.5s = 45 frames from frame 0.
        assert_eq!(clips[1]["source_range"]["start_time"]["value"], 0.0);
        assert_eq!(clips[1]["source_range"]["duration"]["value"], 45.0);

        // Audio: one clip covering the 3.5s cut, spaces percent-encoded.
        let music = &tracks[1]["children"][0];
        assert_eq!(music["source_range"]["duration"]["value"], 105.0);
        assert_eq!(
            music["media_references"]["DEFAULT_MEDIA"]["target_url"],
            "file:///music/my%20track.m4a"
        );
    }
}
