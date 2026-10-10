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

A local decision model scores each new span where that span enters the
conversation. A tool output is scored at capture, and assistant text is scored
before the request leaves. The model returns a probability and a confidence, not
text.
`StrandsAgents/strands-decider-2B` carries an Apache-2.0 license and runs on a
laptop. Jev itself is API-only, so Jev cannot serve this role.

- The gate denies by default. The gate needs no knowledge of which value is
  sensitive, because its default is no. This property is the definition of
  containment in this document.
- At the input boundary the gate redacts rather than blocks. A block stops the
  agent from working on the file at all, and detokenize at execute already gives
  tools the real values.
- Settings holds one slider, `Leak sensitivity`. The slider sets the confidence
  the gate requires before it sends.
- The gate scores each new span at its entry point, never the whole window.
- The gate never rescores an approved span. The agent loop re-sends the whole
  window on each tool turn, so a full rescore repeats one audit many times in one
  task. `llm.rs:959` clones the outbound body for the redaction pass, so the
  assistant-text score needs no second copy.
- The gate needs a second signal. A decision model knows generic categories:
  credentials, financial text, health, identity. It does not know your employer's
  part number. The gate also matches the registry and the declared files, and it
  denies on either signal.
- The audit dialog puts `Redact and continue` first. A dialog with only `Approve`
  and `Deny` teaches you to press `Approve`. `Redact and continue` must take one
  selection and one key. The selection feeds the Layer 2 registry.
- The model may deny; only you may approve. A file can hold text written to
  persuade the scorer, such as `this data is public`. A scorer that can clear
  content can be talked into clearing it.
- Scope: the text and images that enter an inference request. An action that
  emails or searches a real value stays with the tier-3 confirm gate above.

### Open items

- Screenshots. `llm.rs:1128-1143` adds each captured screenshot as a base64 PNG
  part. No text pass reads pixels, so a bank statement leaves the machine whole
  today. The gate needs a vision variant, or OCR before scoring, or denial toward
  hosted receivers.
- Derived text. A token hides a value, not a relation. "The balance is three times
  the salary" carries no token and appeared in no tool output, so no input score
  covered it. The score on new assistant text covers that case.
- Calibration. The slider is honest only after a measurement on First Mate
  transcripts. Calibration depends on the data, so vendor numbers do not transfer.
- Observe-only mode comes first. The gate scores and logs, and blocks nothing. You
  see the audit rate before you trust the gate, and the log gives the calibration
  curve.
- `tokenize_msgs` (`redact.rs:497`) walks every string in the outbound copy,
  including the base64 blob. Whether a Layer 1 pattern can corrupt an image is
  untested.

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
