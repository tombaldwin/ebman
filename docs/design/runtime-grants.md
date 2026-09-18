# Runtime grants: permission without a restart

**Status:** design note, unbuilt. Written 2026-09-18, after
`--allow-writes=verb,verb` shipped in 0.40.0 and immediately ran into
the objection below. It reshapes stage 5 of
[protection-levels.md](protection-levels.md) rather than following it:
that note assumed the ceiling was static and put the flexibility in
named levels. This one says the ceiling should be static **and wide**,
with the flexibility in time-boxed grants.

## The problem

0.40.0 made the write grant narrow. It did not make it *reachable*.

Getting `dlq_delete` today means: stop, find where the MCP server is
registered, edit the args, restart the client, resume. The maintainer's
own reaction on being told that is the whole motivation for this note:

> If I was using ebman via the front end without issue, then found that
> I had to restart my session and change flags, I'd probably stop
> bothering.

That is an adoption verdict, not a UX quibble. The flag is paid for at
exactly the moment the tool is proving useful — the operator is mid-task,
the agent has just diagnosed something, and the next step is a context
switch out of the conversation into a config file. Most people will not
make that trip, and the ones who do will make it once and then leave the
grant permanently on, which is the outcome the narrow flag existed to
prevent.

So the flag has managed to be both annoying and ineffective: annoying
because it interrupts, ineffective because the interruption pushes
people toward a standing grant.

## What the flag was right about

Keep the reasoning before discarding the mechanism.

A **ceiling set outside the request** is the property worth protecting
(Principle 5 in the protection-levels note). ebman's MCP surface pulls
untrusted text into the agent's context: `worker_queues --peek` returns
raw SQS message bodies, which are whatever the operator's own
application POSTed. 0.40.0 shipped `mcp.peek_bodies` because that
content is sensitive enough to withhold; it is equally untrusted enough
to carry instructions. If a dead-lettered body can talk an agent into
proposing `terminate`, the only defence that does not depend on a tired
human reading a prompt carefully is a ceiling that never had `terminate`
in it.

Elicitation alone does not give that. **Elicitation authorises an
instance; a ceiling bounds the space.** They are different jobs and the
design needs both.

## The shape

**The flag stops meaning "may do" and starts meaning "may ask about".**

Set once at install, deliberately wide, because a grant to *ask* costs
nothing. `--allow-writes` with no verbs becomes the sensible default
rather than the reckless one.

**Grants happen at runtime, and expire.** Two routes:

1. **Operator issues one.** `ebman grant dlq_delete --env poly-batch
   --ttl 1h`, or `:grant` in the TUI. This reuses the cross-process
   marker machinery that `:freeze-deploys` already uses — the TUI
   writes, the MCP server reads it live on the next gate check, with the
   pid-liveness and pid-reuse handling already solved in `src/freeze.rs`.
2. **Agent asks.** MCP elicitation: the server pauses mid-call and asks
   the client to put a prompt in front of the operator.

Either way the server then emits `notifications/tools/list_changed`, the
client refetches, and the write tools appear **mid-session**. No
restart, no file.

**Route 1 does not depend on elicitation.** This is the load-bearing
property of the design and the reason it is buildable now: if a client
does not support elicitation, the operator approves in the TUI or a
terminal instead, and the feature still works. Elicitation is an
upgrade, not a prerequisite.

### The plan IS the permission request

The best idea here is free, because the machinery already exists.

Every write is two-phase: a plan, then a confirm. **Planning is
read-only** — it validates, resolves the queue, names the message — so
it can be allowed with no grant at all. That means the plan can be
produced *before* permission is sought, and then used as the content of
the request.

The prompt an operator sees stops being a category judgement:

> Allow `dlq_delete`?

and becomes a factual one:

> Delete message `d3b07384…` from `poly-batch-dlq` — EB task "Remove
> unattended jobs", dead-lettered 9h ago, receive_count 4. Queue depth
> 12.

The second is a decision a person can actually make at 11pm. The first
is one they will approve on reflex. This costs nothing to build: the
plan already renders in exactly that form.

#### But a plan is silent about the stakes

**Reviewed 2026-09-18 and this section was wrong as first written.** A
plan describes the operation, and the reason to refuse usually lives
outside the operation.

The example above is accurate, specific, and complete about the action
— and if it had appeared in front of the maintainer that morning he
would have approved it, because nothing in it says *this message is the
only live fixture for an end-to-end test of a feature shipped an hour
ago*. That was the actual reason to keep it. It is not a property of
the message, the task, or the queue. It is a property of the week.

So the failure mode is not an under-specified plan. It is a plan fully
specified about mechanics and silent about stakes, which is **more**
dangerous than a vague one: it reads as complete, and a prompt that
looks like it contains everything relevant discourages the pause in
which the operator remembers what it does not contain.

**The plan must state what the action FORECLOSES, not only what it
does.** For a delete: *this message will not be readable again, and it
is the only one in the queue.* Derivable from state we already hold,
one line, and the sentence that would have caused the pause.

This is [ARCHITECTURE.md](../../ARCHITECTURE.md) rule 6 — *a result must
carry its own negative space* — applied to a plan rather than a result.
The same rule that makes `peeked` report whether we looked makes a plan
report what it destroys.

#### The plan must stay server-authored

A permission prompt written by the party requesting permission is a
persuasion surface regardless of intent. The worry is not a scheming
agent; it is the ordinary gradient where an agent that writes plans, and
notices which plans get approved, writes more of those. Nobody has to
decide that for it to happen.

**ebman already has the right property and the design must protect it
rather than build it.** Every field in a plan — action, env,
application, health, status, queue url, message id, task name, recent
events — is rendered server-side from AWS or fixture state through
`util::json_string`. There is no agent-supplied prose anywhere in a
plan. The agent chooses *which* thing, never how it is described.

Which means the reviewer's suggested addition — let the agent supply a
short reason, marked as the requester's claim — is the one part of this
that would **introduce** the risk rather than contain it. Recommendation:
do not add it. The agent's argument already exists, in the conversation
the operator is reading. Copying it inside ebman's frame gives it
authority it has not earned, and the operator loses the ability to tell
the tool's account of the world from the requester's case for acting on
it. Keep those in different places, which is where they are now.

#### Where it becomes noise: volume, not detail

Rich plans survive being read three times and stop being read at the
fourth. Forty dead-lettered messages from one bad deploy, each with a
beautifully specific prompt, and by the fifth the operator is clicking
through a form.

So, explicitly: **plan-as-prompt is the shape of the FIRST ask, and
approving it issues the window.** It is not the shape of every
subsequent act inside that window, or the window buys nothing. The
time-boxed grant is what stops detail from decaying into ceremony.

And a repeat should announce itself: *"this is the second time you have
been asked about this message"* is cheap, and a repeat is the signal
that either the grant is not sticking or something is looping.

### Scope grants to env + verb + TTL

Not just verb. Incidents are about one environment, and
`dlq_delete on poly-batch for 1h` is both tighter and closer to how the
operator is already thinking. It also collapses most of the injection
concern: a persuaded agent still cannot act outside the environment the
human named.

### Declaring an incident revokes outstanding grants

`:freeze-deploys` / `:incident` already exist and are already read
cross-process by the MCP server. Composing them is nearly free and is
the right instinct — the moment things go wrong is the moment ambient
permission should lapse, not persist.

### Visibility, or we have traded one failure for its mirror

The flag's failure mode is *permanent and forgotten*. Dynamic grants
risk the opposite: *invisible and unaccounted*. Both are the same
defect — the operator cannot answer "what can this thing do right now?"

So: live grants visible in the TUI (header pill or `:grants`), and the
audit line records **which grant authorised each write**, not just that
a write happened. That closes the loop — "granted at 23:04 for one hour,
used twice by 23:12" should be reconstructable from the log.

### `ebman mcp doctor`

The server learns at handshake exactly what the client declared. It
should say so:

    Claude Code — elicitation: no · tools/list_changed: yes
    → operator-issued grants will work; agent-initiated asks will not.

This is an adoption fix more than a debugging one. It is the difference
between "this feature is broken" and "your client does not carry that
half", and an agent reporting the former is a support cost that never
had to exist.

The pattern is established and has already cost something. On
2026-09-17 the TUI's update checker wrote
`newer ebman released on crates.io current="0.36.0" latest=0.38.0` to
the log three times. The information was correct, timely, and in the
right file — and had no route to the agent, which spent that period
reporting capability gaps against a binary two releases old. The
version line in the `instructions` block exists because of that. `mcp
doctor` is the same fix for capabilities: **a fact with no route to its
consumer is not a fact that consumer has.**

## What this is not

**Not a security boundary.** Anything that can write files can write the
grant marker, exactly as it could edit a config key. This protects
against mistake, drift and momentum — which is what actually goes wrong
— and the docs must say so plainly rather than implying more. ebman's
existing line holds: the boundary is IAM.

**Not per-action prompting by default.** If forty messages dead-letter,
prompting per message produces rubber-stamping, which is worse than a
scoped grant because it *looks* like control. A bounded grant — this
env, this verb, one hour — is more honest and safer than forty prompts
nobody reads. Per-action asks are right for the destructive tail
(`terminate`, `dlq_purge`), not for routine work.

## Open questions

Two are client behaviour and cannot be settled from inside ebman. Both
should be answered before building, not designed around:

- **Does Claude Code declare elicitation support?** Decides whether
  route 2 is a primary path or an upgrade. The instrument that measures
  this was dead until 0.40.0 — `ebman mcp serve` had no file logging at
  all, so the one `tracing::` call on the surface wrote nowhere. It now
  records `elicitation=<bool>` per connection. **No measurement yet**:
  every line currently in the log is a synthetic probe.

  *Documentary* evidence, which is not the same claim: Claude Code's
  own MCP documentation describes elicitation dialogs as implemented,
  in passing, while explaining call backgrounding — "the server is
  blocked on your input, not slow, so Claude Code defers the move until
  the dialog closes". A client that has worked out the interaction
  between elicitation and backgrounding is not one that declines the
  capability. Predicted value: `true`. If it comes back `false`, Claude
  Code implements the dialogs without declaring the capability to stdio
  servers, which would itself be worth knowing.

  Design elicitation as the primary path on that basis, but do not skip
  the check: *documented support* and *a declared capability on this
  transport* are different claims.

  **The measurement needs the maintainer, not an agent.** It requires a
  real Claude Code client to connect to ebman 0.40.0, and restarting
  the client is a human action — an agent cannot restart the session it
  is running inside. One connection writes the line.
- **Does the client refetch on `tools/list_changed`?** If it ignores the
  notification, the feature degrades to "the agent must already know the
  tool name" — workable but poor, and worth knowing first.

And one that is ours:

- **Does a grant survive a server restart?** A marker file says yes by
  construction. TTL makes that mostly safe, but the freeze marker's
  pid-liveness logic exists because "mostly" was not good enough there
  either.

## Cost

A few days, not an afternoon. `src/freeze.rs` supplies the hard part —
cross-process state with liveness and reuse handling, already trusted
for a safety decision. The new work is the grant vocabulary, the TTL,
the `list_changed` emission, the audit correlation, and the TUI surface.
