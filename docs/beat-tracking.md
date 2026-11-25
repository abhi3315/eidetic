# Beat tracking and downbeats

Reels cut to music need two things from the track: where the beats are, and
where the bars start. Eidetic has two trackers for that. This page covers
which one runs, where the model comes from, and what the planner does with
the result.

## The two trackers

**Ellis** (`eidetic_ingest::beats::track_beats`, always built). Spectral-flux
onsets, an autocorrelation tempo estimate, then Ellis 2007 dynamic-programming
beat placement. It needs no model and no network. It has no idea where bars
start, though. Its `downbeats` are every 4th beat, anchored on the loudest
onset. That's a guess, and it's labelled as one everywhere.

**Beat This!** (`eidetic_ml::downbeats`, behind the `downbeats` Cargo
feature). The ISMIR 2024 model from Foscarin, Schlüter and Widmer (CP-JKU),
run through the [`beat-this`](https://crates.io/crates/beat-this) 1.0.0 crate
by danigb ([beat-this-rs](https://github.com/danigb/beat-this-rs), MIT). It
infers beats *and* downbeats, so 3/4, mixed meters and pickup bars come out
right instead of being forced into fours.

## Choosing one: `EIDETIC_BEAT_TRACKER`

| Value | Behaviour |
|---|---|
| unset / `auto` (default) | Uses Beat This only if this build has the `downbeats` feature **and** both model files are already cached and pass their SHA-256 check. Anything else means Ellis. **Never downloads.** In a feature build the grid records why it fell back (`fallback`). In a default build there is nothing to fall back from, so it's plain Ellis with no note. |
| `ellis` | Ellis only. The model cache is never touched. |
| `beat-this` | The deliberate opt-in. Downloads the two pinned files on first use (about 11 MB total, into `$EIDETIC_MODELS_CACHE/beat-this/<revision>/`), verifies them, and runs inference. **Any failure is an error**, not a silent Ellis substitute: you asked for inferred bars. That covers a build without the feature, a failed download, a digest mismatch, a model that won't load, or an inference error. Set `auto` or `ellis` to get a reel anyway. |

Unknown values are rejected, the same rule `EIDETIC_ACCELERATOR` follows.
The variable applies to `eidetic reel --audio` and to the MCP `beats` and
`create_reel` tools. MCP clients can't choose the tracker, so an agent can
never trigger a download. Only the operator's environment can.

The usual flow is to run one reel with `EIDETIC_BEAT_TRACKER=beat-this` to
fetch the model, then leave the variable unset. `auto` picks up the cached
files from then on.

Sessions come from the same ort builder as SigLIP and the face models, so
`EIDETIC_ACCELERATOR` applies to them too.

## Model provenance and caching

Both artifacts are pinned to one immutable upstream commit,
`089b509247e6fdcec666511c0dcf0d5f39c21e73` of danigb/beat-this-rs:

| File | Bytes | SHA-256 |
|---|---|---|
| `mel_spectrogram.onnx` | 270,742 | `fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9` |
| `beat_this_small.onnx` | 10,555,592 | `a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f` |

URLs have the form
`https://raw.githubusercontent.com/danigb/beat-this-rs/<revision>/models/<file>`.

- Downloads stream into a temp file beside the destination. The file must be
  exactly the pinned length and digest, then it's fsynced and renamed into
  place. An interrupted or tampered fetch leaves nothing a later load would
  accept.
- Cached files are re-hashed on every load. A corrupt file is refused under
  `auto` (with a fallback reason) and re-fetched under `beat-this`.
- The cache directory is keyed by revision, so a future pin never trusts
  files that were verified against an older one.
- No model file is committed to this repository (`*.onnx` is gitignored).

The weights are the "small" Beat This! checkpoint, exported to ONNX by
beat-this-rs. The upstream Beat This! project publishes its code and
checkpoints under the MIT license. Check the upstream repositories for the
authoritative terms before redistributing the files yourself.

## What the grid records

`BeatGrid` (persisted in `reel.eidetic.json` and returned over MCP) carries:

- `tracker`: `ellis` or `beat-this`
- `model`: `{name, revision, sha256}` for inferred grids
- `fallback`: the reason a Beat This attempt ended in an Ellis grid

`BeatGrid::downbeats_inferred()` is true only for a Beat This grid that has
a model record. MCP spells the answer out as
`provenance.downbeats: "inferred" | "heuristic-every-4th-beat"`, in both
the `beats` result and every cut list's `beat_grid`.

Project files from before v0.9 have none of these fields. They load as
Ellis with heuristic bars, which is what actually made them. The project
`version` stays 1 because the change only adds fields.

Every grid goes through `BeatGrid::sanitize` on construction and on
deserialization. Non-finite or negative beats are dropped, beats are sorted
and deduplicated, downbeat indices are remapped to the same beat *times*
(indices that point nowhere are dropped), empty or non-finite energy spans
are removed, and a bogus BPM is re-derived from the median beat interval.
Beat This downbeats that don't sit within 70 ms of a beat are dropped, not
invented.

## What the planner does with it

With inferred downbeats (`reel::beat_spans`):

- one span per bar, in any meter. A 3/4 bar is a 3-beat shot.
- high-energy bars split at their middle beat (4 → 2+2, 3 → 2+1), but only
  when both halves clear the 0.6 s floor.
- pickup beats before the first bar become their own lead-in span. A pickup
  shorter than 0.6 s merges into the first bar instead of flash-cutting.
- each high-energy section's hero shot goes on the first span that starts
  on a bar line *inside* the section. A pickup span at the start of a drop
  is passed over, and so is a bar that starts before the drop but gets
  counted as hot because its midpoint falls inside the section.

Ellis grids keep the v0.8 behaviour exactly: 4-beat spans (2 in high-energy
sections) from the first beat, with the hero on the first span of each
section. Their guessed bars never drive a cut.

## Limits and costs

- **Dependency weight.** `beat-this` 1.0.0 depends on `rten`, `symphonia`
  (all codecs), `rubato`, `hound`, `clap` and `glob` unconditionally, even
  though Eidetic uses none of them at runtime. Eidetic decodes with ffmpeg
  and runs the models on its own ort. They're pure Rust, compile-time cost
  only, and only in `downbeats` builds. The crate's own `ort` feature stays
  off: it forces `ort/load-dynamic` and `coreml`, which would change how
  every other model in the binary loads. Eidetic implements the crate's
  `Runtime`/`Model` traits over the workspace ort instead. Forking the crate
  to drop the dead dependencies is possible (MIT) but hasn't been done.
- **Decoding twice.** Beat This gets its own 22.05 kHz ffmpeg decode.
  Upsampling the 16 kHz decode Ellis uses would leave the model's top mel
  bands empty. Energy sections still come from the 16 kHz decode.
- **Whole-track mel.** The mel model sees the whole track in one call. That's
  fine for reel-length music, but a one-hour file costs proportionally more
  memory.
- **Synthetic audio is ambiguous.** On the smoke test's click track (an
  accented hit every 4 beats) the model finds the right tempo and 4-beat
  bars, but puts the bar line two beats off the louder hit. Real music gives
  it far more to go on. The tests assert tempo and bar length, not phase.
- **No stable tempo.** When Beat This returns fewer than four beats, the grid
  is `None`, just as with Ellis. The reel falls back to voiceover mode
  (speech builds) or fixed windows.

## Testing

Default `cargo test` never downloads anything. With the feature:

```bash
cargo test --workspace --features eidetic-cli/downbeats --offline
```

The real-inference tests (`crates/eidetic-ml/tests/downbeats_smoke.rs`, and
`auto_with_cached_models_infers_downbeats_end_to_end` in
`eidetic-cli/src/project.rs`) read the model files from
`EIDETIC_BEAT_THIS_MODELS`, or from `tmp/beat-this-support/` in the repo root
when that's set up locally. They copy the files into a temp cache and load
them through the same digest check production uses. With no files present
they print `SKIPPED` and pass. They never fetch.
