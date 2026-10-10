# Context Compaction — Design

This document is the design detail for context compaction.

**Problem.** The agent forgets the task the user gave it. The agent also forgets
the rules it agreed to follow. Both failures are window-management bugs, not
model bugs. Both failures are measurable on a normal day of use.

## Evidence that forced the design (measured 2026-10-09, 161 provider requests)

These bullets record the state before the rules below shipped. Each bullet names
the rule that closed the gap.

- The history window counted **rows, not tokens**. `ChatView.vue` shipped
  `messages.value.slice(-30)`. On a 166-item drivers chat, the last 30 rows held
  **zero user turns**. That request reached the provider with no user turn at
  all. 5 of 12 chat invocations hit the 30-row cap. Rule 1 and rule 3 replaced
  that cap with a token threshold.
- Request size is median 47.9 KB, p90 783 KB, max 1.46 MB. Size is **not** a
  cost or latency problem. Requests over 900 KB were the _fastest_: median 2 s,
  against 3 s for requests under 60 KB. The log has no 413 error and no
  context-overflow error. The request layout is append-only, so the prefix is
  byte-stable and provider-cached. That prefix is the system prompt, the
  history, and the screenshots.
- Screenshots are the bulk of those bytes. The log has 42 attach events. One
  461 KB PNG became a 664 KB request. Screenshots are vision **tokens in the
  window**, so they matter for the trigger, not for cost. The next request
  contains no screenshot, because `HistoryItem` stores text only. Visual memory
  therefore ends when the user speaks.
- Nothing knew a model's window. `providers.json` defined no window field,
  `modelapi.rs` discarded the `max_model_len` the endpoint reported, and
  `llm.rs` parsed no `usage`. `model_context` plus `WINDOW_CACHE` in
  `modelapi.rs` now read that report, and `llm.rs` reads `/usage/prompt_tokens`.
  `providers.json` still defines no window field: rule 1 prefers the user
  override, then the endpoint report, then the fallback.
- `load_skill` returns the whole `SKILL.md` (`execute.rs`), but `tool_report`
  still attaches no body, so the `.chat` ledger stores only
  `📖 load_skill filesystem ✅`. Resume therefore re-derived capability from that
  ledger line, and gated tools unlocked **without their instructions in
  context**. Rule 6 re-injects each loaded body at request build, so the ledger
  shape no longer decides what the model sees.

## The rule that governs everything else

Compaction **invalidates the prefix cache**. Rewriting history changes the
prefix, so the next request re-processes the whole prefix. The prefix therefore
stays byte-stable by default. Compaction fires only when the window forces a
fold. Compaction prunes old tool output only when the cache is already cold, or
when the trailing suffix is small. A cold cache means the endpoint has been idle
past the provider's TTL. Every compaction causes one cache miss. The freed
space must be space that the run actually needs.

## Mechanism

`src-tauri/src/context.rs` defines the arithmetic. Both chat paths in `llm.rs`
apply that arithmetic the same way. Each path applies the arithmetic once before
it builds the request, and again between turns inside a run.

1. **Unit is tokens, never rows.** The trigger is the `Compact at __% context`
   setting, which is global and defaults to 85%. The window comes first from the
   user's override. Otherwise the endpoint's report supplies the window:
   `max_model_len`, or `model_info` on Ollama. `WINDOW_CACHE` in `modelapi.rs`
   caches that report per endpoint plus model. Otherwise the window is
   `DEFAULT_WINDOW_TOKENS`. The estimator counts locally: text at chars/4, and
   image parts at a flat `IMAGE_TOKENS` allowance, not at their base64 length.
   The estimate also adds the tool-registry overhead, which is byte-stable. When
   the endpoint reports `usage.prompt_tokens` on the stream, that count floors
   the estimate. The estimate guesses only the rows appended since that report.
2. **Fold to a target, and skip folds that free too little.** `target_tokens` is
   75% of the threshold. A request trimmed to exactly the threshold is over the
   line again on the next tool result. Each fold requires one extra summarizer
   call. Before this rule existed, a 12k window at 60% folded four times in one
   run. Compaction skips and logs a fold that frees less than
   `MIN_FOLD_SAVINGS_TOKENS`. One such fold on a 6k window freed only 259 tokens.
3. **Cut where the wire allows.** Before a run, `turn_anchored_cut` cuts the
   persisted ledger at a turn boundary. The kept tail always begins at a user
   row, so the cut never splits a turn. That cut removes a prefix, so any
   instruction in the removed rows goes to the summarizer (rule 5). Inside a run
   there is usually one turn, and that turn is a long tool loop. Anchoring to
   the start of that turn would keep everything. Inside a run, `middle_out_cut`
   removes the middle instead. The kept rows are the opening row plus the newest
   rows that fit. `middle_out_cut` removes whole tool rounds between those two
   groups, so no `tool` row loses the call that produced it.
4. **Prune before summarizing.** Pruning elides the oldest tool-output bodies
   down to their existing spill pointer at `~/.firstmate/output/out-*.txt`.
   Pruning protects the newest `prune_protect_tokens`. That budget is 20k, capped
   at half the threshold. The cap leaves a small-window model an older half
   to prune. Pruning never touches `load_skill` and `read_skill_resource`
   bodies. The pass computes the markers first, and applies the markers only
   when the freed space clears `MIN_PRUNE_SAVINGS_TOKENS`. A pass that frees too
   little leaves the transcript untouched, because it would still cause the same
   cache miss. That pass does
   not elide rows and then report nothing. Pruning makes no model call.
5. **Summarize the dropped rows, and keep the instruction in hand.** The brief
   is framing, not a turn. The brief therefore folds into the system prompt
   through `set_summary`. The new brief replaces any earlier brief instead of
   stacking. Every fold receives the conversation's first user row verbatim
   (`pinned_opening`). The exception is a fold whose rows already include that
   row. The in-run cut keeps that first row on the wire, so the dropped
   middle contains no instruction at all. A summarizer cannot copy a line that the
   fold did not include. One shipped transcript's brief read "user request:
   not explicit in transcript". Without the pinned row, a later fold would see
   only the previous brief. That fold would paraphrase the request until the
   request drifted. What the summarizer receives is content:
   `prompts/COMPACT.md`, layered like `AGENTS.md`. With no copy at any layer,
   compaction skips and logs the fold, instead of running on compiled-in prose.
   The console keeps its full scrollback, and an inline divider marks the fold.
   The divider is worded for the reader: "summarized the earlier chat so it
   could keep going". The tooltip shows the row counts. The persisted
   `summary` row is where the next request resumes. A 166-row ledger therefore
   needs one summarizer call. That ledger never re-folds rows that an earlier
   fold already covered.
6. **Re-inject loaded skill bodies at request build.** The console rebuilds the
   loaded set from the ledger rows, and `system_prompt_with_skills` appends each
   loaded skill's `SKILL.md` to the system prompt. That re-injection fixes the
   resume gap independently of compaction, and closes the gap again after a fold.

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

- **Mid-loop compaction ships (rule 3).** A long tool loop is the common case.
  Compaction only after each turn would let that loop overflow. Watch whether
  folds land on tool rounds the model still needed.
- **Keep-newest budget.** ~20k tokens is the reference default. Confirm that
  default against the models First Mate actually points at.
- **Stale spill pointers.** Startup deletes all but the 50 newest files in
  `fm_out_dir`. A spill pointer quoted in an old conversation can therefore
  point at a file that no longer exists. Decide whether compaction protects
  referenced spill files or accepts that loss.
