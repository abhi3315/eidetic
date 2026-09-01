# Goals: v0.9 — eyes, taste, and recipes

Status: proposed (2026-09-01)

v0.8 shipped the agentic edit engine and the first real-library dogfood proved the loop works — a themed 27s reel went from prompt to postable through create → audit → swap → render, entirely over MCP. It also proved exactly where the loop leaks, because every leak had to be patched by hand from outside the tool. v0.9 closes those leaks. Basis: the dogfood session (2026-09-01, Rishikesh/Mussoorie library, 325 assets) plus a four-stream market/community/tech research pass the same day.

## What the dogfood session actually showed

1. **The agent judged pixels out-of-band.** Every editorial catch that mattered — three files of the same posing scene filling three slots, a clip whose *content* is sideways, a weak "rapids" match that was really a stream — was made by extracting frames with raw ffmpeg, outside the MCP surface. A shell-less client (Claude Desktop, LM Studio) cannot do this at all. The published evidence agrees: EditDuet (SIGGRAPH 2025) tested almost exactly our architecture and the winning ingredient was a critic that sees a **grid of per-slot keyframes** — a contact sheet, not video. No MCP media product ships this.
2. **Semantic scores measure the wrong thing for hot sections.** The rafting reel picked all photos: a static photo *of* rafting outranks shaky footage of rafting, because scores measure semantics, not energy. Motion is invisible to the planner.
3. **Search can't see sameness.** Near-duplicate scenes are different files with different ids; asset-level dedup in swaps is blind to them. The embeddings that would catch it are already in the database (`eidetic dupes` uses them).
4. **"Every 4th beat anchored on the strongest onset" is an approximation with an expiry date.** Real downbeats are now available in Rust: the `beat-this` crate (Beat This!, ISMIR 2024) has an ort backend — infrastructure we already ship — and a 10 MB model.
5. **The taste that made the good reel is not persisted anywhere.** "Open on POV motion, vistas as breathers, hero on the drop, no adjacent same-scene shots, close on faces" lived in the agent's conversation and died with it.

## The market finding that shapes this

The four-square position (local / whole-library / beat-synced / agent-driven) is still unoccupied, but the window is narrowing at both ends: Google Photos shipped beat-aligned highlight templates (Dec 2025, cloud, template-locked, their music only); Jumper holds local+agent but stops at the NLE handoff (no rendering, no beats); Immich has a 107-upvote highlight-reel request its maintainers declined, and a tiny active project (immich-video-memory-generator) slowly converging on slideshow-with-music. Meanwhile OpenMontage (23.6k stars) owns "Claude Code makes videos" mindshare — with *generated* stock content. Positioning must say: your footage, your people, real frames only.

The loudest community pain maps directly onto this plan: wrong-shot selection (Opus Clip: "2–3 usable of 20"), uncorrectable auto-memories (641 me-toos on one Apple thread), and craft — HN's dismissal pattern for this category is "just cuts clips together, no rhythm". Phases 1–2 are the defense on all three.

## Phase 1 — the agent gets eyes: `preview_sheet`

One new MCP tool: a tiled grid of per-slot keyframes (midpoint frame, slot number, duration, transition overlaid), written as an image the calling agent reads with its own vision. This is the EditDuet loop, and it is precisely the manual workflow that fixed both dogfood reels.

**Privacy contract change, handled explicitly:** this returns pixels to the model. It ships OFF by default and turns on only with `eidetic mcp --allow-previews` — the metadata-only default stays true, the README documents the trade, and the fully-local LM Studio path makes even the opt-in leave-nothing-behind. (A default-on version was rejected: the "never pixels" line is the product's identity and half its launch story.)

Also in this phase: the op-ordering ergonomics fix from the session (a `transition` op aimed at a slot that a `reorder` in the same batch relocates gets silently refused; resolve ops against the post-reorder layout or document the two-call pattern in tool descriptions).

## Phase 2 — the planner gets taste: quality signals + downbeats

- **Per-moment motion scores** at embed time (frame-diff over the sampled frames — auto-editor ships an entire product on this signal). Planner: hot sections prefer motion; cut lists expose the score.
- **Near-duplicate detection surfaced everywhere**: cosine over stored embeddings, shown as `similar_to: <slot>` in cut lists, used as a diversity penalty at plan time and a candidate filter in swaps. This alone would have fixed both flat dogfood reels with zero agent effort.
- **Real downbeats** via `beat-this` (ort feature, small model, cached like other models): bar-quantized spans, cut-on-downbeat, hero shots on bar boundaries. The existing Ellis tracker stays as the no-model fallback.
- **Orientation/pixel sanity in cut lists**: dimensions, rotation, and a "content may be sideways" flag where detectable — cheap metadata that warns where vision would.
- **Aesthetic scoring: cautiously, last.** LAION-style heads are tiny MLPs over embeddings we already store, but they encode stock-photo taste, not family-footage taste. Ship behind a flag, weigh it lightly, and let dogfooding decide if it survives. It is the one Phase-2 item allowed to die quietly.

## Phase 3 — recipes: the intent layer (and the template play)

A **reel recipe** is a single shareable JSON file describing intent, not clips: sections mapped to music structure (intro/verses/drops by energy + downbeats), each with a search query template, constraints (min motion, min score, must/must-not contain a person, kind quotas), an ordering policy (hook-first, close-on-faces), and a transition style. `create_reel` accepts a recipe; a `validate` tool checks one without planning (the json2video pattern). The cut list stays the deterministic ground truth; the recipe makes the *taste* reproducible across libraries and tracks.

Structure-adaptive is the differentiator: CapCut templates demand exact clip counts and fixed durations (a documented top complaint); recipes adapt to any library and any track because the beat grid supplies the structure. Recipes reference music *structure*, never specific songs — no licensing entanglement.

Ship **8–10 free recipes in-repo** (trip montage, POV ride, squad reel, day-to-night, waterfall day…). They are the demo material and the schema's proof of expressiveness. The paid-marketplace idea is real but sequenced out — see Rejected/deferred.

## Phase 4 — reach: Immich import + launch hygiene

- **Read-only Immich import**: point eidetic at an Immich library (or its export) without moving files. The most organized community that wants this outcome already exists and was told no; meet them where their photos are.
- **Distribution checklist** (the research is unambiguous about what r/selfhosted and HN reward): prebuilt release binaries already exist — add Homebrew tap + cargo-binstall metadata + AUR; keep CPU-works-by-default loud; auto model download with progress (exists — say so); ffmpeg detection with per-OS install hint at startup; a 30-second demo GIF of "point at folder → ask Claude → beat-synced reel" at the top of the README. Position against OpenMontage explicitly: memories, not generated content.

## Rejected / deferred (the ideas that didn't survive)

- **Template marketplace ("eidetic site", free tool + paid templates)** — right model, wrong moment. Recipes are trivially copyable JSON; the durable paid product is freshness/curation, and freshness cannot be sold to zero users. The schema is designed day-one as if the marketplace will exist (single shareable file, versioned); revisit only with real traction, as curated packs or a hosted gallery — never by paywalling the tool.
- **CapCut draft exporter** — CapCut has no official MCP/API; community projects reverse-engineer the local draft format. Tempting wedge into the creator audience, but it builds on a foundation ByteDance can break in any update, from a company whose template economy this would siphon. Watch the community projects; build only on demonstrated demand. OTIO already covers the pro handoff.
- **Full music-structure labeling (intro/verse/chorus via allin1-class models)** — Python/NATTEN-bound, painful to embed; energy sections + real downbeats cover the editing decisions that matter.
- **Aesthetic scoring as a core signal** — demoted to flagged experiment (see Phase 2) for taste-mismatch reasons.
- **Default-on agent pixel access** — permanently rejected; opt-in only.
- **Render receipts / provenance hashes** (Kinocut-style) — determinism already exists: the project file fully determines the render. A hash adds ceremony, not capability.
- **NLE bridges beyond OTIO** (Resolve/Premiere MCPs) — the field's API-mirroring tool sprawl (285-tool servers) is the documented anti-pattern; eidetic stays outcome-shaped.

## Risks

1. **Craft ceiling remains the judgment axis.** If renders read robotic, nothing else matters; Phase 2 is the defense, and preview-loop iteration (Phase 1) is the recovery path.
2. **Convergence**: Google on craft (their beat templates will improve), Immich ecosystem on distribution. Speed matters more than secrecy; the combination plus the iterate loop plus OTIO stays the moat.
3. **Scope creep via recipes**: a recipe language can grow into a programming language. Constraint: recipes are declarative JSON validated against one schema version; anything needing logic belongs in the agent, not the recipe.
