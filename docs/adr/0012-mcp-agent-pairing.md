# ADR-0012: Agent pairing = stdio MCP server, metadata-only tools

Date: 2026-09-01
Status: accepted; amended 2026-11-26 (see Amendment: `preview_sheet`) and 2026-11-27 (see Amendment: output ownership)

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

## Amendment: `preview_sheet` (2026-11-26)

goals-v0.9.md phase 1 makes one exception to "metadata only": `preview_sheet`, which returns a JPEG contact sheet of a reel project to the model so a shell-less vision client can judge the cut.

- **Off by default.** The tool is listed and callable only when the operator starts the server with `eidetic mcp --allow-previews`; without the flag a call is refused before any file is read. The metadata-only rule above stays the default, and the rejected alternative above still holds for every other tool. A default-on version was rejected for the reason the hard rule exists.
- **Library only.** Every slot source must resolve (symlinks and `..` followed) to a regular file inside the library directory, or the whole project is refused before ffmpeg runs. Projects `create_reel` plans against the current library pass.
- **Accepted limit.** The rule bounds the paths a project names, not where a library file came from: `render` and `export_otio` read any source a project names, so a render can hold frames from outside files. Since the output ownership amendment below, no MCP tool writes into the library directory, so such a render can no longer become a library file that `preview_sheet` accepts; a file the operator puts there still can.

## Amendment: output ownership (2026-11-27)

A reel project file is a write manifest: `render` writes its output and the `.preview.mp4` beside it, `export_otio` writes the `.otio` beside it, and `edit_reel` saves the file itself. `create_reel` used to be the only check, refusing outputs that existed when it planned. A project the agent did not plan (hand-written, shared, or edited to point elsewhere), or a file created after planning, let those tools replace unrelated files.

- **One rule at every MCP write.** A tool may write a destination only if nothing exists there, or the existing regular file is one this pipeline wrote for this project. `project::check_destination` applies it to `render`, its preview, `export_otio`, the `edit_reel` save, and `create_reel`'s claims (output, preview, `.otio`, project file, every `.vN.mp4`), before any subprocess and again just before the write is published.
- **The proof travels with the files, not in a database.** Every project carries a random `id` (a UUID in the project file). Renders carry `eidetic-project:<id>` as their mp4 `comment` tag, read back with ffprobe forcing the mov demuxer (so a disguised playlist cannot make ffprobe open other inputs); OTIO exports carry it as `metadata.eidetic_project`. The project file proves itself: the save goes through only while the file on disk is still the project the call loaded. If ffprobe is missing, an existing render cannot be proved and the error says to install it. No table was added: the files must stay movable and the project file is already the edit state.
- **Links and the library.** A destination that is a symbolic link, or under a directory that is one, is refused without being followed, using `symlink_metadata` on every ancestor rather than comparing canonical paths (which would refuse every Windows path). This is strict: a host whose working directory sits under a link (`/home` → `/var/home`, macOS `/var`) must use real paths, and the error names the real directory. A destination whose directory resolves inside the library directory is refused even when its tag matches: ingest copies files byte for byte, so an imported Eidetic render keeps its tag, and a tag proves nothing about a content-addressed file. A reel the operator rendered into the library can then only be re-rendered with the CLI.
- **`edit_reel` cannot move a project's outputs**: no edit op changes the output, and the save goes back to the path that was loaded. Only `eidetic reel edit --output` re-points a project.
- **Legacy projects are adopted, not trusted.** A project written before ids existed loads with none. `render` and `export_otio` refuse it before any write and tell the agent to call `edit_reel` with `"ops": []`, which records a fresh id (any edit does; the result says `adopted: true`). Adoption grants nothing that "nothing exists there" didn't already: renders and exports an earlier version left beside the project carry no id and stay protected. The operator reclaims them with `eidetic reel edit <project>` (plus `--export otio` for the `.otio`), whose load fills a missing id, saves it, and tags what it writes; old previews are simply deleted.
- **The CLI is unchanged.** `eidetic reel` and `eidetic reel edit` keep their own existence checks and never call the ownership check; they only gain the tag (and an atomic `.otio` write, which replaces a link at the destination instead of writing through it).
- **Not a sandbox.** The rule keeps a project's write manifest off unrelated files; it does not confine the server, which still reads any slot source and runs as the user. The id is an identity, not a secret: anyone who can run ffprobe on a render can copy it into a hand-written project, which can then replace that project's tagged outputs, which are Eidetic outputs. A file appearing between the last check and the rename is still replaced; the window is the time it takes to publish, not the length of the render.

## References

- goals-v0.8.md (market findings, phases, privacy boundary)
- docs/director-playbook.md (phase 2: the conventions an agent should follow)
- modelcontextprotocol/rust-sdk (`rmcp`)
