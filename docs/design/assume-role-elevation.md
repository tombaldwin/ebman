# Assume-role for MCP writes

**Status: design, for review. Nothing here is built.** Layer four of
[runtime-grants.md](runtime-grants.md), step 6 of its implementation
order. That note sketched the idea and left the shape open. This one
settles it so it can be built in slices without being redesigned.

## What it is for

Today an agent's write goes out under the operator's own credentials:
whatever profile the MCP server runs with, or the `profile` the tool
call names. That leaves three gaps:

1. **Nothing narrows what an agent can do.** The ask approves *this
   request*, but the credential behind it can do anything the operator
   can. If ebman has a bug, or the agent can reach a shell, the
   credential is the operator's.
2. **CloudTrail cannot tell the agent from the operator.** Both act as
   the same principal. "What did the agent do" is answerable only from
   ebman's own audit log, which ebman writes and anything with a shell
   can edit.
3. **An operator whose own identity cannot write has no way to lend an
   agent a specific write** without lending it a whole profile.

A role per class of write fixes all three with one mechanism. The MCP
server assumes a configured role for each approved write, and the
write goes out under that role's session:

- **IAM enforces the ceiling.** The role's policy bounds what the
  credential can do, and ebman holds no logic that could get it wrong.
- **CloudTrail names the agent's writes.** The session name carries the
  client and the plan, in a log ebman does not write.
- **A role can be wider than the base identity** (elevation) **or
  narrower** (a ceiling). The mechanism is the same; which one it is
  depends only on the policies the operator writes.

## Decisions

### D1. The ask stays the authority, and the role is the ceiling

The runtime-grants sketch had the STS session *be* the grant: ask once,
then the session lasts as long as STS allows. Request-as-unit since
deleted every "between asks" state, and this does not bring one back.

**Every write is still one plan and one confirm, asked on a connection
that can ask.** The role decides *what the credential can do*, never
*whether a write may happen*. An assumed session is used for the
approved request and then dropped (D4), so holding a live session
grants nothing.

This is also what keeps the rule that config may only say no: a role
in config is a menu of credentials, not permission to write. The
permission is the trust policy, which the operator writes in IAM, and
the ask, which the operator answers each time.

### D2. Config maps verbs to roles; nothing detects a gap

The sketch had ebman notice that the current credentials cannot do a
write (by `iam:SimulatePrincipalPolicy`) and then offer a role.
**Dropped.** Simulation is advisory (SCPs, resource policies and
conditions can still deny), the probe needs its own permission, and a
choice that depends on a probe is one the operator cannot predict by
reading config.

Instead, config states the mapping:

```toml
mcp.role.deploys.role_arn = "arn:aws:iam::111122223333:role/EbmanAgentDeploy"
mcp.role.deploys.verbs = ["deploy", "restart", "rebuild", "set_option"]

mcp.role.dlq.role_arn = "arn:aws:iam::111122223333:role/EbmanAgentDlq"
mcp.role.dlq.verbs = ["dlq_resend", "dlq_delete", "dlq_purge"]
# mcp.role.dlq.external_id = "..."
# mcp.role.dlq.source_profile = "ops"
```

The keys follow `accounts.NAME.*`, and they reuse `AccountSpec` for
`role_arn`, `external_id` and `source_profile`. A verb in no role's
list goes out under the base credentials, as today, unless D6's strict
mode is on. A verb in two roles' lists is a config error, and it fails
closed: every MCP write refuses until it is fixed, as an unreadable
`safety.` line does. Choosing one silently would be the fail-open the
safety parser was rewritten to stop.

`terminate` is deliberately absent from the example, not from the
design. It can be mapped like any other verb.

### D3. The plan names the role, and the assume happens after the yes

The plan already resolves `caller: CallerIdentity` and shows it in the
confirmation ("as arn:aws:sts::…:assumed-role/…"), because the
confirmation is the only moment the operator can act on it. With a role
mapped, the plan resolves the role instead, and the confirmation says
so:

> Restart `poly-api-prod` — as role `EbmanAgentDeploy`
> (arn:aws:iam::111122223333:role/EbmanAgentDeploy), from
> arn:aws:iam::111122223333:user/ops

**`sts:AssumeRole` is called at confirm, after the operator says yes,**
not at plan time:

- A declined ask makes no STS call. So **every AssumeRole event in
  CloudTrail with an ebman session name is an approved write**, a
  property an auditor can check without ebman.
- Planning stays read-only, which runtime-grants relies on ("planning
  can be allowed with no approval at all").

Plan time does a cheap local check: is the role's account the account
the env is in? `CallerIdentity` and the 0.47 per-region identity
already know the target account. A mismatch refuses at plan time with
both accounts named, rather than reaching AWS as an AccessDenied the
agent will misread as a permissions problem.

### D4. One session per write, never cached across requests

Each confirmed write assumes its role with `DurationSeconds = 900`
(the STS floor), dispatches, and drops the credentials.

- **One write, one session, so the session name identifies the
  write** (D5). A cached session would put several writes under one
  name, and CloudTrail would lose the attribution this exists for.
- No credential lifetime state to manage, expire or leak across
  requests.
- The cost is one STS call per write. Writes are rare, and an approved
  write has already waited for a human.

**The one deliberate exception is undo.** `dlq_undo` re-sends what a
`dlq_delete` removed, within `UNDO_WINDOW_SECS` (600s, inside the 900s
session). The undo buffer entry keeps the delete's credentials in
memory beside the message body it already holds there, and the undo
goes out under the same session. In CloudTrail, the delete and its undo
then sit under one session name, which is what they are: one request
and its reversal. It also means `dlq_undo` stays one step and needs no
AssumeRole of its own, which matters when an undo is urgent and STS is
the thing failing. The role's policy must therefore allow
`sqs:SendMessage` as well as `sqs:DeleteMessage`. The docs say so, and
so does `doctor` (D7).

### D5. The session name carries the client and the plan

STS allows 2–64 characters from `[\w+=,.@-]`.

```
ebman-mcp-<client>-<plan>
```

`<client>` is the MCP client's declared name, mapped to that alphabet
and cut to 24 characters. `<plan>` is the first 12 characters of the
plan token's hash, never the token itself, which is a live credential
for its confirm until it is used.

The ebman audit line for the write gains `role=<arn>` and
`session=<name>`, beside the existing `via=mcp client=… can_ask=…`. An
operator can then join ebman's log to CloudTrail on the session name,
or skip ebman's log and use CloudTrail alone.

Today's cross-account switch uses `ebman-<target>`; that stays as it
is. It is a different feature.

### D6. Strict mode is opt-in: `mcp.require_role = true`

With it set, an MCP write whose verb maps to no role refuses rather
than falling back to the base credentials, with `rule=no_role` and a
remedy naming the config key. This is for the operator whose point is
"agent writes never go out as me". It is off by default because
turning it on with no roles configured closes every MCP write. That is
a legitimate choice, but it has to be the operator's.

It is a refusal, so it follows the existing rules: decided by the
shared gate, audited `stage=refused`, and named by `doctor`.

### D7. Failure is a refusal that names the role, audited

If AssumeRole fails (access denied, an MFA-gated role, an unknown ARN,
a throttled STS), nothing is dispatched. The confirm returns an error
that names the role and AWS's message, and the attempt is audited
`stage=refused rule=role_unassumable` with the role ARN. Nothing ran, so
it is not `stage=failed`, which means a dispatch that AWS rejected.

The failure does not fall back to the base credentials. A fallback
would make a policy typo silently undo the ceiling, which is the
fail-open shape the safety parser was rewritten to remove.

`doctor` gains a line per configured role: the mapped verbs, whether
the account matches the identity it runs as, and whether
`sts:AssumeRole` succeeds. Doctor does call AssumeRole. That writes a
CloudTrail event with the session name `ebman-doctor`, so the
CloudTrail property in D3 stays exact. For a DLQ role, doctor also
names the undo requirement from D4. It cannot check it: simulation is
advisory, and probing a real `SendMessage` is a write.

### D8. MCP only, in the first cut

The TUI and the CLI act for the person at the keyboard, who is the
principal CloudTrail should name. The gaps this design closes are about
a second actor. A `--role NAME` for `ebman action` is easy to add later
on the same plumbing, and is listed under Later, not built.

Demo assumes nothing: it has no credentials and touches nothing real.

## What it does not do

- **It is not a boundary against an agent that can reach a shell when
  the base identity is admin.** That agent can use the base credentials
  without going through ebman. In that setup the role is still a
  ceiling for every write that does go through ebman, plus attribution.
  The docs must say this in those words, as runtime-grants requires,
  and not present a downgrade as protection.
- **It does not shorten anything below 15 minutes,** but per-write
  sessions (D4) mean a session outlives its write by at most that long,
  unused.
- **It does not need `SimulatePrincipalPolicy`** for anything.

## Slices

Each slice can ship on its own and is useful if the next never comes.

1. **Config and the gate.** Parse `mcp.role.*` and `mcp.require_role`
   (pure `parse`, docs, the docs-drift guard), fail closed on a verb
   mapped twice, and add `rule=no_role` to the shared gate. No STS yet:
   with roles configured, strict mode already refuses unmapped verbs.
2. **The plan names the role.** Resolve the role at plan time, check
   the account locally (D3), and render the confirmation line.
3. **Assume at confirm.** Inject a role-client factory into `Server`,
   since `AwsClient::assume_role` is unreachable under test by design
   and tests seed the factory. Add the session name (D5), the audit
   fields, and `role_unassumable` (D7).
4. **Undo under the delete's session** (D4).
5. **`doctor`** (D7), and the operator docs: `docs/headless.md`'s MCP
   section, `docs/safety-and-privacy.md`, `docs/configuration.md`, and
   an example trust policy and role policy per mapped verb class. EB's
   own requirements (an `UpdateEnvironment` caller needs the
   CloudFormation, Auto Scaling and S3 permissions EB acts with) belong
   in that example. They are the part an operator writing a narrow
   role will get wrong first.

Then a live check against a real account, as for every AWS-shaped
feature: a mapped restart and a DLQ delete with its undo, read back in
CloudTrail by session name.

## Later, not designed here

- **A per-request session policy.** AssumeRole accepts an inline policy
  that intersects with the role's. For the SQS verbs that could narrow
  the credential to exactly the plan, for example `sqs:DeleteMessage`
  on that queue's ARN. It would make request-as-unit hold in IAM, not
  only in ebman. EB verbs need too many underlying permissions for a
  tight per-request policy to be written correctly, so they stay on the
  role alone.
- **`SourceIdentity`** set to the operator's identity, which persists
  through role chaining and which a trust policy can require. It needs
  `sts:SetSourceIdentity` in the trust policy, so it would be an option,
  not a default.
- **`ebman action --role NAME`** for the CLI (D8).

## Decisions for the maintainer

The note takes a position on each. These are the ones worth a second
look, because the opposite answer is also defensible:

- **D4's undo exception.** The alternative is an AssumeRole of the
  undo's own, under the verb's mapped role. It is cleaner in the
  mapping and worse when STS is degraded.
- **D6 off by default.** On by default would make configuring a role
  imply "only through roles". That reads naturally, but it would close
  every unmapped verb the moment the first role is added.
- **D8, MCP only.** Agent writes are the stated reason for the feature.
  The TUI case is real but different: an operator choosing to act under
  a narrower role is a convenience, not an attribution gap.
