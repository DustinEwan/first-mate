# Cloud Redaction — Design

This document is the design detail for roadmap §4 (Phase A). The `§`
references below point to `docs/ROADMAP.md`.

**Problem.** A public model must never see credentials, addresses, health data,
or financial data. A local model needs none of that protection. The feature is
therefore one rule: when the resolved provider is remote, redact.

## Core round-trip

- A redaction pass runs over every outbound request when
  `provider.kind != local`. The pass detects secrets, replaces each one with a
  reversible placeholder such as `⟦EMAIL_1⟧`, and sends the request.
- The token map is **session-scoped**, because the model echoes tokens across
  turns. The token map lives in memory only. Nothing persists that map, and
  nothing sends that map anywhere.
- Detokenize happens at three harness boundaries, never in the model.
  - **Display**: the console renders the real value, or obfuscates that value at
    the §5 privacy level.
  - **Tool/action execute**: a tool call such as "email `⟦EMAIL_1⟧` the
    summary" contains a token. Execution substitutes the real address at that
    point, under the profile's trust tier.
  - **Tool output inbound**: exec and file output receive the same redaction
    pass before that output enters a cloud prompt. Redaction and detokenize
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

- Profile settings seed the registry with accounts, addresses, and employer.
- The registry grows from Layer 1 and Layer 3 hits after a one-time user
  confirmation.
- The registry then matches exact values anywhere, in any format.
- §3 stores that registry sealed.

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

The bias is to over-redact outbound traffic to a cloud provider. Detokenize
restores the real value for display and for actions. A false positive therefore
removes a token that detokenize restores before the user sees it. A false
negative is the leak. Per-class toggles and the disclose-once override guard
against over-redaction degrading model task quality.

## On-disk truth and the console indicator

How much real text the chat log stores is a privacy-level choice, decided in §5.
Level L1 stores _redacted_ text, which minimizes on-disk truth. Level L2 and
above store sealed truth, as §3 describes. The console can therefore reveal that
truth on demand.

The console shows a "Redacted N items" chip on each cloud turn. Clicking that
chip shows which classes fired, and never shows the values.

**Dependency:** Profile model bindings (§1) decide when redaction runs. §3
decides what is worth protecting on disk instead.
