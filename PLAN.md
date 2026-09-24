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

*Refreshed 2026-09-19, after 0.42.0. The previous window's blocking
question — whether an MCP client can be asked something mid-request —
is answered, measured and shipped, which unblocks stage 5 and changes
what the remaining stages are for.*

### Done since the last refresh

- **0.42.0 shipped** — writes without a flag on any client that can ask
  the operator; `--read-only`; batch DLQ plans; caller identity in every
  plan; `writes_via`. Live on crates.io, Homebrew and the MCP Registry,
  and **exercised against real infrastructure**: a dead-lettered message
  cleared on the uFlexi fleet through plan → dialog → approve.
- **The elicitation round-trip is observed, not inferred** — approve,
  decline and walk-away, against a real client. Stage 3's instrument is
  no longer dead.
- **Four reviewers found a fail-open before it shipped** — a dead ask
  channel dispatched an unapproved `terminate`. A re-review after the
  fix pass found three more defects the fix pass had itself introduced,
  including a guard that asserted a defect and talked a second reviewer
  out of reporting it.

### Now — 0.43

Three items, all of them promises already made in public. The tag notes
name the first two as deliberate gaps, so they are debts, not ideas.

0. **A declared capability is not a human** *(behaviour + measurement)*
   — **partly done 2026-09-19.** `stage=asked` records the question,
   answer and latency; `doctor` and `safety-and-privacy.md` now state
   the limit and name the over-claim to avoid ("the confirmation was
   approved", never "a human approved it"). ~~Still owed: script one
   non-Claude-Code elicitation-declaring client~~ — **done
   2026-09-20**: `a_scripted_client_can_complete_a_typed_confirmation`
   drives the real binary over stdio, declares elicitation, and
   answers its own dialog including the typed field. The protocol half
   no longer depends on one vendor. It still cannot tell you whether a
   human-facing UI renders an input box, which is the part only a live
   client answers.

   *Original entry:*

   The load-bearing claim of 0.42.0 is that writes are safe by default
   because a person answers. Nothing verifies a person is there.
   `effective_scope` grants every verb on a self-reported capability
   bit in the client's `initialize` frame, and a framework that
   declares `elicitation: {}` and routes the question to its own model
   satisfies every ask — producing a **cleaner** audit trail than an
   honest operator, which `protection-levels.md` calls out by name as
   the wrong incentive.

   That note prescribed the mitigation and 0.42.0 shipped without it.
   Partly repaid already: `stage=asked` now records the question, the
   answer and the latency, so an approval leaves a trace and a
   twenty-millisecond answer is distinguishable from someone reading a
   foreclosure line. Remaining:

   - `doctor` and the docs must never describe elicitation-approved as
     "a human confirmed" — the honest phrasing is that the client said
     it would ask.
   - **Script one non-Claude-Code elicitation-declaring client.** The
     whole "asking is a known quantity" claim rests on n=1. This is the
     same twenty minutes item 2 gets, and it is worth more.

   Note for whoever picks this up: item 1 is **no defence here.** A
   self-answering client types an environment name back as readily as
   it clicks. Name-back defends against a rubber-stamping human, which
   is a different threat.

1. ~~**Terminate name-back parity**~~ — **done 2026-09-19**, and widened to `dlq_purge` on the maintainer's ruling: both are strict-typed-name confirms in the TUI, so both are now typed over MCP. Fails closed on a client that cannot render a text field. **Still owed: the live verification** — the non-empty `requestedSchema` has not been exercised against a real client, which was the whole reason this was sequenced behind a prototype. Do that before it ships in a tag.

   *Original entry, for the reasoning:*

   In the TUI a human types the environment name back before a
   terminate. Over MCP `confirm_name` is supplied by the AGENT, so the
   human's whole contribution to destroying an environment is one
   click. "Same bargain as the TUI" is the justification for
   writes-by-default, and for this one verb it is not true.

   Elicitation supports the missing gate: a `requestedSchema` with a
   required string property, validated against the env name, refusing
   on mismatch.

   **The ordering is the point.** A non-empty `requestedSchema` is
   client behaviour nothing has exercised — the live matrix validated
   the zero-field form only. So: prototype the schema, verify against a
   real client, THEN ship. Shipping it untested would repeat the exact
   sin 0.42.0 spent a day correcting.

   Fails closed if a client cannot render it: no typed field means no
   match, which refuses. That is the right direction, and it is also
   why this must be measured — a silent refusal of every terminate is a
   poor way to discover a rendering gap.

2. ~~**The headless configuration**~~ — **measured 2026-09-20, and
   the measurement changed the fix.** `claude -p` declares
   `elicitation: true`, gets `writes: every verb`, and does **not**
   hang: it auto-declines a confirmation in under a second.

   So there is no availability problem — and a worse one. The reply
   was recorded and reported as *"declined by the operator"* with no
   human anywhere in the session, and the agent relayed "the operator
   simply said no" to its user. A false attribution, produced by the
   tool, repeated by the agent.

   Fixed: nothing now claims a person answered. The reason reads "the
   confirmation was declined", and the guidance tells the agent not to
   attribute it, naming `-p` and CI harnesses as the concrete case.
   The flat do-not-retry prohibition is unchanged.

   `stage=asked` records the latency, which is what makes the two
   distinguishable afterwards — a sub-second decline is not somebody
   reading a foreclosure line.

   **Ruled 2026-09-20: log it, do not act on it.** No code change; this
   is the shipped behaviour. See "Not scheduled" for the reasoning and
   the one thing that would reopen it.

   *Original entry:*

   `claude -p` and CI harnesses use the same client binary and
   plausibly declare the same capability with no human to render to.
   For those, every write waits out the ask window and then denies —
   including writes an operator explicitly granted with
   `--allow-writes`, because the ask fires on capability alone.

   The tag notes state this as untested rather than claiming it works.
   Twenty minutes with `claude -p` settles it. If it reproduces, the
   fix is not obvious and is a **stop condition**, not a small item —
   an autonomous run must measure and then stop, rather than reach for
   a flag design:
   possibly a startup flag asserting no human is present, possibly
   honouring the flag as its own gate when the ask cannot be delivered.
   **Measure first; the answer changes which.**

3. ~~**A resend whose delete half failed**~~ — **done 2026-09-19.**
   The item reports `RESENT BUT NOT REMOVED`, says a duplicate now
   exists, and tells the agent not to retry that id. Mock-client
   tested both branches: a delete-half failure on `dlq_delete` must
   NOT claim a duplicate, because nothing was sent.

   ~~**Found while testing, not yet fixed:** an SQS failure surfaces as
   the SDK's bare `"service error"`~~ — **done 2026-09-22.** Measured
   before touching anything: a mocked IAM denial on `delete_message`
   rendered as `AccessDenied: service error`. The class was right and
   the sentence naming the missing permission — `sqs:DeleteMessage` —
   was gone.

   `AwsErrorMeta` now carries the service's `message` alongside the
   code and request id, `flatten_err_to_string` renders it, and the
   five SQS calls go through `wrap_aws`. Same denial now reads
   `AccessDenied: DeleteMessage failed: User is not authorized to
   perform sqs:DeleteMessage`. An unclassified code (`ReceiptHandle
   IsInvalid`) reaches the operator too, where it previously fell
   through to the `Debug` sniff and surfaced nothing.

   All three layers proven separately — drop the capture, drop the
   render, or make the call site a pass-through: CAUGHT each time.

   **The class is wider than SQS and was left alone deliberately.** 77
   `.send()` call sites outside `aws/sqs.rs` still take the bare `?`,
   exactly one of which is already converted. 13 modules is past the
   stop condition; it is now its own backlog item with the per-file
   counts.

   *Original entry:*

   `dispatch_one_dlq_message` sends before deleting, deliberately. If
   the send succeeds and the delete fails, the item reports `ok: false`
   with a bare error — but the message IS now on the main queue and the
   original is still in the DLQ. An agent that retries the "failed" id,
   which the batch report invites, mints another duplicate per attempt.
   Found by the correctness reviewer, twice, in both reviews.

   The error needs to say a duplicate now exists and not to resend that
   id. Half a day including the mock-client test.

### Then — re-scope stage 5 against runtime-grants *(analyse, half a day)*

*This entry said "unblocked" and that was momentum wearing a label. A
review took it apart three ways and all three hold:*

- **The data is not the data.** Stage 3's prerequisite is a
  *population* question — "if MOST clients declare it". What arrived is
  one connection, one client, one day. That answers the *mechanism*
  question and not the one the ladder's middle rungs were waiting on.
- **Data was not the only block.** The neutral action vocabulary is
  named in `protection-levels.md` as load-bearing for levels and
  budgeted as part of them. It has not moved. "Nothing in it changed,
  it simply became buildable" was false on the note's own text.
- **`runtime-grants.md` reshapes stage 5 rather than following it.**
  Config may only say no; no key grants; request-as-unit; parity
  default. 0.42.0 shipped that reshape. Stage 5 as written is
  `safety.level = "trusted"` config keys and an ask tri-state that
  request-as-unit collapsed into ask-on-every-write.

So the work is not "build stage 5". It is: **say what levels still buy
after 0.42.0.** Plausible answer — something for headless CLI
principals, approximately nothing for an elicitation-capable MCP
connection where every write already asks and pins already refuse.
"Killed by evidence" is an outcome this file celebrates; parts of
stage 5 are candidates, and pretending otherwise guarantees the
thrown-away work the stages section exists to prevent.

### The protection-levels stages, in order

*Stages 1-4 and 4b are done. Stage 5 (levels + the `Decision` type) was
blocked on the elicitation data stage 3 instruments — the ladder's
middle rungs are defined in terms of asking, and 4b deliberately did
not prejudge them. **That block lifted on 2026-09-19**: 0.42.0 shipped
the ask and its behaviour is measured, not assumed. See "Then" above.*

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

   **The instrument was dead until 2026-09-18, and stage 5 was waiting
   on data it could never receive.** Every subcommand returns from
   `main` before `init_logging` — deliberate, and correct for a flag
   that prints and exits, but the MCP server is a daemon. The whole
   subcommand surface held exactly one `tracing::` call and it was this
   one. A probe declaring elicitation support moved the log by zero
   bytes. `mcp serve` now initialises file logging (`setup` does not —
   it promises it writes no files); the same probe now records
   `elicitation=true`, and a plain client `elicitation=false`.

   So the count starts at zero on 2026-09-18, not at the stage-3 date.
   Nothing observed before then was recorded anywhere.

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

4b. ~~Verb-scoped `--allow-writes`~~ — **done 2026-09-18.** Taken
   ahead of stage 5, on evidence.

   `--allow-writes=dlq_resend,dlq_delete` grants those verbs and
   nothing else. Not in the original order; it went in front of the
   levels because the coarse flag had stopped being a design concern
   and started blocking a real grant — a field session declined to ask
   for write access rather than accept terminate-on-Prod as the price
   of deleting one dead-lettered message.

   Half a day against stage 5's two weeks, and it prejudges nothing: a
   named level later compiles down to a verb set. Pure ceiling, so
   Principle 5 holds trivially — a server flag, not request content.

   The operator ruling that shaped it: a **uniform** grant, no tiered
   ceremony per environment. Demo is not a lesser Prod when a client is
   watching it, and a ladder that says otherwise teaches operators to
   click through the cheap rungs.

   Two defects, both found by guards rather than by review — worth
   noting because both were in the new work and both read as fine:
   `confirm_action` was being filtered out of a narrow grant (every
   narrow grant could plan a write and never dispatch one), and `mcp
   setup` had been advertising five write verbs since the three DLQ
   ones shipped. Both lists are now derived; a `docs_drift` guard pins
   the third.

   Eleven mutations CAUGHT, two of which were only caught after the
   first attempt at them proved to be a no-op — one did not compile,
   one was semantically identical to the original.

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

### From the scheduled mutation sweep, 2026-09-21

The nightly whole-tree run finished (6h25m, 2118 survivors overall,
76 in `cli/mcp`). Triaged against the week's new code:

- **Fixed:** `succeeded += 1` in `dispatch_dlq_and_audit` survived
  `-=` and `*=`. The batch test asserting `"succeeded":2` runs in
  demo, and the demo branch renders that field from
  `dlq_targets.len()` rather than from the counter — so the live
  counter was never incremented by any test. Demo and live computing
  the same field two ways with only demo covered is the divergence
  this cycle kept finding elsewhere. Now covered by a mock-client
  batch that deletes two messages for real.
- **Fixed:** the confirm-time re-read depth (`DLQ_BATCH_CAP * 3`)
  survived being divided. Under-fetching means a full batch cannot be
  found, so messages the operator approved report as "not among those
  returned" while the rest dispatch. Pinned on the REQUEST — the first
  call must ask SQS for its per-call maximum of 10 — rather than by
  simulating SQS sampling, which would be testing the mock.
- **Accepted equivalent:** `ask_outcome_from`'s
  `Some("decline") | Some("cancel")` arm can be deleted with no
  behaviour change, because the catch-all is also `Declined` — the
  fail-closed default is deliberate. The explicit arm states intent;
  it does not carry behaviour. Recorded so nobody re-investigates.
- **Accepted:** `forget_ask` replaced with `()` survives. It leaks one
  `pending_asks` entry per timed-out ask, bounded by connection
  lifetime, and no observable behaviour changes — the ask has already
  been answered by its own timeout. Worth a drop-guard if
  `pending_asks` ever grows unbounded; not now.
- **Not chased:** the remaining `cli/mcp` survivors are in `run`, the
  frame loop that owns stdin. `src/ui/` survivors remain deliberately
  uncovered, as recorded below.

### The four-stage structural refactor, 2026-09-21/22

From the software-architect pass. Reviewed after each stage, as asked.

1. **`cli/lint.rs` — `run` 606 lines -> 308.** `run_cycle` extracted
   with a `client_for` closure as the testability seam; the
   `FIX_DISPATCH_FAILED` static replaced by a returned `CycleReport`
   whose degraded state is *derived* from its reasons rather than
   tracked alongside them. Found and fixed a live bug: EBL015 reported
   clean when `ListPlatformVersions` failed. **Reviewed — but not
   done; see the open item below.**

   The review named **three side channels** still inside `run_cycle`.
   Two are closed: `exit_after_drain(2)` became
   `CycleReport::usage_error` and the wall clock became a passed-in
   `now` (`c50a490`), and the `--fix` block came out with the three
   tests that first reached it (`8985966`).

   **This was recorded nowhere but those commit messages** — the entry
   above said "Done, reviewed" while a third of it was outstanding,
   which is the invisible-follow-up failure `CLAUDE.md` names. Hence
   the open item.

- [ ] **`run_cycle`'s third side channel: it prints instead of
      reporting** *(architecture — needs a scope ruling before dev)*

      Nine sites write straight to the operator's terminal from inside
      the function extracted to make the cycle testable:
      `src/cli/lint.rs:998` and `:1089` in `run_cycle`, and seven in
      `apply_fixes_for_env` (`:2538`–`:2639`). Output no test can
      assert on — the same shape as the two channels already closed.

      **Not one item.** The two in `run_cycle` are mechanical. The
      seven in `apply_fixes_for_env` are interactive `--fix` output
      and are a design question (does the report carry rendered lines,
      a typed event stream, or a writer?), so they need a ruling
      rather than a guess. Split accordingly when it is picked up.

      Evidence the class is live, not cosmetic: chasing it on
      2026-09-24 turned up a real bug at `:1089` — a partly-failed
      EBL015 pass printed behind `!quiet` and exited clean. Fixed
      separately; two mutations CAUGHT. That is three defects this
      function has now shipped in its output wiring.

2. **`app/input.rs` — `handle_key` 842 lines -> 188.** `OverlayRoute`
   makes the overlay dispatch exhaustive over all 14 variants with no
   wildcard, and `key_arm_order` now parses both keymap files. Found
   and fixed a live bug: `About` was missing from a hand-written
   overlay-variant list. **Done, reviewed.**

3. **`audit.rs` — escaping into one renderer.** `Field` + `detail_from`.
   Closed four live forge paths (`action=` was interpolated raw by
   `dispatched`/`completed`/`skipped`/`undone`) and one door out of the
   file (`append_raw`, whose sole caller shipped three unescaped
   fields including an EB *application* name). **Done, reviewed — and
   the review was worth more than the refactor:**

   - The commit claimed "wire format unchanged" and it was false. Six
     fields were always-quoted before and became conditionally quoted,
     rewriting the bytes of every `lint --fix` line. Invisible to 82
     audit tests because `parse_kv_pairs` reads both forms. Fixed with
     `Field::Quoted` + a test that asserts on bytes, not on the parse.
   - The commit claimed `append_extras` was deleted. It was still live
     behind the DLQ writer.
   - The "chokepoint" claim held only inside `audit.rs`.

   The pattern across all three: the *narrative* over-claimed while the
   code under-delivered, in the same commit. Cf.
   `state-claims-only-as-wide-as-checked`.

4. **Relocate the `cli/mcp` test mass** to `src/cli/mcp/tests/`, on the
   `src/app/tests/` precedent. `mod.rs`, `writes.rs`, `tools.rs` only —
   explicitly not a repo-wide sweep. Each self-scanning guard must be
   re-pointed at the `scan.rs` helpers and re-proven with a planted
   violation. **Done, reviewed.**

   Production halves: `mod.rs` 5,841 -> 1,715, `writes.rs`
   4,305 -> 2,652, `tools.rs` 2,759 -> 1,862, with the 168 MCP test
   names byte-identical across all three moves.

   Two parenting strategies, because one does not fit both: `mod.rs`'s
   tests keep their path exactly (`mcp::tests`), while `writes.rs`'s
   and `tools.rs`'s stay CHILDREN of their parents via `#[path]` —
   re-parenting them compiled and then failed on visibility, and the
   only way through would have been `pub(crate)` on a dozen private
   items of the module that owns the write gate.

   **What it found, which is the point:**

   - `a_resend_sends_before_it_deletes` had been asserting on its own
     source since a rename in `beae2bd`. The `.expect()` that should
     have caught it was satisfied by the guard's own literal, sitting
     in the file it read. Its `send < del` also compared `Option`s,
     where `None < Some(_)` — a missing send SATISFIED the ordering
     check.
   - `production_half` was deleting 27 lines of `src/aws.rs`,
     including `pub(crate) struct AwsErrorMeta`, because an
     out-of-line `mod tests;` has no body to skip past. A scan helper
     reporting clean over code it never saw.
   - The write-gate guard could pass over zero files, and its
     "`mod.rs` is the gate" exemption silently excused all 1,715 lines
     of `cli/mcp/mod.rs`.

   Three of these predate the refactor by months. The relocation did
   not cause them; it is what made them visible, which is the argument
   for doing this kind of move at all.

### Also open

- 17 backlog items, mostly design rulings and accepted seam.
- `draw_table`'s inline `DisplayRow::Env` arm — re-measured and left as a
  readability item with a known borrow-checker wall.
- The sub-60-column table cliff, recorded with two options.
