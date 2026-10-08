# First Mate Roadmap

North star: a desktop assistant that _does tasks on your PC_ — not a chat
window. Everything below serves that: the right agent for the job, skills
that compound, and the trust layer that makes it safe to point an agent at
a machine full of personal data.

Owner priorities are §1–4; §5 is the competitive scan (what Copilot ships,
what users actually praise) and §6 the features we pulled from it.

## 1. Agent Profiles

**Problem.** One global system prompt + one skill set = a "kitchen sink"
agent that is mediocre at everything and carries every tool into every
task.

**Design sketch.**

- A profile = `{name, system_prompt, model binding, skill allowlist,
harness policy}` stored under `~/.firstmate/profiles/*.toml`; the
  existing per-conversation system prompt resolution
  (`skills::get_system_prompt`) becomes profile-scoped.
- Profile switcher in the console header + `use profile <name>` as a
  routed command; conversations record their profile so history shows
  which agent said what.
- Model binding per profile (e.g. local model for anything touching
  files, strong cloud model for research) makes item 4 mostly
  declarative.
- Ship with 2 built-ins: `general` (today's behavior) and `sysadmin`
  (exec-heavy, local-model default). Creating profiles starts as editing
  the TOML; a Settings UI tab comes later.

**Open questions:** per-profile conversation namespaces vs tagged shared
list; whether hotkey can summon a specific profile directly.

## 2. Agent-Defined Skills

**Problem.** Skills are the compounding unit (a `SKILL.md` + optional
scripts), but today only the user writes them. Repeatable procedures the
agent discovers die in the chat log.

**Design sketch.**

- New tool `propose_skill`: agent drafts a `SKILL.md` (frontmatter: name,
  description, when-to-use; body: the procedure it just executed) into a
  `skills/_proposed/` staging dir.
- Nothing auto-activates: the console surfaces proposed skills; user
  approves (edits allowed) → moved into the active profile's skill set.
  Approval is the trust boundary — a self-modifying agent without a
  review step is how prompt-injection becomes persistence.
- Skills are _owned by profiles_ (item 1): a skill proposed under
  `sysadmin` is invisible to `general` unless explicitly promoted.
- Dedup guard: before proposing, agent must diff against existing skills
  (the tool refuses if a near-duplicate exists) — keeps profiles from
  drifting back toward kitchen sink.

**Open questions:** skill versioning when the agent revises an approved
skill (re-approval flow); expiry/staleness scoring.

## 3. Encrypted Chat Sessions / Config (Windows Hello-gated)

**Problem.** Conversations and `settings.json` are plaintext in
`%APPDATA%`. Any agent that touches mail/banking leaves plaintext
evidence of it. Recall's post-fiasco redesign is the template: local
encryption + Windows Hello (passkey/TPM-backed) as the gate, not a
password file.

**Design sketch.**

- Cryptographic layer: AES-256-GCM per conversation file; the data key
  is wrapped by a **DPAPI-NG / CNG key protected by Windows Hello**
  (`NCrypt` with a user+device+platform bound key, released only after
  Hello verification) — same mitigation class Microsoft ships for Recall
  snapshots. No key material on disk in the clear, ever.
- Opt-in per conversation at creation ("seal this session"); sealed
  sessions decrypt lazily behind a Hello prompt when opened from the
  console. `settings.json` gets a `secrets` section (API keys!) moved to
  the same envelope.
- Non-sealed conversations stay plaintext (searchability, cheap
  migration); the file header (`FMENC1`) makes the mix obvious to
  tooling.
- Failure mode matters: TPM reset / hardware loss = sealed data is
  unrecoverable. Offer a recovery key (printed once, offline) at first
  seal.

**Open questions:** Hello prompt frequency (per open vs per app run);
whether `winapp`/exec output spilled to `fm_out` should inherit the
session's seal (it should — that's where the sensitive content actually
lands).

## 4. Sensitive-Info Obfuscation for Cloud Models

**Problem.** Public models must never see credentials, addresses, health
or financial data. Local models need none of this — so the feature is
"when the resolved provider is remote, redact."

**Design sketch.**

- A redaction pass over every outbound request when
  `provider.kind != local`: detect → tokenize → send → detokenize on
  the streamed way back.
- Tier 1 (deterministic, always on): regex + checksum detectors for
  secrets (API-key shapes, JWTs), cards (Luhn), phone/email, Windows
  paths under `Users\<name>`. Reversible placeholder tokens
  `<EMAIL_1>`, `<KEY_2>`.
- Tier 2 (configurable per profile): user-defined patterns ("my
  employer", account numbers) and an optional small local NER model via
  the existing ollama binding.
- The token map lives in memory for the request only; chat logs store
  the _redacted_ text for remote sessions (local sessions store truth).
- Console affordance: a "redacted N items" chip on each cloud turn,
  click-through to what classes fired (never the values).

**Dependency:** profile model bindings (§1) decide when this runs; item 3
decides what's worth protecting on disk instead.

## 5. Competitive scan — Copilot on Windows, and what users reward

What Microsoft ships (2025–2026) and the observed reception:

| Feature                                      | What it is                                                                                                                    | User signal                                                                                                                                                                                                              |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Recall                                       | Snapshot timeline + semantic "when did I see X"                                                                               | Loud backlash, but **actual users rave** — "first truly great and useful AI productivity tool" (r/Windows11); WIRED: useful with privacy trade-offs. Lesson: the _capability_ is loved; the _default posture_ was hated. |
| Recall security redesign                     | Windows Hello-gated, encrypted, filtered snapshots                                                                            | The groaning follows _unencrypted-by-default_ storage, not the feature. Validates item 3's design class.                                                                                                                 |
| Click to Do                                  | Select anything on screen → summarize/copy/search/save actions                                                                | Mixed; loved as an accessibility surface (screen-reader supported), meh as "AI party trick." Lesson: actions on _selections_ beat actions on _screenshots_.                                                              |
| Copilot Actions + Agent Workspace (Oct 2025) | Agents do multi-step tasks (files, emails, docs) inside a **sandboxed workspace, low-privilege agent account, scoped access** | The security model IS the pitch. Lesson: agentic ambition without containment reads as reckless.                                                                                                                         |
| Voice Access                                 | Full voice control of the PC                                                                                                  | **Most positive sentiment of any Windows AI feature** ("oh my god windows voice access is incredible"); built for motor impairment, adopted by everyone.                                                                 |
| Live Captions / Hey Copilot                  | Translation, voice wake                                                                                                       | Well liked, low controversy.                                                                                                                                                                                             |

Synthesis: users reward (a) _memory/search over their own machine_,
(b) _voice_, (c) _actions scoped to what's in front of them_, and punish
(d) plaintext risk, (e) unbounded agents. Items 1–4 already answer (d);
§6 picks up (a)–(c).

## 6. Research-derived roadmap entries

**Later — "Here" (Click to Do, First Mate's way).** Hotkey on any app's
selection (or a screen region) → route to the active profile: summarize,
translate, explain, "file this", "draft reply". Selection-first (the
praised half of Click to Do), region-capture behind explicit consent.
Needs: a vision-capable profile model; UIA selection capture (the winapp
harness already speaks UIA).

**Later — Voice input.** Push-to-talk on the summon hotkey → local STT
(whisper.cpp or Windows Speech recognition API) → console. Voice Access's
sentiment says the demand is real; local STT keeps it inside the item-3/4
trust boundary. (Full voice _control_ of the PC is explicitly out of
scope — Voice Access already owns that, well.)

**Later — Session memory ("when did I…?").** Semantic index over the
user's _own_ First Mate conversations and spilled outputs — Recall's
loved half, applied to data we already own, sealed under item 3. Avoids
screen-constant capture entirely: no ambient recorder, same benefit.

**Next — Harness trust tiers.** Copilot Actions' containment as the
model for our exec harness (today: full user privileges):
tier 0 read-only, tier 1 workspace-scoped writes, tier 2 destructive with
per-call confirm, tier 3 out-of-band (sending anything external). Profile
defaults pick the tier; `route.rs` already gates program disclosure —
this extends the same seam. This is the precondition for letting §2's
self-authored skills run unattended.

## Non-goals

- Ambient screen capture (Recall-style recorder) — item 3's threat model
  plus user sentiment says no; session memory (§6) covers the benefit.
- Full voice control of the OS — Voice Access does it, accessibility
  first.
- Being a coding harness — the console stays a task runner; deep coding
  workflows belong to dedicated tools (winapp routes what's needed).
- Cloud sync / accounts — local-first, encrypted at rest, your TPM or
  nothing.
