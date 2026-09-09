# Protection levels for operator tools

**Status:** design note, nothing implemented. Written 2026-09-09 for
`ebman`, but the rules in [Principles](#principles) are meant to hold for
any tool that lets a person — or an agent acting for them — change
production. `pgman` is the second consumer and the reason this is
written generally.

**Revised after review, 2026-09-09.** Two reviewers read it against the
code. Corrections worth knowing if you read the first draft: ebman has
**two** write gates rather than one, so the cost estimate was wrong;
pgman is a **migration**, not a greenfield consumer, and already ships a
safety engine of a different shape; the decision model needed
**obligations**; `ask` is a transport problem before a policy one; and
the config example both violated Principle 5 and used syntax the parser
cannot read. Each is marked in place rather than quietly fixed, because
the wrong version is the one people will have skimmed.

## The problem

An operator tool that can only refuse everything or permit everything
forces a bad choice. Turn protection off and it protects nothing; leave
it on and people work around it. Agents sharpen this: they act faster
than a human reviews, they retry, and given an opaque refusal they will
look for another route to the same effect.

`ebman` today has most of the raw material — a per-env `read_only` pin,
account pins, a session `--read-only`, a cross-process deploy freeze,
confirm modals (including a type-the-environment-name gate on DLQ
purge), a 5-second undo window, an audit line per dispatch tagged with
provenance, and MCP writes off unless `--allow-writes`. What it does not have is a way to
say *how careful to be* in one place, or to answer an agent's "may I?"
in a form the agent can act on.

## Principles

These are the transferable part. They are stated as rules because each
one was learned from something that went wrong somewhere.

### 1. The boundary is not yours. Say so.

IAM is the security boundary for `ebman`; database roles are for
`pgman`. Anyone holding credentials can bypass the tool entirely with
the vendor CLI. A protection level is a guardrail against **mistakes**,
not an adversary, and the documentation must say that plainly.

A guardrail described as a control gets trusted for the wrong things,
and the failure is silent until the day it matters. Claim exactly the
protection you provide.

### 2. Every escape hatch becomes the habit.

If `--force` exists and works, `--force` is what people type — and what
they put in the runbook, and what the agent learns to send. This is not
hypothetical: this repo's own `CLAUDE.md` makes "widening an allowlist
to make a guard go quiet" a stop condition precisely because it is the
cheapest wrong path and it always works.

So: an escape must be **non-defaultable** — impossible to pre-commit to
a script or reduce to muscle memory — and must leave a record. A `--yes`
that skips everything is the wrong shape.

Stated that way because the obvious example does not travel.
`ebman`'s type-the-environment-name-to-purge works because a human is at
a TUI; a headless CI principal cannot type anything interactively, and
pgman's equivalent friction is a different kind entirely — wrapping the
statement in a transaction the operator must then commit, which is
friction *after* dispatch rather than before.

### 3. Refusals must be machine-readable, and say whether to give up.

A refusal in prose tells an agent nothing except that it failed. It
cannot tell "you will never be allowed this" from "a human can unlock
it" from "try a different resource" — so it retries, or it finds a side
door, or it stops when it should have asked.

Emit a document, not a sentence. The two fields that change behaviour
most are **`retryable`** and **`remedy`**.

### 4. Three outcomes, not two.

`allow` / `deny` forces every uncertain case to one extreme. The useful
middle is **`ask`**: the agent is not blocked and the human is not
bypassed. `sudo`, `polkit` and Claude Code's own permission model all
settled on this shape; a tool that skips it will grow it later under a
worse name. pgman reached the same tri-state independently
(`Guard { Allow, Confirm, Block }`), which is the best evidence this
principle is discovered rather than invented.

**But `ask` is a transport question before it is a policy one**, and a
ladder defined in terms of asking is worthless if the primary agent
transport cannot express it. See the section below before designing
levels — this is the sequencing mistake most likely to waste the work.

### 5. A principal cannot raise its own level.

The ceiling is set by *who is asking*, established outside the request —
a server flag, a config file, a credential — never by a parameter the
caller supplies. `ebman`'s `--allow-writes` already has this property
because it is a server flag; keep it when the model gets richer.

Corollary: identify principals distinctly enough to be useful. "An
agent" is too coarse; `mcp:claude-code` is a principal, and a human at
the TUI is a different one — but see the config section, because an
identity the caller supplies is a label, not an authentication.

The harder corollary: **the ceiling must be enforced at a layer the
request content cannot reach, and each tool must enumerate its own
in-band escape routes.** This is domain work the engine cannot do for
you. ebman's split helps because the client sends structured tool calls;
pgman's does not, because a request there is free-text SQL and
`SET transaction_read_only = off` is a perfectly ordinary statement — it
needs a dedicated check (`attempts_read_only_escape`) built on its SQL
lexer to catch a level being raised in-band.

### 6. Presets must be printable.

Named levels are the friendly surface. They are only trustworthy if the
tool can print exactly what a level expands to. An opaque preset becomes
the thing nobody can reason about — the same way the hand-maintained
`WRITE_COMMANDS` and `CONFIRM_STATE` lists in this repo needed guards
before anyone could rely on them.

`<tool> safety explain` should print the full expansion for the current
principal, with the reason each rule applies.

### 7. Encode practice as preconditions, not prose.

"Don't deploy on a Friday" in a runbook is a wish. As a precondition it
is enforceable, inspectable, and overridable with a record.

This is the weakest principle here and should be treated as a direction
rather than a rule: its only concrete instance is deferred out of v1
below, and it quietly conflates two different things. *"May I?"* is
permission, decided from policy. *"Should I, yet?"* is advice, decided
from live state — is staging green, has the canary soaked. They want
different inputs and probably different machinery, and running them
together is exactly the seam where domain logic leaks into a supposedly
neutral engine. Keep them separable.

### 8. Every decision is auditable, including the allows.

Recording only refusals tells you what was blocked and nothing about
what happened. `ebman` already writes an audit line per dispatch tagged
with provenance (`via=mcp client=<name>`); the policy decision belongs
on the same line.

## Adopt, don't invent

Most of this exists. Inventing it again costs interoperability.

| Borrow | From | For |
|---|---|---|
| `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint` | MCP tool annotations | Letting *any* MCP client render the right consent UI with no tool-specific knowledge |
| `principal / action / resource / context` | Cedar (AWS, open source) | The decision model's shape — familiar, and AWS-native suits the domain |
| `allow` / `ask` / `deny` | polkit, sudo, Claude Code | The tri-state from Principle 4 |
| `prevent_destroy` | Terraform lifecycle | The per-resource pin. `safety.envs.*.read_only` is already this |
| verbs over resources | Kubernetes RBAC | An action vocabulary, rather than a bespoke one |

**MCP annotations are the first thing to do.** `ebman` emits none today
(verified 2026-09-09) — an MCP client cannot currently distinguish
`list_environments` from a write tool except by reading prose. It is
contained, standards-compliant, and tells us how clients actually behave
before we commit to anything larger.

*(Check the field names against the current MCP spec revision before
implementing — the list above is from memory of the schema, not from
reading it today.)*

## What is genuinely ours

No standard covers these, so they are the shared invention — and the
part `pgman` reuses.

**Levels.** A short ordered set, sugar over rules. Working names:

| Level | Intent |
|---|---|
| `observe` | Reads only. The safe default for an unattended agent. |
| `guarded` | Reversible writes on non-production. Anything else asks. |
| `trusted` | Production writes allowed; irreversible ones still ask. |
| `unrestricted` | No level-based refusal. Confirms and audit remain. |

Per-principal, so an agent and a human can differ. Written as dotted
keys, because ebman's config parser is a line-based `key = value` reader
with no section headers — the first draft used TOML tables the parser
cannot read:

```toml
safety.level = "trusted"                       # a human at the TUI
safety.principals.mcp.level = "guarded"        # anything over MCP
safety.principals.mcp:claude-code.level = "observe"
```

**The effective level is the MINIMUM of every matching entry, never the
most specific one.** This is not a detail — most-specific-wins opens a
hole straight through Principle 5. The MCP principal is identified by
`clientInfo.name`, which is **self-reported by the caller** (ebman uses
it today only for audit tagging). Under most-specific-wins, a client
that simply renames itself stops matching the stricter entry and falls
back to the more permissive transport default: the caller would have
raised its own ceiling with a parameter.

Taking the minimum means a name-specific entry can only ever *tighten*
below the transport ceiling. And the docs must say plainly that
`clientInfo.name` is self-reported — it is a label for auditing and
convenience, not an authenticated identity. Principle 1 again.

**Fail closed.** `config::parse` today silently skips any line it cannot
read, so a typo'd `safety.envs.prod.read_only` vanishes and the
environment is writable. A levels stanza inheriting that posture would
mean `level = "observ"` silently granting the default. An unparseable or
absent safety stanza must resolve every principal to `observe`, and that
requires changing the parser's error handling — so it is work, not a
sentence.

**Precedence, and what it means for migration.** Pins and freezes are
level-independent and always win: `unrestricted` means "no *level-based*
refusal", not "no refusal". So an existing `safety.envs.*` config gets
strictly no weaker when levels arrive, which is the migration guarantee.

**The refusal document.**

```json
{ "decision": "deny",
  "rule": "safety.level",
  "principal": "mcp:claude-code",
  "action": "terminate",
  "resource": "api-prod",
  "retryable": false,
  "remedy": "human",
  "detail": "terminate requires level >= unrestricted; principal is at guarded" }
```

`remedy` says what would make this work, and is the field an agent
should branch on:

| `remedy` | Meaning | Agent should |
|---|---|---|
| `human` | Someone must raise the level, thaw a freeze, or act themselves | stop and report |
| `config` | A pin is in the way | stop and report which pin |
| `modify-request` | The action as *shaped* is refused; a narrower one may pass | rewrite and retry once |
| `wait` | A time-bounded window will expire | retry after `retry_after` |

Three corrections to the first draft:

- **`modify-request` was missing**, and it is pgman's most common
  refusal: a `DELETE` with no `WHERE` clause is blocked while the same
  delete with a predicate is allowed. An agent told `human` there
  escalates when it should simply narrow the statement.
- **A freeze is not `wait`.** ebman's freeze marker has no TTL — it ends
  when a human thaws it or the owning process dies. An agent given
  `wait` will poll *through an incident*, which is precisely when it
  should be quiet. Freeze maps to `human`. `wait` is only legitimate
  with a mandatory `retry_after`; without a horizon it is an invitation
  to spin.
- **`other-resource` is dropped.** "This action is fine elsewhere",
  without saying where, invites an agent to enumerate the fleet probing
  for something unpinned — the side-door behaviour Principle 3 exists to
  prevent. If the constraint that failed can be named, name it in
  `detail`; otherwise say nothing.

`retryable` is derivable from `remedy` and is kept only as a
convenience. Two fields that can disagree eventually will, so it is
defined as `remedy == "wait"` and nothing else may set it.

Every document carries a **correlation id** shared with its audit line,
which is what makes Principle 8's near-miss question answerable.

**Refusals are not audited today.** `audit.rs` records dispatched,
completed, skipped and undone — there is no refused. A TUI refusal is a
toast and an MCP refusal is a prose error, so the agent that tried
`terminate` on prod six times leaves no trace. Principle 8 should be
read as requiring a line for every decision including denials, not only
for allows.

## Three things the first draft got structurally wrong

### A decision is not one of three values

`allow / ask / deny` is not enough, and pgman already proves it. Its
shipped `Decision` (`pgman/src/safety.rs:231`) carries a `Guard`
alongside `wrap_in_tx`, `blocked_by_read_only` and `read_only_escape` —
because the useful answer is often *"allow, but wrapped in a
rollback-able transaction"* or *"allow, but with a 30-second statement
timeout"*. ebman has the same shape without naming it: the 5-second undo
window and the type-to-confirm purge are conditions attached to an
allow.

polkit and XACML call these **obligations**. The decision type needs an
opaque obligations channel from day one, or every tool bolts its
mitigations on beside the engine and the shared-seam story dies quietly.

`ask` is likewise not one thing. A keypress confirm and a
type-the-resource-name confirm are different strengths, and Principle 2
says the difference is the whole point — so ask carries a strength, or
the vocabulary supplies it.

### `ask` is a transport question before it is a policy question

The levels ladder is *defined in terms of* asking, so a ladder whose
middle rungs cannot be expressed over the primary agent transport is
sugar over nothing. A stdio MCP tool call cannot casually block on a
human. MCP added **elicitation** for this, but client support is uneven.

The trap: ebman already has a two-phase `confirm_token` flow on its MCP
write path, and an implementer will wire `ask` to it and believe the job
is done. Read that code's own comment — *"the agent that plans is the
agent that receives the token"*. That is **agent-confirms-itself with
human visibility**, not a human in the loop. It is a reasonable
mechanism; it is not `ask`.

So `ask` must be specified per surface, and the degradation named:

| Surface | `ask` becomes |
|---|---|
| TUI | the existing confirm modal, at the required strength |
| CLI | interactive prompt when a TTY is present; otherwise refuse with the document and exit 3 |
| MCP | elicitation where the client supports it; **otherwise `deny` with `remedy: human`** — never silently downgraded to allow |

**Decided 2026-09-09, and the "where the client supports it" is now
measurable rather than hopeful.** Elicitation is a *client* capability
declared at `initialize`, so the server can know per connection whether
`ask` is expressible on that transport. ebman was discarding that field;
it now captures and logs it (`client_supports_elicitation`), and nothing
branches on it yet.

That ordering is deliberate. The open question was whether elicitation
is usable in the clients that matter, and the design note could not
answer it. Guessing would have meant either building a middle rung that
silently degrades to deny for everyone, or assuming support that is not
there. Logging what real clients declare answers it with data before
stage 5 depends on it — and if the answer turns out to be "almost
nobody", that is the stop condition firing with evidence behind it.

Two properties the detector must have, both pinned by tests: a client
declaring elicitation is recognised (or `ask` degrades to a refusal for
everyone, and the ladder's middle collapses), and a client declaring
nothing is *not* treated as able to answer (or a level that should have
asked will silently proceed).

Note also what `ask` may **not** be wired to. ebman's two-phase
`confirm_token` flow looks like the obvious mechanism and is not one:
its own comment records that the agent which plans is the agent which
receives the token. That is agent-confirms-itself with human
*visibility* — a reasonable safeguard, and not a human in the loop.

### Decisions are not always one-shot

The model assumes a single pre-flight "may I?". ebman's own multi-region
rollout already disproves that: it re-reads the freeze *between*
regions, halts the un-dispatched ones, cannot recall those already sent,
and reports `skipped (rollout halted)`. So the engine needs
re-evaluation points and partial-completion semantics, and the refusal
document needs somewhere to say "stages 1–3 already executed".

This is worse for a tool whose action *is* a pipeline: an `ask`
arriving at stage 3 of 7 must pause something already running, which is
an async approval rather than a modal.

## Splitting it for reuse

**Engine — genuinely neutral, and smaller than it first looked.** The
decision type (with its obligations channel), the refusal-document
schema and serialiser, preset expansion *given tool-supplied rung
definitions*, the `safety explain` renderer, the audit record shape.

**Vocabulary — per tool, and it owns most of the hard work:**

- **Action derivation**, not merely action names. ebman's actions are an
  enum. pgman's come from `classify(sql)` — hundreds of lines of
  heuristics with CTE unwrapping, `WHERE`-refinement and escape
  detection. The engine consumes a classified action; producing one is
  the domain problem.
- **Attribute computation as functions, not tables.** "Reversible" is
  not a property of a verb. In pgman it is *manufactured* by wrapping in
  a transaction; on a Kubernetes tool, deleting a pod with an owner
  reference self-heals while deleting a PVC is data loss — same verb,
  and the answer depends on live state. So context gathering can involve
  I/O before a decision is even possible.
- **Resource resolution and hierarchy.** ebman's pins are a two-level
  lookup (env, then account). A namespaced tool needs ancestry — a pin
  on a namespace must cover a pod inside it. Cedar's answer is entity
  ancestry, which this note borrowed the four-tuple from and then
  dropped; either the engine grows ancestry matching or the vocabulary
  supplies `applicable_pins(resource)`, and the latter moves real policy
  logic back out of the engine.
- **Obligations**, per the section above.

### pgman is a migration, not a greenfield consumer

An earlier draft invented a pgman vocabulary — "databases, roles; query
/ vacuum / drop / kill-session". That was written from imagination, and
it is wrong. **pgman already ships a 2,375-line safety engine**
(`pgman/src/safety.rs`) with its own config at
`~/.config/pgman/safety.toml`. It has no role resources and no
kill-session action; `VACUUM` is folded into an `AlterDdl` category; and
its real action space is heuristic classification of free-text SQL.

Two consequences, and the first is the encouraging one:

**pgman independently arrived at `Guard { Allow, Confirm, Block }`** —
the tri-state of Principle 4, reached without coordination. That is the
strongest available evidence that the principle is real rather than
invented.

**But its ladder is a vector, not a scalar.** pgman configures a
per-database profile holding a tri-state *per statement category*
(insert / update / update-without-where / delete / delete-without-where
/ truncate / drop / ddl), plus orthogonal dials like
`statement_timeout_ms` and `auto_tx`. Its danger axis is *blast radius
of the statement* — a `WHERE`-less DELETE outranks a keyed DELETE on the
same table — where ebman's is *resource criticality × reversibility*.
The endpoints (`observe`, `unrestricted`) map cleanly; the middle rungs
do not decompose the same way.

So: rung **names** may be shared, rung **definitions** are per tool. And
that carries a cost worth stating — an operator or agent moving between
two tools will assume `guarded` means the same thing in both. If the
ladder is not genuinely comparable, do not reuse the names.

Note also that pgman's only principal today is a human at the TUI; it
has no MCP server and no headless write path. Its live override axis is
the **resource** (which database), not the principal. A design that
leads with `[safety.principals.*]` points its implementer at the wrong
axis first.

### On the tb-tui-common precedent

That crate shares theme, overlay, splash, font probing and text input —
presentational code with no domain meaning. It settles the *packaging*
question (crates.io publication, path override for co-development). It
does not settle whether policy **semantics** split cleanly, and the
first draft's "settled by precedent" over-claimed.

## What this actually costs in ebman

An earlier draft of this note claimed the write path funnels through one
pure gate, so widening it would be cheap. **That is wrong**, and the
correction changes the plan rather than a sentence.

There are **two** gates, and the divergence is deliberate:

- `cli::write_refusal` — CLI, MCP, and `lint --fix`.
- `App::read_only_reason` / `is_read_only_for` (src/app/safety.rs) — the
  ~25 TUI dispatch sites.

`src/config.rs` says so outright: the TUI paths *"deliberately do NOT
route through this — they compose additional session gates (global
`--read-only`, `:freeze-deploys`, demo mode) with their own precedence
and toast wording; only the config-pin layer is shared semantics."*

Nor does a guard pin "every dispatch site". The one that exists scans
`src/cli` for direct `pin_reason` calls — it catches the
half-composition that produced 0.14.1, not a path that calls neither
gate. TUI coverage is a different mechanism: behavioural sweeps over the
hand-maintained `WRITE_COMMANDS` list. And `write_refusal` is not pure —
it reads `AWS_PROFILE` from the environment as a fallback.

So the milestone order is:

1. **Converge the two gates** onto one decision function taking a
   fully-materialised context (no ambient env reads, no clock). This is
   the hard part and the note's original framing hid it. It is also what
   makes the engine extractable at all.
2. **Emit MCP annotations** — independently useful, and see below: it is
   also how the action vocabulary gets built.
3. **Levels** on top.

Shipping levels before step 1 means the shared crate is consumed by
ebman's CLI while ebman's own TUI stays on a fork — the worst possible
advertisement for it.

The residual risk is the inverted one: it is easy to make a gate richer
and hard to keep call sites honest about the new distinction. This repo
has been bitten exactly there — a widened `Option` flattened back by
`unwrap_or_default()` at every caller. After widening, grep for what
destroys the distinction and pin it with a guard.

## Deliberately not proposed

- **A policy language.** Levels plus per-resource pins cover the cases
  we have. Rego or Cedar-as-text is a large surface for a need nobody
  has demonstrated; revisit only when a real rule cannot be expressed.
- **Time-based rules** (no Friday deploys) in v1. Principle 7 wants
  them; they need a clock in the decision context, which is a testability
  question worth settling separately.
- **Anything that implies a security guarantee.** See Principle 1.
- **Hierarchical resources** in v1. Cedar's entity ancestry is the known
  answer and this note borrowed the four-tuple without it; ebman's pins
  are a flat two-level lookup and that is enough for environments. A
  namespaced tool needs it before it can adopt this.
- **Sharing rung names across tools whose ladders are not comparable.**
  See the pgman section. Shared names with different meanings are worse
  than different names.
