# First Mate Roadmap

First Mate is a desktop assistant that _does tasks on your PC_, not a chat
window. Every item below serves that goal, and the goal has three parts. Each
job needs the right agent for that job. Skills accumulate value across tasks.
A trust layer makes it safe to point an agent at a machine full of personal
data.

This is a **living document**. The document records direction and build order.
§1–5 describe features, §6 records the reception evidence, §7 lists derived
capabilities, and §8 lists the build order. The last section lists the
non-goals. When priorities move, update this document, because the document is
only as accurate as its last update. Design detail lives in the documents this
one points to, not here.

## 1. Agent Profiles

**Problem.** One global system prompt and one skill set produce one agent that
sends every tool on every task. That agent is mediocre at everything.

**Design sketch.**

- A profile is `{name, system_prompt, model_binding, skill_allowlist,
  tier_default, privacy_floor}` stored under `~/.firstmate/profiles/*.toml`.
- The existing per-conversation system prompt resolution
  (`skills::get_system_prompt`) becomes profile-scoped.
- The console header shows a profile switcher, and `use profile <name>` is a
  routed command.
- Each conversation records the profile it ran under, so history shows which
  agent said what.
- Each profile binds a model. For example, a local model suits anything that
  touches files, and a strong cloud model suits research. That binding makes
  §3 mostly declarative.
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
- The drafted frontmatter lists name, description, and when-to-use, and the
  body records the procedure the agent just executed.
- Nothing activates on its own. The console surfaces proposed skills, and the
  user approves them, with edits allowed. Approval moves the skill into the
  active profile's skill set.
- Approval is the trust boundary. A self-modifying agent with no review step is
  how prompt-injection becomes persistence.
- Profiles own skills (§1). A skill proposed under `sysadmin` is invisible to
  `general` unless the user promotes that skill explicitly.
- A dedup guard runs before each proposal. The agent must diff against existing
  skills, and the tool refuses a near-duplicate. That guard stops a profile from
  growing back into one agent that sends every tool on every task.

**Open questions.** Versioning of an approved skill that the agent revises is
open, along with the re-approval flow. The rule for retiring a skill that has
gone stale is also open. No scoring design supports that rule yet.

## 3. Sensitive-Info Obfuscation

**Problem.** A public model must never see credentials, addresses, health data,
or financial data. The dividing line is not "local vs remote". The dividing
line is _who receives the data_. An agent on the same LAN is local, and you may
still not want that agent reading your payroll. A hosted model reached through
your own proxy is remote, and that model is still yours. One control governs the
pass: you choose what that receiver may see. An address never decides that
answer.

**The switch.** The Settings UI shows one checkbox per provider, named `Hide
sensitive information`. The label says what the checkbox does. The checkbox
starts checked for every public provider. A user-added endpoint also starts
checked, because you did not inspect that address. `Spec::hide_sensitive_default`
holds that choice, and a roster row that omits the key starts checked. The three
on-machine rows set `hideSensitiveDefault: false` in `providers.json`.
`Spec::is_local` stays the hint that shows the Base URL field.

**Design sketch.** A reversible tokenize pass runs on every outbound request
whose provider has the switch on. That pass runs detect → tokenize → send.
Detokenize runs at display, and at tool execute. A tool gets real values, so its
output needs no detokenize pass. Detokenize never happens in the model.
Detection stacks four controls: deterministic shape and checksum checks, a
user-seeded entity registry, provenance marks, and a decision server. You run
the decision server,
and the app calls that server over the typed-decision API.
Over-redaction is the accepted bias, because detokenize restores the real text
everywhere you can see it. The full design lives in `docs/roadmap/REDACTION.md`.

**Dependency.** Profile model bindings (§1) decide which provider a turn uses,
and that provider's switch states whether redaction runs. Encryption at rest
decides what is worth protecting on disk instead.

## 4. Privacy — two axes, one Settings section

The transit pass and the screen answer different threats, so the product exposes
two controls, not one ladder. A receiver sees one provider's text. A reader
behind the user sees every provider's text. The Settings UI shows both controls
under a `Privacy` section, away from the provider and model fields.

| Control | Scope | Levels | Threat it answers |
| --- | --- | --- | --- |
| **Hide from the LLM** | per provider | `off`, `exact`, `wide` | A receiver the user does not trust |
| **Hide on screen** | global | `show`, `blur`, `hello` | A reader of the screen, screenshots, screen sharing |

- `exact` runs §3 Layers 1–2. It redacts a value with a checksum, or a value in
  the entity registry. `wide` adds Layer 4 context, which also tokenizes a
  benign id that sits near a trigger word. `off` sends real values. A receiver
  the user owns is the only case for `off`.
- `blur` renders tokens or blurs in the console, and one click reveals one item.
  `hello` adds the Windows Hello check that the reveal session below defines.
  `show` is today's behavior.
- One dependency crosses the two axes, and it is disk, not threat.
  Click-to-reveal needs the real value on disk. §3 keeps real values in the
  stored transcript, so encryption at rest is a precondition for `blur` and `hello`.
- A profile may raise either axis. A profile may not lower either axis.
- Before `wide` ships, the entity registry must ask before it grows. A false
  positive that enters the registry matches that value everywhere, forever.
  `docs/roadmap/REDACTION.md` "Detection layers" holds the layer detail.
- A later control may store only redacted text. That control removes the real
  value from disk, so it also removes click-to-reveal. The UI must state that
  loss beside the control.

### The reveal session

`hello` gates display, not the key. One state governs the console: the reveal
session is open or closed. While the session is closed, the console shows the
tokens from the §3 pass. The API key field shows a masked value. While the
session is open, the console shows real values.

| Event | Effect |
| --- | --- |
| The user presses Reveal | The app asks for Windows Hello, then opens the session |
| The console window hides | The app closes the session |
| The session lifetime elapses | The app closes the session |
| The user presses Extend | The app asks for Windows Hello, then resets the lifetime |
| The user presses End reveal | The app closes the session |
| The process exits | The session ends, because the flag lives in memory |

- The lifetime is 10 minutes, and it is the backstop. The app has no idle timer,
  because no input does not mean no person: the user reads what the agent wrote.
  The user holds the explicit End control, and the hide rule covers the rest.
- The window has `alwaysOnTop: true`. A visible console therefore stays readable
  while the user steps away, until the lifetime elapses. The lifetime bounds
  that time, and the UI states the bound instead of implying safety.
- Hiding the window closes the session, so the tray icon needs no state. No open
  session can exist while the console is hidden.
- The hotkey and the close button both hide the window through
  `window::hide_window`. The reveal-session hook sits in that one function, so
  both paths close the session.
- Extend costs a Windows Hello check. A free extension would keep real values
  shown forever after one check.
- Expiry closes display only. The file key stays unlocked, because the app must
  read its own files. An expiry is not a data lock, and the UI must not say one.
- The mask must come from the render path. The transcript uses a virtualized
  scroller. A mask on the document alone would show a real value again when the
  row scrolls back into view.
- The agent cannot open, extend, or close a session. A passkey answers a
  physical-presence question, and the agent has no answer to give. Contrast
  `delevate`, which the agent may call, because the agent can finish the work
  that needed elevation.
- The app logs one line per session event, with the reason it closed.
- `CheckAvailabilityAsync` reports a machine with no passkey configured. On such
  a machine the app keeps `blur` behavior, and allows click-to-reveal. The
  Privacy section states that no passkey check runs on this machine.

## 5. Workspaces

**Problem.** The agent's work product has no home. One disk-inventory task wrote
nine `.ps1` scripts and ten `.txt` reports into `%USERPROFILE%`. Its closing
message spent 19 filenames on an apology. A screenshot landed as
`screenshot.png` in a git working tree, because `winapp ui screenshot` defaults
to the app's working directory. `write_file` accepts any path, so each task
picks its own litter. Cleanup then becomes a delete the user must audit file by
file.

`~/.firstmate/tmp` and `~/.firstmate/output` already exist, and `trim_dir` caps
each at 50 entries at startup. That is storage, not a workspace: the files carry
no link to the task that made them, and nothing sends the agent's writes there.

**Design sketch.**

- A conversation owns a workspace at `~/.firstmate/workspaces/<conversation-id>/`.
  A run gets a subfolder of it, so one task's files list and clear as a set.
- The workspace is the default destination, not a jail. When the user names a
  destination, the agent writes there.
- `write_file`, the screenshot path, and spilled tool output resolve to the
  current run's workspace when no destination is given. `output/` becomes that
  folder's spill subdirectory.
- The console shows the run's workspace as one link. The console answers "what
  did it leave behind" in one click. Clearing that folder is one action, not an
  inventory of filenames.
- Deleting a conversation deletes its workspaces, which replaces the 50-entry
  trim with a lifetime the user understands.
- Skill scripts are not workspace files. A skill's `scripts/` folder is durable
  content (§2), and a workspace holds only what one task produced.
- The winapp skill passes an explicit output path for every screenshot. The
  image then lands in the workspace, not in the current directory.

**Open questions.** Whether a workspace survives the conversation that made it
is open. Whether the user can point a workspace at a real project folder is also
open. That choice puts the agent's scratch beside the work it is doing.

## 6. The Copilot lesson: usefulness, not features

The 2025–2026 reception evidence for Copilot leaves four jobs standing. The
four are: recover lost context, use what's in front of me, bounded delegation,
and low-friction intent capture. Users consistently reject one combination:
plaintext at rest, plus an unconfirmed action that can change any file or
account. Encryption at rest, §3, and the trust tiers answer those two complaints.
Evidence and
full analysis: `docs/roadmap/COPILOT-ANALYSIS.md`.

## 7. Derived capabilities

Each entry states its job first. First Mate chooses the mechanism.

**Next — Harness trust tiers.** _(job: bounded delegation)_ Users delegate only
when the harness grades the trust they place. Today the exec harness runs with
the user's full privilege. The tiers are: 0 read-only, 1 workspace-scoped
writes, 2 destructive with per-call confirm, and 3 anything sent out-of-band. A
profile default picks
the tier. `route.rs` already gates program disclosure, and these tiers extend
that same gate. Graded tiers are the precondition for everything else below,
and for §2's self-authored skills running unattended.

**Later — Continuity.** _(job: don't lose what I knew)_ This capability is not
a searchable archive for humans. The _agent_ resurfaces relevant history at the
moment of relevance. An example: "you restructured these logs in March — same
failure?". First Mate already owns the raw material: every conversation, every
spilled output, and every skill execution. Index that material with embeddings
over the encrypted stores, and let the active profile query its own past. There is
no ambient capture. Continuous screen recording was Recall's most expensive and
least valued choice. First Mate serves the same job from work product First Mate
already owns.

**Later — Focus transfer.** _(job: use what's in front of me)_ The summoned
command gets an implicit argument. That argument is the active selection, the
clipboard, or the active window. The command might summarize, translate, "file
this", or "draft reply".
The result lands back in the source app, not in a panel the user has to visit.
The hotkey console already removes the step of leaving the current application.
That extra step removed most of Click to Do's value. The first missing piece is
UIA selection capture, and the winapp harness already speaks UIA. The other
missing piece is a way to write the result back into the source application.

**Later — Voice input.** _(job: capture intent at speed of thought)_ The user
holds the summon hotkey, talks, and releases. The console then shows the
transcribed request. Speech-to-text is local only, with whisper.cpp or the
Windows Speech API, because voice must never cross the §3 boundary. This
capability is input-only. Full voice control of the OS belongs to Voice Access,
and Voice Access does that well.

**Later — Scheduled delegation.** _(job: this chore runs for me, and I approve
it)_ Bounded delegation ends here. An approved skill binds to a trigger: a
schedule, a file arrival, or a window title. The bound skill runs under its tier
and reports into the console. The approved §2
skill is itself the approval artifact. That is why self-authored skills must
ship with review, versioning, and tier binding before automation exists.

## 8. Execution checklist

The dependency spine is **A → B → C → D → E → F**. Redaction comes first,
because that work is self-contained and protects every turn whose receiver the
user does not trust. Phase G (§5) has no dependency, because its scratch
directories already exist and need only routing. Profiles come next, because
three items depend on a profile.

Two shipped items sit outside that spine, because they predate this checklist.
Context compaction in `src-tauri/src/context.rs` folds a long conversation before
the window overflows. `docs/COMPACTION.md` holds the design. Encryption at rest in
`src-tauri/src/crypto.rs` encrypts every store file. `docs/ENCRYPTION.md` holds
the design.

### Phase A — Sensitive-info redaction (§3)

- [x] Trust registry: `hide_sensitive` on the resolved provider is the single
      trigger source. The Settings UI checks that box for a public provider and
      for a user-added endpoint. An endpoint address is never a privacy
      decision, and `is_local` stays a Base-URL hint.
- [x] `redact.rs` layer 1 replaces each detected secret with a `⟦CLASS_N⟧`
      token. Layer 1 detects Luhn+BIN cards, IBAN mod-97, ABA mod-10, SWIFT, and
      SSN validity. Layer 1 also detects crypto checksums, API-key shapes, JWT,
      email, phone, and `%USERPROFILE%` paths.
- [x] One token map serves every conversation and every provider. `redact::init`
      points that map at the sealed `token_map.json` beside `settings.json`, so a
      reopened conversation resolves the markers that conversation stored.
      Detokenize stays stream-safe across placeholder boundaries in SSE chunks.
      `redact::tests::a_token_from_an_earlier_run_resolves_from_the_sealed_file`
      and `redact::tests::the_stored_table_holds_no_plaintext_value` prove the
      sealed table.
- [x] Detokenize runs at display, and at tool and action execute. A tool receives
      real values, so its output needs no detokenize pass.
- [x] A repair pass fixes mangled tokens by fuzzy match on `⟦…⟧` shapes.
- [x] `entity_registry.json` holds the exact strings the pass replaces, and
      First Mate encrypts that file. `settings::registry` reads that file, and
      each run seeds the pass from it.
- [ ] Profiles seed that file, and the registry grows by confirmation from
      layer-1 and layer-3 hits. The seeding half needs Phase B.
- [ ] Provenance marks sensitive-path reads, UIA field names, and banking
      origins. The harness blocks password-manager clipboard reads.
- [ ] Each class has its own toggle, and a disclose-once override exists.
- [x] The "Redacted N items" chip opens a class breakdown, and that breakdown
      never shows values.
- [x] Tests cover round-trip fidelity and untouched code blocks.
      `every_class_round_trips` and `a_fenced_config_block_returns_byte_identical`
      hold those rules.
- [x] Three tests prove that the sealed token map survives a restart and stays
      closed to `read_file`:
      `crypto::tests::a_store_read_never_opens_the_token_map`,
      `redact::tests::a_token_from_an_earlier_run_resolves_from_the_sealed_file`,
      and `redact::tests::the_stored_table_holds_no_plaintext_value`.
- [x] Settings holds the decision-server URL, the enable box, and the flag
      threshold. Unset or unchecked means the exact rules alone.
- [x] The client posts one `noul` question per message to `POST {base}/v1/systemone`.
      A non-loopback URL needs your confirmation before the first message leaves.
- [x] The Settings `Test` action probes one benign text and one text holding a
      credential.
      That action calls a server unusable when both answers land at or above 0.8.
- [x] An unreachable scorer holds the send. The console names the host and the two
      ways forward: start the server, or turn off the Privacy Model. A debug build
      records the one `GATE:` line that names the failing address.
- [x] The Privacy Model judges the text First Mate puts on the wire: your new
      message, and each tool result. It never judges the instruction set, text
      the provider authored, or text that already left the PC.
- [x] The audit card holds a flagged message: the message, its score, your
      selection, then approve or deny. Deny holds the send, and you write the
      next message. `docs/roadmap/REDACTION.md` holds the design.
- [x] The scorer sends one message per request. A message over the server budget
      gets overlapping windows, and one flagged window flags the message. The
      client reads the budget from `GET {base}/v1/models`, and a 413 or 422
      answer shrinks the window. The scorer sees every character of the message.
- [ ] The configured server must accept image input. When the server accepts no
      image, the console says that the image went out unjudged.
- [ ] `criteria` in `decide.rs` states what counts as a yes and what counts as a
      no. The wording of the question and the wording of the criteria are the only
      tuning. Re-measure the two `Test` scores after any change.
- [ ] First Mate injects `<!>` into the message row at the position of the value
      that already left. The console renders the marker highlighted.
- [ ] The leak report holds one sealed row per leak. Each row links into the
      console at the message that carried the value.
- [ ] The console exports the leak report as a PDF. The user may protect that PDF
      with a generated password or a password of their own.
- **Accept:** a checked box produces the same tokenized request on a local
  endpoint and on a public endpoint. That request carries a fake card, a JWT,
  and an email. A registry-seeded account number with no checksum also leaves
  tokenized. That turn renders correctly on return. An unchecked box leaves the
  text unchanged. With a decision server configured, a private sentence that no
  pattern matches still leaves the machine tokenized. With no server, that
  sentence leaves the machine unchanged. A flagged message stops before the send
  and waits on the audit card. A row that an earlier run sent never goes to the
  Privacy Model a second time. A value that already left the PC is named by its
  class, and the notice names no value. A long message whose flagged text sits
  past the first window still stops before the send.

### Phase B — Agent profiles (§1)

- [ ] `profiles.rs` defines the TOML schema `{name, system_prompt, model_binding, skill_allowlist, tier_default, privacy_floor}`.
- [ ] `get_system_prompt` becomes profile-scoped, and conversations record
      their profile.
- [ ] The built-in profiles are `general` and `sysadmin`, and `sysadmin`
      defaults to a local model.
- [ ] The console has a profile switcher, and `use profile <name>` is a routed
      command.
- [ ] Model binding, §3's trigger, and §2's skill ownership each read the
      profile.
- **Accept:** two profiles with different models and skills, verifiable from a
  conversation's recorded profile.

### Phase C — Privacy section (§4)

Encryption at rest ships, and the screen axis depends on it, because the
transcript holds real values.

- [ ] The Settings UI gains a `Privacy` section, and the `Hide sensitive
      information` checkbox leaves the provider form.
- [ ] The `hide_sensitive` map of booleans becomes a map to `off`, `exact`, or
      `wide`. `true` migrates to `exact`, `false` migrates to `off`, and a
      provider outside the roster still lands on `exact`.
- [ ] The roster key `hideSensitiveDefault` becomes `redactionDefault`, and the
      three on-machine rows keep `off`.
- [ ] The registry asks once before it grows, and that gate opens `wide`.
- [ ] The screen axis adds `blur`: tokens in the console, one click reveals one
      item, and the log stores the real text, which First Mate encrypts.
- [ ] The `hello` level adds the reveal session: one Windows Hello check opens
      it, a hide or a 10-minute lifetime closes it, and Extend costs a check.
- [ ] The app logs one line per session event, with the reason it closed.
- [ ] A profile holds a floor for each axis, and can raise either axis but never
      lower it.
- **Accept:** a screenshot under `blur` contains zero sensitive values. A hidden
  console shows masked values on the next summon until one Hello check passes. A
  provider set to `off` sends the real value.

### Phase D — Agent-defined skills (§2)

- [ ] The `propose_skill` tool writes drafts into the `skills/_proposed/`
      staging directory.
- [ ] The console shows an approval surface with approve, edit, and reject, and
      nothing activates automatically.
- [ ] A profile owns its skills, and promotion to another profile is explicit.
- [ ] The dedup guard blocks near-duplicate proposals, and a versioning and
      re-approval flow exists.
- **Accept:** the agent proposes, you approve, and the next session uses that
  skill under one profile only.

### Phase E — Harness trust tiers (§7 "Next")

- [ ] The exec harness enforces tiers 0–3, and that enforcement extends the
      gate that `route.rs` already implements.
- [ ] The profile `tier_default` is enforced behavior, not stored metadata.
- [ ] Tier 2 and above show a confirm UI on each call. Tier 3 gates detokenize
      at execute.
- [ ] Approval gates skill actions: the user confirms before the agent clicks or
      types, under tier enforcement.
- **Accept:** a `general`-profile agent cannot delete outside its workspace
  without an explicit confirm.

### Phase F — Derived capabilities (§7), in this order

- [ ] Continuity adds embeddings over encrypted conversations and
      outputs, plus agent-side relevance queries. The item excludes ambient
      capture.
- [ ] Focus transfer (needs B) adds UIA selection capture. The focused
      selection, clipboard, or window becomes the implicit argument. The agent
      writes the result back to the source application.
- [ ] Voice input (needs A) adds push-to-talk on the summon hotkey, and
      speech-to-text is local only.
- [ ] Scheduled delegation (needs D + E) binds an approved skill to a trigger.
      The bound skill runs under its tier.

Cross-cutting: every F capability respects the §3 and §4 boundary. Voice,
embeddings, and selections leave the machine only when the redaction pass ran on
that turn. Nothing in D or F ships without its review gate
and its tier gate.

### Phase G — Workspaces (§5)

- [ ] A conversation gets a workspace folder, and a run gets a subfolder of it.
- [ ] `write_file`, the screenshot path, and tool-output spill resolve to the
      run's workspace when no destination is given.
- [ ] The winapp skill passes that path to every screenshot, so no image lands
      in the app's working directory.
- [ ] The console links the run's workspace, and one action clears it.
- [ ] Deleting a conversation deletes its workspaces, and `trim_dir` becomes a
      backstop instead of the only lifetime rule.
- **Accept:** a task that writes ten files leaves ten files in one folder that
  the console links. `git status` and `%USERPROFILE%` stay clean.

## Non-goals

- Ambient screen capture, which is a Recall-style recorder. §7 serves the
  continuity job from First Mate's own work product. Continuous capture would
  reintroduce the exposure that encryption at rest exists to prevent.
- Full voice control of the OS. Voice Access already does that job, and Voice
  Access puts accessibility first.
- Being a coding harness. The console stays a task runner, and deep coding
  workflows belong to dedicated tools. The winapp harness routes what's needed.
- Cloud sync and accounts. First Mate is local-first and encrypted at rest,
  and the key lives on your TPM or nowhere.
