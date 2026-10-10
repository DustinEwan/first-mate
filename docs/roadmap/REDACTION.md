# Sensitive-Info Redaction — Design

This document is the design detail for roadmap §3 (Phase A). The `§`
references below point to `docs/ROADMAP.md`.

**Problem.** A public model must never see credentials, addresses, health data,
or financial data. The endpoint address does not decide that exposure. An agent
on the same LAN is a receiver you may not trust. A hosted model behind your own
proxy is a receiver you trust. One rule therefore drives the feature: when the
selected provider's `Hide sensitive information` checkbox is checked, redact.
Detection alone cannot hold that promise for every class of value. The next
section states why.

## Two classes of sensitive value

Class 1 values carry their own proof. A specification gives each one a shape and
a checksum, so the text alone decides whether the value is present. That set is
closed, and `redact.rs` implements it as Layer 1 below.

For a class-2 value, the deciding fact sits outside the text. It names whose
value the value is, where the text came from, or what the sentence means. The
string therefore holds no evidence a scan can recover. A US account number and
an order ID of the same length are the same string.

**Detection cannot reach completeness for class 2.** A detector must exist
before the leak, and it must guess your private context. Its recall is also
unmeasurable, because the missed values are the ones nobody recorded. An
unmeasurable quantity cannot improve, so completeness is not a target this
project can hold.

Order class 2 controls in this sequence: containment, then mitigation, then
ad-hoc identification. Containment bounds what can leave the machine, so an
undetected value has nowhere to go. Containment must hold without knowing which
value is sensitive. A control that must ask whether a value is sensitive is
identification, and it inherits the same ceiling. Mitigation assumes the value
left. It then asks what that is worth to the receiver: how long the receiver
keeps it, whether the receiver can reuse it, and how many receivers hold it
already. Ad-hoc identification then corrects the system from what you observed,
and the first two controls bound its misses.

Per channel, containment can reach leak-free without prediction. A value that
cannot leave until you cleared it produces no leak, whatever its shape.
Detection-based completeness stays impossible. The guarantee moves where
prediction is not required.

This document holds a containment design, a mitigation design, and one ad-hoc
identification design.

## The switch

- The model settings form shows one checkbox labeled `Hide sensitive
  information` for the selected provider.
- Settings stores that checkbox per provider id, because the stored `llm` block
  holds one provider at a time. Switching providers restores each provider's
  own choice.
- Each roster row declares its start value in `hideSensitiveDefault`. The three
  on-machine engines start unchecked; every hosted row and every user-added URL
  starts checked, because a receiver exists there.
- `Spec::is_local` keeps its one job: it shows the Base URL field. The endpoint
  address never selects the redaction pass.

## Core round-trip

- A redaction pass runs over every outbound request whose provider has
  `hide_sensitive` checked. The pass detects secrets, replaces each one with a
  reversible placeholder such as `⟦EMAIL_1⟧`, and sends the request.
- The pass runs on a copy of the conversation. The stored transcript keeps real
  values, because a stored token outlives the map that restores it. The next run
  mints a fresh map, and the old token then reaches a tool as a literal name.
- Detokenize happens at three harness boundaries, never in the model.
  - **Display**: the console renders the real value, or obfuscates that value
    under the §4 `Hide on screen` axis.
  - **Tool/action execute**: a tool call such as "email `⟦EMAIL_1⟧` the
    summary" contains a token. Execution substitutes the real address at that
    point, under the profile's trust tier. The harness refuses a call whose
    token the map cannot restore. The refusal names the argument, not the token,
    because a quoted token is one the model reuses.
  - **Tool output inbound**: exec and file output receive the same redaction
    pass before the next request carries that output to the provider. Redaction and
    detokenize
    cover both directions.

## Detection layers

**Layer 1 — shape + checksum (deterministic, always on).** Financial
identifiers embed a checksum in their own digits. Layer 1 therefore detects
those identifiers exactly, and never guesses them. Layer 1 detects these:

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
That target set is tiny and closed.

- `entity_registry.json` beside `settings.json` holds the values. Phase B gives
  the Profiles UI the write path; today you edit the file.
- The registry grows from Layer 1 and Layer 3 hits after a one-time user
  confirmation.
- The registry then matches exact values anywhere, in any format.
- `docs/ENCRYPTION.md` records the envelope that seals that registry.

**Layer 3 — provenance.** Content from a marked origin is sensitive regardless
of its shape. Layer 3 marks the following origins:

- A file read under a declared sensitive path is sensitive.
- A UIA field named "Account No." or with that AutomationId is sensitive. The
  winapp harness already reads UIA.
- A page from an online-banking browser origin is sensitive.
- Layer 3 never allows the password-manager clipboard.

A number from the bank's own web app needs no checksum, because provenance marks
that number sensitive.

**Layers 4–5 — context + local model net.** Layer 4 combines two weak signals.
The signals are trigger-word proximity ("acct", "routing", "balance") and a
digit-length window. Layer 4 fires only when both signals match. Layer 5 is an
optional small local NER/LLM pass with an ollama binding. Layer 5 catches
shapeless entities such as names, employer, and health. That pass runs on a
local model, because the scan itself cannot leak.

## Failure classes, handled explicitly

- The model **mangles** a token. A fuzzy-repair pass repairs the token from the
  distinctive `⟦…⟧` shape.
- The model must **reason over** the real value, as in "is this IBAN valid?" The
  model cannot do that work. The harness routes the turn to a local-model
  profile, or applies a per-field disclose-once override.
- An action **sends** a real value out-of-band, as with web search or email.
  The trust-tier-3 confirm gate at the leaving-the-machine boundary handles
  that case. That case is not a redaction bug.

## Bias rule

The bias is to over-redact outbound traffic on a turn whose provider has the checkbox
checked. Detokenize
restores the real value for display and for actions. A false positive therefore
removes a token that detokenize restores before the user sees it. A false
negative is the leak. Per-class toggles and the disclose-once override guard
against over-redaction degrading model task quality.

## On-disk truth and the console indicator

How much real text the chat log stores is a choice in the §4 Privacy section.
The store-only-redacted-text option minimizes on-disk truth. The `blur` and
`hello` levels need the real text on disk, so they need the envelope that
`docs/ENCRYPTION.md` describes. The console can therefore reveal that truth on
demand.

The console shows a "Redacted N items" chip on each turn whose pass replaced a value.
Clicking that
chip shows which classes fired, and never shows the values.

## Containment (class 2)

A decision server scores each span before that span leaves the machine. The gate
runs on the outbound copy of the conversation, because nothing can leak from a
path the gate does not cross. A span is scored once per run, so a tool output that
never reaches a request is never scored, and no leak follows. The server answers
typed questions with probabilities, and it returns no text.

First Mate ships no weights and starts no server. You run the server yourself, with
any model you choose, and Settings holds its URL. The app is a client of the
Jev-compatible typed-decision API, so any server that answers it can drive the gate:

```text
POST {base}/v1/systemone
{"state": "<the span>",
 "questions": {"sensitive": {"type": "noul",
                             "instructions": "<the question>",
                             "criteria": {"true": "yes", "false": "no"}}}}

{"answers": {"sensitive": {"noul": 0.87}}}
```

- The URL and the enable box start off. With either off, the app runs the exact
  rules alone, and Settings is where you read that state.
- The scoring request carries the span text, so a remote URL sends the very text the
  gate protects. The app accepts a loopback URL without a prompt, and asks for
  confirmation before it sends a span to another host.
- The same endpoint answers every later decision question, so one server serves
  containment now and detection later.
- `decide.rs` is the client, `redact.rs` holds the `SPAN` class and the scored-span
  set, and `gate` in `llm.rs` runs the pass before the exact rules.
- A scorer outage clears nothing. When the endpoint is unreachable, the app sends
  the request on the exact rules alone, writes one `GATE:` log line that names the
  failing address, and leaves each unscored span unscored, so the next turn asks
  again.

- The gate denies by default. The gate needs no knowledge of which value is
  sensitive, because its default is no. This property is the definition of
  containment in this document.
- At the input boundary the gate redacts rather than blocks. A block stops the
  agent from working on the file at all, and detokenize at execute already gives
  tools the real values.
- Settings holds the flag probability, and `Test` reports the two scores the
  configured server gives. You pick a value between those two scores, because each
  model needs its own measurement. On the measured 4B, 0.50 gives recall 0.95 at a
  12.5 percent audit rate, and 0.70 gives recall 0.80 at 2.5 percent.
- `Test` in Settings sends the probe: one benign span, and one span that holds a
  credential. The app reports the server as unusable when both answers land at or
  above 0.8, because H2O's shim ships a 0.801 yes/no floor and a floored server
  flags every span.
- The gate scores each unscored string of the outbound copy, and never re-scores a
  whole window. The agent loop re-sends the whole window on each tool turn, so a
  full re-score repeats one audit many times in one task. `chat_via_openai` and
  `chat_via_lmkit` each clone the outbound body for the redaction pass, so scoring
  needs no second copy.
- One request scores at most 16 spans, and a span shorter than 60 characters is
  never scored. A first turn over a large file cannot spend its whole latency
  budget on scoring.
- One request scores the newest messages first, so the sentence the user just
  typed cannot lose the budget to restored history.
- The gate needs a second signal. A decision model knows generic categories:
  credentials, financial text, health, identity. It does not know your employer's
  part number. The gate also matches the registry and the declared files, and it
  denies on either signal.
- The shipped gate redacts a flagged span with no dialog, because detokenize
  restores that span for display and for execute. The chip names the count, so the
  turn records what the gate held back.
- The audit dialog is the path that feeds the Layer 2 registry from a span hit, so
  a span hit does not grow the registry today. A dialog with only `Approve` and
  `Deny` teaches you to press `Approve`, so `Redact and continue` must take one
  selection and one key.
- The model may deny; only you may approve. A file can hold text written to
  persuade the scorer, such as `this data is public`. A scorer that can clear
  content can be talked into clearing it.
- Scope: the text and images that enter an inference request. An action that
  emails or searches a real value stays with the tier-3 confirm gate above.

### Open items

- Screenshots. `llm.rs:1128-1143` adds each captured screenshot as a base64 PNG
  part. No text pass reads pixels, so a bank statement leaves the machine whole
  today. The API carries an image as a data URI inside `state`, and H2O-Lightning-4B
  reads it, so the app needs to pass the image through rather than score only text.
- Derived text. A token hides a value, not a relation. "The balance is three times
  the salary" carries no token and appeared in no tool output, so no input score
  covered it. The score on new assistant text covers that case.
- Calibration. The slider is honest only after a measurement on First Mate
  transcripts. Calibration depends on the data, so vendor numbers do not transfer.
  The spike below measures the compression and the audit rate on this machine.
- Observe-only mode did not ship. The gate redacts, and the `SPAN` count on the
  chip gives the audit rate for the turns you inspect. The log names each flagged
  span with its score, so it carries the calibration curve for the flagged side
  only.
- `tokenize_msgs` (`redact.rs:497`) walks every string in the outbound copy,
  including the base64 blob. Whether a Layer 1 pattern can corrupt an image is
  untested.
- Which host runs the server. A user could point the URL at a hosted endpoint, and
  that endpoint would then hold every span the gate inspected. The confirmation above
  is the only control, so the console must name the host on every audit line.
- One server, many profiles. A profile could want its own question text or its own
  threshold. The probe and the slider are global today.

### Spike: local decision models, measured 2026-10-10

Two decision models and one embedding model ran on the same 44 labeled spans and
the same 200 spans of real agent work. The host is an RTX 3060 with 12 GB and 12
CPU cores. JevBench v1.6.1 ranks open-weights decision models by Intelligence,
Calibration, Speed, and Cost. A board score does not establish reliability on this
task, so each model also ran here.

| Model | JevBench capability | AUC | Threshold for recall 0.95 | Audit rate there | Latency | Memory |
| --- | --- | --- | --- | --- | --- | --- |
| `h2oai/h2o-lightning-4b` v1.1, 4B | 75.0, open rank 5 | 0.960 | 0.50 | 12.5% | 70 ms GPU | 11.6 GB VRAM, 5.4 GB RSS |
| `StrandsAgents/strands-decider-2B`, 2B | unranked | 0.891 | 0.10 | 98.5% | 109 ms GPU, 1767 ms CPU | 6 GB VRAM, 9 GB CPU RSS |
| `all-MiniLM-L6-v2`, 23M, kNN | not a decision model | 0.735 | none | 0%, misses | 59 ms CPU | 1.0 GB RSS |

H2O-Lightning-4B is Apache-2.0, 9.1 GB, served by vLLM 0.30.0 with the shim flag
`SHIM_NOUL_FLOOR=0`. The shipped config floors every yes/no answer at 0.801, which
would flag every span, so the floor must stay off for a threshold to mean anything.

- Positives median 0.862, negatives median 0.248. Threshold 0.50 gives recall 0.95
  with 5 false positives of 24.
- On real work the mean is 0.306 and the max is 0.774. Threshold 0.50 flags 25 of
  200 spans. Threshold 0.70 flags 5 of 200, at recall 0.80.
- The same two error classes remain, narrower. The weakest positives are private
  prose: 0.473, 0.531, 0.555. The strongest negatives are examples of secrets:
  0.760 and 0.721.
- The GPU budget is the blocker. The server holds 11.6 GB of the 12 GB card and
  5.4 GB of host RSS. A machine without a large GPU cannot host it, and CPU is out
  of reach.

Strands Decider 2B is Apache-2.0, 4.55 GB, and is not on the board.

- Its scores compress: positives median 0.238, negatives median 0.092, floor 0.070.
  On real work no span reached 0.5, so no threshold separates the classes. At
  threshold 0.15 the audit rate is 44.7 percent, and at 0.10 it is 98.5 percent.
- On CPU it holds AUC 0.887 at 1767 ms per span and about 9 GB RSS. Warm-cache load
  is 17.7 s.

The embedding model scores a span by similarity to labeled exemplars. Its four
lowest scores are the four private prose facts, so an embedding carries the topic of
a span, not whether the topic is private.

- Model choice, not the model class, caused the first failure. The ranked 4B gives a
  usable operating point at a 12.5 percent audit rate.
- The registry and the declared paths still decide the exact cases. The 4B ranks an
  example SSN above three real private facts, and it scored the internal SKU scheme
  low in the 2B run.
- First Mate ships the client, not the server, so the hardware budget is yours. The
  numbers above say what a 4B costs and what it catches, and the probe records the
  same two numbers for the server you run.
- Nothing under 4B on the board reaches capability 55. The 270M Gemma clone stayed
  untested, because Gemma 3 270M is gated and needs a Hugging Face login.

## Mitigation (class 2)

Mitigation starts after a value left the machine. The receiver controls retention,
and no provider gives a control that deletes one string from its logs. So the app
offers four small controls and promises nothing beyond them.

- Stop the re-send. `ChatView.vue:533` sends the whole transcript on each turn, so
  one leaked value leaves again on every turn. The `<!>` mark on the turn tells you
  to start a new conversation or to switch the provider.
- Name the one action for the value class. Rotation is the only mitigation that
  works whatever the receiver does.

  | Value | Rotatable | Action you can take |
  | --- | --- | --- |
  | Password, API key, session token | Yes | Rotate the value. |
  | Card number | Yes | Cancel the card and take a new one. |
  | Bank account number | No | Monitoring: alerts from the bank. |
  | SSN | No | Monitoring: credit freeze and fraud alerts. |
  | Health fact, employer, salary | No | No action exists. |

- Count the receivers. The same transcript sent to three providers is three
  exposures. Settings shows which providers received which conversation.
- Write a leak report.

### Leak report

The leak report is the artifact that policy and compliance read. First Mate's work
ends at that record.

- Each row names the class and never the value. A report that carried the value
  would become a second store for the secret. The chip in "On-disk truth and the
  console indicator" follows the same rule.
- Each row holds: the time, the provider id and Base URL, the model, the class, the
  count, the conversation id, the row id, the tool that produced the span, and the
  retention terms of that provider at that moment.
- The retention terms make the row compliance-grade. The row states the exposure
  window: 30 days, flagged and up to 2 years, zero retention, or unknown.
- Each row records your response: rotated, frozen, or ignored. Detection without
  action leaves the case open.
- The file is append-only JSONL, and the envelope in `docs/ENCRYPTION.md` seals it.
 The report shows which files held sensitive content, even with no values inside.
- Export is required. A report you cannot hand to a lawyer or a bank is not
  evidence.
- Scope: the report covers sends First Mate made. It cannot cover a leak by another
  route, and it cannot confirm deletion by a receiver.
- The report inherits the gate's recall. It records what the gate saw, so the
  observe-only measurement and the calibration curve come first.

### Provider terms that bound mitigation

- OpenAI API data is not used to train models, unless you opt in. Abuse-monitoring
  logs last up to 30 days. See
  `https://developers.openai.com/api/docs/guides/your-data`.
- Anthropic keeps no prompt content by default, except Covered Models at 30 days. A
  flagged chat may last 2 years even under zero retention. See
  `https://platform.claude.com/docs/en/manage-claude/api-and-data-retention`.
- Zero Data Retention needs a sales arrangement, per organization. An individual
  cannot obtain it, so the retention tail for a normal user is 30 days, or 2 years
  when content is flagged.

## Ad-hoc identification (class 2, future)

You notice a plaintext value in the chat log. You select it, open the context
menu, and choose `Redact`. That gesture can work where a pattern set cannot,
because you know the value. The log is evidence, not a guess.

- The rule targets the value, not the conversation. A value in one log usually
  sits in other logs, in the file the agent read, and in the file itself. A
  per-conversation rule closes one path of four.
- The gesture is the write path Layer 2 needs, so the registry grows from a leak
  you observed. You need no upfront configuration, because nobody writes such a
  list in advance.
- The action cannot retract what already left. The console keeps a persistent
  `<!>` marker on the turn where the value first went out, and that marker states
  the value reached the provider before the rule existed. The word `Redact` alone
  would imply an undone exposure.
- The pass edits the outbound copy only, and re-derives that copy per request. The
  stored transcript keeps real values, per the Core round-trip rule. A rule baked
  into stored rows fails on resume against another provider, because those rows
  then carry plaintext again.
- You never write a pattern. The UI collects the forms you observed, and the model
  writes a pattern that matches every collected form. That limit is real: this
  path fits numbers and codes. Prose-shaped sensitivity, such as a name or a
  diagnosis, is out of reach.
- The rule list shows each rule, the rows it touched on the last request, and an
  off switch. A broad pattern on an 8-digit value can also match order IDs. A
  silent no-match and a silent over-match both destroy your trust in the control.
- Provider-side retention is out of reach. Once a request arrived, only the
  receiver's own policy decides what survives. That case belongs to mitigation.

**Dependency:** Profile model bindings (§1) decide which provider a turn uses, and
that provider's checkbox decides whether the pass runs. Encryption at rest
decides what is worth protecting on disk instead.
