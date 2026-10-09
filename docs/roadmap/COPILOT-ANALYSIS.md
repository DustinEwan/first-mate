# The Copilot Lesson: usefulness, not features

This document is the market analysis feeding the roadmap. Every § reference
points to `docs/ROADMAP.md`. The evidence is 2025–2026 Windows AI reception.
That evidence is dated by design. Re-read this evidence before extending the
conclusions.

Microsoft's 2025–2026 AI features are mechanisms. Each one serves some underlying
user need. This document calls that need a _job_.

Users never name that need. They praise or criticize how the feature arrives
instead. That feedback is about the feature's defaults, its privacy posture, and
the extra steps it forces on the user. This document therefore derives each job
from the pattern in that feedback. Four jobs survive the evidence:

| Job (the need the user actually has) | Evidence from reception | What the job rules in / out |
| --- | --- | --- |
| **"Don't make me lose what I already saw/knew."** Recovery of lost context: the thing I read, the fix I did, the decision I made last month. | Recall: constant backlash, yet devoted actual users ("first truly great and useful AI productivity tool"). The backlash tracked the _plaintext-by-default posture_, not the recovery value. The redesigned Hello-gated version kept the praise and lost some of the fear. | The job is real and worth serving. The job does NOT imply an ambient recorder. An ambient recorder is one way to serve that need, and it brings exposure that the need does not require. |
| **"Use what's already in front of me."** The current selection/window should be the argument to the next action, without copy-paste rituals. | Click to Do: reactions were mixed when it was shown as a demo, and it was genuinely valued as an accessibility surface. Making the user leave the app and paste into a panel removed most of the value. | The job is real, and the extra steps in the delivery are the failure mode. A globally hotkeyed assistant already avoids those steps. |
| **"Take this chore off my hands — I'll approve the plan."** Offloading repeatable multi-step drudgery while the user keeps approving each plan. | Copilot Actions: adoption talk is almost entirely about the containment model (workspace, low-privilege account, scoped access). Users weighed that containment model, not the demonstration. Nobody brags about one-shot agent tricks. | Delegation with explicit, bounded trust is the product. Unbounded autonomy reads as reckless and convinces no one. |
| **"Capture my intent at the speed of thought."** Low-friction expression for requests too long to type. | Voice Access: the most uniformly positive sentiment of any Windows AI feature, adopted far beyond its accessibility audience. Nobody wants voice _control_ in the middle of a task. They want voice _input_ instead. | Voice as an input modality. Whole-OS voice control is someone else's product. |

Users' negative feedback is just as clear. Users consistently hate only two
things: plaintext-at-rest storage, and an unconfirmed action that can change any
file or account. Roadmap §3–4 and the trust tiers answer those two complaints.
The roadmap can therefore pursue the four jobs above without taking on the same
plaintext-at-rest exposure.
