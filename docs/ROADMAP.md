# First Mate Roadmap

First Mate is a desktop assistant that _does tasks on your PC_, not a chat
window. Every item below serves that goal, and the goal has three parts. Each
job needs the right agent for that job. Skills accumulate value across tasks.
A trust layer makes it safe to point an agent at a machine full of personal
data.

This is a **living document**. It records direction and build order. §1–5 hold
features, §6 holds derived jobs, §7 holds destinations, and §8 holds the build
order. The last section holds the non-goals. Update this document when
priorities move, because this document is only as accurate as its last update.
Design detail lives in the documents this one points to, not here.

## 1. Agent Profiles

**Problem.** One global system prompt and one skill set produce one agent that
carries every tool into every task. That agent is mediocre at everything.

**Design sketch.**

- A profile is `{name, system_prompt, model binding, skill allowlist, harness
  policy}` stored under `~/.firstmate/profiles/*.toml`.
- The existing per-conversation system prompt resolution
  (`skills::get_system_prompt`) becomes profile-scoped.
- The console header holds a profile switcher, and `use profile <name>` is a
  routed command.
- Each conversation records the profile it ran under, so history shows which
  agent said what.
- Each profile binds a model. For example, a local model suits anything that
  touches files, and a strong cloud model suits research. That binding makes
  item 4 mostly declarative.
- Two profiles ship built-in: `general`, which is today's behavior, and
  `sysadmin`, which is exec-heavy and defaults to a local model.
- Creating a profile starts as editing the TOML file. A Settings UI tab comes
  later.

**Open questions.** The choice between per-profile conversation namespaces and
one tagged shared list is open. Whether the hotkey can summon a specific
profile directly is also open.

## 2. Agent-Defined Skills

**Problem.** A skill is a `SKILL.md` plus optional scripts, and that pair is
the unit which accumulates value across tasks. Only the user writes skills
today. A repeatable procedure that the agent discovers stays lost in the chat
log.

**Design sketch.**

- A new tool `propose_skill` lets the agent draft a `SKILL.md` into the
  `skills/_proposed/` staging directory.
- The drafted frontmatter holds name, description, and when-to-use, and the
  body holds the procedure the agent just executed.
- Nothing activates on its own. The console surfaces proposed skills, and the
  user approves them, with edits allowed. Approval moves the skill into the
  active profile's skill set.
- Approval is the trust boundary. A self-modifying agent with no review step is
  how prompt-injection becomes persistence.
- Profiles own skills (item 1). A skill proposed under `sysadmin` is invisible
  to `general` unless the user promotes that skill explicitly.
- A dedup guard runs before each proposal. The agent must diff against existing
  skills, and the tool refuses a near-duplicate. That guard keeps a profile from
  growing back into one agent that carries every tool into every task.

**Open questions.** Versioning of an approved skill that the agent revises is
open, along with the re-approval flow. The rule for retiring a skill that has
gone stale is also open. No scoring mechanism is designed for that rule yet.

## 3. Encrypted Chat Sessions / Config (Windows Hello-gated)

**Problem.** Conversations and `settings.json` are plaintext in `%APPDATA%`.
An agent that touches mail or banking leaves plaintext evidence of that work.
The template here is the redesign Recall shipped after its public failure. That
design uses local encryption plus Windows Hello (passkey/TPM-backed) as the
gate, and it uses no password file.

**Design sketch.**

- Each conversation file is encrypted with AES-256-GCM.
- A **DPAPI-NG / CNG key protected by Windows Hello** wraps the data key:
  `NCrypt` with a user+device+platform bound key, released only after Hello
  verification.
- That is the same class of mitigation Microsoft ships for Recall snapshots.
- No key material is ever on disk in the clear.
- Sealing is opt-in per conversation, chosen at creation as "seal this
  session".
- A sealed session decrypts lazily behind a Hello prompt when the console opens
  that session.
- `settings.json` gets a `secrets` section for API keys, and that section moves
  into the same envelope.
- Non-sealed conversations stay plaintext, so search and migration stay cheap.
- The file header `FMENC1` tells tooling which files are sealed and which are
  plaintext.
- A TPM reset or hardware loss makes sealed data unrecoverable.
- The first seal therefore offers a recovery key, printed once and kept
  offline.

**Open questions.** How often the Hello prompt appears is open: per open, or
per app run. Whether `winapp` and exec output spilled to `fm_out` inherits the
session's seal is also open. The answer here is yes, because `fm_out` output is
where the sensitive content actually lands.

## 4. Sensitive-Info Obfuscation

**Problem.** A public model must never see credentials, addresses, health data,
or financial data. The dividing line is not "local vs remote". The dividing
line is _who receives the data_. An agent on the same LAN is local, and you may
still not want that agent reading your payroll. A hosted model reached through
your own proxy is remote, and that model is still yours. The control this
product exposes is the user's intent toward the destination. That control is
never an inference drawn from an IP address.

**The switch.** One checkbox on the provider is named `Hide sensitive
information`, and the label says what the checkbox does. The checkbox starts
checked for every public provider. Whether the checkbox also starts checked for
user-added endpoints is the open call. That choice would make redaction opt-out
everywhere. `Spec::is_local` stays exactly what it is today: the hint
that shows the Base URL field.

**Design sketch.** A reversible tokenize pass runs on every outbound request
whose provider has the switch on. That pass runs detect → tokenize → send.
Detokenize happens at the harness boundaries: display, tool execute, and
inbound tool output. Detokenize never happens in the model. Detection stacks
deterministic shape+checksum checks, a user-seeded entity registry, provenance
marks, and an optional local-NER net. Over-redaction is the accepted bias,
because detokenize restores the real text everywhere the user can see. Full
design: `docs/roadmap/REDACTION.md`.

**Dependency.** Profile model bindings (§1) decide which provider a turn uses,
and that provider carries the switch. Item 3 decides what is worth protecting
on disk instead.

## 5. Privacy level — one slider over §3–4

§3, §4, and screen rendering are one user-facing setting, not three separate
controls:

| Level | Posture | Threat it answers |
| --- | --- | --- |
| **L0 — local encryption only** | §3 sealing is on. Nothing is obfuscated in transit or on screen. | A stolen disk, or another account on the machine |
| **L1 — + obfuscation to untrusted receivers** | §4 round-trip is on wherever the provider's switch is on. The chat log stores _redacted_ text. | Exposure at the receiver |
| **L2 — + hidden on screen** | The console renders obfuscated text (tokens or blurs), and each item reveals on click. | Someone reading the screen over the user's shoulder, screenshots, screen sharing |
| **L3 — + reveal requires passkey** | Every reveal is a Windows Hello event, and every reveal is logged. Screenshots capture only obfuscated text. | An unattended machine |

- The trade is explicit at L2. Click-to-reveal needs the real text on disk, so
  L2 and L3 store that real text sealed (§3). L1 stores only redacted text.
  L1 therefore keeps less real text on disk, and L2/L3 obfuscate at _render_
  time instead.
- The privacy level is a global default. A profile can raise that level but
  never lower it (`profile.privacy_level >= global`).
- §4 runs wherever the provider's `Hide sensitive information` switch is on.
  A LAN endpoint is a recipient like any other. L2 and above still apply to
  rendering, because that threat is eyes on the screen rather than a receiver.
- An L3 reveal is per item, not per session. One Hello prompt per reveal is the
  point.

## 6. The Copilot lesson: usefulness, not features

The 2025–2026 reception evidence for Copilot leaves four jobs standing. The
four are: recover lost context, use what's in front of me, bounded delegation,
and low-friction intent capture. Users consistently reject one combination:
plaintext at rest, plus an unconfirmed action that can change any file or
account. §3–4 and the trust tiers answer those two complaints. Evidence and
full analysis: `docs/roadmap/COPILOT-ANALYSIS.md`.

## 7. Derived capabilities

Each entry states its job first. The mechanism is ours to choose.

**Next — Harness trust tiers.** _(job: bounded delegation)_ Users delegate only
when the trust they place is graded. Today the exec harness runs at full user
privilege. The tiers are: 0 read-only, 1 workspace-scoped writes, 2 destructive
with per-call confirm, and 3 anything sent out-of-band. A profile default picks
the tier. `route.rs` already gates program disclosure, and these tiers extend
that same gate. Graded tiers are the precondition for everything else below,
and for §2's self-authored skills running unattended.

**Later — Continuity.** _(job: don't lose what I knew)_ This capability is not
a searchable archive for humans. The _agent_ resurfaces relevant history at the
moment of relevance. An example: "you restructured these logs in March — same
failure?". First Mate already owns the raw material: every conversation, every
spilled output, and every skill execution. Index that material with embeddings
over sealed data (§3), and let the active profile query its own past. There is
no ambient capture. Continuous screen recording was Recall's most expensive and
least loved choice. First Mate serves the same job from work product First Mate
already owns.

**Later — Focus transfer.** _(job: use what's in front of me)_ The summoned
command gets an implicit argument. That argument is the active selection, the
clipboard, or the active window. The command might summarize, translate, "file
this", or "draft reply".
The result lands back in the source app, not in a panel the user has to visit.
The hotkey console already removes the step of leaving the current application.
That extra step is what made Click to Do feel bad to use. What is still missing is
UIA selection capture, and the winapp harness already speaks UIA. The other
missing piece is a way to write the result back into the source application.

**Later — Voice input.** _(job: capture intent at speed of thought)_ The user
holds the summon hotkey, talks, and releases. The console then shows the
transcribed request. Speech-to-text is local only, with whisper.cpp or the
Windows Speech API, because voice must never cross the §3/§4 boundary. This
capability is input-only. Full voice control of the OS belongs to Voice Access,
and Voice Access does that well.

**Later — Scheduled delegation.** _(job: this chore runs for me, and I approve
it)_ This is where bounded delegation ends up. An approved
skill binds to a trigger: a schedule, a file arrival, or a window title. The
bound skill runs under its tier and reports into the console. The approved §2
skill is itself the approval artifact. That is why self-authored skills must
ship with review, versioning, and tier binding before automation exists.

## 8. Execution checklist

The dependency spine is **A → B → C → D → E → F → G**. Redaction comes first,
because that work is self-contained and protects today's cloud users. Profiles
come next, because three items depend on a profile. Sealing comes before
self-authored skills, because the product must not gain agent-written skills
while conversations are still plaintext.

### Phase A — Sensitive-info redaction (§4)

- [ ] Trust registry: `hide_sensitive` on the resolved provider is the single
      trigger source, checked by default for public providers. An endpoint
      address is never a privacy decision, and `is_local` stays a Base-URL hint.
- [ ] `redact.rs` layer 1 replaces each detected secret with a `⟦CLASS_N⟧`
      token. Layer 1 detects Luhn+BIN cards, IBAN mod-97, ABA mod-10, SWIFT, and
      SSN validity. Layer 1 also detects crypto checksums, API-key shapes, JWT,
      email, phone, and `%USERPROFILE%` paths.
- [ ] The token map is scoped to the session, and detokenize is stream-safe
      across placeholder boundaries in SSE chunks.
- [ ] Detokenize runs at display, at tool and action execute, and on inbound
      tool output.
- [ ] A repair pass fixes mangled tokens by fuzzy match on `⟦…⟧` shapes.
- [ ] The entity registry holds profile-seeded values, and it grows by
      confirmation from layer-1 and layer-3 hits. The registry is sealed under
      §3.
- [ ] Provenance marks cover sensitive-path reads, UIA field names, banking
      origins, and a clipboard deny for password managers.
- [ ] Each class has its own toggle, and a disclose-once override exists.
- [ ] The "Redacted N items" chip opens a class breakdown, and that breakdown
      never shows values.
- [ ] Tests cover round-trip fidelity, untouched code blocks, and the rule that
      the token map is never persisted.
- **Accept:** a cloud turn with fake card, JWT, email, and a registry-seeded
  checksum-less account number leaves the machine tokenized, and that turn
  renders correctly on return.

### Phase B — Agent profiles (§1)

- [ ] `profiles.rs` defines the TOML schema `{name, system_prompt, model_binding, skill_allowlist, tier_default, privacy_level}`.
- [ ] `get_system_prompt` becomes profile-scoped, and conversations record
      their profile.
- [ ] The built-in profiles are `general` and `sysadmin`, and `sysadmin`
      defaults to a local model.
- [ ] The console has a profile switcher, and `use profile <name>` is a routed
      command.
- [ ] Model binding is wired so §4's trigger and §2's skill ownership read from
      the profile.
- **Accept:** two profiles with different models and skills, verifiable from a
  conversation's recorded profile.

### Phase C — Hello-sealed sessions (§3)

- [ ] Spike first, because the biggest unknown is the NCrypt CNG
      user+device+platform-bound key with Hello-gated unwrap. Build that proof
      of concept, then drop or commit the approach before anything else is
      built.
- [ ] The `FMENC1` container encrypts each conversation file with AES-256-GCM.
- [ ] "Seal this session" is offered at creation, and opening a sealed session
      shows a Hello prompt lazily.
- [ ] The `settings.json` secrets, which are API keys, move into the same
      envelope.
- [ ] Exec output spilled to `fm_out` inherits the seal of its session.
- [ ] The recovery-key flow prints the key once. The app states plainly that a
      TPM loss makes sealed data unrecoverable.
- **Accept:** a sealed file is ciphertext to a second Windows account on the
  same disk. The same file opens with one Hello prompt.

### Phase D — Privacy slider (§5)

- [ ] `privacy_level` is a global setting, and a profile can raise that level
      but never lower it.
- [ ] L1 turns §4 on, and the chat log stores redacted text.
- [ ] L2 obfuscates at render time and adds click-to-reveal, and the log stores
      the real text sealed.
- [ ] L3 gates each reveal behind Hello and writes a reveal audit line.
- **Accept:** a screenshot at L2 contains zero sensitive values, and a reveal
  works with one Hello.

### Phase E — Agent-defined skills (§2)

- [ ] The `propose_skill` tool writes drafts into the `skills/_proposed/`
      staging directory.
- [ ] The console shows an approval surface with approve, edit, and reject, and
      nothing activates automatically.
- [ ] A profile owns its skills, and promotion to another profile is explicit.
- [ ] The dedup guard blocks near-duplicate proposals, and a versioning and
      re-approval flow exists.
- **Accept:** the agent proposes, you approve, and the next session uses that
  skill under one profile only.

### Phase F — Harness trust tiers (§7 "Next")

- [ ] The exec harness enforces tiers 0–3, and that enforcement extends the
      gate that `route.rs` already implements.
- [ ] The profile `tier_default` is enforced behavior, not stored metadata.
- [ ] Tier 2 and above show a confirm UI on each call, and tier 3 gates
      detokenize at execute.
- [ ] Skill actions are approval-gated: the user confirms before the agent
      clicks or types, under tier enforcement.
- **Accept:** a `general`-profile agent cannot delete outside its workspace
  without an explicit confirm.

### Phase G — Derived capabilities (§7), in this order

- [ ] Continuity (needs C) adds embeddings over sealed conversations and
      outputs, plus agent-side relevance queries. Ambient capture is excluded.
- [ ] Focus transfer (needs B) adds UIA selection capture, treats the
      selection, clipboard, or window as an implicit argument, and writes back
      to the source application.
- [ ] Voice input (needs A) adds push-to-talk on the summon hotkey, and
      speech-to-text is local only.
- [ ] Scheduled delegation (needs E + F) binds an approved skill to a trigger
      and runs that skill under its tier.

Cross-cutting: every G capability respects the §3/§4/§5 boundary. Voice,
embeddings, and selections never leave the machine unless the profile's model
is remote _and_ redaction ran. Nothing in E or G ships without its review gate
and its tier gate.

## Non-goals

- Ambient screen capture, which is a Recall-style recorder. The continuity job
  (§7) is served from First Mate's own work product. Continuous capture would
  reintroduce the exposure that §3 exists to prevent.
- Full voice control of the OS. Voice Access owns that space, and Voice Access
  puts accessibility first.
- Being a coding harness. The console stays a task runner, and deep coding
  workflows belong to dedicated tools. The winapp harness routes what's needed.
- Cloud sync and accounts. First Mate is local-first and encrypted at rest,
  and the key lives on your TPM or nowhere.
