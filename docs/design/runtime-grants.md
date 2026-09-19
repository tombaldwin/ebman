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

**There is no pre-set permission.** The `--allow-writes` flag stops
being a capability decision at all. Write tools are advertised by
default, subject to whatever the operator has forbidden.

**Permission happens at the moment of need, in the conversation.**
Three mechanisms, in preference order:

1. **The client's own tool prompt.** Claude Code already has a tool
   approval system, and ebman already annotates every write
   `destructiveHint: true`, `readOnlyHint: false`. If the client
   prompts on that, the operator gets in-conversation approval with
   **no new ebman machinery at all** — the only reason it does not
   happen today is that write tools are not advertised without the
   flag, which the section above removes.

   The two-phase shape lands well here: the plan call is harmless and
   puts the full detail in the transcript, so the prompt arrives on
   `confirm_action`, immediately after the operator has read the plan.
   Its weakness is that the prompt says "allow confirm_action?" and
   shows a token — opaque about *what* is being confirmed.

2. **ebman elicitation.** Server-authored prompt, so it can carry the
   plan and the foreclosure line. Mechanism 1 is the gate; this is what
   makes the gate legible. Needs the client to declare elicitation.

3. **Operator-issued grant** — `ebman grant dlq_delete --env poly-batch
   --ttl 1h`, or `:grant` in the TUI, reusing the cross-process marker
   machinery `:freeze-deploys` already uses, with the pid-liveness and
   reuse handling solved in `src/freeze.rs`. **The fallback**, for
   clients that cannot elicit and for pre-opening a window when the
   operator already knows they are about to do twenty of these.

**Corrected after the parity ruling.** An earlier draft had route 3
making write tools "appear mid-session" via
`notifications/tools/list_changed`. That only makes sense on a server
where they were *absent* — and parity-by-default means they never are.
Two consequences, both of which shrink the work:

- **`tools/list_changed` is not needed.** Restrictions are per
  environment and tools are global, so no grant ever changes the tool
  set. Nothing appears, so nothing needs announcing. It stays out
  until something actually varies the listing.
- **A grant is a temporary lift of a standing restriction**, not an
  addition of capability. "prod is `read_only`, and for the next hour
  `dlq_delete` is allowed there." That is still a permission, and still
  consistent with *config may only say no*: the config's no is
  permanent and the lift is transient and made in the conversation,
  which is the whole distinction this note turns on.

Which also means route 3 is much smaller than it looked — a marker
with a TTL that `write_gate` consults, not a capability system.

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

#### A plan is basis for a mechanical yes, never a discretionary one

The argument above — that the agent's case lives in the conversation,
so it need not be imported into the plan — assumes the operator is
reading the conversation. **Elicitation is precisely the case where
they may not be.** That is its appeal: approval happens in-client, at
the moment of need, without going anywhere else. In a remote or
headless-adjacent setup the dialog may be all they see, and at that
point the plan really is the entire basis for the decision.

The tempting fix is to import the agent's reason into the dialog. That
is wrong for the reason already given, and the right conclusion is the
uncomfortable one:

> If the operator cannot see why the agent is asking, they should not
> be approving a discretionary write on the strength of the plan alone.

That is a reason to refuse, not a reason to enrich the prompt. An
approval given without the context is not a more efficient approval; it
is a worse one, and the dialog must not be built to make it feel
adequate.

**So the plan is sufficient basis for a MECHANICAL yes — is this the
message I meant, is the count right — and never for a discretionary
one.** The fixture case is the type specimen: every mechanical fact in
that prompt was correct and the right answer was still no.

**This looked like an argument for moving grants to the operator route
— `ebman grant …` in a terminal — and that conclusion was drafted here
and then overruled.** The maintainer's response, on being told grants
would be issued by him after shipping:

> WHY WOULD WE NEED TO GRANT IT AFTER WE SHIP — surely that's the point,
> that we don't have to do that.

He is right, and the draft had reintroduced the original friction
wearing different clothes. A terminal command is not a restart, but it
is still leaving the conversation to pre-authorise something, which is
the thing being designed away.

**Ruling: elicitation carries the grant request. The CLI route is the
fallback**, for clients that cannot elicit and for pre-opening a window
when the operator already knows they are about to do twenty of these.

What survives from the argument above is the discipline, not the
routing: **the dialog must not be built to stand alone.** No importing
the agent's reasoning into it, no enriching it until an
under-informed approval feels adequate. The design assumes the operator
has the conversation, states that assumption plainly, and declines to
paper over the case where they do not. ebman cannot detect whether a
human is reading the transcript, so this is a posture, not a check —
and the honest form of the posture is to keep the prompt thin and the
context elsewhere.

**Which re-promotes the measurement.** An earlier draft demoted it on
the reasoning that grants came from the operator anyway. That reasoning
is dead. If the client cannot elicit, there is no server→client→human
channel mid-call, the in-conversation ask is not buildable, and the
operator is left with exactly the CLI friction this note exists to
remove.

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

## The four layers, named

The note grew section by section and called the last one "layer four"
without ever naming the others. For a reader who was not in the
conversation:

| layer | what it answers | who maintains it |
|---|---|---|
| 1. **IAM** | what is *possible* | the operator, in roles they already audit |
| 2. **Restrictions** (`safety.*`) | what is *forbidden here*, standing | the operator, in config that may only say no |
| 3. **The ask** | what is happening *now* | the operator, in the conversation |
| 4. **Assume-role** | a *temporary* widening of layer 1 | AWS, enforced by STS |

Layer 1 is enforced by AWS and is the only real boundary. Layer 2 is
fast, offline and expresses what IAM cannot — freeze, incident, "never
this environment". Layer 3 is the human gate. Layer 4 is designed and
deliberately not in the first cut.

## Layer four: borrow the permission from AWS

**Designed, not built. Target: a release soon.** Recorded here at the
maintainer's request so it can ship without being redesigned. It buys
nothing for a setup that already runs as admin — uFlexi's does — and is
aimed at everyone else.

The observation that makes it worth doing: **`sts:AssumeRole` IS the
time-boxed grant, done properly.** Everything the sections above
hand-roll, AWS already does better.

| hand-rolled | AWS equivalent |
|---|---|
| TTL on a marker file, honoured by ebman | session duration, enforced by STS |
| the `--allow-writes` ceiling | the role's policy, which the operator already maintains and audits |
| ebman's audit log | CloudTrail, written independently of ebman |
| "not a security boundary — anything that can write files can write the marker" | assumed credentials, which cannot be forged locally |

That last row matters most. The note admits below that a grant marker
protects against mistake and drift but not against a compromised agent.
An assumed role does not need the caveat.

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

Two are client behaviour and cannot be settled from inside ebman. Both
should be answered before building, not designed around:

- **Does Claude Code declare elicitation support?** Decides whether
  mechanism 2 exists at all. If it does not, the in-conversation ask
  falls back to whatever mechanism 1 gives for free, and the operator
  is left with the CLI grant for anything more — which is the friction
  this note exists to remove. The instrument that measures
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
- ~~**Does the client refetch on `tools/list_changed`?**~~ **Moot.**
  Under parity-by-default the tool set never varies, so ebman has no
  reason to send the notification and the client's handling of it does
  not matter. Removed rather than left as an open question nobody needs
  answered.

- **Does Claude Code prompt before calling a tool annotated
  `destructiveHint: true`?** This decides how much of mechanism 1 comes
  for free, and it is the cheapest of the three to find out: it is
  answered the first time an agent attempts a write against an
  advertised tool. Unknown, and not assumed either way.

Answered since drafting: **`/mcp` shows tools, not declared
capabilities** — checked by the maintainer. So `ebman mcp doctor` tells
an operator something the client does not, which is the case for
building it.

And one that is ours:

- **Does a grant survive a server restart?** A marker file says yes by
  construction. TTL makes that mostly safe, but the freeze marker's
  pid-liveness logic exists because "mostly" was not good enough there
  either.

## Implementation order

1. **Invert the config.** Restrictions only: a global standing
   `read_only`, alongside the per-env pins that already exist. Remove
   the opt-in permission. This is the change the maintainer's rule
   implies, it stands regardless of what the client can do, and it is
   the bulk of the work.
2. **Advertise write tools by default**, subject to those restrictions.
   On a client that prompts for destructive tools this alone may
   deliver mechanism 1 — in-conversation approval, no further work.

   **Conditional on the gate existing.** Flipping this default without
   knowing that the client prompts, or that elicitation is available,
   converts a safe-by-default surface into an open-by-default one. Do
   not ship the flip and the measurement in the same release.
3. **Elicitation** (mechanism 2), if the measurement says it is
   available, to make the prompt say something worth reading: the plan,
   and what the action forecloses.
4. **Operator-issued grants** (mechanism 3): a marker with a TTL that
   `write_gate` consults as a temporary lift of a standing restriction,
   plus visibility and audit correlation. No `list_changed` — see the
   parity correction above.
5. **Assume-role** (layer 4), targeted at a release soon after.

Steps 2 and 3 are small once 1 is done. The honest unknown is how much
of this the client gives for free at step 2, which is answered the
first time an agent attempts a write.

## Cost

A few days, not an afternoon. `src/freeze.rs` supplies the hard part —
cross-process state with liveness and reuse handling, already trusted
for a safety decision. The new work is the grant vocabulary, the TTL,
the `list_changed` emission, the audit correlation, and the TUI surface.
