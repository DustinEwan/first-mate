# Cloud Redaction — Design

Design detail for roadmap §4 (Phase A). § references below point to
`docs/ROADMAP.md`.

**Problem.** Public models must never see credentials, addresses, health
or financial data. Local models need none of this — so the feature is
"when the resolved provider is remote, redact."

## Core round-trip

- A redaction pass over every outbound request when
  `provider.kind != local`: detect → tokenize → send; reversible
  placeholders `⟦EMAIL_1⟧`.
- The token map is **session-scoped** — the model echoes tokens across
  turns — and lives in memory only: never persisted, never sent.
- Detokenize happens at three harness boundaries, never in the model:
  **display** (the console renders truth — or obfuscates it, per the
  §5 level), **tool/action execute** ("email `⟦EMAIL_1⟧` the summary"
  substitutes the real address at execution time, under the profile's
  trust tier), and **tool output inbound** (exec/file output gets the
  same pass _before_ it enters a cloud prompt — both directions
  covered).

## Detection layers

**Layer 1 — shape + checksum (deterministic, always on).** Financial
identifiers carry built-in math, so they are exactly detectable, not
guessed: cards (Luhn + BIN network prefixes), IBAN (ISO 13616
per-country length table + mod-97), ABA routing (mod-10 weights 3-7-1),
SWIFT/BIC (ISO 9362 format), SSN (position-dependent validity), crypto
addresses (base58/bech32 checksums), secrets (API-key shapes, JWTs),
email/phone, Windows paths under `Users\<name>`.

**Layer 2 — entity registry (closed set).** The hard cases (US account
numbers: no standard, no checksum, 8–17 digits, shape-indistinguishable
from order IDs) don't need open-world detection — the valuable targets
are _the user's own_ values, a tiny closed set. Seed in profile settings
(accounts, addresses, employer); auto-grow from layer-1/3 hits after
one-time user confirmation; exact-match anywhere, any format,
thereafter. Registry stored sealed (§3).

**Layer 3 — provenance.** Content from marked origins is sensitive
regardless of shape: file reads under declared sensitive paths, UIA
fields named/auto-id'd "Account No." (the winapp harness already reads
UIA), online-banking browser origins, password-manager clipboard (never
allowed). A number from the bank's own web app needs no checksum to be
known.

**Layers 4–5 — context + local model net.** Trigger-word proximity
("acct", "routing", "balance") + digit-length windows firing only in
combination; then an optional small local NER/LLM pass (ollama binding)
for shapeless entities (names, employer, health) — local so the scan
itself can't leak.

## Failure classes, handled explicitly

- The model **mangles** a token: fuzzy-repair pass on the distinctive
  `⟦…⟧` shape.
- The model must **reason over** the real value ("is this IBAN valid?" —
  it can't): route to a local-model profile or a per-field
  disclose-once override.
- An action **sends** a real value out-of-band (web search, email): that
  is the trust-tier-3 confirm gate at the leaving-the-machine boundary,
  not a redaction bug.

## Bias rule

Over-redact outbound to cloud — detokenize restores display and actions,
so a false positive costs a token the user never sees; a false negative
is the leak. Per-class toggles + disclose-once guard against
over-redaction degrading model task quality.

## On-disk truth and console affordance

Chat-log truth is a privacy-level choice (§5): L1 stores _redacted_ text
(minimize on-disk truth); L2+ stores sealed truth (§3) so the console
can reveal on demand.

Console affordance: a "redacted N items" chip on each cloud turn,
click-through to what classes fired (never the values).

**Dependency:** profile model bindings (§1) decide when this runs; §3
decides what's worth protecting on disk instead.
