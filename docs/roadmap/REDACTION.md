# Sensitive-Info Redaction — Design

`REDACTION.md` is the design detail for roadmap §3 (Phase A). The `§`
references below point to `docs/ROADMAP.md`.

**Problem.** A public model must never see credentials, addresses, health data,
or financial data. The endpoint address does not decide model exposure. An agent
on the same LAN is a receiver you may not trust. A hosted model behind your own
proxy is a receiver you trust. One rule drives the feature: when the user checks
`Hide sensitive information` for the selected provider, run the redaction pass.
Detection alone cannot guarantee redaction for every class of value. The next
section states why.

## Two classes of sensitive value

Class 1 values carry proof inside the value text. A specification gives each
Class 1 value a shape and a checksum. The text alone decides whether a Class 1
value is present. The Class 1 set stays closed. `redact.rs` implements the Class
set as Layer 1 below.

For a class-2 value, the deciding fact sits outside the text. The deciding fact
names the owner of the value. The deciding fact names the origin of the text or
the meaning of the sentence. The string therefore holds no evidence a scan can
recover. A US account number and an order ID of the same length are the same
string.

**Detection cannot reach completeness for class 2.** A detector must exist
before the leak. The detector must guess your private context. Nobody records a
missed value, so the recall of a detector stays unmeasurable. An unmeasurable
quantity cannot improve. Completeness is therefore not a target the project can
hold.

Order class 2 controls in one sequence: containment, then mitigation, then
ad-hoc identification. Containment bounds what can leave the machine, so an
undetected value has nowhere to go. The containment controls must hold even when
the gate cannot name a sensitive value. Identification is the control asking
whether a value is sensitive. Identification inherits the same recall limit as
detection. Mitigation assumes a value already left. Mitigation then asks what
the leaked value is worth to the receiver. The questions are how long the
receiver keeps the value, and whether the receiver can reuse the value. The last
question is how many receivers already hold the value. Ad-hoc identification
then corrects the system from what you observed. Containment and mitigation
bound the misses left for ad-hoc identification.

Per channel, containment can reach leak-free operation without prediction. A
value cannot leave the machine until you clear the value. No leak follows,
whatever the shape of the value. Detection-based completeness stays impossible.
Containment gives the guarantee in the place where prediction is unnecessary.

`REDACTION.md` holds three designs: containment, mitigation, and one ad-hoc
identification design.

## The switch

- The model settings form shows one checkbox labeled `Hide sensitive
  information` for the selected provider.
- Settings stores the checkbox state per provider id, because the stored `llm`
  block holds one provider at a time. The Settings UI restores each provider's
  own choice when the user switches providers.
- Each roster row declares the start value of the row in `hideSensitiveDefault`.
  The three on-machine engines start unchecked. Every hosted row and every
  user-added URL starts checked, because a hosted receiver exists.
- `Spec::is_local` keeps one job: `Spec::is_local` shows the Base URL field. The
  endpoint address never selects the redaction pass.

## Core round-trip

- A redaction pass runs over every outbound request whose provider has
  `hide_sensitive` checked. The pass detects secrets, replaces each secret with
  a reversible placeholder such as `⟦EMAIL_1⟧`, and sends the request.
- The pass runs on a copy of the conversation. The stored transcript keeps real
  values. Click-to-reveal needs the real value on disk, and the §4 `blur` and
  `hello` levels offer click-to-reveal. The token map below outlives the process,
  so a marker in a stored row still resolves after a restart.
- Detokenize happens at three harness boundaries, never in the model.
  - **Display**: the console renders the real value, or obfuscates the real
    value under the §4 `Hide on screen` axis.
  - **Tool/action execute**: a tool call such as "email `⟦EMAIL_1⟧` the
    summary" contains a token. The harness substitutes the real address at
    execute, under the profile's trust tier. The harness refuses a call whose
    token the map cannot restore. The refusal text names the argument, not the
    token, because the model reuses a quoted token.
  - **Tool output inbound**: the pass redacts exec output and file output before
    the next request carries the output to the provider. Redaction and
    detokenize
    cover both directions.

### The token map

One table maps every marker to the value behind that marker. `redact.rs` holds
that table in one `Table` struct: `values`, `tokens`, `repairs`, and `counters`.
One `Store` wraps the table in `Arc<Mutex<Table>>`. `static SHARED: OnceLock<Store>`
makes that store process-wide. `Redactor::new()` takes that store, so every
conversation and every provider shares one table.

`lib.rs` calls `redact::init` in the `setup` closure, right after `crypto::init`,
with `config_dir.join(crypto::TOKEN_MAP)`. `crypto::TOKEN_MAP` is
`token_map.json`, so the table file is at
`%APPDATA%\com.dusti.firstmate\token_map.json`, beside `settings.json` and
`entity_registry.json`.

- `Table::load` opens `token_map.json` at startup through `crate::crypto::open`.
- `Store::commit` serializes the table to JSON and seals that JSON with
  `crate::crypto::seal`. `Store::commit` writes that file whenever the table
  gains a mapping, so a crash cannot lose a marker a provider saw.
- `crypto::upgrade_stores` lists `TOKEN_MAP`, so the next start seals a plaintext
  copy an older run left.
- `crypto::open_store_bytes` refuses `token_map.json` by file name, so `read_file`
  returns the sealed bytes. `read_file` never hands the marker table to the
  model. A sealed spill file in the same directory still opens.
- The table is global across conversations, so a marker seen in one conversation
  resolves in another conversation. Per-class counters are global too, so one
  value keeps one marker across runs.

A per-run table restarts each per-class counter at 1, so one value carries a
different marker in each run. A marker that no live table issued stays in the
stored text as written. No later run can resolve that marker. One persisted
table gives one marker per value across runs. A reopened conversation therefore
resolves the markers that conversation stored.

Three tests hold that design:
`crypto::tests::a_store_read_never_opens_the_token_map`,
`redact::tests::a_token_from_an_earlier_run_resolves_from_the_sealed_file`, and
`redact::tests::the_stored_table_holds_no_plaintext_value`.

## Detection layers

**Layer 1 — shape + checksum (deterministic, always on).** Financial
identifiers embed a checksum in the digits of the identifier. Layer 1 therefore
detects each such identifier exactly, and Layer 1 never guesses. Layer 1 detects
these values:

- A card number matches Luhn plus BIN network prefixes.
- An IBAN matches the ISO 13616 per-country length table plus mod-97.
- An ABA routing number matches mod-10 weights 3-7-1.
- A SWIFT/BIC code matches the ISO 9362 format.
- An SSN matches position-dependent validity rules.
- A crypto address embeds a base58 or bech32 checksum.
- A secret matches an API-key shape or a JWT.
- An email address or a phone number matches a fixed shape.
- A Windows path under `Users\<name>` matches the user-profile root.

**Layer 2 — entity registry (closed set).** A US account number has no standard
and no checksum. The number is 8 to 17 digits long. The shape of a US account
number is indistinguishable from an order ID. Layer 2 therefore needs no
open-world detection, because the valuable targets are the user's own values.
The Layer 2 target set is tiny and closed.

- `entity_registry.json` beside `settings.json` holds the values. Phase B gives
  the Profiles UI the write path; today you edit the file.
- The registry grows from Layer 1 and Layer 3 hits after a one-time user
  confirmation.
- The registry then matches exact values anywhere, in any format.
- `docs/ENCRYPTION.md` records the envelope. First Mate encrypts the entity
  registry like every other store.

**Layer 3 — provenance.** Content from a marked origin is sensitive regardless
of the shape of the content. Layer 3 marks the following origins:

- A file read under a declared sensitive path is sensitive.
- A UIA field named "Account No." is sensitive. A UIA field whose `AutomationId`
  matches "Account No." is also sensitive. The winapp harness already reads UIA.
- A page from an online-banking browser origin is sensitive.
- Layer 3 never allows the password-manager clipboard.

A number from the bank's own web app needs no checksum, because provenance marks
the number sensitive.

**Layers 4–5 — context + local model net.** Layer 4 combines two weak signals.
The signals are trigger-word proximity ("acct", "routing", "balance") and a
digit-length window. Layer 4 fires only when both signals match. Layer 5 is an
optional small local NER/LLM pass with an ollama binding. Layer 5 catches
shapeless entities such as names, employer, and health. Layer 5 runs on a local
model, because the scan itself cannot leak.

## Failure classes, handled explicitly

- The model **mangles** a token. A fuzzy-repair pass repairs the token from the
  distinctive `⟦…⟧` shape.
- The model must **reason over** the real value, as in "is the IBAN valid?" The
  model cannot do the reasoning. The harness routes the turn to a local-model
  profile, or applies a per-field disclose-once override.
- An action **sends** a real value out-of-band, as with web search or email.
  The trust-tier-3 confirm gate at the leaving-the-machine boundary handles the
  send. The send is not a redaction bug.

## Bias rule

The bias is to over-redact outbound traffic on a turn whose provider has the
checkbox checked. Detokenize restores the real value for display and for
actions. A false positive therefore removes a token, and detokenize restores
the token before the user sees the value. A false negative is the leak.
Per-class toggles and the disclose-once override guard against over-redaction
degrading model task quality.

## On-disk truth and the console indicator

How much real text the chat log stores is a choice in the §4 Privacy section.
The store-only-redacted-text option minimizes on-disk truth. The `blur` and
`hello` levels need the real text on disk. The `blur` and `hello` levels
therefore need the envelope described in `docs/ENCRYPTION.md`. The console can
therefore reveal the stored truth on demand.

The console shows a "Redacted N items" chip on each turn whose pass replaced a
value. On a chip click, the console lists the fired classes and never shows the
values.

## Containment (class 2)

A decision server scores each span before the span leaves the machine. The gate
runs on the outbound copy of the conversation, because a leak needs a path the
gate crosses. The gate scores each span once per run. Any tool output reaching
no request gets no score, so no leak follows. The server answers typed
questions with probabilities, and the server returns no text.

First Mate ships no weights and starts no server. You run the server yourself,
with any model you choose. Settings holds the server URL. The app is a client of
the Jev-compatible typed-decision API, so any server answering the API can
drive the gate:

```text
POST {base}/v1/systemone
{"state": "<the span>",
 "questions": {"sensitive": {"type": "noul",
                             "instructions": "<the question>",
                             "criteria": {"true": "yes", "false": "no"}}}}

{"answers": {"sensitive": {"noul": 0.87}}}
```

- The URL and the enable box start off. With either control off, the app runs the
  exact rules alone. The Settings UI shows the state of the two controls.
- The scoring request carries the span text, so a remote URL sends the very text
  the gate protects. The app accepts a loopback URL without a prompt. The app
  asks for confirmation before sending a span to a non-loopback host.
- The same endpoint answers every later decision question, so one server serves
  containment now and detection later.
- `decide.rs` is the client. `redact.rs` holds the `SPAN` class and the
  scored-span set. `gate` in `llm.rs` runs the pass before the exact rules.
- A scorer outage clears nothing. When the endpoint is unreachable, the app
  sends the request on the exact rules alone. The app writes one `GATE:` log
  line with the failing address. The app leaves each unscored span unscored, so
  the next turn asks the server again.
- The gate denies by default, so the gate needs no knowledge of the identity of
  a sensitive value. The deny-by-default answer is the definition of containment
  in this document.
- At the input boundary the gate redacts rather than blocks. If the gate blocked
  the input, the agent could not work on the file at all. Detokenize at execute
  already gives each tool the real value.
- Settings holds the flag probability, and `Test` reports the two scores from the
  configured server. You pick a value between the two reported scores, because
  each model needs its own measurement. On the measured 4B, 0.50 gives recall
  0.95 at a 12.5 percent audit rate. The 0.70 setting gives recall 0.80 at
  2.5 percent.
- `Test` in Settings sends the probe: one benign span, and one span holding a
  credential. When both probe answers land at or above 0.8, the app reports the
  server as unusable. H2O's shim ships a 0.801 yes/no floor, and a floored
  server flags every span.
- The gate scores each unscored string of the outbound copy, and never re-scores
  a whole window. The agent loop re-sends the whole window on each tool turn. A
  full re-score therefore repeats one audit many times in one task.
  `chat_via_openai` and `chat_via_lmkit` each clone the outbound body for the
  redaction pass, so scoring needs no second copy.
- One request scores at most 16 spans. The gate skips every span shorter than 60
  characters. A first turn over a large file cannot spend the whole latency
  budget of the turn on scoring.
- One request scores the newest messages first. The sentence the user just typed
  cannot lose the scoring budget to restored history.
- The gate scores no authored text. `prompts/AGENTS.md` and each loaded
  `SKILL.md` form the system message. You review the authored text before the
  authored text ships. A false positive in the system message replaces the whole
  instruction set with one token. Such a false positive breaks the task and
  protects nothing.
- The gate scores assistant text, tool output, and tool-call arguments. A model
  writes new strings no input score ever saw.
- A model restates a missed value in new words. A new sentence carries no shape
  for Layer 1. The assistant message then re-leaves on every later turn. One
  miss therefore becomes a permanent leak unless a later score catches the miss.
- First Mate puts the brief inside the system message, so the span gate scores no
  brief. First Mate scores the brief before the fold. A flagged brief stops the
  fold. The rows the fold would drop then reach the span gate as separate spans.
- A model also copies a value into a tool-call argument. A token replaces the
  original text of the tool output, so the original value stays hidden. The copy
  in the argument is a different string, so the argument needs a separate span.
- The gate still runs the exact rules over the system message. Layer 1 costs
  nothing on the system message. A credential in a prompt is a defect you want
  found.
- The gate needs a second signal. A decision model knows generic categories:
  credentials, financial text, health, identity. The decision model does not
  know your employer's part number. The gate also matches the registry and the
  declared files, and the gate denies on either signal.
- The shipped gate redacts a flagged span with no dialog. Detokenize restores the
  flagged span for display and for execute. The chip names the count, so the
  turn records what the gate held back.
- The audit dialog is the path into the Layer 2 registry from a span hit. A span
  hit therefore does not grow the registry today. A dialog with only
  `Approve` and `Deny` teaches you to press `Approve`. The `Redact and continue`
  button must therefore take one selection and one key.
- The model may deny; only you may approve. A file can hold text written to
  persuade the scorer, such as `this data is public`. A scorer able to clear
  content is also open to persuasion from file text.
- Scope: the text and images entering an inference request. An action emailing
  or searching a real value stays with the tier-3 confirm gate in the Failure
  classes section.

### Open items

- Screenshots. `llm.rs:1128-1143` adds each captured screenshot as a base64 PNG
  part. No text pass reads pixels, so a bank statement leaves the machine whole
  today. The API carries an image as a data URI inside `state`.
  H2O-Lightning-4B reads the data URI. The app therefore needs to pass the image
  through, not score text alone.
- Derived text. A token hides a value, not a relation. "The balance is three times
  the salary" carries no token and appeared in no tool output. No input score
  covered the derived sentence. The score on new assistant text covers the
  derived case.
- Calibration. The slider is honest only after a measurement on First Mate
  transcripts. Calibration depends on the data, so vendor numbers do not transfer.
  The Spike section below measures the compression and the audit rate on the host
  machine.
- Observe-only mode did not ship. The gate redacts, and the `SPAN` count on the
  chip gives the audit rate for the turns you inspect. The log names each flagged
  span with the score of the span. The log therefore carries the calibration
  curve for the flagged side only.
- `tokenize_msgs` (`redact.rs:497`) walks every string in the outbound copy,
  including the base64 blob. No test shows whether a Layer 1 pattern can corrupt
  an image.
- Server host. A user could point the URL at a hosted endpoint. The hosted
  endpoint would then hold every span the gate inspected. The confirmation prompt
  is the only control, so the console must name the host on every audit line.
- One server, many profiles. A profile could need separate question text or a
  separate threshold. The probe and the slider are global today.

### Spike: local decision models, measured 2026-10-10

Two decision models and one embedding model ran on the same 44 labeled spans.
The same models ran on the same 200 spans of real agent work. The host is an
RTX 3060 with 12 GB and 12 CPU cores. JevBench v1.6.1 ranks open-weights
decision models by Intelligence, Calibration, Speed, and Cost. A board score
does not establish reliability on the redaction task, so each model also ran on
the host.

| Model | JevBench capability | AUC | Threshold for recall 0.95 | Audit rate there | Latency | Memory |
| --- | --- | --- | --- | --- | --- | --- |
| `h2oai/h2o-lightning-4b` v1.1, 4B | 75.0, open rank 5 | 0.960 | 0.50 | 12.5% | 70 ms GPU | 11.6 GB VRAM, 5.4 GB RSS |
| `StrandsAgents/strands-decider-2B`, 2B | unranked | 0.891 | 0.10 | 98.5% | 109 ms GPU, 1767 ms CPU | 6 GB VRAM, 9 GB CPU RSS |
| `all-MiniLM-L6-v2`, 23M, kNN | not a decision model | 0.735 | none | 0%, misses | 59 ms CPU | 1.0 GB RSS |

H2O-Lightning-4B is Apache-2.0, 9.1 GB, served by vLLM 0.30.0 with the shim flag
`SHIM_NOUL_FLOOR=0`. The shipped config floors every yes/no answer at 0.801. A
0.801 floor flags every span, so the floor must stay off before a threshold
means anything.

- Positives median 0.862, negatives median 0.248. Threshold 0.50 gives recall 0.95
  with 5 false positives of 24.
- On real work the mean is 0.306 and the max is 0.774. Threshold 0.50 flags 25 of
  200 spans. Threshold 0.70 flags 5 of 200, at recall 0.80.
- The same two error classes remain, narrower. The weakest positives are private
  prose: 0.473, 0.531, 0.555. The strongest negatives are examples of secrets:
  0.760 and 0.721.
- The GPU budget is the blocker. The server holds 11.6 GB of the 12 GB card and
  5.4 GB of host RSS. A machine without a large GPU cannot host the server, and a
  CPU host is out of reach.

Strands Decider 2B is Apache-2.0, 4.55 GB, and is not on the board.

- The scores of Strands Decider 2B compress: positives median 0.238, negatives
  median 0.092, floor 0.070. On real work no span reached 0.5, so no threshold
  separates the classes. At threshold 0.15 the audit rate is 44.7 percent. At
  threshold 0.10 the audit rate is 98.5 percent.
- On CPU the 2B model holds AUC 0.887 at 1767 ms per span and about 9 GB RSS.
  Warm-cache load takes 17.7 s.

The embedding model scores a span by similarity to labeled exemplars. The four
lowest embedding scores are the four private prose facts. An embedding therefore
carries the topic of a span, not whether the topic is private.

- Model choice, not the model class, caused the first failure. The ranked 4B gives a
  usable operating point at a 12.5 percent audit rate.
- The registry and the declared paths still decide the exact cases. The 4B ranks
  an example SSN above three real private facts. The 2B model scored the internal
  SKU scheme low in the 2B run.
- First Mate ships the client, not the server, so the hardware budget is yours.
  The numbers above state what a 4B costs and what a 4B catches. The probe
  records the same two numbers for the server you run.
- Nothing under 4B on the board reaches capability 55. The 270M Gemma clone stayed
  untested, because Hugging Face gates Gemma 3 270M behind a login.

## Mitigation (class 2)

Mitigation starts after a value left the machine. The receiver controls
retention. No provider offers a control for deleting one string from the
provider log. The app therefore offers four small controls and promises nothing
beyond the four controls.

- Stop the re-send. `ChatView.vue:533` sends the whole transcript on each turn, so
  one leaked value leaves again on every turn. The `<!>` mark on the turn tells you
  to start a new conversation or to switch the provider.
- Name the one action for the value class. Rotation works whatever the receiver
  does, and no other mitigation does.

  | Value | Rotatable | Action you can take |
  | --- | --- | --- |
  | Password, API key, session token | Yes | Rotate the value. |
  | Card number | Yes | Cancel the card and take a new one. |
  | Bank account number | No | Monitoring: alerts from the bank. |
  | SSN | No | Monitoring: credit freeze and fraud alerts. |
  | Health fact, employer, salary | No | No action exists. |

- Count the receivers. The same transcript sent to three providers is three
  exposures. Settings shows the provider list for each conversation.
- Write a leak report.

### Leak report

The leak report is the artifact policy and compliance read. First Mate's work
ends at the leak report.

- Each row names the class and never the value. A report with the value would
  become a second store for the secret. The chip in "On-disk truth and the
  console indicator" follows the same rule.
- Each row holds the time, the provider id, and the Base URL. Each row holds the
  model, the class, and the count. Each row holds the conversation id and the row
  id. Each row holds the tool behind the span. Each row holds the retention terms
  of the provider at the moment of the send.
- The retention terms make the row compliance-grade. The row states the exposure
  window: 30 days, flagged and up to 2 years, zero retention, or unknown.
- Each row records your response: rotated, frozen, or ignored. Detection without
  action leaves the case open.
- The file is append-only JSONL, and First Mate encrypts it with the envelope
  in `docs/ENCRYPTION.md`. The report names each file holding sensitive content,
  even with no values inside.
- The app must export the report. A report you cannot hand to a lawyer or a bank
  is not evidence.
- Scope: the report covers sends First Mate made. The report cannot cover a leak
  by another route. The report cannot confirm deletion by a receiver.
- The report inherits the gate's recall. The report records what the gate saw, so
  the observe-only measurement and the calibration curve come first.

### Provider terms that bound mitigation

- OpenAI does not use API data to train models, unless you opt in.
  Abuse-monitoring logs last up to 30 days. See
  `https://developers.openai.com/api/docs/guides/your-data`.
- Anthropic keeps no prompt content by default, except Covered Models at 30 days. A
  flagged chat may last 2 years even under zero retention. See
  `https://platform.claude.com/docs/en/manage-claude/api-and-data-retention`.
- Zero Data Retention needs a sales arrangement, per organization. An individual
  cannot obtain Zero Data Retention. The retention tail for a normal user is 30
  days, or 2 years when the provider flags the content.

## Ad-hoc identification (class 2, future)

You notice a plaintext value in the chat log. You select the value, open the
context menu, and choose `Redact`. The gesture can work where a pattern set
cannot, because you know the value. The log is evidence, not a guess.

- The rule targets the value, not the conversation. A value in one log usually
  sits in other logs too. The same value sits in the file the agent read, and in
  the file itself. A per-conversation rule closes one path of four.
- The gesture is the write path Layer 2 needs, so the registry grows from a leak
  you observed. You need no upfront configuration, because nobody writes such a
  list in advance.
- The action cannot retract what already left. The console keeps a persistent
  `<!>` marker on the turn where the value first went out. The marker states the
  value reached the provider before the rule existed. The word `Redact` alone
  would imply an undone exposure.
- The pass edits the outbound copy only, and the pass re-derives the copy per
  request. The stored transcript keeps real values, per the Core round-trip rule.
  A rule in stored rows fails on resume against another provider, because the
  stored rows then carry plaintext again.
- You never write a pattern. The UI collects the forms you observed. The model
  writes a pattern matching every collected form. The limit is real: the gesture
  fits numbers and codes. Prose-shaped sensitivity, such as a name or a
  diagnosis, is out of reach.
- The rule list shows each rule, an off switch, and the rows the rule touched on
  the last request. A broad pattern on an 8-digit value can also match order IDs.
  A silent no-match and a silent over-match both destroy your trust in the
  control.
- Provider-side retention is out of reach. Once a request arrived, only the
  receiver's own policy decides what survives. The provider-retention case
  belongs to mitigation.

**Dependency:** Profile model bindings (§1) decide the provider of each turn. The
checkbox of the selected provider decides whether the pass runs. Encryption at
rest decides what is worth protecting on disk instead.
