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

### Now

0. **Blocked on the clock:** the 03:00 UTC sweep. Everything else in
   "Now" is done.

1. **Triage tonight's sweep** *(mechanical)* — first full-tree run since
   133 tests landed. Re-baselines every per-file figure below. Split
   reachable from seam before drawing any conclusion.

2. ~~Sweep tail~~ — **done 2026-08-27.** `aws/eb.rs`'s reachable
   remainder, `drain_webhooks`, `mode_dlq_handlers.rs`, `forms.rs`'s
   Boolean/MultiSelect arms and `spawn_refresh.rs`'s throttle back-off.
   Nineteen mutations, all CAUGHT.

   Left behind on purpose, and both accounted for rather than hidden:
   `audit.rs`'s writer seam (~15) and `action_flow.rs`'s `spawn_action`
   are the same `replace <fn> with ()` shape as the SDK seam. What is
   still *reachable* is four survivors in `aws/eb.rs::list_events_inner`
   — paging logic that needs the loop split from the SDK call, which is
   an architecture item rather than tail work.

3. ~~Two decisions~~ — **done 2026-08-27.** The freeze marker now
   checks process start time as well as pid existence, and the `!yes`
   gate has a positional guard. Both recorded in `BACKLOG.md` with what
   was rejected and why.

### Next

4. ~~`lint::run` split~~ — **re-scoped 2026-08-27, and the original
   justification was wrong.** The item claimed the split "makes its
   remaining ~29 survivors reachable at all". Reading them: they are
   `!quiet` / `!json` output-suppression, the lowest-value survivor
   class there is, and reaching them means asserting on captured
   stdout. Meanwhile the safety net for a 604-line refactor of the
   subcommand that gates users' CI is four `tests/cli.rs` invocations,
   none of which test behaviour — two arg-validation rejections and a
   help-text check.

   So the split was not done. What was done instead is the part that
   carried the risk: `--baseline` drift and the `--watch` interval were
   *decisions* with no cover, sitting inline between two pages of
   printing. Both are now pure, tested functions (9 tests, 6 mutations,
   all CAUGHT). All 26 pre-existing tests in `cli/lint.rs` were
   argument parsing.

   The 604-line body remains, and still trips the ~300-line trigger at
   `PLAN.md:91`. It is now a *readability* item rather than a coverage
   one, and it wants the integration-test net built first — which is
   what item 6 is for. Re-file it after the QA lane, not before.

5. ~~Cursor wrap unification~~ — **done 2026-08-27.** 22 sites across
   11 modules, not the 12 across 5 the backlog recorded. The per-site
   tests written on 2026-08-26 are what made it safe, and breaking the
   shared helper now fails 12 of them — which is the evidence the
   migration is wired rather than merely compiling.

6. ~~QA lane~~ — **premise disproved 2026-08-27.** The item called a
   PTY-driven `--demo` + `ctl` harness "the only route to the ~440
   `ui/draw_*` survivors". It is not a route at all in the sense meant:
   those survivors were already reachable. `crate::ui::draw` is called
   by 56 render sites in the suite via `support::render`, and the
   survivors persisted because nothing asserted on the output.

   Shown rather than argued: three mutations in three different `ui/`
   files (footer's first-run row, header's alert plural, table's pin
   star) were each NOT CAUGHT, then CAUGHT after six tests using the
   harness that already existed. Cost: no new dependency, no PTY, no
   subprocess.

   The ~440 also turned out to be two populations. Layout arithmetic
   stays deferred for the reason the backlog already gave — pinning a
   pixel breaks on every legitimate layout change. State-reporting
   branches (`read_only`, `alerts`, `pinned`, `first_run_hint`) are
   worth pinning and four now are, the read-only badge in both
   directions. The remaining state-reporting branches are not
   enumerated; `BACKLOG.md` records the honest number as "unknown
   subset of 440".

   A `ctl`-driven lane may still be worth building, but for what it
   uniquely tests — the socket transport, key-spec parsing end to end,
   and `main.rs`'s TUI setup, none of which `cargo test --lib`
   compiles. That is a different and much smaller item, and it is the
   net `lint::run` (item 4) wants. Re-file it that way.

### Then

7. **Parked calls** *(behaviour)* — `WRITE_COMMANDS`, `serde_yml`,
   Unicode column math. Decide or delete; none should stay open a third
   time.
8. **Release** — full procedure per `CLAUDE.md`, including
   `cargo semver-checks` against the last published version *as a
   patch*.
9. **Features** — `:queue` inspector, mouse column resize, pill caps,
   `:custom-platform-create`, then the speculative tail.

### Not scheduled

The ~440 `ui/draw_*` survivors, on purpose. If they are ever wanted the
shape is golden-frame snapshots at fixed sizes (`insta`, as `pgman`
uses), not per-survivor tests — and item 6 is the prerequisite.
