# Plan

The rolling window of work. `BACKLOG.md` is the reservoir; this is what
is active now.

**One hard rule: an item lives in exactly one of the two files.** Two
entries describing the same freeze PID-reuse hole appeared in
`BACKLOG.md` and survived several sessions before a grooming pass found
them. Duplication across two files would be worse.

`CLAUDE.md` is the working agreement — how to build, what green means,
what the stop conditions are. This file is only *what to do next*.

---

## How the loop runs

Each session: read this file, work the window top-down, prune and refill
it before finishing. Size the window by "enough that the next session
never idles", not by a fixed period — items get re-scoped as facts
arrive, and a batch committed a week ahead just goes stale. Three items
in flight is usually right; more than six means the window is a wish
list.

### Item classes

The stage set scales with the item. Assign the class when it enters the
window; upgrade it if the work turns out bigger than it looked.

| class | stages |
|---|---|
| **mechanical** — covered by an existing guard, or a test for logic that already works | dev → verify → green → commit |
| **behaviour** — changes what the tool does | analyse → dev → verify → docs → green → commit |
| **architecture** — refactor, new seam, anything touching >3 modules | analyse → design note → dev → verify → **review** → docs → green → commit |

Most items are mechanical. Forcing six stages onto them adds ceremony
and catches nothing: the best work of 2026-08-26 — the DLQ purge gate,
the nine wrong-env guards — went dev → verify → commit in one pass.

### Gates

**Verify-the-claim is its own gate, not part of "test".** Break the code
the test claims to pin, watch it fail, restore. An item is not done
without a `CAUGHT` line in the report.

This is not ceremony. On 2026-08-26, *five* tests covered less than
their names claimed — `field_token`, the `vpc_context` sibling guards,
the `spawn_listener` source anchor, the saved-configs inert set, the
`FORWARDED` cross-check. Every one was caught by re-applying the
mutation. **None** was caught by reading the test.

The other gates are `CLAUDE.md`'s: `cargo fmt --all`, `cargo clippy
--all-targets -- -D warnings`, `cargo test`, docs updated, backlog
updated.

### Outcomes

An item leaves the window as one of:

- **done** — gates passed.
- **re-scoped** — the work is real but different from what was written.
- **killed by evidence** — the premise was wrong. A legitimate outcome,
  and one to record rather than quietly drop: on 2026-08-26 the rule-3
  entry ("nothing sweeps for a handler that forgot the check") described
  a guard that would have looked for something that isn't there, the
  rollout freeze's "conscious choice" did not survive contact with the
  exposure window, and "widening the confirmation guard is not obviously
  mechanical" was simply false.
- **skipped** — a stop condition fired. Record which, in one line.

### Parallelism

Fan out **read-only** work freely: surveys, reviews, "which files touch
X". Concurrent *edits* to one checkout are a different matter — the
duplicate backlog entries above are what that looks like. Independent
dev items go in isolated worktrees; edits to `PLAN.md` and
`BACKLOG.md` stay on the main line.

### Measurement

Track **reachable** survivors from the nightly sweep, never the headline
percentage. `aws/eb.rs` at 86 survivors is 11 reachable and 75 SDK seam,
and quoting the raw number flatters the tree by counting 75 mutants no
test can kill.

### Architecture review — triggered, not scheduled

Periodic reviews get skipped; triggered ones do not. Fire one when any
holds:

- a function passes ~300 lines (`cli/lint.rs::run` at 622 would have
  tripped this long ago),
- a code review finds more than two defects,
- three releases have elapsed since the last one.

---

## Current window

*Refreshed 2026-09-25, after 0.44.0. Everything the 0.43 window
promised is shipped, ruled or verified live; the finished write-ups
moved verbatim to `docs/backlog/archive.md` ("PLAN.md window, retired
2026-09-25"). Items 1 and 2 were found buried inside entries marked
done — the third and fourth such finds in a week — which is why they
now head the list.*

### Now

1. **No `stage=asked` line has ever landed in the real audit log** *(measurement — needs Tom at the keyboard, ~5 min)*

   Found on 2026-09-25 while setting up the live typed-confirm
   verification (archived): `~/.cache/ebman/audit.log` holds zero, ever.
   Explained, not broken, as far as it was checked: demo suppresses
   every audit write (`writes.rs:2188`, and `Audited::record` at
   `:1017`), the dev registration is `--demo`, and the only real MCP
   write on record (a production `sqs-delete`, 2026-09-19 22:08,
   `dispatched`+`completed`, no `asked`) ran a 0.42.0 build — the stage
   first shipped in 0.43.0 (`b7e4ced`). But the "Not scheduled" latency
   ruling rests on *"`stage=asked` records the latency"*, and that has
   only ever been observed in tests. One non-demo confirm, even a
   decline, settles it.

   **First, a build that can write the line.** The `ebman` on PATH is
   Homebrew 0.42.0 (checked 2026-09-25), which predates `stage=asked`
   (0.43.0), so a test against it proves nothing. `brew upgrade ebman`,
   or register the local release build without `--demo`.

2. **`run_cycle`'s third side channel: it prints instead of reporting** *(architecture — needs a scope ruling before dev)*

   Nine sites wrote straight to the operator's terminal from inside
   the function extracted to make the cycle testable. Nine still do,
   re-counted 2026-09-25: `src/cli/lint.rs:652` in `run_cycle`,
   `CycleReport::degrade`'s own `eprintln!` (`:393`), and seven in
   `apply_fixes_for_env` (`:2367`–`:2471`). The old `:1089` site was
   the EBL015 bug fixed in c704ea0. Output no test can assert on — the
   same shape as the two channels already closed.

   **Not one item.** The two in `run_cycle` are mechanical. The
   seven in `apply_fixes_for_env` are interactive `--fix` output
   and are a design question (does the report carry rendered lines,
   a typed event stream, or a writer?), so they need a ruling
   rather than a guess. Split accordingly when it is picked up.

   **2026-09-25: the maintainer does not have a preference.** The
   recommendation stands on its own: a TYPED report (events the cycle
   returns, rendered by `run`), because the MCP `lint` tool is a second
   consumer and a writer or pre-rendered lines would make it re-parse
   text. The two `run_cycle` sites are mechanical either way.

   Evidence the class is live, not cosmetic: chasing it on
   2026-09-24 turned up a real bug at `:1089` — a partly-failed
   EBL015 pass printed behind `!quiet` and exited clean. Fixed
   separately; two mutations CAUGHT. That is three defects this
   function has now shipped in its output wiring.

3. ~~**Re-scope stage 5 against runtime-grants**~~ — **done 2026-09-25:
   re-scoped, and most of stage 5 killed by evidence.** The question was
   "what do levels still buy after 0.42?" Rung by rung, against what
   0.42–0.44 ship:

   | rung | what it was for | what already does it |
   |---|---|---|
   | `observe` | reads only | `--read-only`, `safety.read_only`, and "a client that cannot ask gets reads" |
   | `guarded` | reversible non-prod writes; the rest asks | on an elicitation client EVERY write asks — stricter than this rung |
   | `trusted` | prod allowed; irreversible asks | the same; stricter again |
   | `unrestricted` | no level-based refusal | the parity default |

   The middle rungs were defined in terms of asking, and request-as-unit
   made asking universal wherever it is possible. Where it is NOT
   possible — the headless CLI (`action --yes`, `lint --fix --yes`,
   `audit replay --yes`) — "ask" degrades to deny, so a level there is
   just a verb/env restriction. That is the ONE population levels would
   newly govern, and nothing on record asks for it: no CI user, no
   incident. `Decision { obligations }` has no consumer either — the one
   real obligation (type the env name for terminate / dlq_purge) is
   hard-coded per verb and fine that way.

   **What survives, because it is needed for other reasons:** the typed
   verb vocabulary. It is the prerequisite three backlog items were
   parked on "until stage 5" (`write_refusal` split, the neutral action
   vocabulary, the annotations table's home); it is the writer-side fix
   for the audit-label split (2bdd4b7 fixed the reader side); the
   implementation review named it the missing piece for stage 6; and
   assume-role needs it to map a verb to a role. So assume-role does not
   replace stage 5 — it builds on the one part of it worth keeping.

   **Needs a ruling before it can be built — see item 3′ below.**

3′. ~~**The typed verb vocabulary**~~ — **done 2026-09-25 (fb35586)** on the maintainer's ruling (`RestartAppServer`, `SetOption`). Scoped to the verbs more than one surface writes; guarded by `no_surface_spells_a_shared_verb_itself`. The `write_refusal` split it was meant to absorb is back in BACKLOG.md on its own merits.

   *Original entry:*

   One `Verb` enum carrying `audit_label()`, `destructive()` and the
   foreclosure text; `CliVerb` / `ReplayVerb` / `WriteVerb` / the TUI's
   `Action` stay as parse layers mapping into it; audit writers take a
   `Verb`, not a `&str`. Absorbs the three parked backlog items.

   **The ruling:** which spelling every surface WRITES. Restart is
   `RestartAppServer` on three surfaces and `Restart` on MCP; option
   writes are `SetOption` (MCP, batch, `lint --fix`) or
   `UpdateOptionSettings` (TUI forms, deploy). The reader already
   accepts both (`audit::ACTION_ALIASES`), so either choice is
   compatible with every existing log; it changes what NEW lines say.
   Recommendation: `RestartAppServer` (the majority, and the AWS API
   name) and `SetOption` (the verb, not the call; shorter in a filter).

### Next

4. **Assume-role elevation for MCP writes** (`runtime-grants.md`
   layer four / step 6). `sts:AssumeRole` is the time-boxed grant done
   properly: STS enforces the TTL, the role policy is the ceiling, and
   CloudTrail audits it independently of ebman — so the record does not
   depend on ebman being honest about itself.

   `AwsClient::assume_role` already exists for cross-account switching,
   so the plumbing is there. Buys nothing for a setup running as admin,
   which is why it is not first; the maintainer asked for it to be
   designed now and rolled out in a release soon after, not built into
   the current cut.

   Attribution is the sleeper benefit: writes land as the assumed role
   rather than the operator's own identity, so "what did the agent do"
   is answerable from CloudTrail alone.

   *Moved from `BACKLOG.md` on 2026-09-25.* Sequenced after item 3
   because it IS runtime-grants layer four: the re-scope decides whether
   it replaces part of stage 5 or builds on it, so neither gets
   designed twice.

### Ready to release

`[Unreleased]` carries a user-visible fix — a partly-failed EBL015
pass now degrades the run (non-zero exit) instead of exiting clean —
and two packaging guards. Enough for a patch release whenever the
maintainer wants one; not scheduled here.

### Later — the remaining protection-levels stages

*Stages 1–4 and 4b are done (archived). Each stage must be
independently shippable and useful even if the next one never
happens; if that stops being true, the stage is wrong. Item 3 above
decides what stage 5 still is.*

5. ~~**Levels, and the decision type they need**~~ — **killed by
   evidence 2026-09-25** (see item 3 above); only the verb vocabulary
   survives, as item 3′. Kept below for the reasoning, and because the
   headless-CLI case is the one thing that would reopen it.

   *Original entry:*

   Named rungs over the decision function, per principal, effective
   level = minimum of matching entries.

   This now also carries what stage 4 deferred: `Decision { outcome,
   obligations, refusal }`, and the correlation id that ties a refusal
   to the retry that followed it. Both get real producers here —
   `guarded` is precisely "allow with an obligation" — so they can be
   built against a consumer rather than guessed at. pgman's shipped
   `Decision` (`wrap_in_tx`, `read_only_escape`) stays the evidence for
   the shape.

   ~~Requires the config parser to **fail closed**~~ — **done
   2026-09-09**, ahead of the levels themselves, because the fail-open
   was live: `safety.envs.prod = true`, `.readonly`, and a non-boolean
   value were each skipped in silence, leaving the env writeable while
   the operator believed it pinned. Now a line under `safety.` that
   cannot be acted on refuses every write, with the offending line named
   at startup and in the refusal. Five mutations CAUGHT.

   The levels inherit this: a typo'd level name refuses rather than
   granting the default.

   Ship with `ebman safety explain`, or the preset is unauditable and
   Principle 6 is violated by its own implementation.

6. **Extract the shared engine** *(architecture — only after 1–5 settle)*

   And treat pgman as a **migration**: it has a shipped engine, a
   different ladder shape (a per-category vector, not a scalar), and a
   config file with users. The open question is config compatibility,
   not adoption.

   **Do not start this early.** An engine extracted from one consumer is
   a guess about the second.

### Not scheduled

- **Refusing an implausibly fast answer.** `stage=asked` records how
  long a confirmation took, and a sub-second answer is not somebody
  reading a foreclosure line — headless `claude -p` auto-declines in
  well under the 22 seconds its whole session took, against minutes
  for a human on the same dialog. **Ruled 2026-09-20: record it, do
  not gate on it.**

  Two reasons, and the second decides it. A threshold does not stop
  anyone who means it — a client that auto-answers can sleep two
  seconds first, one line — so it catches only accidents, at the cost
  of refusing a fast human who already knows what a purge does. And
  the case measured is an auto-*decline*, which is fail-safe; the
  dangerous shape is an auto-*approve*, which no known client does. A
  magic number defending an unobserved threat while misfiring on
  observed behaviour is the wrong trade.

  This also keeps the design honest about its own limit: a false
  attestation is not detectable at the time, and latency does not
  change that — it makes it reviewable afterwards, which is the
  smaller and true claim.

  **What reopens it:** a client observed auto-*approving*. Then
  surface it in the result first (dispatch, but say "answered in
  180ms") and only gate if it proves common — at which point the
  threshold is chosen from the latency distribution already in the
  log, not guessed. That is the argument for logging now: it is what
  generates the evidence any gate would need.

- **Hierarchical resources.** Cedar's entity ancestry is the known
  answer; ebman's flat env/account pins do not need it. A namespaced
  tool would.
- **Time-based preconditions** (no Friday deploys). Wants a clock in the
  decision context, which is a testability question worth settling on
  its own.
- **A policy language.** Declining this remains the best decision in the
  design note.
