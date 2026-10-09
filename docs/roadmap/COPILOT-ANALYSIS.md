# The Copilot Lesson: usefulness, not features

This document is the market analysis feeding the roadmap. Every § reference
points to `docs/ROADMAP.md`. The evidence is 2025–2026 Windows AI reception.
That evidence is dated by design. Re-read this evidence before extending the
conclusions.

Microsoft's 2025–2026 AI features are mechanisms. Reception of those features
tells us which _jobs_ underneath them are real. Users cannot articulate the job.
Users praise or punish the packaging instead, and the reader infers the job from
that reaction pattern. Four jobs survive the evidence:

| Job (what the user is actually hiring for) | Evidence from reception | What the job rules in / out |
| --- | --- | --- |
| **"Don't make me lose what I already saw/knew."** Recovery of lost context: the thing I read, the fix I did, the decision I made last month. | Recall: constant backlash, yet devoted actual users ("first truly great and useful AI productivity tool"). The backlash tracked the _plaintext-by-default posture_, not the recovery value. The redesigned Hello-gated version kept the praise and lost some of the fear. | The job is real and worth serving. The job does NOT imply an ambient recorder. An ambient recorder is one costly way to serve that job. |
| **"Use what's already in front of me."** The current selection/window should be the argument to the next action, without copy-paste rituals. | Click to Do: mixed as a spectacle, genuinely valued as an accessibility surface. The chat-app packaging (leave your app, paste into a panel) removed most of the value. | The job is real, and the friction of the delivery is the failure mode. A globally hotkeyed assistant already avoids that copy-paste friction. |
| **"Take this chore off my hands — I'll approve the plan."** Offloading repeatable multi-step drudgery while the user keeps approving each plan. | Copilot Actions: adoption discourse is almost entirely about the containment model (workspace, low-privilege account, scoped access). The buyer is choosing that containment model, not the demonstration. Nobody brags about one-shot agent tricks. | Delegation with explicit, bounded trust is the product. Unbounded autonomy reads as reckless and persuades no buyer. |
| **"Capture my intent at the speed of thought."** Low-friction expression for requests too long to type. | Voice Access: the most uniformly positive sentiment of any Windows AI feature, adopted far beyond its accessibility audience. Nobody wants voice _control_ in the middle of a task. They want voice _input_ instead. | Voice as an input modality. Whole-OS voice control is someone else's product. |

The punishment signal is just as clear. Users consistently hate only two
things: plaintext-at-rest storage, and an unconfirmed action that can change any
file or account. Roadmap §3–4 and the trust tiers are the answer to that
punishment signal. The roadmap can therefore pursue the four jobs above without
taking on the same plaintext-at-rest exposure.
