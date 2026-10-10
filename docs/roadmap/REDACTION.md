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

This document holds no containment or mitigation design yet. It holds one
ad-hoc identification design, below.

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
