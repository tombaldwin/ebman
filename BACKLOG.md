# ebman backlog

Living list of done / pending / dropped work. New entries get added at the bottom of their section. Priority tiers below are loose — pick what fits.

---

## Done

Moved to [`docs/backlog/archive.md`](docs/backlog/archive.md) — every
completed entry, so this file can be read whole.

That matters because `CLAUDE.md` makes updating this file a condition of
"done" for every landed item. At 375KB it could not be held in context,
so it got read in fragments — and entries were duplicated, or
contradicted, or a follow-up got buried inside a `[x]` entry where
nobody scanning for `- [ ]` would ever see it.

The first split moved only sections with zero open items, which left 100
completed entries sitting in the mixed sections — two thirds of the
file. Completed 2026-09-09: **this file now holds open items only.**
Before moving them they were scanned for the buried-follow-up case
above; two used follow-up language and both turned out to be prose about
a stale premise, not outstanding work.

## Backlog

Tier definitions:
- **Refactors** — structural / design tightening surfaced by code review.
- **Tier 0** — distribution & hygiene before shipping publicly.
- **Tier 1** — blocks daily-driver replacement of the AWS console.
- **Tier 2** — UX patterns directly borrowed from e1s / lazygit / lazydocker.
- **Tier 3** — observability and smart surfacing.
- **Tier 4** — multi-account / org-scale operations.
- **Tier 5** — safety, audit, and destructive-action workflow.
- **Tier 6** — power-user, scripting, and extensibility.
- **Tier 7** — polish and quality of life.
- **Tier 8** — maybe / unprioritised; not committed to scope.

#### 0.29 queue — 0.28 pre-tag review deferrals (2026-08-20)

The write/freeze pre-tag review (2 lenses) fixed 2 Critical + 2 Important + 1 Minor before tag (see CHANGELOG). Deferred, non-blocking:
- [ ] **`draw_table`'s `DisplayRow::Env` arm is still inline** (165
  lines of a 389-line function — re-measured 2026-08-28, it was recorded
  as ~200).

  **Re-assessed 2026-08-28 and still not worth it.** The arm reads 12
  `App` fields and 6 outer locals, and ~116 of its 165 lines are per-row
  resolution that happens BEFORE it builds `CellCtx` — so an extraction
  needs a second context struct of roughly eighteen fields to move one
  function's body. Passing `&App` instead provably does not work, for
  the reason recorded below: the rows borrow `app.environments`, and a
  whole-struct borrow defeats the field-level split that lets
  `render_stateful_widget` take `&mut app.table_state` afterwards.

  Left open because the ~300-line trigger is real, but it is a
  readability item with a known wall and no behavioural payoff. Do it
  when something else forces the file open, not on its own. Original
  note follows. The `Separator` arm was extracted in
  0.30; this one captures ~30 `App` fields and would need a context
  struct, which is where the value per edit drops off sharply. Same
  reason as above for surfacing it: the note lived inside a completed
  entry. See the `Separator branch extracted` entry for what the attempt
  learned (holding `&App` in the context does not compile — the rows
  borrow `app.environments` and it defeats the field-level split that
  lets `&mut app.table_state` coexist).

- [ ] **Whole-tree mutation sweep: triage the remaining survivors.**
  The first complete-ish sweep (2026-08-25, 16 shards, ~95% of the tree)
  produced 2832 caught / 2599 missed — a 52% kill rate on viable
  mutants.

  **The v0.34.2..HEAD slice is DONE (2026-08-27)** — 478 mutants run
  locally via `scripts/sweep.sh`, 352 caught / 105 missed, and all 105
  triaged: the real gaps closed, the SDK and whole-function seam left
  alone, and three genuine equivalents recorded in place (`AGE`'s match
  arm, `hints_to_fit`'s final `>`, `newest_date`'s comparison) so they
  are never re-investigated.

  What remains is the other ~5700 mutants. **Corrected 2026-08-28: that
  is about 17 hours on this machine, not the 5+ days recorded earlier.**
  The 5-day figure came from timing the FIRST EIGHT mutants of a run,
  which are dominated by build warm-up — 0.8/min against the 6.14/min
  the completed 478-mutant run actually sustained. An estimate taken
  before a cache is warm is not an estimate of the steady state, and
  this one was wrong by 7.7x in the direction that says "don't bother".

  17 hours is an overnight job. Started 2026-08-28 23:37 UTC via
  `scripts/sweep.sh` with no argument.
  Artifacts reproducible by re-running the `mutants` workflow, or
  `scripts/sweep.sh` with no argument.

  Worked so far, all in the write/safety cluster: the `:rollback`
  wrong-env guard, the Terminate type-the-name guard, `AuditFilter`,
  `parse_kv_pairs` boundaries, and the deploy watchdog conditions.

  **Remaining, by consequence rather than count:**
  - `src/app/input.rs` (228) — the keymap. A surviving "delete match
    arm" means a key silently stops working. User-visible, not
    dangerous.
  - `src/ui/*` (~539 across detail / header / overlays / table /
    chrome) — render code; mostly a wrong pixel.
  - `src/aws/eb.rs` (105) — response parsing.
  - the rest of `src/cli/lint.rs` (87) and `src/app.rs` (82).

  Expect a large equivalent-mutant tail: two of the first six
  investigated were equivalent (the byte-identical `AbortUpdate` arm,
  the redundant whitespace skip in `parse_kv_pairs`), and both pointed
  at real duplication rather than missing tests.

#### `aws/` fourth review pass — 2026-08-22

Reviewed the third-review fixes and the write-safety tests. Fifteen findings; the severe ones were all defects in those fixes.

Still open, recorded rather than fixed:

#### Supply-chain + API gates — 2026-08-22

Three gates added to CI. Two of them found something on the first run, which is the argument for having them.

- [ ] **Migrate off `serde_yml`** — RE-SCOPED 2026-08-23 after looking properly. Two findings changed the shape of this:

  **It was nine consumers, not one.** The entry said "only remaining use is `saved_config.rs`". In fact five more were live, and **four of them were JSON being parsed by a YAML parser** — including `parse_baseline`, whose own error message says "baseline JSON parse failed", and three round-trip tests asserting output *is valid JSON* while reading it with a YAML reader that accepts things JSON rejects. All five moved to `serde_json` (already a direct dependency), so the surface is now exactly two files: `saved_config.rs` (EB saved configurations) and `eb_cli.rs` (`.elasticbeanstalk/config.yml`) — both genuine YAML. The `json_surfaces_are_parsed_by_a_json_parser` guard was scoped to the two files I happened to be editing when I wrote it; it covers all five now.

  **There is no obviously-right replacement**, which is why this stays open rather than being done today. Every serde-integrated YAML crate in the ecosystem is stale: `serde_yaml` deprecated (Mar 2024), `serde_yaml_ng` last released May 2024, `serde_norway` Dec 2024. The only actively-developed option is `saphyr` (released 2026-08-18) — but it is 0.0.x and is a parser rather than a serde integration, so it means hand-writing the deserialisation for both files rather than swapping a crate name. That is a trade-off with no clear winner, so it wants a deliberate decision, not a drive-by.

  Meanwhile the waiver's blast radius is two files instead of nine, and neither parses anything an attacker supplies — EB writes the saved configs, the EB CLI writes the other.

  **Surface re-verified 2026-08-27, and the "two files" claim holds** —
  but only `serde_yml::` call sites count. Seven files *mention* the
  crate; five of those are comments and guard lists. Real callers:
  `eb_cli.rs` (1 call) and `saved_config.rs` (5).

  **Measured what a migration would actually cost**, so the decision
  stops being abstract. Neither file needs general YAML — no anchors,
  aliases, multi-document streams, tags, block scalars or flow style:

  - `eb_cli.rs` — a two-level map, `global: {profile, default_region,
    application_name}`, all optional strings.
  - `saved_config.rs` — `OptionSettings: {namespace: {name: scalar}}`,
    plus scalar coercion to string. The `other =>` arm re-serialises
    sequences/mappings so a malformed row still shows something.

  **The UNSOUND path is gone as of 2026-08-27, without migrating.**
  RUSTSEC-2025-0068 is specifically about the SERIALIZER —
  `serde_yml::ser::Serializer`'s emitter can segfault. `saved_config.rs`
  held the codebase's only serializer call, `serde_yml::to_string`, in
  the fallback arm rendering a non-scalar option value. It is
  hand-written now (`coerce_value_at`), so every remaining use is
  parsing, which the advisory does not implicate.

  That was worth finding: the earlier assessment on this entry reasoned
  about the blast radius of PARSING ("neither parses anything an
  attacker supplies") while the advisory was about writing. Right
  conclusion, wrong half of the crate.

  The hand-written version also renders better — `to_string` emitted
  real YAML, so a sequence became `- a\n- b` and a `trim` left a newline
  inside what is displayed as a single diff cell. It is `[a, b]` now,
  with a depth cap so a hand-edited file cannot recurse until the stack
  ends.

  **Still a maintainer decision, but a smaller one** — hand-roll two
  shallow parsers, take `saphyr` (active but 0.0.x and not
  serde-integrated), or hold the waiver. Three reasonable shapes, so it
  stays a stop condition under `CLAUDE.md`. What changed is the
  urgency: this is now a maintenance risk rather than a soundness one,
  and `deny.toml`'s waiver says so.

  Re-checked the alternatives 2026-08-27: `serde_norway` last released
  **Dec 2024**, `serde_yaml_ng` **May 2024** — both staler than the
  crate they would replace, despite being what the advisory recommends.
  `saphyr` 0.0.12 shipped 2026-08-18 and is the only one actively
  developed.

  Worth stating plainly, because the entry above buries it: this is
  RUSTSEC-2025-0068, **unsound and unmaintained**, ebman's own direct
  dependency, and the only waived advisory in `deny.toml` that is. The
  waiver says "not waived indefinitely" but names no trigger and no
  date, which is how a temporary waiver becomes a permanent one.
- [ ] **The fleet table degrades below ~60 columns.** Once `DROP_ORDER`
  is exhausted the never-dropped set (NAME 18 + STATUS 10 + HEALTH 3 +
  VERSION 9 + AGE 6, plus COST 8 and REGION 12 when enabled — neither is
  in `DROP_ORDER`) exceeds the budget, `column_widths` returns minimums
  that do not fit, and ratatui squeezes NAME again: 13 cells at 60
  columns, 4 at 40. That is the same "rows do not say which env they
  are" defect 0.36.0 fixed at 80, returning at a narrower width, and the
  title still says only "N cols hidden".

  Found 2026-08-28 by a release-panel reviewer who ran the binary in a
  real PTY at several widths — not by any test, all of which use widths
  at or above 80.

  Options if picked up: add COST and REGION to `DROP_ORDER`; or below a
  floor width, stop pretending it is a table and render one env per line.
  Sub-60 terminals are rare but reachable in a tmux split.


#### Minor (batchable)
Also queued from the 0.26 pre-tag architecture review: rewrite_credential_error + probe helpers out of app.rs; ui.rs submodule split; MCP registry unification (gate on v2 writes); EBL015 warnings surface in MCP; per-tool client dedup.

#### ARCHITECTURE rule guards — 2026-08-25

The five rules in `ARCHITECTURE.md` are the ones the compiler doesn't
enforce. Rules 4 and 5 had nothing behind them at all; both do now, and
both guards were mutation-verified before being believed.

- [ ] **The wrapped-literal guard can't see the collapsed form** —
  found 2026-08-25 by introducing the defect. `CLAUDE.md` records this
  class shipping three times;
  `no_wrapped_string_literal_leaves_an_indentation_hole` now catches the
  *wrapped* shape (a literal split across lines with no `\`
  continuation, which embeds the newline and the next line's indent).
  It cannot see the same defect once collapsed onto one line — a single
  literal carrying a bare 18-space run mid-sentence — which is exactly
  what a tool-assisted edit produces when something eats the
  continuations. That is how it happened: a Python heredoc treated the
  `\` as its own line continuation and emitted the collapsed form,
  which every existing check passed.

  Not built, because the shape is a real trade-off rather than an
  oversight. Measured over production sources, a run of N spaces
  mid-literal matches: **28** at N=3, **7** at N=6, **3** at N=8, **2**
  at N=12 — and at every threshold the survivors are legitimate column
  alignment (`REGION  ENV  CURRENT  TARGET`, `Tags  loading…`, the
  rollout header in `render.rs`). So the guard needs either a threshold
  that misses shallow holes or an allowlist that grows with every new
  table header, which is a guard people route around. Worth someone
  picking a shape deliberately; not worth guessing one mid-run.


#### Mutation sweep — first complete run, 2026-08-26

Run 32928330276, 03:56–11:04 UTC, against `0adc76a`. **6053 mutants:
2989 caught, 2637 missed, 28 timeout, 399 unviable — score 53.1%.**
Every shard finished under the 350-minute cap (slowest 3h47m), so the
24-way split is right and does not need revisiting.

Aggregated survivor list: `scratchpad/sweep/all-missed.txt` (session
scratch — regenerate from the run's artifacts if it has been cleaned).

Read the raw counts carefully. Roughly 1000 survivors are arithmetic and
comparison flips, and the four biggest `ui/*` files contribute 505
between them — render code, where a survivor means a wrong pixel rather
than a wrong action. Working top-down by count is the wrong order.

- [ ] **`ui/` draw functions — ~440 survivors, deliberately not
  covered.** Everything left in `ui/` sits inside a `draw_*` that writes
  to a ratatui `Frame`: `draw_table` (43), `draw_detail_health` (38),
  `estimated_info_row_width` (35), `draw_why_red_overlay` (31),
  `draw_header` (31), and a long tail. The survivors are layout
  arithmetic — column widths, truncation points, padding, elision
  thresholds.

  Reaching them means asserting on rendered frames, and the value per
  survivor is low: a mutation moves a pixel, and a test that pins a
  pixel breaks on every legitimate layout change. The project already
  has render tests for the things that matter (each help topic draws its
  own title, the resources tree, the overlays), and the one render
  surface with real consequence — **redaction** — has **zero
  survivors**, which was worth checking and is the headline result of
  looking at `ui/` at all.

  If this is ever picked up, the useful shape is a small number of
  golden-frame snapshots at fixed terminal sizes, not per-survivor
  tests. `pgman` already uses `insta` for exactly that.

  **Refined 2026-08-27 — "~440 survivors" is two populations, not
  one, and the reasoning above only applies to the first.**

  *Layout arithmetic* (column widths, truncation points, elision
  thresholds) — the argument above stands unchanged. Low value per
  survivor, brittle under legitimate layout change, defer.

  *State-reporting branches* — `if app.read_only`, `if app.alerts > 0`,
  `if app.pinned.contains(..)`, `if app.first_run_hint`. These are not
  layout; they are the mapping from App state to what the operator is
  told, and a wrong answer misinforms rather than misaligns. They are
  worth pinning, they are cheap to pin, and a test for one does not
  break when a column moves.

  They were also never unreachable. `crate::ui::draw` is called by 56
  render sites in the suite already. Three mutations — footer's
  first-run row, header's alert plural, table's pin star — were each
  NOT CAUGHT, then CAUGHT once six tests were written against the
  existing `support::render` harness. No new infrastructure, no PTY.

  So the survivor count was read as a statement about reachability when
  it is only a statement about assertions.

  **Population enumerated 2026-08-27, so "unknown subset of 440" now
  has a real number.** 26 distinct `App` fields gate a conditional in
  `src/ui/`. Ten are now asserted in a render test — `read_only` (both
  directions, since a badge stuck *on* is the dangerous failure),
  `alerts`, `pinned`, `first_run_hint`, `frozen` (including the
  past-5-minutes staleness), `incident` (headline and empty-headline
  branches), `update_available`, `sso_expiry` (counts down, and shows
  nothing once expired), `multi_selected`.

Since extended to nineteen — `tf_managed_envs` (the IaC drift
  warning inside the confirm modal, pinned in both directions),
  `pending_dispatch` (the undo window: what, which key, how long),
  `armed_watchdogs` and `watching_deploys` (across the singular/plural
  boundary, since two armed watchdogs rendered as one hides the second),
  `newly_red` and `newly_added` (marker on the right row and no other).

`worker_dlq_stale` followed, making twenty — it marks a DLQ
  depth as last-known rather than live, and the number reads identically
  either way, so the suffix carries the entire difference between "the
  queue drained" and "nothing was read".

  **6 remain**, all UX rather than operator-state: `cfg`, `costs`,
  `event_panel`, `loading_since`, `loading_visible_until`, `plugins`.
  Left deliberately — none of them reports fleet or safety state, so
  their failure mode is a cosmetic one.

  Reproduce the count with the grep in the 2026-08-27 session — it
  matches `if app.X`, `if let Some(_) = app.X` and `while app.X` in
  `src/ui/*.rs`, and counts a field as covered when it appears in
  `render.rs` / `overlays.rs` / `detail.rs`. That second half is
  *directional, not exact*: it proves the field is set in some test,
  not that the rendered output was asserted. Treat 13 as an upper bound
  on what is genuinely pinned and a lower bound on what is left.


### Feature candidates — competitive scan (2026-05-24)

Ten new ideas surfaced by a backlog/peer-TUI review after the 0.7.0 ship. Ordered roughly by operator-value-per-hour. None overlap with already-tracked items; the niche items already on the backlog (custom-platform create, topology graph, Route 53, etc.) stay where they are. Sized for a 0.9 batch — pick from the top.

- ~~**`:upgrade`**~~ Withdrawn (2026-05-24). The existing `:update` (`src/app.rs:9168`) carries an explicit design comment against auto-upgrade: "Doesn't actually upgrade — operators on AWS-touching tools prefer conscious upgrades, and self-replacing the binary across Cellar / cargo-bin / tarball layouts has too many platform footguns." That decision predates this BACKLOG entry; the entry was written without checking. `:update` already detects the install channel and yanks the right `brew upgrade ebman` / `cargo install ebman --force` command to the clipboard, so the gap is just "paste vs press enter." Not worth pushing against the existing design call without a fresh prompt.
- [ ] **`:queue` action-queue inspector** — Builds on `:pending`. Show currently-dispatched + recently-completed writes across *all* envs (not just selected), with per-row abort for cancellable ops (best-effort; most EB writes aren't cancellable but the dispatch ack can be discarded). Useful when running batch ops — operator sees what's still in flight without scrolling event tape. **Held (2026-05-24)** — `:pending` already shows the same data globally (iterates `self.pending_actions` across all envs). The genuinely new piece would be per-row abort, but most EB writes (UpdateEnvironment, deploys, restarts) aren't cancellable server-side — only the local dispatch ack can be dropped, which limits the operational meaning of an "abort" action. Without abort, `:queue` collapses to `:pending --in-flight` (one line of filter logic). Defer until the abort semantics are designed honestly.
- ~~**Profile / region quick-chord**~~ Withdrawn (2026-05-24) — already shipped, just not as Ctrl chords. `p` and `r` (plain keys in Normal mode at `src/app.rs:3311-3312`) open the Profile / Region picker overlays directly. Better than the Ctrl chords the BACKLOG entry proposed: no modifier required, and `Ctrl-R` would have clashed with the existing manual-refresh keybind anyway. The BACKLOG entry was written without re-grepping the existing keybinds — closing the loop honestly.
### Top priority — console-parity + peer-TUI polish (2026-05-21)

Surfaced by a critical console-vs-ebman + ebman-vs-peer-TUI comparison. Ranked by user-value-per-hour. The smaller ergonomics items in particular (autocompletion, did-you-mean, first-run hint) are the gap that makes ebman look unpolished next to k9s / lazygit — high impact, low cost.

**Secondary** (same review, smaller payoff or design call needed):

- [ ] **Mouse: column resize via drag + right-click row menus** — PARTIAL: drag already exists for the events-panel divider (`input.rs` `drag_origin`), so the interaction pattern is proven; what's missing is table COLUMN resize and right-click menus. Wheel + click-to-select is the current floor. Operators coming from console expect drag + right-click. TBD whether this is worth the design cost for a primarily-keyboard tool.
### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. ~~Blocker: TIER column is `Constraint::Length(7)`~~ — STALE: TIER is `Length(11)` now and `pill_chain` already renders wedge-capped pills in the header, so the machinery exists and the width objection is gone. What remains is the design call about applying it per-row. Old note: and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.

### Console parity — write-side gaps (operators currently open the console for these)

Gaps surfaced during the 2026-05-19 console-vs-ebman comparison. Each entry is a console feature with no ebman equivalent. Ordered by daily-operator frequency.

- [ ] **`:custom-platform-create <packer-config>`** — the last console
  write-side gap. Delete already shipped as
  `:custom-platform-delete <arn>`; create is the other half, via
  `elasticbeanstalk:CreatePlatformVersion`. Niche, but a real gap for
  operators who maintain in-house base AMIs — the console offers a
  wizard that builds a new custom AMI from a Packer template.

  **Why it keeps slipping** (skipped 2026-07-15, tagged "fine to slip to
  0.26", and it slipped): it needs S3-bundle upload plumbing plus
  minutes-scale polling of `CreatePlatformVersion`, and the polling has
  more than one reasonable shape — fire-and-forget with a toast, or a
  progress surface like the deploy watcher. None of it is verifiable
  against live EB from here, which is the actual blocker rather than the
  effort.

  *Merged 2026-08-25 from two entries that were the same feature — one
  carried why it was skipped, the other why it matters.*

### Tier 6 — power-user / scripting
- [ ] **Embedded recorder** — record + replay sessions to `.cast` (asciinema). Deferred — needs its own input-capture + replay infrastructure.

### Tier 8 — maybe / unprioritised
- [ ] **Snapshot at a point in time** — "what envs looked like 1h ago" (would need local history).
- [ ] **Visual resource topology graph** — console shows a "Resources" graph linking ASG → EC2 instances → ELB → target groups. We have `:resources` as a text dump which most operators prefer; the graph is nice-to-have but rarely the reason someone opens the console.
- [ ] **Route 53 / custom DNS integration** — console offers a one-click "set up custom domain" wizard tied to a Route 53 hosted zone. Niche and easy to do via AWS CLI or the Route 53 console directly.

## Skipped — needs retry

Populated by autonomous runs per `CLAUDE.md` stop-conditions. Each entry: one-line reason. Drop the entry once retried (successfully or with the user's deliberate decision to defer further).

- **Embedded asciinema recorder (Tier 6)** — needs its own input-capture/replay infrastructure; defer.
- **`:custom-platform-create` (0.25 BONUS)** — S3-bundle upload plumbing + minutes-scale CreatePlatformVersion polling with multiple reasonable shapes; unverifiable against live EB in an autonomous run. Slipped to 0.26 as the lineup anticipated.
- **EBL015 / EBL018 (0.25 lint batch)** — each needs new AWS surface (per-platform DescribePlatformVersion dates / aws-sdk-wafv2 GetWebACLForResource); recorded in docs/lint-rules.md roadmap with reasons.

**Retried successfully** (kept here briefly so the history's discoverable):

- **README screenshots / demo gif** — rendered 2026-06-04 from an interactive session (`vhs demo.tape`), so the no-TTY blocker no longer applies. The fixture was reskinned to the PROJECT IRONWOOD world (`poly` fleet + the Grey `ironwood` env on a distinct Go platform); see the demo-lore Done entry above.
- **Option settings editor** — shipped in 0.3.0 (`:env`, `:set-option`, `:capacity` modal, every per-namespace command).
- **Split `src/app.rs`** — shipped as task #66 (ten `cmd_*.rs` sub-modules); app.rs 14,277 → 12,478.
- **`sts:AssumeRole` account switcher** — shipped in 0.3.0 (`accounts.NAME.role_arn` config + `:account NAME` switcher). [[multi-account-discovery]].

---

## Dropped / explicitly out of scope

- Multi-service AWS dashboard (RDS / ECS / Lambda). Stays out of scope — ebman is EB-focused on purpose; generic-AWS TUIs already exist (clawscli, cloudlens) and sprawl.
- `Ctrl-N` to dismiss alert badge. Removed when alerts switched from "transitions since last ack" to "currently Red".

---

## Notable inspirations

- **[e1s](https://github.com/keidarcy/e1s)** — same problem shape (k9s-for-ECS). UX template; `b` console deeplink and `d` describe overlay come from here.
- **[k9s](https://github.com/derailed/k9s)** — original model. Resource aliases, `:` command bar, drill-down.
- **[stu](https://github.com/lusingander/stu)** — Rust + ratatui S3 explorer; same stack idioms.
- **[gitui](https://github.com/gitui-org/gitui)** — ratatui async patterns under load.
- **[lazydocker](https://github.com/jesseduffield/lazydocker)** — panel + tab metaphor mirrors our drill-down.
- **[lazygit](https://github.com/jesseduffield/lazygit)** — per-panel hint strip, contextual action menu.
- **[gh dash](https://github.com/dlvhdr/gh-dash)** — sectioned dashboards inspired the "env groups as tabs" idea.
- **[bottom](https://github.com/ClementTsang/bottom)** — ratatui dashboard widget patterns; Metrics tab follows this.
- **[harlequin](https://github.com/tconbeer/harlequin)** / **[atuin](https://github.com/atuinsh/atuin)** — fuzzy-find UI patterns for filtering long streams.
- **[tig](https://github.com/jonas/tig)** — paged event-log + ref panel for timeline views.

- [ ] **Split `cli::write_refusal` the way `app/safety.rs` is split.**
  0.37 extracted `write_refusal_parts` (decide + render, pure) with
  `write_refusal` as the auditing funnel, which fixed the demo-mode
  leak. It stops short of the TUI's three-way shape (`refusal_for` /
  `render_refusal` / `audit_refusal`): the CLI's rendering is still
  inline in the pure half. Worth finishing when stage 5's `Decision`
  lands, since that changes the return type anyway. Raised by the 0.37
  architecture review.

- [ ] **A neutral action vocabulary, shared by refusals and dispatches.**
  0.37.1 gave TUI refusals the dispatch label via a checked table
  (`REFUSAL_ACTION_LABELS`) plus an explicit override where an `Action`
  is in scope. That is correct but partial by construction: verbs that
  guard a chooser rather than a specific write have no single dispatch
  label, and the labels themselves are still string literals scattered
  across the dispatch sites rather than one vocabulary. The table would
  become derived rather than maintained once that vocabulary exists —
  which is the same thing the MCP annotations item below needs. Do them
  together, at stage 5.

- [ ] **The MCP annotations table is the wrong long-term home for the
  action vocabulary.** `src/cli/mcp/annotations.rs` is `pub(super)`
  inside the transport module and keyed by MCP tool names — including
  `confirm_action`, which is transport machinery rather than an action —
  and covers only the 14 MCP tools, while the action space the
  protection levels must govern is wider (DLQ purge, alarm-create,
  rollback, swap). When stage 5 needs it, move it to a neutral module
  keyed by action verb and derive the MCP annotations from that, rather
  than wiring the levels engine to `cli::mcp::annotations` and growing a
  second table that drifts. Raised by the 0.37 architecture review.
