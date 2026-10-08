# First Mate Roadmap

North star: a desktop assistant that _does tasks on your PC_ — not a chat
window. Everything below serves that: the right agent for the job, skills
that compound, and the trust layer that makes it safe to point an agent at
a machine full of personal data.

Owner priorities are §1–4. §5 reads Copilot's track record for the
_usefulness_ underneath its features — the jobs users actually hire for,
inferred from what gets praised vs punished — and §6 derives First Mate
capabilities from those jobs, not from Microsoft's mechanisms.

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

## 5. The Copilot lesson: usefulness, not features

Microsoft's 2025–2026 AI features are mechanisms; their reception tells us
which _jobs_ underneath them are real. Users can't articulate the job —
they praise or punish the packaging — so the job is inferred from the
reaction pattern. Four jobs survive the evidence:

| Job (what the user is actually hiring for)                                                                                                    | Evidence from reception                                                                                                                                                                                                                                                   | What it rules in / out                                                                                                    |
| --------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| **"Don't make me lose what I already saw/knew."** Recovery of lost context — the thing I read, the fix I did, the decision I made last month. | Recall: constant backlash yet devoted actual users ("first truly great and useful AI productivity tool"). The backlash tracked the _plaintext-by-default posture_, not the recovery value — the redesigned Hello-gated version kept the praise and shed some of the fear. | The job is real and worth serving. It does NOT imply an ambient recorder; that's one costly way to serve it.              |
| **"Use what's already in front of me."** The current selection/window should be the argument to the next action, without copy-paste rituals.  | Click to Do: mixed as a spectacle, genuinely valued as an accessibility surface. The chat-app packaging (leave your app, paste into a panel) ate most of the value.                                                                                                       | The job is real; the friction of the delivery is the failure mode. A globally hotkeyed assistant already avoids that tax. |
| **"Take this chore off my hands — I'll approve the plan."** Offloading repeatable multi-step drudgery while keeping control of the leash.     | Copilot Actions: adoption discourse is almost entirely about the containment model (workspace, low-privilege account, scoped access) — because _trust is the purchase_, not the demo. Nobody brags about one-shot agent tricks.                                           | Delegation with explicit, bounded trust is the product. Unbounded autonomy reads as reckless and sells nothing.           |
| **"Capture my intent at the speed of thought."** Low-friction expression for requests too long to type.                                       | Voice Access: the most uniformly positive sentiment of any Windows AI feature, adopted far beyond its accessibility audience. Nobody wants voice _control_ when in flow — they want voice _input_.                                                                        | Voice as an input modality; hands-free OS operation is someone else's product.                                            |

The punishment signal is just as clear: plaintext-at-rest storage and
unbounded blast radius are the only things users consistently hate. Items
§3–4 and the trust tiers below are that answer, so the four jobs above can
be pursued without importing the liability.

## 6. Derived capabilities

Each entry states the job first; the mechanism is ours to choose.

**Next — Harness trust tiers.** _(job: bounded delegation)_ Delegation is
only sellable as graded trust. Today the exec harness runs at full user
privilege; tiers: 0 read-only, 1 workspace-scoped writes, 2 destructive
with per-call confirm, 3 anything sent out-of-band. Profile defaults pick
the tier; `route.rs` already gates program disclosure — this extends the
same seam. Precondition for everything else below, and for §2's
self-authored skills running unattended.

**Later — Continuity.** _(job: don't lose what I knew)_ Not a searchable
archive for humans — the _agent_ resurfaces relevant history at the
moment of relevance ("you restructured these logs in March — same
failure?"). First Mate already owns the raw material: every conversation,
every spilled output, every skill execution. Index them (embeddings over
sealed data, §3) and let the active profile query its own past. Zero
ambient capture: the recorder mechanism was Recall's most expensive and
least loved choice; we serve the same job from work product we already
have.

**Later — Focus transfer.** _(job: use what's in front of me)_ The active
selection, clipboard, or window becomes an implicit argument to the
summoned command — summarize, translate, "file this", "draft reply" — and
the result lands back in the source app, not in a panel the user has to
visit. Our hotkey-console already deletes the app-switch tax that sank
Click to Do's feel; what's missing is UIA selection capture (the winapp
harness speaks UIA) and write-back affordances.

**Later — Voice input.** _(job: capture intent at speed of thought)_
Hold the summon hotkey → talk → release → transcribed request in the
console. Local STT only (whisper.cpp or the Windows Speech API) so voice
never crosses the §3/§4 boundary. Input-only: full voice control of the
OS belongs to Voice Access, which does it well.

**Later — Scheduled delegation.** _(job: take this chore off my hands)_
The end-state of bounded delegation: an approved skill bound to a trigger
(schedule, file arrival, window title) runs under its tier and reports
into the console. The approval artifact is the §2 skill itself — which is
why self-authored skills must ship with review, versioning, and tier
binding before automation exists.

## Non-goals

- Ambient screen capture (Recall-style recorder) — the continuity job
  (§6) is served from our own work product; the recorder mechanism brings
  §3's threat model for free.
- Full voice control of the OS — Voice Access does it, accessibility
  first.
- Being a coding harness — the console stays a task runner; deep coding
  workflows belong to dedicated tools (winapp routes what's needed).
- Cloud sync / accounts — local-first, encrypted at rest, your TPM or
  nothing.
