# First Mate Roadmap

North star: a desktop assistant that _does tasks on your PC_ — not a chat
window. Everything below serves that: the right agent for the job, skills
that compound, and the trust layer that makes it safe to point an agent
at a machine full of personal data.

This is a **living document**: it records direction and build order
(§1–5 features, §6's derived jobs, §7 destinations, §8 build order, and
the non-goals). Update it when priorities move — it is only as good as
its last revision. Design detail lives in the docs it points to, not
here.

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

**Design sketch.** A reversible tokenize pass on every outbound request
when the resolved provider is remote: detect → tokenize → send;
detokenize at the harness boundaries (display, tool execute, tool
output inbound), never in the model. Detection stacks deterministic
shape+checksum checks, a user-seeded entity registry, provenance marks,
and an optional local-NER net; over-redaction is the accepted bias
because detokenize restores truth everywhere the user can see. Full
design: `docs/REDACTION.md`.

**Dependency:** profile model bindings (§1) decide when this runs; item 3
decides what's worth protecting on disk instead.

## 5. Privacy level — one slider over §3–4

§3, §4, and screen rendering are one user-facing knob, not three:

| Level                                      | Posture                                                                                         | Threat it answers                     |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------- | ------------------------------------- |
| **L0 — local encryption only**             | §3 sealing on; nothing obfuscated in transit or on screen                                       | Stolen disk / other accounts          |
| **L1 — + obfuscation to public providers** | §4 round-trip on for remote providers; chat log stores _redacted_ text                          | Provider-side exposure                |
| **L2 — + hidden on screen**                | Console renders obfuscated (tokens/blurs); click-to-reveal per item                             | Shoulder-surfing, screenshots, shares |
| **L3 — + reveal requires passkey**         | Every reveal is a Windows Hello event; screenshots capture only obfuscated text; reveals logged | Unattended machine                    |

- The trade is explicit at L2: click-to-reveal needs truth on disk, so
  L2+ stores sealed truth (§3) where L1 stores only redacted text. L1
  minimizes on-disk truth; L2/L3 keep it sealed and obfuscate at
  _render_ time instead.
- Global default; a profile can ratchet up but never down
  (`profile.privacy_level >= global`).
- Local-model sessions skip §4 at every level (nothing leaves the
  machine); L2+ still applies to rendering — the threat there is eyes,
  not providers.
- L3 reveal is per-item, not per-session: one Hello prompt per reveal
  is the point.

## 6. The Copilot lesson: usefulness, not features

Four jobs survive the 2025–2026 Copilot reception evidence — recover
lost context, use what's in front of me, bounded delegation, and
low-friction intent capture — and the one thing users consistently
punish is plaintext-at-rest plus unbounded blast radius (§3–4 and the
trust tiers are that answer). Evidence and full analysis:
`docs/COPILOT-ANALYSIS.md`.

## 7. Derived capabilities

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

## 8. Execution checklist

Dependency spine: **A → B → C → D → E → F → G**. Redaction first
(self-contained, protects today's cloud users); profiles next (three
items hang off them); sealing before self-authored skills so the trust
story stays coherent.

### Phase A — Cloud redaction (§4)

- [ ] Provider registry: resolve `kind = local | remote` per model — single trigger source
- [ ] `redact.rs` layer 1: Luhn+BIN cards, IBAN mod-97, ABA mod-10, SWIFT, SSN validity, crypto checksums, API-key shapes/JWT, email/phone, `%USERPROFILE%` paths → `⟦CLASS_N⟧` tokens
- [ ] Session-scoped token map; stream-safe detokenize (placeholder boundaries across SSE chunks)
- [ ] Detokenize boundaries: display, tool/action execute, tool-output inbound redaction
- [ ] Mangled-token repair pass (fuzzy match on `⟦…⟧` shapes)
- [ ] Entity registry: profile-seeded values + confirm-on-grow from layer-1/3 hits, sealed under §3
- [ ] Provenance marks: sensitive-path reads, UIA field names, banking origins, password-manager clipboard deny
- [ ] Per-class toggles + disclose-once override
- [ ] "Redacted N items" chip → class breakdown, never values
- [ ] Tests: round-trip fidelity, code blocks untouched, map never persisted
- **Accept:** a cloud turn with fake card + JWT + email + a registry-seeded checksum-less account number leaves the machine tokenized and renders correctly on return

### Phase B — Agent profiles (§1)

- [ ] `profiles.rs` TOML schema `{name, system_prompt, model_binding, skill_allowlist, tier_default, privacy_level}`
- [ ] Profile-scoped `get_system_prompt`; conversations record their profile
- [ ] Built-ins `general` + `sysadmin` (local-model default)
- [ ] Console switcher + `use profile <name>` routed command
- [ ] Model binding wired so §4's trigger and §2's ownership read from profile
- **Accept:** two profiles with different models + skills, verifiable from a conversation's recorded profile

### Phase C — Hello-sealed sessions (§3)

- [ ] Spike first (biggest unknown): NCrypt CNG user+device+platform-bound key, Hello-gated unwrap POC — kill or commit before anything else
- [ ] `FMENC1` container: AES-256-GCM per conversation file
- [ ] "Seal this session" at creation; lazy Hello prompt on open
- [ ] `settings.json` secrets (API keys) into the same envelope
- [ ] `fm_out` exec spills inherit the session's seal
- [ ] Recovery-key flow (printed once; TPM loss = unrecoverable, say so loudly)
- **Accept:** a sealed file is ciphertext to a second Windows account on the same disk; opens with one Hello prompt

### Phase D — Privacy slider (§5)

- [ ] Global `privacy_level` setting + per-profile ratchet (up only)
- [ ] L1 = §4 on; chat log stores redacted
- [ ] L2 render obfuscation + click-to-reveal; log stores sealed truth
- [ ] L3 Hello-gated reveal + reveal audit line
- **Accept:** a screenshot at L2 contains zero sensitive values; reveal works with one Hello

### Phase E — Agent-defined skills (§2)

- [ ] `propose_skill` tool → `skills/_proposed/` staging
- [ ] Console approval surface: approve / edit / reject — never auto-activate
- [ ] Profile ownership + explicit promotion
- [ ] Dedup guard; versioning + re-approval flow
- **Accept:** agent proposes, you approve, next session uses it — under one profile only

### Phase F — Harness trust tiers (§7 "Next")

- [ ] Tiers 0–3 enforced in the exec harness (extend the `route.rs` seam)
- [ ] Profile `tier_default` enforced, not metadata
- [ ] Per-call confirm UI for tier 2+; detokenize-at-execute gated by tier 3
- **Accept:** a `general`-profile agent cannot delete outside its workspace without an explicit confirm

### Phase G — Derived capabilities (§7), in this order

- [ ] Continuity (needs C): embeddings over sealed conversations/outputs; agent-side relevance queries — no ambient capture
- [ ] Focus transfer (needs B): UIA selection capture; selection/clipboard/window as implicit argument; write-back to source app
- [ ] Voice input (needs A): push-to-talk on summon hotkey, local STT only
- [ ] Scheduled delegation (needs E + F): approved skill + trigger + tier

Cross-cutting: every G capability respects the §3/§4/§5 boundary —
voice, embeddings, and selections never leave local unless the
profile's model is remote _and_ redaction ran; nothing in E or G ships
without its review/tier gate.

## Non-goals

- Ambient screen capture (Recall-style recorder) — the continuity job
  (§7) is served from our own work product; the recorder mechanism brings
  §3's threat model for free.
- Full voice control of the OS — Voice Access does it, accessibility
  first.
- Being a coding harness — the console stays a task runner; deep coding
  workflows belong to dedicated tools (winapp routes what's needed).
- Cloud sync / accounts — local-first, encrypted at rest, your TPM or
  nothing.
