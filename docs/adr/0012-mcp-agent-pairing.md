# ADR-0012: Agent pairing = stdio MCP server, metadata-only tools

Date: 2026-09-01
Status: accepted

## Context

goals-v0.8.md pivots eidetic from "Immich alternative" to "agentic edit engine": instead of growing its own intelligence, eidetic pairs with an AI agent (Claude Code, Codex, Cursor, or a local LLM) that directs editing with full context of the library. The market research (in goals-v0.8.md) found the unoccupied square is local + whole-library + beat-synced reels + **agent-driven editing**, and that every comparable 2026 product (Jumper, Selects, reap, the DaVinci Resolve MCPs, the Immich MCPs) chose MCP as the pairing mechanism.

The design question is how eidetic exposes itself: what protocol, what process model, what the tools return, and where the edit state lives between agent turns.

## Decision

- **Protocol: MCP over stdio**, via the official Rust SDK (`rmcp` 3.x, default `server`+`macros` features plus `transport-io`). One server buys every MCP client — Claude Code, Codex, Cursor, LM Studio, Ollama frontends — with no per-client work. `eidetic mcp` is a subcommand of the existing binary: no new crate, no daemon, no port; the client spawns and owns the process (`claude mcp add eidetic -- eidetic mcp`).
- **Metadata only, hard rule.** Tools return paths, timecodes, scores, person names and transcript excerpts — never pixels or thumbnails. Pairing with a cloud LLM sends tool *results* to that provider; this rule bounds what can leave the machine and keeps the local-LLM path equivalent. (This is Jumper's privacy design, adopted deliberately.) It also amends the founding anti-goal from "no cloud" to **"no cloud required"**: connecting a cloud agent is an explicit, documented opt-in.
- **Compact output.** Agent sessions compound context fast (the research's cost warning: $5–15/session is common). Every result is small JSON — capped hit lists, rounded numbers, the full cut list only when the tool is about the cut list.
- **The edit state is the reel project file** (phase 0), not server memory. `create_reel` plans and *saves* without rendering; `edit_reel` mutates the file through the same `EditOp` batch the CLI flags compile to; `render` is separate and takes `preview=true` for half-res review. The server stays stateless across calls and restarts, and a human can pick up any project with `eidetic reel edit` mid-conversation.
- **stdout belongs to the protocol.** All shared generation/edit code paths log to stderr only — enforced during the phase-1 refactor that moved generation into `project::plan_projects`.

## Consequences

**Good:**

- Eleven tools (`search_library`, `list_persons`, `faces_in`, `scenes`, `beats`, `transcript`, `library_stats`, `create_reel`, `edit_reel`, `render`, `export_otio`) map ~1:1 onto existing CLI capabilities; the server is wiring, not new logic.
- Plan-without-render makes agent iteration nearly free: the expensive step (ffmpeg) runs once, after the agent has checked every slot's score and swapped the weak ones.
- Client-agnostic by construction: verified by driving the server with raw JSON-RPC over stdio (initialize → tools/list → tool calls → shutdown), which is exactly what any client does.

**Costs / accepted risks:**

- `rmcp` + `schemars` join the dependency tree of the default build (~moderate compile-time cost). Not feature-gated: the MCP server *is* the v0.8 feature.
- Each embedding-backed tool call loads SigLIP fresh (~0.3s CPU); a persistent embedder cache is deliberate future work if it ever matters.
- Tool-level failures are returned as `isError` text (the agent reads and adapts); protocol errors are reserved for malformed requests.

## Alternatives considered

- **Own agent loop over a raw LLM API** — rejected in goals-v0.8.md: loses the entire MCP client ecosystem for free.
- **HTTP/SSE transport** — nothing needs a network hop; stdio is simpler, spawn-scoped, and what every target client speaks natively.
- **Tools returning thumbnails/frames** (so the agent could "see") — rejected permanently; the human judges pixels via preview renders, the agent judges metadata. That division is the product design, not a limitation.

## References

- goals-v0.8.md (market findings, phases, privacy boundary)
- docs/director-playbook.md (phase 2: the conventions an agent should follow)
- modelcontextprotocol/rust-sdk (`rmcp`)
