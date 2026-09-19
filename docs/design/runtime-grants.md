# Runtime grants: permission without a restart

**Status:** design note, unbuilt. Written 2026-09-18, after
`--allow-writes=verb,verb` shipped in 0.40.0 and immediately ran into
the objection below. It reshapes stage 5 of
[protection-levels.md](protection-levels.md) rather than following it:
that note assumed the ceiling was static and put the flexibility in
named levels. This one says the ceiling should come from things the
operator already maintains — IAM, and a config that may only forbid —
with permission granted in the conversation, at the moment of need.

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

## The rule that resolves it: config may only say no

The maintainer's framing, and the cleanest thing in this note:

> Setting flags and restarting isn't good. What would be OK is
> restrictive — a user CHOOSING in advance not to ever allow writes.

That is the asymmetry the design turns on. **Pre-configuring a
restriction is fine**: it is decided calmly, once, it only ever
narrows, and it never interrupts. **Pre-configuring a permission is
not**: it requires predicting what you will need, and the cost is paid
at the worst possible moment.

So the config may only ever say *no*. `safety.envs.prod.read_only =
true` is already exactly the right shape — a standing refusal, set in
advance. What is missing is a global form of it, and the removal of the
opt-in permission entirely.

This keeps the property the flag was protecting. The injection concern
was "bound what can be proposed, so a tired human is not the only
defence" — the restrictions **are** that bound. The difference is that
the operator opts out of what they do not want, rather than opting in
to everything they might need.

## Parity with the TUI is the default

The maintainer's ruling, and the argument that makes the rest cohere:

> The default (ie no flag) should be maximum permissions, just like a
> user using the TUI app.

Same tool, same credentials, same operator. The TUI can terminate prod
today with no flag and no opt-in; requiring one on the MCP surface was
never justified by a different threat, only by unfamiliarity with the
consumer. A surface that is more restrictive than the tool it wraps
teaches people that the restriction is ceremony, which is how they
learn to switch it off permanently.

**What this retires, and what survives inverted.** It retires
`--allow-writes` as a permission — shipped in 0.40.0, designed away the
following day, which is the right outcome but should be recorded rather
than buried. `WriteScope` itself survives, inverted: the same type,
parser, two-layer gating and tests, re-read as a **restriction** —
"only these verbs may ever be asked about" — which is config-may-only-
say-no shaped. The default flips from `None` to `All`.

**The constraint that comes with it, and it is a hard one.** Under the
previous design, a missing approval gate left the operator *safe by
default*: no elicitation meant falling back to a flag. Under this one a
missing gate leaves them *open by default*. The TUI's gate is that a
human is typing, and that `:terminate` demands the environment name
back. The MCP surface's gate is the client prompt or elicitation — and
neither has been measured.

So: **verify the gate before flipping the default, not after.** Step 2
of the implementation order below is conditional on knowing that either
the client prompts on `destructiveHint`, or elicitation is available.
If neither holds, the choice is between open-with-a-loud-warning and
holding the flip until there is a gate — a judgement to make with the
measurement in hand, not in advance of it.

It also means existing users upgrading move from read-only to
write-capable. That belongs at the top of a changelog, not in a
footnote.

## The shape

**The unit of authorisation is the operator's REQUEST.** Not the
individual action, and not a span of time.

That is the maintainer's criterion and it is the thing the rest of this
note failed to settle:

> If I ask to delete certain messages, 1 confirmation is fine. More
> than that and it's easier to do it myself — so what's the point in
> the tool?

A safety model that drives the operator off the tool has protected
nothing. Per-action prompting fails that test at four messages. A
time-boxed window passes it by authorising actions nobody has yet
described, which fails a different test.

So: *"delete messages A, B, C and D"* produces ONE plan covering
exactly those four, ONE confirmation, and dispatches exactly those
four. Nothing outside the enumerated set is authorised, and the
authority expires the instant it is used.

### Why this beats both alternatives

- **No fatigue.** One ask per thing the operator asked for.
- **No unsupervised future.** A window authorises actions nobody has
  described; this authorises a list and nothing else.
- **A stronger injection bound than a window, not weaker.** Everything
  that can happen is named in the text the operator approves. A
  persuaded agent cannot act outside the list, because the list *is*
  the authorisation. Under a window it could.
- **It survives the mechanical/discretionary test** that killed
  windows: the operator says yes to a specific and complete
  description, which is exactly the mechanical yes a plan can carry.

### What already exists, and the one thing that does not

The two-phase protocol IS request-as-unit approval, conversationally:
the agent plans, the plan lands in the transcript, the operator reads
it and says go, the agent confirms. That works today.

What is missing is that **nothing forces the operator into the loop**.
An agent can plan and immediately confirm without ever surfacing the
plan. The protocol is honour-system, and an honour system is not a
gate.

**That is what elicitation buys, and it is the whole of what it buys.**
At confirm time the server asks the operator directly, showing the
plan, rather than trusting that the agent surfaced it. One dialog, one
answer, the batch dispatches.

### The trigger rule, stated plainly

Earlier drafts never said when the ask fires, which made the note
unbuildable. It fires here:

> **Every write dispatches through `confirm_action`, and on a
> connection that declared elicitation, `confirm_action` asks the
> operator. There is no write that skips it and no state in which it is
> suppressed.**

No exceptions for non-destructive verbs, no "first ask issues a
window", no verb tiers. One rule, and the batch is what keeps it cheap.

### Clients that cannot be asked keep the flag

The capability is known at handshake, so the decision is per
connection:

- **Declares elicitation** → write tools advertised by default, subject
  to standing restrictions, and every confirm asks.
- **Does not** → the `--allow-writes` opt-in stays, exactly as it
  behaves today.

This resolves the ordering hazard a reviewer found in an earlier draft,
which had step 2 (advertise by default) shipping before step 3 (build
the ask) and leaving a window of open surface with no gate. Under this
rule the flip and the gate are the same change: the surface opens only
for connections that can be asked, so they cannot ship apart.

### Above a readable size, refuse

Four messages enumerate. Two hundred do not, and a plan that
summarises — "200 messages matching X" — asks the operator to approve
something they have not read. That is the appearance of control
without control, which this note rejects everywhere else.

So a plan that cannot be enumerated is refused, naming the cap. The
operator narrows the selection or uses `dlq_purge`, which is one
deliberate action carrying one honest foreclosure line.

### Partial failure reports what did not happen

Batch dispatch continues past a failure and reports per item. Stopping
at the second of four leaves two in an unknown state and forces a
re-plan against a fleet that has changed underneath; continuing gives a
complete account. Rule 6 applies to the result: it names what
succeeded, what did not, and why.

### The plan IS the permission request

The best idea here is free, because the machinery already exists.

Every write is two-phase: a plan, then a confirm. **Planning is
read-only** — it validates, resolves the queue, names the messages — so
it can be allowed with no approval at all. The plan then becomes the
content of the request.

The prompt an operator sees stops being a category judgement:

> Allow `dlq_delete`?

and becomes a factual one:

> Delete message `d3b07384…` from `poly-batch-dlq` — EB task "Remove
> unattended jobs", dead-lettered 9h ago, receive_count 4. Queue depth
> 12.

The second is a decision a person can make at 11pm. The first is one
they will approve on reflex.

#### But a plan is silent about the stakes

**Reviewed and this section was wrong as first written.** A plan
describes the operation, and the reason to refuse usually lives outside
the operation.

The example above is accurate, specific and complete about the action —
and if it had appeared in front of the maintainer that morning he would
have approved it, because nothing in it says *this message is the only
live fixture for an end-to-end test of a feature shipped an hour ago*.
That was the actual reason to keep it. It is not a property of the
message, the task or the queue. It is a property of the week.

So the failure mode is not an under-specified plan. It is a plan fully
specified about mechanics and silent about stakes, which is **more**
dangerous than a vague one: it reads as complete, and a prompt that
looks complete discourages the pause in which the operator remembers
what it does not contain.

**The plan must state what the action FORECLOSES, not only what it
does.** For a delete: *this message will not be readable again, and it
is the only one in the queue.* Derivable from state already held, one
line, and the sentence that would have caused the pause.

This is [ARCHITECTURE.md](../../ARCHITECTURE.md) rule 6 — *a result
must carry its own negative space* — applied to a plan rather than a
result. Shipped in 0.41.0.

For a batch the foreclosure line aggregates: four messages destroyed,
none recoverable after the undo window, and the queue depth after.

#### The plan must stay server-authored

A permission prompt written by the party requesting permission is a
persuasion surface regardless of intent. The worry is not a scheming
agent; it is the ordinary gradient where an agent that writes plans,
and notices which plans get approved, writes more of those.

**ebman already has the right property and the design must protect it
rather than build it.** Every field in a plan is rendered server-side
from AWS or fixture state. There is no agent-supplied prose in a plan;
the agent chooses *which* thing, never how it is described.

One correction from review: this is not absolute. `set_option`'s plan
renders old → new values, and the new value is an agent-supplied
string appearing inside ebman's frame. The accurate claim is *no
agent-supplied description* — payload values appear, as the operation
itself rather than as argument about it.

Which means the suggestion to let the agent supply a short reason,
marked as its claim, is the one part that would **introduce** the risk.
Declined. The agent's argument already exists in the conversation;
copying it inside ebman's frame gives it authority it has not earned.

#### A plan is basis for a mechanical yes, never a discretionary one

That argument — the agent's case lives in the conversation — assumes
the operator is reading the conversation, and elicitation is precisely
where they may not be.

The tempting fix is to import the agent's reason into the dialog. It is
wrong for the reason just given, and the right conclusion is the
uncomfortable one:

> If the operator cannot see why the agent is asking, they should not
> be approving a discretionary write on the strength of the plan alone.

That is a reason to refuse, not to enrich the prompt. The dialog must
not be built to make an under-informed approval feel adequate.

**Request-as-unit keeps this honest** in a way windows did not. The
approval covers exactly what the plan describes, so a complete
description is a sufficient basis for it. A window required the
operator to authorise actions the plan did not describe, which is the
discretionary case wearing a mechanical costume.

## The four layers, named

| layer | what it answers | who maintains it |
|---|---|---|
| 1. **IAM** | what is *possible* | the operator, in roles they already audit |
| 2. **Restrictions** (`safety.*`) | what is *forbidden here*, standing | the operator, in config that may only say no |
| 3. **The ask** | is THIS request approved | the operator, once per request, at confirm |
| 4. **Assume-role** | a *temporary* widening of layer 1 | AWS, enforced by STS |

Layer 1 is enforced by AWS and is the only boundary that holds against
a principal with a shell. Layer 2 is fast, offline, and expresses what
IAM cannot — freeze, incident, "never this environment". Layer 3 fires
on every write, exactly once per request. Layer 4 is designed and
deliberately not in the first cut.

An earlier draft left layer 3 with no trigger rule, which a reviewer
correctly called out as making the note unbuildable: two incompatible
models coexisted, one where the ask fires only to lift a restriction
and one where it fires on every write. Under the first, an operator who
configures nothing is never asked anything, and the injection defence
for the default user is nothing. The trigger rule above settles it.

## Layer four: borrow the permission from AWS

**Designed, not built. Target: a release soon.** Recorded here at the
maintainer's request so it can ship without being redesigned. It buys
nothing for a setup that already runs as admin — uFlexi's does — and is
aimed at everyone else.

**Rewritten after request-as-unit.** An earlier draft sold this as
"`sts:AssumeRole` IS the time-boxed grant, done properly" — a fair
argument against the hand-rolled TTLs and grant markers that draft
proposed, and moot now those are deleted. The case for layer 4 is
simpler and does not depend on them.

**It is the only ceiling that holds against a principal with a shell.**
Layers 2 and 3 are ebman asking nicely: anything that can write files
can edit the restriction config, and anything that can call tools can
decline to surface a plan. For an agent confined to MCP that is a real
boundary — see the deployment note below — but for one with shell
access it is a courtesy. IAM is not, and assume-role is how an operator
gives an agent a narrow IAM identity for a stretch of work without
handing it their own.

| | ebman's layers 2-3 | assume-role |
|---|---|---|
| enforced by | ebman, locally | AWS |
| forgeable by a shell | yes | no |
| audited in | ebman's log | CloudTrail, independently |
| expires | when the request completes | when the STS session does |

The last column is the honest difference: request-as-unit authority
ends the moment it is used, which is tighter than any session. What
assume-role adds is not a longer leash but a *credential* that is
narrow regardless of what ebman does.

### It reuses machinery that already exists

`AwsClient::assume_role` (`src/aws.rs`) already assumes a role from a
source profile, with `external_id` support and a `role_session_name`,
and `config::AccountSpec` already holds the shape. It was built for
cross-account switching; this points the same mechanism at capability
elevation *within* an account. The new work is a duration, a menu, and
the ask — not the plumbing.

### Shape

Config lists roles that MAY be assumed. That is a menu, not a grant,
which keeps it on the right side of the rule that config never says
"yes, now":

```toml
[roles.dlq_cleanup]
role_arn = "arn:aws:iam::123456789012:role/EbmanDlqCleanup"
max_session_secs = 900
```

The flow: the agent needs a write the current credentials cannot do →
ebman sees the gap (by `iam:SimulatePrincipalPolicy`, or by having been
told) → it asks, in the conversation, naming the role and the duration
→ on approval it assumes, and the window IS the STS session. It expires
by construction, with no expiry logic of ebman's to get wrong.

### Attribution is the sleeper benefit

`role_session_name` should carry the client and the grant id —
`ebman-claude-<grant>` rather than today's `ebman-<target>`. CloudTrail
then shows which agent session performed which API call, in a log ebman
does not write and cannot edit. An operator can answer "what did the
agent actually do" without trusting ebman's own audit trail, which is a
materially different assurance from the one this note otherwise offers.

### Limits, to state rather than discover

- **15 minutes is the STS floor** for a session. "For the next two
  minutes" is not expressible.
- **MFA-gated roles do not work unattended.** A role requiring MFA
  cannot be assumed by a background agent.
- **Setup cost is real**: the base principal needs `sts:AssumeRole` on
  each role, and somebody has to write the policies. This is a feature
  for operators who already run scoped IAM, not a way to introduce them
  to it.
- **It buys nothing for admin-mode setups.** If the base credentials are
  already unrestricted, assuming a narrower role is a pure downgrade the
  agent could decline to take. It is worth having anyway — the downgrade
  is the point — but it must be opt-in, and it must not be presented as
  protection where the base identity is unconstrained.
- **Simulation is not enforcement.** `SimulatePrincipalPolicy` can
  return allow where SCPs, resource policies, session policies or
  conditions will deny. Advisory only. And the probe itself needs
  permission: a denied probe is "could not check", never a clean bill of
  health — `ProbeOutcome` in `src/cli/lint.rs` already encodes that
  distinction and the reason for it.

## Keeping a copy of what was deleted

**Designed, not built.** Raised by the maintainer as a way to soften
the worst foreclosure: if ebman kept the message, `dlq_delete` stops
being irreversible and the prompt stops needing to frighten anyone.

It is a good instinct and it collides with something shipped the same
day, so the shape matters more than the idea.

### The collision

`mcp.peek_bodies` exists because those payloads carry seller and buyer
identifiers for a live staffing platform. **Writing the same bodies to
`~/.cache/ebman` is strictly worse than showing them to an agent** —
durable rather than transient, backed up, and collected by support
bundles. A tool that withholds a body from the MCP surface while
writing it to disk is incoherent, and for that fleet it would make
ebman something holding personal data with a retention policy it never
previously had.

### And it would not be a restore

Putting a message back means re-*sending* it: new message id, reset
receive count, new timestamps, attributes rebuilt by the sender rather
than preserved. That is "post a similar message", not "undelete".
Describing it as restoration would be precisely the over-claim the
foreclosure line exists to prevent, in the one place an operator is
relying on the claim to decide.

### Three tiers, because they carry different risk

1. **Metadata, always.** Message id, task name / path / scheduled time,
   receive count, timestamps, body length and a hash. Recorded in the
   audit line. Answers *"what did I delete"* without storing payload.
   This also closes an existing gap — see below — and is worth doing on
   its own.
2. **Body in memory, short window.** Process lifetime, minutes, never
   touching disk. This covers the realistic mistake, which is not "I
   need this back next week" but "wait, that was the wrong one" — and
   it covers the fixture case that motivated the foreclosure line:
   the message would have come back had anyone said so within the
   window. Needs a cap, because `dlq_purge` can be thousands of
   messages.
3. **Body on disk, explicit opt-in, off by default**, with a stated
   retention and the same 0600 treatment the log gets. For operators
   whose payloads are not sensitive and who want a real undo.

### The axis distinction, so this does not read as a contradiction

*Config may only say no* governs **write permissions**, where parity
with the TUI means open by default. **Data retention** is a different
axis, where the safe default is closed: do not hold what you do not
need. Both defaults are the cautious one *for their own axis*. They
only look opposite.

### The gap this exposed, which is independent and worth fixing now

A `dlq_delete` audit line records that a message was deleted from
`poly-batch`. It does not record **which** message: `write_extras_parts`
carries `via` / `client` / `can_ask` / `version` / `settings`, and the
target is the environment name. So the log can tell an operator that
something was deleted and never what.

That is tier 1 without any of the retention questions, and it is the
prerequisite for the rest: a copy is worth little if the log cannot say
what it was a copy of.

## What this is not

**Layers 2 and 3 are not a security boundary.** Anything that can write
files can write the grant marker or the restriction config. They protect
against mistake, drift and momentum — which is what actually goes wrong
— and the docs must say so plainly rather than implying more. ebman's
existing line holds: the boundary is IAM, which is layers 1 and 4, and
is the reason those are in this design at all.

**Not per-action prompting by default.** If forty messages dead-letter,
prompting per message produces rubber-stamping, which is worse than a
scoped grant because it *looks* like control. A bounded grant — this
env, this verb, one hour — is more honest and safer than forty prompts
nobody reads. Per-action asks are right for the destructive tail
(`terminate`, `dlq_purge`), not for routine work.

## Open questions

Client behaviour, and what has been settled:

- ~~**Does Claude Code declare elicitation support?**~~ **ANSWERED,
  2026-09-19: yes.**

      MCP client connected client=claude-code elicitation=true

  One real connection to ebman 0.41.0 after `/mcp` Reconnect. The
  instrument that produced it was dead until 0.40.0 — every subcommand
  returned from `main` before `init_logging`, so the single `tracing::`
  call on the MCP surface wrote to a subscriber that was never
  installed. The measurement deciding the shape of this design was
  three releases and one unnoticed bug away from being unavailable.

- ~~**Does the client refetch on `tools/list_changed`?**~~ **Moot.**
  Nothing varies the tool set mid-connection. The advertised surface is
  decided once, at handshake, by whether the connection declared
  elicitation.

- **Does an elicitation dialog survive `TOOL_TIMEOUT_SECS`?** The cap
  is 30s (`src/cli/mcp/mod.rs`) and a human deciding whether to delete
  production data will routinely take longer. A reviewer flagged that
  protection-levels.md already established the answer in principle —
  an unanswerable ask degrades to DENY, never to allow — but the frame
  loop's contract has to change to keep a call alive while a dialog is
  open. **This is the first thing to establish when building**, because
  it is the difference between a gate and a tool that times out under
  use. A peer session reports Claude Code does not background a call
  blocked on an open dialog, which suggests the client handles its half.

- **Is a declined ask audited, and what is the agent told?** It should
  be: a decline leaves no trace today, and "the operator said no" is
  exactly the near-miss the `stage=refused` work exists to make
  visible. The agent should be told plainly it was declined, with no
  remedy naming a control — because the control is a person who has
  just said no, and an agent that retries a decline is the failure mode
  here.

Answered since drafting: `/mcp` shows tools, not declared capabilities
— checked by the maintainer — so `ebman mcp doctor` tells an operator
something the client does not.

## Implementation order

1. ~~**Invert the config.**~~ **Shipped in 0.41.0** as
   `safety.read_only` — a standing refusal honoured by the TUI, the
   CLI and the MCP surface, which the session toggle cannot lift.
   There is deliberately no config key that grants.
2. **Keep the call alive while a dialog is open.** The frame loop
   currently caps a tool call at `TOOL_TIMEOUT_SECS`. Establish this
   first: everything below is unbuildable if the ask times out under
   ordinary use.
3. **Ask at `confirm_action`** on connections that declared
   elicitation, showing the plan and its foreclosure line. Deny on
   no-answer. Audit the decline.
4. **Advertise write tools by default on those connections**, subject
   to standing restrictions. Steps 3 and 4 are one change: the surface
   opens only where the ask exists, so they cannot ship apart.
5. **Batch plans** — one plan covering a set, refused above an
   enumerable cap, dispatching with per-item results.
6. **Assume-role** (layer 4), targeted at a release soon after.

Deleted from an earlier version of this list, and worth recording so it
is not reinvented: time-boxed grants, a grant marker reusing
`freeze.rs`, TTLs, revocation on incident, `ebman grant` / `:grant`,
`tools/list_changed` emission, and the grant-visibility surface. All of
them existed to answer "what happens between asks", and under
request-as-unit there is no between.

## Cost

Smaller than when this note was first written, because most of what it
proposed has been deleted rather than built. The remaining work is the
frame-loop change (step 2), the elicitation round-trip (step 3), the
conditional advertising (step 4), and batch plans (step 5). Two to
three days, and step 2 should be timeboxed first because a negative
result there changes the design rather than delaying it.
