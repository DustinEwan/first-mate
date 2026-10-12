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
- The pass touches three harness boundaries, and never the model.
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

Layer 1 and Layer 2 ship. Layer 3, Layer 4, and Layer 5 are design only.

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
  The trust-tier-3 confirm gate handles that send (roadmap Phase E). The send is
  not a redaction bug.

## Bias rule

The bias is to over-redact outbound traffic on a turn whose provider has the
checkbox checked. Detokenize restores the real value for display and for
actions. A false positive therefore hides a benign value, and detokenize restores
that value before you see it. A false negative is the leak. Per-class toggles and
the disclose-once override are the planned guards against over-redaction.

## On-disk truth and the console indicator

The conversation record is the wire record. `conversations.rs` writes one line per
message, in the form one request carried it. A stored row is therefore the
evidence that a value reached a receiver. The send path writes the file, because
only the send path holds that form. The console holds the detokenized text it
renders, and `load_conversation` detokenizes each stored row before the console
shows it.

The record holds handles, and `token_map.json` holds the values behind them. The
envelope in `docs/ENCRYPTION.md` covers both files. A transcript restored
without its token map therefore shows handles. The §5 privacy levels decide what
the console renders, not what the record holds.

A leak scan reads the record. A value in a stored row left the machine. The scan
reads one row once, when the row goes in. A new value in `entity_registry.json`
is the only event that reopens an old row, and `leak::rescan` then reads every
stored row once and adds no row the report already holds.

A fold writes the rows it dropped, and the report lists each fold. One click
opens those rows read-only. That view shows the wire copy, so a handle stays a
handle there, and the conversation view shows the real value. The pass runs on
the summarizer transcript too, because that call is an outbound send like any
other. With the switch on, the summarizer sees handles, and its brief carries
those handles into the next system prompt.

The console shows a "Redacted N items" chip on each turn whose pass replaced a
value. On a chip click, the console lists the fired classes and never shows the
values.

## Containment (class 2)

A decision server scores a message before that message leaves the machine. The gate
runs on the outbound copy of the conversation, because a leak needs a path the
gate crosses. The gate scores each message once per run. A tool output that
reaches no request gets no score, so no leak follows. The server answers typed
questions with probabilities, and the server returns no text.

First Mate ships no weights and starts no server. You run the server yourself,
with any model you choose. Settings holds the server URL. The app is a client of
the typed-decision API:

```text
POST {base}/v1/systemone
{"state": "<one message>",
 "questions": {"sensitive": {"type": "noul",
                             "instructions": "<the question>",
                             "criteria": {"true": "yes", "false": "no"}}}}

{"answers": {"sensitive": {"noul": 0.87}}}
```

The API is external. `https://docs.typesafe.ai/api` holds the request shape, the
three question types, and the error codes. First Mate sends one `state` and one
`noul` question. The answer is one probability.

A server reads the `noul` criteria keys `"true"` and `"false"`. A server ignores
every other key in silence. `decide.rs` sends those two keys. The wording of the
question and the wording of the criteria are the only tuning.

The model keeps nothing between requests, so a second request on the same text
returns the same answer. One probability covers one `state`, so one probability
cannot name the message that holds a value. First Mate scores one message per
request, because First Mate streams messages as they arrive.

The probability is calibrated across many answers, so one answer can be wrong. A
flagged message therefore goes to you, not straight to a marker.

- The URL and the enable box start off. With either control off, the app runs the
  exact rules alone. The Settings UI shows the state of the two controls.
- The scoring request holds the text the gate protects, so the server must belong to
  you or to your organization. A hosted third-party endpoint is out of scope. The
  app cannot tell a network server from a hosted endpoint by address. The app
  therefore asks for confirmation before the first message leaves. The console
  names the host on every audit line.
- The same endpoint answers every later decision question, so one server serves
  containment now and detection later.
- `decide.rs` is the client. `redact.rs` holds the `SPAN` token class and the set of
  text already judged. `gate` in `llm.rs` runs the pass before the exact rules.
- Settings stores the decision-server address with its `/v1` suffix. The client
  strips that suffix before it appends an endpoint, so the client never sends a
  doubled `/v1`.
- The console names each model once. The Agent Model answers you. The Privacy Model
  reads the words before they leave the PC. A packaging test bans the older names in
  the UI files. `decide.rs` and the `decision_` setting keys keep the older
  identifier. A rename there would break each saved settings file.
- A scorer outage holds the send. First Mate sends nothing. The console names the
  host, the cause, and the two ways forward: start the server, or turn off the Privacy
  Model in Settings. The app writes one `GATE:` log line with the failing address.
  A send without a verdict would make containment depend on a network.
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
- `Test` in Settings sends the probe: one benign text, and one text holding a
  credential. When both probe answers land at or above 0.8, the app reports the
  server as unusable. H2O's shim ships a 0.801 yes/no floor, and a floored
  server flags every message.
- The gate judges outgoing text: your new message, and each tool result. Text
  the provider authored arrives from the provider, so the gate judges none of
  it. Its echo on a later turn is continuity, not a new send.
- The gate never judges text the Agent Model already holds. An audit of text
  that already left changes nothing.
- `Redactor::sent` holds one conversation's text for one run. Nothing about a
  judgement reaches a file.
- The agent loop re-sends the whole window on each tool turn. `record` marks the
  text that cleared, so a resend costs no new judgement.
- A run that holds sends nothing. The console removes the newest user row and puts
  the text back in the composer. The gate judges a later send of that text again.
- When the pass replaces a value that already left the PC, the console names
  the class. The notice says the earlier send cannot be undone, and names no
  value. The app writes one `LEAK:` log line.
- No length floor and no per-request cap limit the judgement. The measured 4B
  answers in 70 ms on a GPU. A large tool output therefore answers inside the
  time a tool turn already spends.
- A message over the server budget gets overlapping windows, and one flagged window
  flags the message. The client reads `max_sequence_tokens` and
  `max_question_tokens` from `GET {base}/v1/models`. The client sizes each window
  from those two numbers, at four characters per token. A refused answer states the
  measured count and the limit for some servers. The client shrinks the window by
  that ratio. A refused answer that states nothing halves the window.
- Windows overlap by 10 percent, so a value at a boundary still appears whole
  inside one window. The exact rules run over the whole message regardless, so a
  shape never depends on a window. The audit card highlights each flagged window,
  so you start reading where the model saw something.
- The gate must judge a screenshot, so the configured server must accept image
  input. The app sends the image part to that server, and the text walk skips the
  base64 blob. When the server accepts no image, the console says that the image
  went out unjudged.
- A flagged message stops before the send and opens the audit card in the
  console. The card shows the flagged message and its score, because the decision
  model cannot name the value it saw. You select the text, redact it, then
  approve or deny. Approve sends with your redactions. Deny holds the send, and
  you write the next message.
- The card takes the composer's place. The console hides the stop row while the card
  waits, because the run waits on you. The wait has no timeout.
- A hold is your decision or an outage, not a fault inside First Mate. The console
  writes the sentence for each cause, and Rust names only the cause.
- The gate scores no authored text. `prompts/AGENTS.md` and each loaded
  `SKILL.md` form the system message. You review the authored text before the
  authored text ships. A false positive in the system message replaces the whole
  instruction set with one token. Such a false positive breaks the task and
  protects nothing.
- The gate scores your new message and each tool result. A tool result is new
  text that leaves the PC, so the score covers it.
- The gate offers only message content to the scorer. A replaced value leaves the
  machine as one handle. A handle in place of a role value would break the request.
- A model restates a missed value in new words. A new sentence holds no shape
  for Layer 1. The gate scores no assistant text, so no score catches that
  restatement. The registry and the leak notice are the guards for that case.
- First Mate puts the brief inside the system message, so the gate scores no
  brief. First Mate scores the brief before the fold. A flagged brief stops the
  fold. The rows that fold would drop are text the Agent Model already holds.
- A model copies a value into a tool-call argument. The model saw only markers,
  so that argument holds a marker. The pass tokenizes each argument, and the
  harness restores the value at execute.
- The gate still runs the exact rules over the system message. Layer 1 costs
  nothing on the system message. A credential in a prompt is a defect you want
  found.
- The gate needs a second signal. The Privacy Model knows generic categories:
  credentials, financial text, health, identity. The Privacy Model does not
  know your employer's part number. The gate also matches the registry. Layer 3
  will add the declared files, and the gate will deny on either signal.
- The exact rules replace a value with no dialog, and the chip names the count.
  Detokenize restores the value for display and for execute. A Privacy Model flag
  opens the audit card, and you decide.
- You choose from three buttons: `Redact selection`, `Deny`, and `Approve`. Your
  selection seeds the pass for the rest of the run. Nothing writes that value to
  `entity_registry.json` today, so a flagged value does not grow the registry.
- The Agent Model cannot clear a flagged message. Only you approve the send. A file can hold text written to
  persuade the scorer, such as `this data is public`. A scorer able to clear
  content is also open to persuasion from file text.
- Scope: the text and images entering an inference request. An action emailing
  or searching a real value stays with the tier-3 confirm gate in the Failure
  classes section.

### Open items

- Screenshots. `chat_via_openai` appends each captured screenshot as an `image_url`
  part with a `data:image/png;base64,` URL. No text pass reads pixels, so a bank
  statement leaves the machine whole today. The image field is a server extension,
  not a contract field: `strands-decider serve --vision` takes `images` as base64,
  and the `openjev` helper takes one image as `state.screenshot` or `state.image`.
  Choose the shape per server at setup. The console names the shape the server
  accepts.
- Derived text. A token hides a value, not a relation. "The balance is three times
  the salary" holds no token and appeared in no tool output. No input score
  covered the derived sentence. The score on new assistant text covers the
  derived case.
- Calibration. The slider is honest only after a measurement on First Mate
  transcripts. Calibration depends on the data, so vendor numbers do not transfer.
  The Spike section below measures the compression and the audit rate on the host
  machine.
- Observe-only mode is not in the product. The gate redacts, and the `SPAN` count on
  the chip gives the audit rate for the turns you inspect. The log names each flagged
  value with the score of that value. The log therefore records the calibration
  curve for the flagged side only.
- One server, many profiles. A profile could need separate question text or a
  separate threshold. The probe and the slider are global today.

### Spike: Privacy Model candidates, measured 2026-10-10

Two Privacy Model candidates and one embedding model ran on the same 44 labeled
texts.
The same models ran on the same 200 texts of real agent work. The host is an
RTX 3060 with 12 GB and 12 CPU cores. The Jev Decision Index at
`https://huggingface.co/spaces/multimodalart/jev-decision-index` ranks open
reproductions by the Full score: 20 percent public benchmarks, 50 percent private
tests, and 30 percent new-domain tasks. A separate Vision board ranks the
image-capable entrants. JevBench v1.6.1 measures coherence, not capability. Neither
board establishes reliability on the redaction task, so each model also ran here.

| Model | JevBench coherence | AUC | Threshold for recall 0.95 | Audit rate there | Latency | Memory |
| --- | --- | --- | --- | --- | --- | --- |
| `h2oai/h2o-lightning-4b` v1.1, 4B | 75.0, open rank 5 | 0.960 | 0.50 | 12.5% | 70 ms GPU | 11.6 GB VRAM, 5.4 GB RSS |
| `StrandsAgents/strands-decider-2B`, 2B | unranked | 0.891 | 0.10 | 98.5% | 109 ms GPU, 1767 ms CPU | 6 GB VRAM, 9 GB CPU RSS |
| `all-MiniLM-L6-v2`, 23M, kNN | not a Privacy Model | 0.735 | none | 0%, misses | 59 ms CPU | 1.0 GB RSS |

H2O-Lightning-4B is Apache-2.0, 9.1 GB, served by vLLM 0.30.0 with the shim flag
`SHIM_NOUL_FLOOR=0`. The shipped config floors every yes/no answer at 0.801. A
0.801 floor flags every message, so the floor must stay off before a threshold
means anything.

- Positives median 0.862, negatives median 0.248. Threshold 0.50 gives recall 0.95
  with 5 false positives of 24.
- On real work the mean is 0.306 and the max is 0.774. Threshold 0.50 flags 25 of
  200 texts. Threshold 0.70 flags 5 of 200, at recall 0.80.
- The same two error classes remain, narrower. The weakest positives are private
  prose: 0.473, 0.531, 0.555. The strongest negatives are examples of secrets:
  0.760 and 0.721.
- On this machine the 4B server holds 11.6 GB of the 12 GB card and 5.4 GB of host
  RSS. The same machine ran the 2B on CPU at 1767 ms per question. Those two numbers
  describe this machine, and no other.

Strands Decider 2B is Apache-2.0, 4.55 GB, and is not on the board.

- The scores of Strands Decider 2B compress: positives median 0.238, negatives
  median 0.092, floor 0.070. On real work no text reached 0.5, so no threshold
  separates the classes. At threshold 0.15 the audit rate is 44.7 percent. At
  threshold 0.10 the audit rate is 98.5 percent.
- On CPU the 2B model holds AUC 0.887 at 1767 ms per question and about 9 GB RSS.
  Warm-cache load takes 17.7 s.

The embedding model scores a text by similarity to labeled exemplars. The four
lowest embedding scores are the four private prose facts. An embedding therefore
carries the topic of a text, not whether the topic is private.

- Model choice, not the model class, caused the first failure. The ranked 4B gives a
  usable operating point at a 12.5 percent audit rate.
- The registry and the declared paths still decide the exact cases. The 4B ranks
  an example SSN above three real private facts. The 2B model scored the internal
  SKU scheme low in the 2B run.
- First Mate includes the client, not the server, so the hardware budget is yours.
  The numbers above state the memory the 4B needs and the values the 4B catches on
  this machine. The probe records the same two numbers for the server you run.
- Nothing under 4B on the JevBench board reaches coherence 55. The 270M Gemma clone
  stayed untested, because Hugging Face gates Gemma 3 270M behind a login.
- Model selection is a later decision. These rows record two models on one machine
  on one date.

## Mitigation (class 2)

Mitigation starts after a value left the machine. First Mate cannot retrieve a
value from a provider, and no provider offers a control that deletes one string
from its log. First Mate therefore promises two things: the user sees exactly
what left and where it went, and the record of that fact survives.

- Mark the leak in place. `leak.rs` keeps one sealed row per leak: the value,
  its character range, the class, the provider id, the model, the Base URL, and
  the time of the send. The console colors the leaked value red, the same red as
  `.msg.error`. The console appends `⚠` (U+26A0) in that red at the end of the
  row. The tooltip names the class, the provider, the model, and the time. A
  click opens the leak report row for that leak. The console matches the value
  into the conversation's rows when it renders them, so every row that carried
  the value shows the marker. The marker survives a resume, because the report
  file survives it.
- The marker is never literal text in the row. `ChatView.vue` sends the whole
  transcript to the provider on each turn. Literal marker text would tell the
  receiver that the value leaked. The Agent Model could also echo that text. The
  redaction tokens `⟦…⟧` are literal text, because they must replace the value.
  The leak marker has no such need, so the console draws it at render time.
- State the leak, and state it exactly. The console names the value, the
  conversation, the message, the provider, the model, and the time of the send.
  The console gives no advice, because the user cannot undo the send.
- Keep the record. The leak report below holds one row per leak, and each row
  links into the console at the message that carried the value.
- Record the re-send. A leaked value leaves again on every later turn under a
  provider whose switch is off. The report gains one row for each such send.

The product ships the inline marker, the exact statement, and the leak report.
The console keeps its one red line per run too, because that line sits beside the
row. The recall measurement in the last bullet below stays open.

### Leak report

The leak report is the artifact the user, a lawyer, an insurer, or an auditor
reads. First Mate's work ends at the report.

- Each row names the leaked value in full. The user, or the user's organization,
  owns the security of the report once First Mate writes it. First Mate's duty is
  an exact record, not a redacted one.
- Each row holds the value and its class. Each row holds the conversation, the
  character range of the value in the text that left, and the tool that carried
  it, when a tool did. Each row holds the provider id, the model, the Base URL,
  and the time of the send.
- Each row links into the console. The user opens the conversation, and the
  console scrolls the message into view. The user then reads the leak in context.
  A row whose conversation was never saved has no stored message, so the console
  leaves that row inert.
- First Mate stores no retention claim. The client cannot learn how long one
  provider keeps one message, so no row states an exposure window. The provider
  terms below record what each provider publishes.
- First Mate keeps the report for as long as the user wants it. The default is
  forever, and no rotation or expiry exists.
- The store file is `leaks.jsonl`, append-only, sealed with the envelope
  `crypto.rs` writes. The file holds real values under the TPM key or the
  recovery key. The file opens on this machine only. `crypto::open_store_bytes`
  refuses the file to `read_file`, so the agent cannot read the record back.
- The console exports the report as a PDF, and `lopdf` writes that file. The
  export lands in the user's Downloads folder, because the file holds the real
  values in the clear. The user may protect the PDF with a password of at most
  127 bytes. First Mate generates the password, or the user types one. The
  export panel states that the PDF holds the real values. The user then knows
  what the file carries once it leaves this machine.
- The console chip keeps its count-only rule, because the chip sits in the chat log
  beside the row. The report is the place that names the value.
- Scope: the report covers sends First Mate made. The report cannot cover a leak by
  another route. The report cannot confirm deletion by a receiver.
- The report inherits the gate's recall. The report records what the gate saw, so
  the observe-only measurement and the calibration curve come first.

### Provider terms that bound mitigation

First Mate stores no retention claim in a report row. These terms are background
for the reader of this document, because they bound what any mitigation can
promise.

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
- The action cannot retract what already left. The console keeps the `⚠` marker
  from the Mitigation section on the row where the value first went out. The marker
  says the value reached the provider before the rule existed. The word `Redact`
  alone would imply an undone exposure.
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
