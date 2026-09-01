# The director playbook

Conventions for an AI agent directing reel edits through the `eidetic mcp`
tools (goals-v0.8.md phase 2). Drop this file into the agent's context —
a project `CLAUDE.md`/`AGENTS.md` that says "follow docs/director-playbook.md
when cutting reels" is enough.

An agent with tools still needs taste. These rules are what separates a
correct render from a postable one; they were learned cutting real reels,
not invented.

## The loop

1. **Scout before you cut.** `library_stats` for scale, `search_library`
   with 2–3 phrasings of the user's idea, `list_persons` if people are
   named. With music: `beats` on the track first — BPM, duration, and the
   high-energy sections decide everything else.
2. **`create_reel`** plans and saves a project file without rendering.
   Iteration on the cut list is free; renders are not. Don't render yet.
3. **Audit every slot** in the returned cut list:
   - A slot scoring far below its neighbours is filler the planner
     couldn't avoid — `search_library` for alternatives and `swap` it.
   - The same asset (or near-identical shots of the same scene) twice in a
     row reads as a stutter — swap one for variety.
   - Check `faces_in` on hero shots when the reel is about a person: the
     right person, not just any face.
4. **`edit_reel`** with a batch of ops. Slot numbers refer to the cut list
   you were just shown; the whole batch resolves against it, so combine
   freely (swap 3 + drop 5 + retime 2 in one call).
5. **`render` with `preview: true`** and show the human. Pixels are the
   human's call — never claim a reel "looks good", you haven't seen it.
6. Apply follow-ups to the same project; render full-res once, at the end.
7. Offer **`export_otio`** when the user wants polish beyond the tools
   (speed-ramps, grading, captions): the cut opens in Kdenlive 25.04+ /
   Resolve 18.5+ with every clip live.

## Editing conventions

- **Hook first.** The opening slot decides whether anyone watches slot
  two. Lead with a strong, legible shot — a face, an action, a vista; not
  a establishing shot that "builds up".
- **Never end weak.** The last slot is the aftertaste. If the planner's
  tail slot has the lowest score in the list, swap or drop it. Ending on
  the second-best shot of the reel is a classic move.
- **Save the hero for the drop.** The planner already puts the best hit on
  the first beat of each high-energy section — respect that when swapping:
  don't move the weakest shot onto a drop, and pin the drop slot once
  you're happy with it.
- **Cut density follows energy.** Verses/calm sections hold shots ~4
  beats, choruses ~2 (the planner does this). If you retime, keep that
  shape: long holds in loud sections feel dead, fast cuts in quiet ones
  feel nervous.
- **Transitions are punctuation, not decoration.** Hard cuts on beats are
  the default and correct. Crossfade only where the *music* changes
  (section boundaries — the planner marks these). Whip/slide are for
  deliberate energy, at most once or twice per reel. Dip-to-black
  mid-reel reads as "the end"; don't — the render already fades the tail.
- **Shot diversity beats score order.** Ten best-scoring hits of the same
  sunset make a worse reel than six sunsets, two faces and two wides. When
  the cut list is monotonous, swap with more specific queries ("kids
  running on the beach", not "beach" again).
- **Respect the frame.** Portrait reels crop hard; scenic panoramas belong
  in landscape output or as `photo_scenic` slots (blur-fill). If a swap
  puts a wide landscape into a portrait reel, expect it to render as the
  blurred-backdrop treatment — that's correct, not a bug.
- **Pin what the human approves.** When the user says "keep that shot",
  `pin` it immediately — later swaps and drops will refuse to touch it.

## What the tools will and won't tell you

- Scores are cosine similarities (~0.05–0.4 range in practice); compare
  *within* a result list, never across queries.
- The sharpness gate already dropped clearly-blurry candidates; a slot
  that made it in is at worst soft. You cannot see pixels — only the human
  can. Preview early.
- `transcript` is recall fuel (what was said, when) — quote it when
  choosing voiceover shots, never burn it in as subtitles.
- Renders are deterministic from the project file: same file, same reel.
  The project JSON is the ground truth; read it if in doubt.

## Cost discipline

Tool results are compact by design; keep the conversation the same way.
Don't re-run `search_library` with the same query to "check"; don't call
`render` full-res to inspect a cut you can preview at half size; don't
regenerate with `create_reel` when `edit_reel` can fix the slot — a
regeneration throws away every pinned decision the human already made.
