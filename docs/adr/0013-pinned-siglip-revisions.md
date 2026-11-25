# ADR-0013: SigLIP artifacts come from pinned commits; model_id names the commit

Date: 2026-09-23
Status: accepted

## Context

Every SigLIP 2 variant (`base`, `large`, `so400m`; ADR-0004, ADR-0007) is an
onnx-community export on HuggingFace: a vision graph, a text graph, the
tokenizer, and for `large`/`so400m` an external-weights companion for the text
graph. Until now all of them were fetched from the repo's `main` branch, and
the `model_id` stored on every `embeddings` / `frame_embeddings` row
(migration 009) was the bare variant name, e.g. `siglip2-base-patch16-256`.

`main` moves. A re-export, a tokenizer fix or a re-quantised graph pushed
upstream would land on the next fresh download (a new machine, a wiped
`~/.cache/eidetic/models`) under the same `model_id`, and its vectors would
rank against the library's existing ones as if they were the same space. Two
exports of one variant share a width, so the width check that used to guard
search could not catch it either. Search also ignored `model_id` entirely and
ranked every stored row of the right width.

## Decision

1. **Each variant pins one immutable commit.** `ModelVariant::revision` in
   `crates/eidetic-ml/src/siglip.rs` holds a full 40-hex commit hash, and every
   required file (vision, text, tokenizer, external text weights) is fetched
   from that commit. `main` is never consulted for SigLIP.

   | Variant | Repo | Commit | Required files verified at that commit |
   |---|---|---|---|
   | `base` | `onnx-community/siglip2-base-patch16-256-ONNX` | `d1114256522a37ffa257a0a58017348ab0058db2` | `onnx/vision_model.onnx`, `onnx/text_model.onnx`, `tokenizer.json` |
   | `large` | `onnx-community/siglip2-large-patch16-384-ONNX` | `257daa8df5100fdd0add12f4c2187aebf0beb409` | the above + `onnx/text_model.onnx_data` |
   | `so400m` | `onnx-community/siglip2-so400m-patch14-384-ONNX` | `8ecf3f2bbb135bec33f4bb721c11479bf6a8a254` | the above + `onnx/text_model.onnx_data` |

   Provenance: read from the official
   `https://huggingface.co/api/models/<repo>/revision/main` metadata on
   2026-09-23. That is, these are the commits `main` pointed at when pinned,
   with the file list checked against the same response. No weights were
   downloaded to derive them.

2. **`model_id` is `<variant>@<commit>`**, for example
   `siglip2-base-patch16-256@d1114256522a37ffa257a0a58017348ab0058db2`. The
   commit determines the whole artifact set, so the id names exactly one set.
   Bumping a pin changes the id, and a unit test fails if the two drift.

3. **Every reader filters by exact `model_id`.** `AssetsRepo::search`,
   `search_similar`, and the `eidetic dupes` inputs
   (`fetch_all_image_embeddings`, `fetch_all_frame_embedding_sets`) take the
   id of the embedder that produced the query. The CLI, MCP server, reel
   planner and HTTP server pass `SiglipEmbedder::model_id()` (or
   `eidetic_ml::active_model()` where no model is loaded). Width stays a
   defensive filter inside ranking, not the identity check.

4. **Cache routing.** hf-hub stores a file under
   `models--<org>--<repo>/snapshots/<commit>/<file>`. If that exact path
   exists it is used without a network call. Anything else, including a
   snapshot an older build pulled from `main`, is ignored, and the file is
   fetched by commit. So a cache that already holds the pinned commit
   (because `main` was that commit when it was downloaded) needs no
   re-download, and one that holds a different commit cannot be picked up by
   mistake.

The face models and whisper still download from `main`. They are out of
scope here and carry their own model ids (ADR-0010, migration 009).

## Compatibility and migration

- **No schema migration, no deletion.** Rows written before this change keep
  their bare-name `model_id`. They stay on disk untouched.
- **They no longer answer queries.** Legacy ids never equal a pinned id, so
  search excludes them. If nothing in the library matches the active id,
  search fails with `ModelMismatch`, naming the active and stored ids and
  telling the user to run `eidetic embed`, rather than returning an empty or
  mixed page. `eidetic dupes` only compares rows from the active id.
- **Re-embedding is explicit.** `eidetic embed` already queues any asset whose
  row has a different `model_id` (migration 009), so after upgrading it
  re-embeds the whole library under the pinned id. Each re-embedded asset
  replaces its own legacy row (image rows upsert, frame rows are replaced per
  video). Until then, a half-migrated library searches only the rows already
  under the new id.
- **Setting `EIDETIC_MODEL` back does not revive legacy rows.** There is no
  bare-name variant to select any more. That is deliberate: nothing records
  which commit produced a legacy row, so claiming it for a pinned commit would
  be a guess.

## Consequences

**Good:** a fresh install and a long-lived one produce comparable vectors or
are told they differ. Upstream changes can't reach the library without an
edit here, a new id and a re-embed.

**Bad / accepted:** upgrading costs one full re-embed, even when the cached
files happen to be byte-identical to the pin. The bare-name rows can't be
proven to match any commit, so paying that cost is the honest option. Moving
to a newer export is a manual change: update the commit and id, update the
test's expected table and this ADR, and re-embed.

## Alternatives considered

- **Keep `main`, record the commit hf-hub resolved.** The id would still be
  decided by whatever upstream served that day, and two machines could
  disagree silently. Rejected.
- **Backfill legacy rows to the pinned id.** Only correct if every legacy row
  came from exactly the pinned commit, which nothing on disk records.
  Rejected; see the compatibility section.
- **Hash the downloaded files into the id.** Exact, but it needs every file on
  disk before the id is known, and `eidetic embed` asks for the id *before*
  loading so an empty queue doesn't cost a 1.4 GiB download. A commit hash is
  already a content address for the whole repo tree, so it gives the same
  guarantee without that.
