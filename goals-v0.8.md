# Goals: v0.8 — the agentic edit engine

Status: proposed (2026-08-22)

The pivot, in the owner's words: eidetic started as an Immich alternative; it should become a production-level reel/editing tool — and instead of growing its own intelligence, it should **pair with an AI agent** (Claude Code, Codex, or a local LLM) that directs the editing with full context of the library. Market-validated on 2026-08-22.

## The market finding that shapes this

No product today combines all four of: **(a)** fully local, **(b)** whole-life-library index (semantic + faces + speech), **(c)** beat-synced auto reels, **(d)** LLM-agent-driven editing. The closest are Jumper (local, MCP-driven, face/transcript search — but aimed at pro NLE workflows, no reels, no self-rendering, $29/mo), Google Photos (whole library + beat-synced highlights — but cloud, non-agentic), and Descript's Underlord (genuinely agentic — but cloud, per-project, ~70% success on complex prompts). Eidetic already has (a)+(b)+(c). This goal adds (d) — the unoccupied square.

The pairing mechanism is settled by the ecosystem: **MCP**. Every comparable 2026 product chose it (Jumper, Selects, reap; half a dozen DaVinci Resolve MCPs; multiple Immich MCPs). One server buys Claude Code, Codex, Cursor and local LM Studio/Ollama clients simultaneously. The design lesson from Jumper's privacy story: **tools return metadata only — paths, timecodes, scores, transcript excerpts — never pixels.**

## Phase 0 — finish v0.7 (prerequisites, not leftovers)

These stopped being polish; they're the agent's working surface:

1. **Reel project files** (v0.7 phase 2): a persisted JSON timeline is the object an agent inspects and mutates turn by turn. Without it every agent instruction is a full regeneration.
2. **`eidetic reel edit`** structured commands (swap/drop/pin/retime/reorder): these become the MCP mutation tools almost 1:1.
3. **OTIO export** (v0.7 phase 3): the "rough cut + human polish" shape is what the research says users actually accept (Underlord lands ~70%; everyone polishes). Kdenlive/Resolve open the agent's cut.
4. **Sharpness gate and section crossfades** (deferred from phase 1): quality floor for shots the agent picks blind.

## Phase 1 — `eidetic mcp`: the library and the edit as tools

A stdio MCP server (official Rust SDK) exposing what the CLI already does, structured for an agent:

- **Context tools** (read-only, metadata-only): `search_library(query)` → asset ids, paths, scores, moments, spoken snippets; `list_persons` / `faces_in(asset)`; `scenes(asset)`; `beats(audio)` → BPM, beat/section map; `transcript(asset)`; `library_stats`.
- **Edit tools**: `create_reel(prompt, audio?, style?)` → project id + cut list; `edit_reel(project, ops[])` — swap/drop/pin/retime/reframe per slot; `render(project)` → output path; `export_otio(project)`.
- Every tool result is compact text/JSON — the cost mitigation the research flags (agent sessions compound context fast).

## Phase 2 — the director playbook

An agent with tools still needs taste. Ship a `CLAUDE.md`-style playbook (and/or a skill) encoding the editing conventions we've already learned: cut density by energy, scenic-vs-people framing, shot diversity, hook-first ordering for reels, "never end on a weak shot". This is where "the reel wasn't actually good" gets fixed — the agent iterates against the cut list *before* rendering, checks every slot's score/sharpness, and re-searches weak slots.

## Phase 3 — local-agent validation

Prove the fully-offline story: the MCP server driven by a local model (LM Studio / Ollama MCP client), same tools, no bytes leaving the machine. This keeps the original anti-goal available to anyone who wants it, and matches Jumper's LM Studio path.

## The privacy boundary (explicit decision)

Pairing with a **cloud** LLM sends tool *results* — transcript excerpts, person names, file paths, prompt text — to that provider. Pixels never leave (metadata-only tools), but this still amends the founding anti-goal. New wording: **"no cloud required"** — the default stays fully offline; connecting a cloud agent is an explicit, documented opt-in, and the local-LLM path is a supported first-class alternative.

## Risks (from the field, with mitigations)

1. **Cost per session** — agentic sessions commonly run $5–15 API-equivalent; long edit conversations compound. Mitigation: metadata-only compact tool outputs, flat-plan usage (Claude Max), local-LLM path.
2. **Quality ceiling** — the market's honest state is "rough cut + human polish" (Underlord ~70%, Opus Clip 22% one-star reviews). Mitigation: project files + OTIO export make polish cheap; the playbook + iterate-before-render loop raises the floor.
3. **Scope creep vs. "not a UI project"** — "production-level editing tool" must NOT mean building a GUI editor. The agent is the interface for direction; Kdenlive/Resolve (via OTIO) are the interface for pixels. The anti-goal stands.
4. **Competitive drift** — Jumper expanding into consumer libraries + rendering, or Google Photos gaining an agent, would crowd the niche. Neither is offline+open; speed matters more than secrecy here.

## Rejected

- Building our own agent loop over a raw LLM API (loses the Claude Code/Codex/Cursor ecosystem MCP buys for free).
- Embedding a web editor UI (unchanged from v0.7's research: the drivable ones are immature; OTIO to real NLEs wins).
- Sending frames/pixels to any model, cloud or local, as tool output.
