# Context Compaction — Design

`src-tauri/src/context.rs` holds the arithmetic. `src-tauri/src/llm.rs` applies
it in both chat paths. `src-tauri/src/settings.rs` holds the two user settings.

## Problem

The agent forgets the task the user gave it. The agent also forgets the rules it
agreed to follow. Both failures are window-management bugs, not model bugs. Both
failures are measurable on a normal day of use.

## Evidence that forced the design (measured 2026-10-09, 161 provider requests)

These bullets record the state before the rules below shipped. Each bullet names
the rule that closed the gap.

- The history window counted **rows, not tokens**. `ChatView.vue` sent
  `messages.value.slice(-30)`. On a 166-item drivers chat, the last 30 rows held
  **zero user turns**. That request reached the provider with no user turn at
  all. 5 of 12 chat invocations hit the 30-row cap. Rule 1 and rule 3 replaced
  that cap with a token threshold. `ChatView.vue` now sends the whole transcript
  with no row cap.
- Request size is median 47.9 KB, p90 783 KB, max 1.46 MB. Size is **not** a
  cost or latency problem. The largest requests answered fastest: median 2 s
  over 900 KB, against 3 s under 60 KB. The log has no 413 error and no
  context-overflow error. First Mate only appends rows, so the prefix stays
  byte-stable and the provider caches it. The prefix covers the system prompt,
  the history, and the screenshots.
- Screenshots make up most of those bytes. The log has 42 attach events. One
  461 KB PNG became a 664 KB request. A screenshot costs vision **tokens in the
  window**, so it matters for the trigger, not for cost. The next request
  carries no screenshot, because `HistoryItem` stores `role` and `content`
  strings only. The model therefore loses every screenshot at the next user
  turn.
- Nothing knew a model's window. `providers.json` defines no window field,
  `modelapi.rs` discarded the `max_model_len` the endpoint reported, and
  `llm.rs` parsed no `usage`. `model_context` now reads that report, and
  `WINDOW_CACHE` caches it per endpoint plus model. `llm.rs` reads
  `/usage/prompt_tokens` from the stream. Rule 1 prefers the user override, then
  the endpoint report, then the fallback.
- `load_skill` returns the `SKILL.md` body. `parse_frontmatter` in `execute.rs`
  removes the front matter. `tool_report` in `report.rs` attaches a
  body only when the call failed. The `.chat` ledger therefore stores
  `📖 load_skill filesystem` plus the `✅` footer, with no instructions.
  `ChatView.vue` rebuilds the loaded set from that ledger line, so a resume
  unlocked gated tools **without their instructions in context**. Rule 6
  re-injects each loaded body at request build, so the ledger shape no longer
  decides what the model sees.

## The rule that governs everything else

Compaction **spends the prefix cache**. Rewriting history changes the prefix, so
the next request re-processes the whole prefix. The prefix therefore stays
byte-stable by default. Compaction fires only when the estimate passes the
threshold. Pruning rewrites only rows older than the protected newest suffix,
and pruning is monotone: a row old enough to prune stays old as the run grows.
The cached prefix therefore churns once, not every turn. Every fold costs one
cache miss plus one summarizer call. The freed space must be space the run
actually needs.

## Mechanism

`src-tauri/src/context.rs` defines the arithmetic. Both chat paths in `llm.rs`
apply it the same way before they build the request. Only `chat_via_openai`
applies it again between turns. `chat_via_lmkit` serves Anthropic and Gemini and
folds once, before the request.

1. **Unit is tokens, never rows.** The trigger is the
   `Compaction threshold (percent of context)` setting, which is global and
   defaults to 85 (`compact_at_percent`). The Settings UI allows 10 to 100.
   `threshold_tokens` clamps the percent to that range. A bad setting therefore
   can neither disable compaction nor fire it on an empty window.
   `resolve_window` takes the window in this order: the `context_window`
   override when it is above 0, then `model_context`, then 0. An unknown window
   gives no budget, so no fold runs. The provider judges the size of the
   request, and `llm.rs` logs `COMPACT: no window known; sending the full
   history`. `model_context` asks the endpoint once per endpoint plus model and
   stores the answer in `WINDOW_CACHE`. `window_of_entry` reads
   `max_model_len`, `n_ctx`, `context_length`, or `context_window` from the
   model's own row. Ollama answers `/api/show`, and `fetch_context` takes the
   `model_info` key that ends with `context_length`. Anthropic and Google
   report no window, so those providers need the override. The estimator counts
   locally. `estimate_text` charges four
   characters per token. `estimate_tokens` charges each image part the flat
   `IMAGE_TOKENS` allowance of 1_200, not its base64 length. A row with no
   string content estimates from its serialized JSON. A tool-call row therefore
   costs what the model already paid. Each caller adds `tools_tokens`, the tool
   registry, which is byte-stable. When the stream reports `usage` with
   `prompt_tokens`, `llm.rs` floors the estimate with that count. Only the rows
   appended since that report get a guess.
2. **Fold to a target, and skip folds that free too little.** `target_tokens`
   returns three quarters of the threshold. A request trimmed to exactly the
   threshold is over the line again on the next tool result. Each fold costs one
   summarizer call. `a_fold_lands_under_the_threshold_with_room_to_grow` in
   `llm.rs` proves a fold lands under the threshold with headroom. The in-run
   path also drops a fold that frees less than `MIN_FOLD_SAVINGS_TOKENS`, which
   is 1_000 tokens. Measured on a 6k window, one fold traded a call for 259
   tokens.
3. **Cut where the wire allows.** Before a run, `plan_history` calls
   `turn_anchored_cut` on the persisted rows. The kept tail always begins at a
   user row, so that cut never splits a turn. That cut removes a prefix, so any
   instruction in the removed rows goes to the summarizer (rule 5). A run
   usually holds one turn, and that turn is a long tool loop. Anchoring to the
   start of that turn would keep everything. Inside a run, `middle_out_cut`
   removes the middle instead. It keeps the opening row when that row is a user
   row. `middle_out_cut` therefore walks the tail back over tool results, and it
   keeps each result with the call that produced it. When the newest turn alone
   exceeds the
   budget, `llm.rs` logs that and sends the oversized request.
4. **Prune before summarizing.** `prune_old_tool_bodies` rewrites the oldest
   `tool` bodies down to one line. It skips bodies under 200 characters. When a
   body already carries a spill pointer, the marker keeps it: `[output truncated
   — full text: <path>]`. Otherwise the marker reads `[older output elided — N
   chars]`. The pointer names a file under `~/.firstmate/output/out-*.txt`, so
   `read_file` still recovers the text. `prune_protect_tokens` protects the
   newest `KEEP_RECENT_TOKENS`, 20_000, capped at half the threshold. The cap
   leaves a small-window model an older half to prune. The pass skips
   `load_skill` and `read_skill_resource` results, because those bodies are the
   instructions for gated tools. `prune_savings_floor` returns the smaller of
   `MIN_PRUNE_SAVINGS_TOKENS`, 2_000, and a quarter of the threshold. A fixed
   2_000 floor is unreachable on a 6k model. The pass computes every
   marker first and applies none until the total clears that floor. A pass that
   frees too little leaves the transcript untouched, because it would still cost
   the same cache miss. Pruning makes no model call.
5. **Summarize the dropped rows, and keep the instruction in hand.** The brief
   is framing, not a turn, so it folds into the system prompt. `summary_into`
   builds that prompt before the request, and `set_summary` rewrites the system
   row inside a run. `summary_into` replaces the whole summary section, so the
   new brief replaces any earlier brief instead of stacking. `pinned_opening`
   gives every fold the conversation's first user row verbatim. `pinned_opening`
   returns nothing when the dropped rows already carry that row. The in-run cut
   keeps that first row on the wire. The dropped middle therefore contains no
   instruction at
   all. A summarizer cannot copy a line the fold never included. One shipped
   transcript's brief read "user request: not explicit in transcript". Without
   the pinned row, a later fold sees only the previous brief. That fold
   paraphrases the request again, and the request drifts. `skills::prompt_content`
   loads the brief rules from `prompts/COMPACT.md`, the same lookup `AGENTS.md`
   uses, so the memory policy stays editable content. The summarizer request
   carries those rules as its system row and the dropped rows as its user row.
   The redaction pass runs on those rows first, because that call is an outbound
   send to the same endpoint (docs/roadmap/REDACTION.md).
   `brief_transcript` caps that transcript at 60_000 characters: it keeps the
   first three quarters and the last quarter, and marks the gap with
   `[...middle of the dropped transcript omitted...]`. `brief_clears_gate` scores
   the brief, because the gate never scores the system prompt. A flagged brief
   stops the fold, so no unjudged brief enters that prompt. An
   unreachable scorer allows the fold, because an outage must not break
   compaction. With no `COMPACT.md` at any layer, `llm.rs` skips the fold and
   logs that choice. The console keeps its full scrollback, and one divider row
   marks the fold. The divider reads `First Mate summarized the earlier chat so
   it could keep going`, and its tooltip names how many rows the fold replaced.
   The record stores the fold, with the rows it dropped, and
   `load_conversation` returns that divider as a `summary` row.
   `plan_history` starts after the newest `summary` row, so no fold re-reads rows
   an earlier fold covered. One request build makes at most one summarizer call.
6. **Re-inject loaded skill bodies at request build.** `ChatView.vue` rebuilds
   the loaded set from the `load_skill` rows in the transcript and passes it as
   `loadedSkills`. `system_prompt_with_skills` appends each loaded skill's
   `SKILL.md` body to the system prompt. That append runs once per run, before
   the first request, because a mutated prompt costs a fresh prefill. That
   re-injection fixes the resume gap independently of compaction. That append
   closes the gap again after a fold.

## Explicitly not

- **No image-lifetime policy.** Dropping older screenshots breaks before/after
  comparison, and before/after comparison is what a UI-driving agent must do.
  Dropping older screenshots frees no space that the cache does not already
  free.
- **No provider-native compaction.** That path needs OpenAI Responses or the
  Anthropic beta. The local llama.cpp has neither.
- Compaction does not build bitmap archives of the history.
- Compaction does not write branch summaries.
- Compaction does not pre-summarize speculatively or asynchronously.
- **No per-model `__%` yet.** That percentage setting is global. Per-model
  config arrives with multi-model selection and fallbacks.

## Open

- **Watch the mid-run folds.** Mid-loop compaction ships (rule 3). A long tool
  loop is the common case. Compaction inside the run keeps that loop in the
  window. Watch whether folds drop tool rounds the model still needed.
- **No mid-run compaction on the lmkit path.** `chat_via_lmkit` folds only
  before the request, so a long Anthropic or Gemini run can outgrow the window
  turn by turn.
- **Keep-newest budget.** `KEEP_RECENT_TOKENS` is 20_000 tokens. Confirm that
  value against the models First Mate actually points at.
- **Stale spill pointers.** Startup calls `trim_dir(&fm_out_dir(), 50)`, which
  keeps the 50 newest files. A spill pointer quoted in an old conversation can
  therefore point at a file that no longer exists. Decide whether compaction
  protects referenced spill files or accepts that loss.
