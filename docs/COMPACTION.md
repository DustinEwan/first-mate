# Context Compaction — Design

Design detail for roadmap §6 (Phase 0). § references below point to
`docs/ROADMAP.md`.

**Problem.** The agent forgets the task it was given and forgets the rules
it agreed to follow. Both are window-management bugs, not model bugs, and
both are measurable on a normal day of use.

## Evidence (2026-10-09, 161 provider requests)

- The history window is **rows, not tokens**: `ChatView.vue` ships
  `messages.value.slice(-30)` after filtering to user/assistant/tool rows.
  On a 166-item drivers chat the last 30 rows held **zero user turns** —
  the request carried "no user turn at all". 5 of 12 chat invocations hit
  the cap.
- Request size: median 47.9 KB, p90 783 KB, max 1.46 MB. Size is **not** a
  cost or latency problem: >900 KB requests were the _fastest_ (median 2 s
  vs 3 s under 60 KB), and there is no 413 or context-overflow error in the
  log. The layout is append-only, so the prefix — system prompt, history,
  screenshots — is byte-stable and provider-cached.
- Screenshots are the bulk of those bytes (42 attach events; one 461 KB PNG
  → 664 KB request). They are vision **tokens in the window**, so they
  matter for the trigger, not for cost. They are also dropped entirely at
  the turn boundary: `prior_history` is text-only, so visual memory ends
  when the user speaks.
- Nothing knows a model's window: `providers.json` carries no field,
  `modelapi.rs` discards the `max_model_len` the endpoint already reports,
  and `llm.rs` never parses `usage`/`prompt_tokens`.
- `load_skill` returns the whole `SKILL.md` (`execute.rs`), but
  `tool_report` attaches no body for it, so the `.chat` ledger keeps only
  `📖 load_skill filesystem ✅`. Capability is re-derived from that line at
  resume, so gated tools unlock **without their instructions in context**.

## The rule that governs everything else

Compaction **invalidates the prefix cache**: rewriting history changes the
prefix, so the next request pays a full re-prefill. Therefore the prefix
stays byte-stable by default, compaction fires only when the window forces
it, and old tool output is pruned only when the cache is already cold
(idle past the provider's TTL) or the trailing suffix is small. We are
spending a cache miss; it has to buy window headroom we actually need.

## Mechanism

`src-tauri/src/context.rs` holds the arithmetic; both chat paths in `llm.rs`
apply it the same way — once before the request is built, and again between
turns inside a run.

1. **Unit is tokens, never rows.** Trigger: `Compact at __% context`
   (global setting, default 85%). Window: the user's override, else the
   endpoint's report (`max_model_len`, Ollama `model_info`; cached per
   endpoint + model), else `DEFAULT_WINDOW_TOKENS`. Estimates are local:
   chars/4, image parts at a flat `IMAGE_TOKENS` allowance instead of their
   base64 length, plus the byte-stable tool-registry overhead. When the
   endpoint reports `usage.prompt_tokens` on the stream, that count floors
   the estimate and only the rows appended since are guesses.
2. **Fold to a target, and only when it pays.** `target_tokens` is 75% of the
   threshold: a request trimmed to exactly the threshold is over the line
   again on the next tool result, and every fold costs a summariser call — a
   12k window at 60% folded four times in one run before this rule existed.
   A fold that would free less than `MIN_FOLD_SAVINGS_TOKENS` is skipped and
   logged rather than paid for (a 6k window produced one that bought 259
   tokens).
3. **Cut where the wire allows.** Before a run the persisted ledger is cut at
   a turn boundary (`turn_anchored_cut`) — the kept tail always begins at a
   user row, so a turn is never split; the cut is a prefix, so whatever
   instruction it swallows goes to the summarizer (rule 5). Inside a run there
   is usually one turn, a long tool loop, and anchoring to its start would keep
   everything; there the middle goes (`middle_out_cut`): the opening row plus
   the newest rows that fit, whole tool rounds removed between them so no
   `tool` row is orphaned from the call that produced it.
4. **Prune before summarizing.** Elide the oldest tool-output bodies to their
   existing spill pointer (`~/.firstmate/output/out-*.txt`), protect the
   newest `prune_protect_tokens` — 20k, capped at half the threshold so a
   small-window model still has an older half to prune — and never touch
   `load_skill` / `read_skill_resource` bodies. Markers are computed first
   and applied only
   when the savings clear `MIN_PRUNE_SAVINGS_TOKENS`, so a pass that cannot
   pay for the cache miss leaves the transcript untouched instead of eliding
   it and reporting nothing. No model call.
5. **Summarize the dropped rows, with the instruction in hand.** The brief is
   framing, not a turn, so it folds into the system prompt (`set_summary`) and
   replaces any earlier brief rather than stacking. Every fold is handed the
   conversation's first user row verbatim (`pinned_opening`) unless the rows it
   is folding already carry it. The in-run cut keeps that row on the wire, so
   the dropped middle holds no instruction at all, and a summarizer cannot copy
   a line it was never shown: a shipped transcript's brief read "user request:
   not explicit in transcript". A later fold would otherwise see only the
   previous brief and paraphrase the request until it drifted. What the
   summarizer is told is content — `prompts/COMPACT.md`, layered like
   `AGENTS.md` — and with no copy at any layer the fold is skipped and logged
   rather than run on compiled-in prose. The console keeps its full scrollback
   with an inline divider, worded for the reader ("summarized the earlier chat
   so it could keep going", row counts left to the tooltip); the persisted
   `summary` row is where the next request resumes, so a 166-row ledger pays
   one summariser call and never re-folds the rows it already covered.
6. **Re-inject loaded skill bodies** at request build: rebuild the loaded set
   from the ledger, then ensure each loaded skill's `SKILL.md` is in the
   request. Fixes the resume gap independently of compaction, and keeps it
   fixed after a compaction.

## Explicitly not

- **No image-lifetime policy.** Dropping older screenshots breaks
  before/after comparison, the thing a UI-driving agent must do, and buys
  nothing a cache doesn't already give us.
- No provider-native compaction (needs OpenAI Responses or the Anthropic
  beta; the local llama.cpp has neither), no bitmap archives of history, no
  branch summaries, no speculative/async pre-summarization.
- No per-model `__%` yet: the setting is global. Per-model config lands
  with multi-model selection and fallbacks.

## Open

- Mid-loop compaction ships (rule 3): a long tool loop is the common case,
  and post-turn-only would let it overflow. Watch whether folds land on
  rounds the model still needed.
- Keep-newest budget: ~20k tokens is the reference default; confirm
  against the models we actually point at.
- Spill rot: startup keeps only the 50 newest files in `fm_out_dir`, so a
  pointer quoted in an old conversation can dangle. Decide whether
  compaction protects referenced spills or accepts the loss.
