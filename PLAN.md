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

*Refreshed 2026-09-09. The previous window (narrow-terminal work, the
mutation sweep, 0.35.0 and 0.36.0) is closed — see `CHANGELOG.md` and
`docs/backlog/archive.md`.*

### Done since the last refresh

- **0.36.0 shipped** — the narrow-terminal release, reviewed by a
  three-way panel that found four things before they went out, including
  a backup file bound for the crates.io tarball.
- **Whole-tree mutation sweep completed locally** — 6209 mutants, 63.3%
  kill rate against 52.1% at the previous full sweep. All 105 survivors
  in the changed-code slice triaged; three recorded as genuine
  equivalents so nobody re-investigates them.
- **`docs/design/protection-levels.md`** written and revised after
  review.

### Now — protection levels

The design note is agreed in principle. What follows is the
implementation order, and the ordering is the important part: the
reviews showed that doing these in the obvious sequence produces work
that has to be thrown away.

Each stage must be independently shippable and useful even if the next
one never happens. If that stops being true, the stage is wrong.

1. ~~Converge the two write gates~~ — **done 2026-09-09.**

   `cli::write_refusal` and `App::read_only_reason` are separate
   implementations over different inputs, and `src/config.rs` documents
   the divergence as deliberate. Nothing shared with pgman is possible
   until there is one decision function.

   Done means: one function over a fully-materialised context — no
   ambient `AWS_PROFILE` read, no clock, no `App` — with the TUI's
   session gates (global read-only, freeze, demo mode) expressed as
   context rather than as a second implementation. Toast wording stays
   where it is; only the *decision* converges.

   Guard it: the existing check catches half-composition in `src/cli`,
   not a path that calls neither gate. A dispatch site that reaches
   neither should fail a test.

   `src/write_gate.rs` holds `decide(&WriteContext) -> Option<Refusal>`:
   values only, no `App`, no `Config` methods, no `std::env`, no clock.
   Both `cli::write_refusal` and `App::read_only_reason` now consult it
   and render the result in their own voice — the TUI keeps the freeze
   age and the `:incident END` hint, the CLI keeps `refusing ENV —
   pinned by …`. Converging the messages too would have been a visible
   regression for no benefit.

   The converged precedence (global → freeze → env pin → account pin) is
   the union of both, and preserves each: the CLI never sets the global
   rung, so its old order is untouched.

   `Config::pin_reason` is gone — it was the third implementation. Its
   tests moved: the precedence cases to `write_gate`, and the replay one
   ported to go through `write_refusal`, which pins the path it claims
   to rather than a helper.

   Five mutations CAUGHT, and two of them were the interesting ones. An
   account pin applying with no profile resolved was NOT caught until
   the fixture gained an empty-string account key — a malformed config
   line produces one, and without it the bug's lookup simply misses. And
   the CLI guard could be blinded entirely with the suite staying green,
   because it only ever fired if someone introduced a violation; it now
   carries a canary that proves it detects on every run.

2. ~~Emit MCP tool annotations~~ — **done 2026-09-09.**

   `readOnlyHint` / `destructiveHint` / `idempotentHint` /
   `openWorldHint` on every tool descriptor. Verify the field names
   against the current spec revision first.

   `src/cli/mcp/annotations.rs` holds one table classifying all 14
   tools, applied in `tool_table` rather than at each descriptor so a
   guard can check the table against what is actually advertised.
   Verified on the wire by driving the real server over stdio in both
   modes: 14 tools, 0 unannotated.

   The classification that took the most thought is `confirm_action`,
   annotated at its **worst case** — it dispatches whatever is pending,
   which may be a terminate, so a client trusting `destructive: false`
   would skip the prompt on exactly the call that needs one. It is also
   the only non-idempotent tool: its token is single-use.

   `restart`, `deploy` and `set_option` are deliberately NOT destructive.
   Flagging everything teaches clients to ignore the flag.

   Five mutations CAUGHT. As intended, the table doubles as stage 4's
   action vocabulary.

3. ~~Specify `ask` per surface~~ — **decided 2026-09-09, and instrumented.**

   Not code: a decision, written down, about what `ask` means on TUI,
   CLI and MCP, and what it degrades to when the transport cannot carry
   it. MCP degrades to *deny*, never to allow.

   This is deliberately ahead of levels. `guarded` and `trusted` are
   defined in terms of asking; a ladder whose middle rungs cannot be
   expressed over the primary agent transport is sugar over nothing.

   The decision is in the design note. The part worth repeating here:
   elicitation is a CLIENT capability declared at `initialize`, so
   whether `ask` is expressible is knowable per connection rather than
   assumed. ebman was throwing that field away; it now captures and logs
   it, and nothing branches on it yet.

   **The stop condition is now instrumented rather than hypothetical.**
   If the logs show almost no client declaring elicitation, the ladder's
   middle rungs collapse to deny and stages 4–5 need re-planning — but
   that will be a conclusion from data, not a guess. Three mutations
   CAUGHT on the detector, including one that would have made every
   client look incapable.

4. ~~Audit every refusal, with its rule and its remedy~~ — **done
   2026-09-09.** Re-scoped; see below.

   `stage=refused` lines across all four enforcement funnels: the TUI's
   `deny_write` / `deny_write_batch`, and `cli::write_refusal` behind
   `ebman action`, `action rollout`, `audit replay`, `lint --fix` and
   both MCP write phases. Each names the rule (`env_pinned`,
   `account_pinned`, `frozen`, `global_read_only`) and a remedy naming
   the exact config key.

   The near-miss is now visible: an agent attempting `terminate` on a
   pinned prod leaves a line per attempt instead of nothing.

   Wired at four funnels rather than ~25 dispatch sites, which the
   existing `cli_write_paths_do_not_reach_past_the_shared_gate` guard is
   what makes safe. `read_only_reason` split into `refusal_for` (typed)
   and `render_refusal` (wording) — the audit needs the rule name, and
   rendering is exactly what discards it.

   Eight mutations CAUGHT across the two halves.

   **Re-scoped: the obligations channel moves into stage 5.** Stage 4 as
   written also carried `Decision { outcome, obligations }` and a
   correlation id. Both were deferred *because nothing produces or reads
   them yet* — the first obligation ("allow, but type-to-confirm")
   arrives with the levels, and a channel with no producer is the
   dead-field defect this repo has now hit three times in one day
   (`client_supports_elicitation` written and never read; a client cache
   added that nothing read; `pin_reason` as a third gate). Adding it
   early would not have made stage 5 cheaper; it would have shipped a
   plausible-looking struct field that no test could fail on.

5. **Levels, and the decision type they need** *(behaviour)*

   Named rungs over the decision function, per principal, effective
   level = minimum of matching entries.

   This now also carries what stage 4 deferred: `Decision { outcome,
   obligations, refusal }`, and the correlation id that ties a refusal
   to the retry that followed it. Both get real producers here —
   `guarded` is precisely "allow with an obligation" — so they can be
   built against a consumer rather than guessed at. pgman's shipped
   `Decision` (`wrap_in_tx`, `read_only_escape`) stays the evidence for
   the shape.

   Requires the config parser to **fail closed**, which is its own
   change: today a malformed line is silently skipped, so a typo'd level
   would grant the default.

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

- **Hierarchical resources.** Cedar's entity ancestry is the known
  answer; ebman's flat env/account pins do not need it. A namespaced
  tool would.
- **Time-based preconditions** (no Friday deploys). Wants a clock in the
  decision context, which is a testability question worth settling on
  its own.
- **A policy language.** Declining this remains the best decision in the
  design note.

### Also open

- 17 backlog items, mostly design rulings and accepted seam.
- `draw_table`'s inline `DisplayRow::Env` arm — re-measured and left as a
  readability item with a known borrow-checker wall.
- The sub-60-column table cliff, recorded with two options.
