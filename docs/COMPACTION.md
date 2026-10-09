# Context Compaction — Design

This document is the design detail for roadmap §6 (Phase 0). Every `§`
reference below points to `docs/ROADMAP.md`.

**Problem.** The agent forgets the task it was given. The agent also forgets
the rules it agreed to follow. Both failures are window-management bugs, not
model bugs. Both failures are measurable on a normal day of use.

## Evidence (2026-10-09, 161 provider requests)
- The history window counts **rows, not tokens**. `ChatView.vue` filters to
  user/assistant/tool rows, then ships `messages.value.slice(-30)`. On a
  166-item drivers chat the last 30 rows held **zero user turns**. That request
  carried "no user turn at all". 5 of 12 chat invocations hit the 30-row cap.
- Request size is median 47.9 KB, p90 783 KB, max 1.46 MB. Size is **not** a
  cost or latency problem. Requests over 900 KB were the _fastest_: median 2 s,
  against 3 s for requests under 60 KB. The log has no 413 error and no
  context-overflow error. The request layout is append-only, so the prefix is
  byte-stable and provider-cached. That prefix is the system prompt, the
  history, and the screenshots.
- Screenshots are the bulk of those bytes. The log has 42 attach events. One
  461 KB PNG became a 664 KB request. Screenshots are vision **tokens in the
  window**, so they matter for the trigger, not for cost. Screenshots are also
  dropped entirely at the turn boundary. `prior_history` is text-only, so
  visual memory ends when the user speaks.
- Nothing knows a model's window. `providers.json` carries no window field.
  `modelapi.rs` discards the `max_model_len` that the endpoint already reports.
  `llm.rs` never parses `usage` or `prompt_tokens`.
- `load_skill` returns the whole `SKILL.md` (`execute.rs`), but `tool_report`
  attaches no body for that call. The `.chat` ledger therefore keeps only
  `📖 load_skill filesystem ✅`. Resume re-derives capability from that ledger
  line, so gated tools unlock **without their instructions in context**.

## The rule that governs everything else

Compaction **invalidates the prefix cache**. Rewriting history changes the
prefix, so the next request re-processes the whole prefix. The prefix therefore
stays byte-stable by default. Compaction fires only when the window forces a
fold. Old tool output is pruned only when the cache is already cold, or when
the trailing suffix is small. A cold cache means the endpoint has been idle
past the provider's TTL. Every compaction causes one cache miss. The freed
space must be space that the run actually needs.

## Mechanism

`src-tauri/src/context.rs` holds the arithmetic. Both chat paths in `llm.rs`
apply that arithmetic the same way. Each path applies the arithmetic once before
the request is built, and again between turns inside a run.

1. **Unit is tokens, never rows.** The trigger is the `Compact at __% context`
   setting, which is global and defaults to 85%. The window comes first from the
   user's override. Otherwise the endpoint's report supplies the window:
   `max_model_len`, or `model_info` on Ollama. That report is cached per
   endpoint plus model. Otherwise the window is `DEFAULT_WINDOW_TOKENS`. Token
   estimates are local. Text is counted at chars/4. Image parts are counted
   at a flat `IMAGE_TOKENS` allowance, not at their base64 length. The
   estimate also adds the tool-registry overhead, which is byte-stable. When
   the endpoint reports `usage.prompt_tokens` on the stream, that count floors
   the estimate. Only the rows appended since that report are guesses.
2. **Fold to a target, and skip folds that free too little.** `target_tokens` is
   75% of the threshold. A request trimmed to exactly the threshold is over the
   line again on the next tool result. Each fold requires one extra summarizer
   call. Before this rule existed, a 12k window at 60% folded four times in one
   run. A fold that frees less than `MIN_FOLD_SAVINGS_TOKENS` is skipped and
   logged rather than run. One such fold on a 6k window freed only 259 tokens.
3. **Cut where the wire allows.** Before a run, `turn_anchored_cut` cuts the
   persisted ledger at a turn boundary. The kept tail always begins at a user
   row, so a turn is never split. That cut removes a prefix, so any instruction
   in the removed rows goes to the summarizer (rule 5). Inside a run there is
   usually one turn, and that turn is a long tool loop. Anchoring to the start
   of that turn would keep everything. Inside a run, `middle_out_cut` removes
   the middle instead. The kept rows are the opening row plus the newest rows
   that fit. Whole tool rounds are removed between them, so no `tool` row is
   orphaned from the call that produced it.
4. **Prune before summarizing.** The oldest tool-output bodies are elided down
   to their existing spill pointer at `~/.firstmate/output/out-*.txt`.
   The newest `prune_protect_tokens` are protected. That budget is 20k, capped
   at half the threshold. The cap leaves a small-window model an older half
   to prune. `load_skill` and `read_skill_resource` bodies are never touched.
   Markers are computed first, and applied only when the freed space clears
   `MIN_PRUNE_SAVINGS_TOKENS`. A pass that frees too little leaves the transcript
   untouched, because it would still cause the same cache miss. That pass does
   not elide rows and then report nothing. Pruning makes no model call.
5. **Summarize the dropped rows, and keep the instruction in hand.** The brief
   is framing, not a turn. The brief therefore folds into the system prompt
   through `set_summary`. The new brief replaces any earlier brief instead of
   stacking. Every fold is handed the conversation's first user row verbatim
   (`pinned_opening`). The exception is a fold whose rows already carry that
   row. The in-run cut keeps that first row on the wire, so the dropped
   middle holds no instruction at all. A summarizer cannot copy a line that
   was never shown to it. One shipped transcript's brief read "user request:
   not explicit in transcript". Without the pinned row, a later fold would see
   only the previous brief. That fold would paraphrase the request until the
   request drifted. What the summarizer is told is content:
   `prompts/COMPACT.md`, layered like `AGENTS.md`. With no copy at any layer,
   the fold is skipped and logged rather than run on compiled-in prose. The
   console keeps its full scrollback, and an inline divider marks the fold.
   The divider is worded for the reader: "summarized the earlier chat so it
   could keep going". The tooltip carries the row counts. The persisted
   `summary` row is where the next request resumes. A 166-row ledger therefore
   needs one summarizer call. That ledger never re-folds rows that an earlier
   fold already covered.
6. **Re-inject loaded skill bodies at request build.** The loaded set is rebuilt
   from the ledger. Each loaded skill's `SKILL.md` is then put back in the
   request. That fixes the resume gap independently of compaction. The same
   re-injection keeps the gap closed after a compaction.

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
  default against the models we actually point at.
- **Stale spill pointers.** Startup keeps only the 50 newest files in
  `fm_out_dir`. A spill pointer quoted in an old conversation can therefore
  point at a file that no longer exists. Decide whether compaction protects
  referenced spill files or accepts that loss.
