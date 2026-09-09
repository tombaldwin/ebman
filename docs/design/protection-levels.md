# Protection levels for operator tools

**Status:** design note, nothing implemented. Written 2026-09-09 for
`ebman`, but the rules in [Principles](#principles) are meant to hold for
any tool that lets a person — or an agent acting for them — change
production. `pgman` is the second consumer and the reason this is
written generally.

## The problem

An operator tool that can only refuse everything or permit everything
forces a bad choice. Turn protection off and it protects nothing; leave
it on and people work around it. Agents sharpen this: they act faster
than a human reviews, they retry, and given an opaque refusal they will
look for another route to the same effect.

`ebman` today has most of the raw material — a per-env `read_only` pin,
account pins, a session `--read-only`, a cross-process deploy freeze,
confirm modals, a 5-second undo window, an audit line per dispatch, and
MCP writes off unless `--allow-writes`. What it does not have is a way to
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

So: friction proportional to blast radius, and never a *silent* escape.
`ebman`'s type-the-queue-name-to-purge is the right shape — it cannot be
muscle-memoried, and it is auditable. A `--yes` that skips everything is
the wrong shape.

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
worse name.

### 5. A principal cannot raise its own level.

The ceiling is set by *who is asking*, established outside the request —
a server flag, a config file, a credential — never by a parameter the
caller supplies. `ebman`'s `--allow-writes` already has this property
because it is a server flag; keep it when the model gets richer.

Corollary: identify principals distinctly enough to be useful. "An
agent" is too coarse; `mcp:claude-code` is a principal, and a human at
the TUI is a different one.

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
is enforceable, inspectable and overridable-with-a-record. The same
machinery that answers "may I?" can answer "should I, yet?" — and this
is where a tool stops being a permission system and starts encoding how
the team actually works.

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

Per-principal, so an agent and a human can differ:

```toml
[safety]
level = "trusted"                      # a human at the TUI

[safety.principals.mcp]
level = "guarded"                      # anything arriving over MCP

[safety.principals."mcp:claude-code"]
level = "observe"
```

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

`remedy` is one of `human` (someone must raise the level or act),
`config` (a pin is in the way), `wait` (a freeze or window will expire),
`other-resource` (this action is fine elsewhere).

## Splitting it for reuse

- **Engine — shared, domain-neutral.** Principal × action × resource ×
  context → decision + refusal document. Level expansion. The
  `safety explain` renderer. No knowledge of environments or databases.
- **Vocabulary — per tool.** `ebman`: environments, applications;
  deploy / restart / rebuild / terminate / purge. `pgman`: databases,
  roles; query / vacuum / drop / kill-session.

This is the split `tb-tui-common` already models for theme and overlay
code, so the packaging question is settled by precedent: a small shared
crate, consumed by both.

## Why this is tractable in ebman

`deny_write` is called from a dozen-plus sites, but they funnel through
one pure function — `write_refusal(config, env, profile, freeze)` — and
a guard test already pins that every dispatch site routes through it.
Widening the model is therefore widening one seam's inputs (add
principal and action) and its output (a document instead of a bool), and
the call sites inherit it.

The risk is the opposite of usual: it is *easy* to make the gate richer
and hard to keep every call site honest about the new distinction. This
repo has been bitten by exactly that — a widened type flattened back to
the old shape by `unwrap_or_default()` at every caller. Any change here
needs the same discipline: after widening, grep for what destroys the
distinction, and pin the result with a guard.

## Deliberately not proposed

- **A policy language.** Levels plus per-resource pins cover the cases
  we have. Rego or Cedar-as-text is a large surface for a need nobody
  has demonstrated; revisit only when a real rule cannot be expressed.
- **Time-based rules** (no Friday deploys) in v1. Principle 7 wants
  them; they need a clock in the decision context, which is a testability
  question worth settling separately.
- **Anything that implies a security guarantee.** See Principle 1.
