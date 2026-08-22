# Goals: v0.7 — the reel studio

Status: planned (2026-08-22)

v0.6 closed both earlier goal documents. This one plans the vision sketched for Instagram-ready reels: *give eidetic a music track and a prompt; it reads the beats, picks the best shots, times cuts to the music, renders — then the user fine-tunes in a real editor or tells eidetic what to change.* Validated by two research passes (beat detection, editor hand-off) on 2026-08-22.

The one-line research summary: **hand-roll the beat tracker (the algorithm is small and well-documented), export OpenTimelineIO for hand tune-up (the auto-editor pattern), and don't touch web-based editors yet (the promising ones can't even import a timeline).**

## Phase 1 — beat-synced portrait reels (the "post right away" core)

`eidetic reel "goa trip highlights" --audio track.mp3 --portrait`

- **Beat grid, pure Rust**: the Ellis dynamic-programming beat tracker (spectral-flux onset envelope → autocorrelation tempo → DP phase), ~300 lines over `rustfft`, running on the 16 kHz PCM ffmpeg already decodes for Whisper. This is exactly `librosa.beat.beat_track`'s algorithm, with a readable paper and reference notebooks to port from. No new native deps — `rustfft` is pure Rust, which keeps the no-native-deps guarantee that ruled out the alternatives: aubio's Rust bindings are dead since 2021 and its CLI isn't one-command on Windows; the two young pure-Rust beat crates (timestretch, stratum-dsp) are single-maintainer and unproven, though timestretch is worth cribbing from.
  - Fallback: if the grid looks degenerate (no stable tempo found), fall back to fixed-interval cuts — never fail the render over a weird track.
- **Cut policy** (the convention every auto-editor converges on): cut on downbeats (every 4th beat, anchored at the strongest onset) in normal sections, every 1–2 beats in high-energy sections, minimum clip ~0.6 s. Energy sections come from ffmpeg's `ebur128` momentary loudness — a filter we already shell out to ffmpeg for everything else.
- **Shots**: existing search picks candidates (semantic + spoken + moments); scene boundaries bound each clip as today; the dupes similarity machinery enforces shot *diversity* (no two near-identical clips); a cheap sharpness gate (Laplacian variance on the candidate frame) drops blurry shots.
- **Portrait framing**: `--portrait` = 1080×1920 + **cover-crop instead of pad** (black side bars are the loudest auto-generated tell). The crop window is face-aware: stored face bboxes pull the window toward people; otherwise center. Photos already cover-crop for Ken Burns.
- **Audio**: the track is laid under the cut, trimmed to the reel length with a fade-out. Reel length defaults to `min(--duration, audio length)`. No bundled/generated music ever — the user supplies the file (and adding trending audio inside Instagram is better for reach anyway, so silent remains the default without `--audio`).
- **Transitions**: hard cuts on beats (the native grammar of reels); a short crossfade only across energy-section boundaries. Nothing fancier — transition zoos read as template spam.

## Phase 1b — framing styles and voiceover mode (user feedback, 2026-08-22)

Field feedback after phase 1: face-centred cover-crop is wrong for scenic footage — a Ladakh panorama must stay cinematic, not zoom into a face — and the driving audio isn't always music: it can be a vlog narration or a voiceover, in which case *what is said* should pick the clips.

- **Per-clip auto framing** (`--frame auto`, the default): clips with detected faces get the face-following cover-crop; clips without faces render **fit-inside over a blurred, darkened cover background** — the standard cinematic treatment for landscape footage in a vertical frame, no black bars, no butchered panoramas. `--frame cover|blur|pad` forces one style.
- **Voiceover mode** (needs a `speech` build): when `--audio` has no stable tempo but Whisper finds speech, the reel becomes narration-driven — each spoken span (merged to ≥2.5 s, split at ~8 s) is embedded as a search query, the best-matching un-recently-used shot fills exactly that span, and the narration itself is the soundtrack. "Here we reached Pangong" puts the Pangong clip on screen while you say it.
- Mode selection is automatic from the audio: beats → music-synced; speech → voiceover; neither → fixed windows.

## Phase 2 — reel projects and AI follow-ups

- Every render persists a **project file** (JSON: ordered clips with asset ids + in/out, audio, beat grid, prompt) instead of being one-shot.
- `eidetic reel edit project.json` applies changes and re-renders fast:
  - `--swap 3 "the beach clip"` — slot 3 replaced by the best search hit for that phrase
  - `--drop 2`, `--pin 1`, `--more "cake cutting"`, `--shuffle`
- Honesty note: follow-ups are **structured commands resolved through SigLIP search**, not freeform chat — eidetic has no local LLM, and bolting one on is a separate decision this plan does not make. The command set above covers the actual editing verbs.

## Phase 3 — hand tune-up: OpenTimelineIO export

- `eidetic reel … --export otio` writes the timeline as a `.otio` file — plain JSON, emitted with serde (the crates.io `opentimelineio` crate is an empty placeholder; we don't need it). One video track (clips as ExternalReference + source_range at each file's real frame rate), one audio track with the music.
- Opens natively in **Kdenlive 25.04+** (new C++ OTIO support) and **DaVinci Resolve free 18.5+** — the two serious Linux editors. Cuts only, no transitions in the export: OTIO importers drop effects anyway, and dissolves are precisely the tune-up the user will do in the editor. This is the proven auto-editor architecture (5k stars: automated cutter → project file → finish in your editor).
- Shotcut (no native OTIO) gets a small MLT exporter later only if it's ever missed; CMX3600 EDL (~50 lines) is the lowest-common-denominator escape hatch.

## Rejected / deferred

- **Embedded or paired web editor** (OpenCut, omniclip, FableCut, OpenChatCut): rejected for now. OpenCut (85k stars) cannot import an external timeline — a portable project format is an open feature request (#719) and the app is mid-rewrite; the projects that *can* be driven by JSON timelines are sub-1.5k-star, months-old Node stacks we'd have to vendor and chase. Revisit when OpenCut #719 lands. FableCut's JSON-timeline/REST design is the one to study if we ever want in-browser nudging.
- **HyperFrames / hosted AI video tools**: cloud — anti-goal, permanently.
- **ML downbeat detection** (madmom/BeatNet class): Python-only, and "every 4th beat anchored at the strongest onset" is the accepted stand-in.
- **Bundled music, auto-captions, template transitions**: not this tool's aesthetic. The reel is the user's media cut to the user's music.

## Build order

1. Phase 1 without faces-aware crop (beat grid + policy + portrait cover-crop + audio bed) — the core payoff.
2. Face-aware crop + sharpness gate — the polish that makes portrait framing trustworthy.
3. Phase 3 (OTIO export) — small and independent; can land any time after the project file exists.
4. Phase 2 (edit commands) — last, once real usage shows which verbs matter.
