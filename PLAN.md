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

*Refilled 2026-10-29: the CLI body paths, the injection and ticket
seams and the MCP freeze tool are done (archived verbatim, "PLAN.md
items done 2026-10-29"); three BACKLOG items moved in. Refreshed
2026-10-24, after 0.47.1. The 2026-09-25 window moved
verbatim to `docs/backlog/archive.md` ("PLAN.md window, retired
2026-10-24"): `run_cycle`'s side channel and the App tab are done, and
both items still open there needed the maintainer at a real account,
so they are one item here. `lint --fix`'s mid-run freeze was ruled
(skip and finish) and is done, along with the precedence item behind
it.*

### Now

1. **Live checks against a real account** *(measurement — needs Tom at
   the keyboard, ~10 min)*

   Two claims rest on tests alone:

   - **`stage=asked` has never landed in the real audit log.** The
     step records how long an MCP write waited for its approval, and
     the "Not scheduled" latency ruling rests on it. `--demo` writes no
     audit lines, and the dev registration is `--demo`, so it needs a
     non-demo MCP connection on a build of 0.43.0 or later (Homebrew is
     0.47.1). One confirm, even a decline, settles it.
   - **The App tab against AWS.** Discovery has run on a real fleet;
     the SEARCH expression and its dynamic instance label, metric math
     over it, the Logs Insights `parse`/`stats` query and
     `CacheClusterId` naming for Redis nodes have not. One configured
     env, opened in the TUI and called through `app_metrics`, settles
     all four.

2. **Assume-role elevation for MCP writes** *(architecture — design
   note first)* (`runtime-grants.md` layer four / step 6).
   `sts:AssumeRole` is the time-boxed grant done properly: STS enforces
   the TTL, the role policy is the ceiling, and CloudTrail audits it
   independently of ebman — so the record does not depend on ebman
   being honest about itself. Attribution is the sleeper benefit:
   writes land as the assumed role rather than the operator's own
   identity, so "what did the agent do" is answerable from CloudTrail
   alone.

   `AwsClient::assume_role` already exists for cross-account
   switching. Buys nothing for a setup running as admin. The maintainer
   asked for it designed first and rolled out in a release soon after.
   The note decides whether it replaces part of runtime-grants stage 5
   or builds on it, so neither is designed twice. **Drafted
   2026-10-24, accepted 2026-10-29 with all three contested calls as
   written:** `docs/design/assume-role-elevation.md`. Next: its five
   slices, in order. It builds on the shipped ask
   (stage 5 was killed on 2026-09-25, so there is nothing to replace)
   and splits into five slices. Its three contestable calls are listed
   at its foot.

3. *(mechanical)* **The `dlq_undo` refusal audit line records `region="-"`** when the
   delete used the default region, while the dispatched / completed lines
   record the resolved one. Same pattern as the other MCP gate refusals —
   fix the class. *0.45 release review.*

### Next

4. *(behaviour)* **Demo audit suppression in one place.** Done per call site: MCP
   checks `Backend::Demo` at seven sites, the TUI `!self.demo_mode` at
   several more, and it has already leaked once. Evidence it is still
   inconsistent: `:thaw-deploys` writes an audit line in demo mode, and
   0112e6d's `:readonly` does not. Shape: a process-wide audit sink mode
   set once at startup, as `set_notify_webhook` does. S.
   `freeze_fleet` (2026-10-29) is an eighth MCP `Backend::Demo` check.

5. *(architecture)* **`App::new` and `App::for_tests` each build the ~115-field struct
   by hand** (`src/app.rs`, two literals of ~260 and ~175 lines).
   `for_tests` is production code too — `new_demo` builds on it — and
   nothing checks that the two agree on defaults, so every new field is
   two edits. Shape: a shared `App::assemble(aws, config, seed)`. S–M.
   From the 2026-09-25 implementation review.
   Since 2026-10-29 the freeze store is one more default the two must
   agree on (`FreezeStore::real()` / `isolated()`).

### Released

**0.47.1 (2026-10-24)**: rebuild without the AWS SDK's SigV4a signer,
which ebman never used and which carried `crypto-bigint` 0.5
(RUSTSEC-2026-0387, no fixed 0.5 release). It landed upstream after
0.47.0's CI and failed cargo-deny on the next push.

**0.47.0 (2026-10-24)**: a fleet freeze no session owns (`ebman freeze`
/ `ebman thaw`, one file per holder under `freeze.d/`, `freeze.json`
kept as a mirror for older readers); costs, Detail, forms, the DLQ
viewer, the pending panel, the saved cursor and terraform matched per
env rather than per name under `:region all`; every CLI subcommand
behind a testable body, with `--help` and a pinned exit-code table; and
the two release reviews' fixes. Breaking: MCP `fleet_cost` keys
`NAME@REGION`, `--help` exits 0, usage before refusal in `action
deploy` / `rollout`, `freeze::clear_marker_if_own` removed.

**0.46.2 (2026-10-23)**: lockfile-only rebuild for RUSTSEC-2026-0385
(`ctutils` 0.4.5, reached through the SDK's `digest`/`hmac`) and the
yanked `clipboard-win` 5.4.1. Both landed upstream after 0.46.1's CI
and failed cargo-deny on the next push.

**0.46.1 (2026-10-23)**: the header shows `+N` under `:region all`;
`:region NAME` and a config edit to `extra_regions` re-derive a running
fan-out; `:region all` with no extras says so and drops the REGION
column; Homebrew installs shell completions; the release workflow's
notes scan no longer dirties the checkout. Not live-checked against
real AWS before the tag; the fan-out paths are covered by tests only.

**0.46.0 (2026-10-22)**: firing alarms and the platform version on the
fleet view (and in `ebman envs` / `list_environments`); the App tab with
in-UI editing and discovery; writes, audit lines and Detail routed by
`EnvKey`, so a name in two regions is two envs. The first crates.io
publish failed on the new notes scan leaving `notes.md` in the checkout;
fixed in `release.yml` and re-run from `main`. Not live-checked against
real AWS before the tag.

**0.45.2 (2026-09-30)**: the MCP server instructions are one text,
pinned whole under Claude Code's 2048-character cut (longest 1693); the
long form is gone and its rules folded in. `requiresUserInteraction`
decided against (archived).

**0.45.1 (2026-09-30)**: MCP writes under Claude Code's auto mode. The
instructions now lead with a digest that survives Claude Code's
2048-character cut, and the allow-rule path was verified live on 2.1.285.
Open decisions from it are in BACKLOG.md (`requiresUserInteraction`, the
three copies of the guidance).

**0.45.0 (2026-09-25)** carries everything the window above marked done,
plus the release review's fixes: the lint assembly shared by every
surface (`lint::inputs` + `app::tui_lint`), coverage gaps that say what
could not be checked instead of reading clean, and the typed verb
vocabulary. Deferred review findings are open items in BACKLOG.md.

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
