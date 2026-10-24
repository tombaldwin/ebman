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

#### Refactors and test hygiene

- [ ] **The App tab editor cannot tell `;label=` inside a metric name
  from a new entry.** Its metrics field joins entries with `; `, so a
  metric named `a;b=c` reads back as two entries (`heap=a`, `b=c|…`) and
  saves both. Pre-existing (the old split on every `;` did the same);
  0.46 keeps a bare `;` inside a name. Needs `;` and `=` in a CloudWatch
  metric name. Fix when it matters: one row per metric in the form
  rather than a `;`-joined field. *0.46 correctness re-review.*

- [ ] **Region and key plumbing, tidy-ups.** From the 0.46 architecture
  review, none a defect:
  - the "row's region, empty means home" rule is written three times
    (`App::region_for`, `EnvKey::of`, `env_ref`'s `region_of`): one helper
    beside `EnvKey`;
  - `NAME@REGION` is parsed twice (`env_ref`, `cmd_swap`): expose
    `split_env_ref`;
  - `EnvKey` lives in `polled.rs` but is the crate-wide target identity:
    move it to `env_ref.rs`;
  - four region-wide writes use `EnvKey::new(region, "")`: a named
    constructor;
  - `EnvVarsForEdit`, `RollbackTarget`, `LogTailOpened` carry
    `(env_name, region)` pairs where an `EnvKey` would do (the arms
    already build one); `AppTabDiscovered`, the Detail, form, DLQ and
    confirm-modal answers were done 2026-10-23;
  - `DetailState::env_name` duplicates `key.name` (~29 readers): a
    method instead of the field;
  - `CostCache.costs` keeps `REGION/NAME` strings, so the format is
    known in five places (`Costs::restore_cached` / `to_cache_map`,
    `cost_cache::parse`'s own `/` test, the MCP demo, `fleet_cost_entries`):
    key it by `EnvKey` and convert only in `parse` / `serialize`, with an
    `EnvKey::to_ref()` (`NAME@REGION`) for MCP and `env_ref.rs`; the `@`
    parser also exists twice (`env_ref.rs`, `cmd_action.rs`);
  - `pending_select` is a `String` re-parsed on every rebuild: parse it
    once at load into a typed saved cursor.
  *2026-10-23 group-1 architecture review, M1/M3/M4/M7.*

- [ ] **Audit-line boilerplate.** ~30 `append_action_dispatched` and ~11
  `append_action_completed` sites pass account / profile / region by
  hand, and the profile is already inconsistent: `shell_session.rs`
  honours `override_profile`, the rest don't, and `AuditIdent` (0.46)
  holds `context.profile`. `App::audit_dispatched` / `audit_completed`
  wrappers that pick it one way, through `AuditIdent`. ~1 h. *0.46 release
  review.*

- [ ] **`app_tab.rs` holds four concerns** (config parsing, CloudWatch
  queries, series assembly, top errors, ~1,000 production lines): split
  into `app_tab/{config,query,series,top_errors}.rs`; `app_tab_form.rs`
  also hosts discovery orchestration. Pure moves, 1–2 h. *0.46 release
  review.*

- [ ] **Audit writers take a typed label, and the verb spelling guard
  goes.** `no_surface_spells_a_shared_verb_itself` (`src/verb.rs`) bans
  any production literal `"Deploy"` / `"Rebuild"` / `"Terminate"` /
  `"Restart"`, menu text and JSON included, and its message ties UI text
  to the audit vocabulary. The structural fix: `append_action_*` takes
  `AuditLabel::{Shared(Verb), Local(&'static str)}` so the compiler
  enforces it (~40 call sites). *0.45 release review.*
- [ ] **Move the lint-input tests next to `lint/inputs.rs`.**
  `probe_outcome_tests`, `disabled_rule_probes` and `lost_coverage` in
  `src/cli/lint.rs` test code that moved to `lint/inputs.rs` in 0.45.
  Separately, `apply_fixes_for_env` is production code sitting after
  ~1,100 lines of test modules in `cli/lint.rs`. *0.45 release review.*
- [ ] **Coverage gaps are untyped strings.** `explain_verdict`
  identifies a warning by its first word; today one formatter,
  `ProbeOutcome::coverage_warning`, produces every warning it sees, so
  that holds. The stronger case: the confirm modal's `not_run:
  Vec<String>` mixes whole-lint failures ("lint could not run: …", no
  rule id) with per-rule gaps, MCP's `rule_skipped` is another format,
  and `fetch_stale_platform_issues`'s branch warnings a third. A typed
  `CoverageGap { rule: Option<RuleId>, why }` rendered at each surface.
  *0.45 release review.*
- [ ] **`PolledByEnv` as one map of states.** Since 2026-10-21 the
  three fleet polls share `PolledByEnv<T>` (one reconcile, one prune,
  keyed by region and name), but it still keeps a value map and two
  sets (`stale`, `absent`) updated together; an enum per key
  (`Value(T)`, `Stale(Option<T>)`, `Absent`) would make the combinations
  explicit. *0.45 re-review, re-scoped 2026-10-21.*
  The platform cache is still two fields for one fact: `latest_stacks` and
  `latest_stacks_error` are one fact in two fields, and the cache is the
  HOME region's only — lint now reports an env elsewhere as not
  evaluated, but a per-region cache would let EBL008 run for it.
- [ ] **`src/app/tests/scan.rs` has outgrown its name and location**
  (~1,100 lines): source-scan primitives, git/packaging checks (account
  IDs, protected names) and the `write_gate` guard, under `app/tests/`
  while `verb.rs`, `aws/tests/` and `cli` all use it. A crate-level
  test-support module split into `scan` and `packaging`. *0.45 release
  review.*
- [ ] **The MCP undo buffer to its own file.** ~200 self-contained lines
  of `src/cli/mcp/writes.rs` (`DeletedMessage`, `Claim`, the
  remember / claim / finish / recoverable methods) → `mcp/undo.rs`.
  *0.45 release review.*
- [ ] **TUI `:lint` no longer primes the confirm modal's input cache.**
  Since it moved to the shared assembly it does not send
  `LintInputsCached`, so a later pre-deploy confirm refetches tags and
  health. Latency only. *0.45 release review.*
- [ ] **The `dlq_undo` refusal audit line records `region="-"`** when the
  delete used the default region, while the dispatched / completed lines
  record the resolved one. Same pattern as the other MCP gate refusals —
  fix the class. *0.45 release review.*

- [ ] **Merged doc comments on `pub(crate)` items are still
  unguarded.** 0.42.0 closed this for the PUBLIC surface —
  `#![warn(missing_docs)]` in `lib.rs`, plus docs for the 18 public
  items that lacked them — because the robbed half of a stolen doc has
  a mechanical signature: the original item ends up undocumented.
  Verified by staging the bug; the robbed function fails the build
  under CI's `-D warnings`.

  It does not reach `pub(crate)`, which is most of the tree.
  `clippy::missing_docs_in_private_items` is the obvious lever and is
  far too noisy — it demands a doc on every private field and helper.

  Worth considering: restrict the lint to *items that already have a
  sibling doc*, or lint only modules where doc density is already
  high. Both need a custom lint or a source scan, and a scan for the
  COMMENT's shape is a dead end — see below.

  **The comment-shape scan is retired.** It returned 19 candidates
  across 15 files in the 0.40 era. Re-run against the 0.42 tree, both
  a strict and a loose form return 2 candidates, and both are false
  positives (a lead-in to a bullet list, and a legitimate
  several-single-sentence-paragraph doc). The real merges named in the
  old entry — `src/app/spawn_refresh.rs`, `src/cli/action.rs` — now
  read correctly. Scoped claim: the tree is clean by those two
  signatures, which is not the same as no merged doc existing
  anywhere. The scan cannot become a guard at a 100% false-positive
  rate, exactly as the old entry predicted.

- [ ] **Move the last 13 `include_str!` guards onto
  `scan::production_source`.** Not a defect today — each crosses a
  directory boundary explicitly. Two latent properties remain: they
  re-point silently if the test file moves, and they read the target
  RAW, so a "this appears nowhere" check also searches the target's
  test module and can be satisfied by a fixture.

  `src/app/tests/parsing.rs:377-382, 400`; `src/terraform.rs:1150-1152`;
  `src/commands.rs:1063, 1129, 1198`. (The three in `app/tests/lint.rs`
  went on 2026-09-25, when that guard moved to all production source.)

  *Restored 2026-09-25:* feabbbc removed this from the backlog without
  archiving it, and the guards were still unmigrated. Its own lesson,
  demonstrated the same day: a list-based guard went blind when the code
  it watched moved files (0a81d5e).

- [ ] **Split `cli::write_refusal`'s rendering from its decision**, the
  way `app/safety.rs` is split (`refusal_for` / `render_refusal` /
  `audit_refusal`). The CLI's rendering is still inline in the pure half
  (`write_refusal_unaudited`). It was parked "until stage 5's `Decision`
  lands, since that changes the return type anyway"; stage 5 was killed
  on 2026-09-25, so that trigger will never fire. Do it on its own
  merits or drop it. *Re-opened 2026-09-25: archived as absorbed by the
  verb vocabulary, which did not in fact do it.*

- [ ] **`App::new` and `App::for_tests` each build the ~115-field struct
  by hand** (`src/app.rs`, two literals of ~260 and ~175 lines).
  `for_tests` is production code too — `new_demo` builds on it — and
  nothing checks that the two agree on defaults, so every new field is
  two edits. Shape: a shared `App::assemble(aws, config, seed)`. S–M.
  From the 2026-09-25 implementation review.

- [ ] **Demo audit suppression in one place.** Done per call site: MCP
  checks `Backend::Demo` at seven sites, the TUI `!self.demo_mode` at
  several more, and it has already leaked once. Evidence it is still
  inconsistent: `:thaw-deploys` writes an audit line in demo mode, and
  0112e6d's `:readonly` does not. Shape: a process-wide audit sink mode
  set once at startup, as `set_notify_webhook` does. S.

- [ ] **Eleven hand-rolled `read_dir` walks in guards bypass
  `scan::source_files`** (`app/tests/dispatch.rs` ×7, `refresh.rs`,
  `aws/tests/core.rs`, `cli/mod.rs`). None is wrong today — each uses
  `strip_line_comment` — so this is consolidation, not a fix, but it is
  the house rule. S.

- [ ] **Finish the injection and ticket seams** *(0.47.0 release
  architecture review)*. Behaviour-neutral, each covered by guards today:
  (1) the CLI write bodies inject their dependencies three ways (action /
  rollout / replay take a profile and a `WriteGate`, in different
  argument orders; lint a `LintSetup` and a freeze closure; freeze/thaw
  nothing, calling `FreezeStore::real()` inline) — one `WriteEnv` value
  would shrink `write_shells_pass_the_real_gate_and_the_ambient_profile`
  to one pattern. M. (2) `FreezeStore` is injected only in the App; the
  CLI and MCP call `FreezeStore::real()` at use (`cli/mod.rs`,
  `mcp/tools.rs`, `mcp/writes.rs`). M. (3) `ActionState`'s six
  `Option` + `loading_*` pairs and `DlqState.loading` are the shape
  `Fetch` replaced in Detail; results match by `EnvKey`, not ticket, so a
  superseded pre-flight answer is still accepted. M.

- [ ] **Shared test fixtures and module placement** *(0.47.0 release
  architecture review)*. A `#[cfg(test)] aws::test_support` with
  `client_with_eb(eb, region)`, `denied(code)` and an env listing would
  replace six mocked-`AwsClient` builders (`audit_replay`, `drift`,
  `action` ×2, lint's `cycle_wiring` — which `explain` reaches into —
  `envs`, `versions`). S–M. `EnvKey` lives in `app/polled.rs` though
  `aws::cost` uses it (`aws` depends on `app`): move it to its own
  module. S. The cost key has three shapes (`(String,String)`,
  `REGION/NAME`, `EnvKey`), and `app/cost.rs` (pricing) beside
  `app/costs.rs` (state) misleads: `app/pricing.rs`. S. `cli/lint.rs`
  and `cli/action.rs` are two-thirds inline tests; `cli/<sub>/tests.rs`
  as `cli/mcp/tests/` does. M. `freeze.rs`'s `/proc` and pid probing is
  not about freezing: a `proc_liveness` module behind `Liveness`. S.

- [ ] **CLI body paths still untested** *(2026-10-23 CLI-body review)*
  Every subcommand is behind a testable body now; these paths through
  them are reached by no test: explain's `required_tags` pass-through;
  `lint --webhook` (the post, the change guard's wiring, the degraded
  suppression; `should_post_webhook` itself is tested) — it needs a
  webhook seam, since the body fires a real POST; a `lint --baseline`
  write that fails; and which cycle's code a multi-cycle `--watch` exits
  with (the mock fleet is the same every cycle). Also:
  `cli::audit_delta::lines_for` panics "append-only" if one test run's
  audit lines cross the 1 MiB rotation mid-test — the pattern the older
  audit tests use, now in four more. S each.

- [ ] **Functions over the 300-line trigger** (2026-09-25 count):
  `draw_action` 673, `draw_why_red_overlay` 474, `execute_command` 418
  (a flat dispatch; judged fine), `draw_detail_health` 410, `draw_table`
  408, `App::new` 348, `handle_detail_mode_key` 343, `apply_refresh`
  334, `lint_body` 333 (was `lint::run`), `draw_help` 321.
  (`run_rollout`, 402, is now a parser, a 242-line body and a report
  renderer.) Worth doing first:
  `draw_action` (one function per `ActionFlow` arm).
  **Blocked on a ruling for `draw_action`:**
  `every_render_surface_is_accounted_for` counts top-level `fn draw_*`
  NAMES (`KNOWN = 41`), so extracting `draw_action_menu` means raising
  `KNOWN` — a stop condition. Should it count dispatch-reachable
  surfaces, or should helpers be named `render_*`?

#### Correctness and safety, from the 2026-09-25 reviews

- [ ] **The pre-deploy lint still fetches its own way.** Since 0.45 it
  shares everything after the fetch with `:lint` / `:explain` (the
  snapshot, `lint::inputs::assemble`, `LintSnapshot::finish`), and a
  failed tag or health fetch is listed. What differs is the fetch: it reads the
  lint-input cache first, and it never runs the EBL020 / EBL018 probes
  — so those two rules cannot fire in the confirm modal, and nothing
  there says so. **Two rulings needed:** how the latency cache
  (`env_tag_cache` / `env_health_cache`) fits the shared fetch, and
  whether a confirm modal should pay for the IAM/WAF probes (if not, it
  should at least list them as not run).

- [ ] **The MCP side of "a disabled EBL008 does not degrade" is
  unpinned.** e15d62b pins it for the CLI; the MCP tool reads
  `lint.disable` from the config file, which the orchestration harness
  cannot inject.

#### Product, from the 2026-09-25 review

- [ ] **Decision: should a TUI rebuild render red?** MCP annotates
  `rebuild` destructive (`Verb::destructive` — it replaces every
  instance), while the TUI's `Action::confirm_in_red` deliberately
  shows only terminate and `ssm-run` in red, pinned by
  `confirm_in_red_covers_terminate_and_ssm_run`. The 0.45 release
  review found the two disagreeing under one name; they were renamed by
  meaning rather than changed. One line to align if wanted.

- [ ] **Decision: should lifting `--read-only` be harder?** 0112e6d
  audits `:readonly off`; it still needs no confirmation, so a session
  started `--read-only` is one keystroke from writable. Options: sticky
  for the session, or a typed confirm to lift it.

- [ ] **An MCP tool to declare a freeze.** `ebman freeze` (2026-10-23)
  gave the CLI one; an agent still cannot stop writes. Declaring only
  restricts, so it is safe to grant without an ask, but it is a new
  tool on the registry surface and lifting must stay with the operator.
  Left out of the freeze work by design (`docs/design/persistent-freeze.md`).

- [ ] **The docs read as a design log.** Operator pages carry history
  and rationale ("Measured, 2026-09-19 …", "Before 0.30 …"), and
  CHANGELOG entries name internal tests. Split each page into reference
  (what is true now) and rationale.

- [ ] **Surface parity.** `:rollback` — the README's headline fix — is
  TUI-only, so it cannot be scripted or proposed by an agent; `why`,
  events and logs exist in the TUI and MCP but not the CLI; names differ
  (`:versions` / `versions` / `list_versions`, `envs` /
  `list_environments`).

- [ ] **Breadth vs the daily core.** 131 commands; two unrelated
  "freezes" (`f` pauses auto-refresh; `:freeze-deploys` locks writes —
  rename the first to "pause"); consider a `?` "top 15" view. Also:
  MCP `recent_logs` reads CloudWatch only, so an env without log
  streaming may return nothing — say so in the tool description.

- [ ] **`draw_table`'s `DisplayRow::Env` arm is still inline** (165
  lines of what is now a 408-line function (`ui/table.rs:594`, re-measured 2026-09-25; 389 on 2026-08-28), it was recorded
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
  **Re-scope before working it (2026-09-25):** the figures below are
  from 2026-08-28. The scheduled run of 2026-09-21 recorded 2118
  survivors (`docs/backlog/archive.md`, "From the scheduled mutation
  sweep"), and PLAN.md's rule is to track REACHABLE survivors, not the
  headline. Start from that run.

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


#### Minor

- [ ] **Per-tool AWS client dedup in the MCP server.** `Server::client`
  (`src/cli/mcp/tools.rs:781`) builds a fresh `AwsClient` — twelve SDK
  clients plus `aws_config::load()` — on EVERY tool call, while the TUI
  reuses one per profile+region through `aws::cached_client`. Verified
  2026-09-25. Rescued from a one-line list of five follow-ups where it
  had been the only one still open.

#### ARCHITECTURE rule guards — 2026-08-25

The five rules in `ARCHITECTURE.md` are the ones the compiler doesn't
enforce. Rules 4 and 5 had nothing behind them at all; both do now, and
both guards were mutation-verified before being believed.

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

`worker_dlq_stale` (now `caches.worker_dlq`'s stale mark) followed, making twenty — it marks a DLQ
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
  not that the rendered output was asserted. Treat twenty as an upper bound
  on what is genuinely pinned and a lower bound on what is left.


### Feature candidates — competitive scan (2026-05-24)

Ten new ideas surfaced by a backlog/peer-TUI review after the 0.7.0 ship. Ordered roughly by operator-value-per-hour. None overlap with already-tracked items; the niche items already on the backlog (custom-platform create, topology graph, Route 53, etc.) stay where they are. Sized for a 0.9 batch — pick from the top.

- [ ] **`:queue` action-queue inspector** — Builds on `:pending`. Show currently-dispatched + recently-completed writes across *all* envs (not just selected), with per-row abort for cancellable ops (best-effort; most EB writes aren't cancellable but the dispatch ack can be discarded). Useful when running batch ops — operator sees what's still in flight without scrolling event tape. **Held (2026-05-24)** — `:pending` already shows the same data globally (iterates `self.pending_actions` across all envs). The genuinely new piece would be per-row abort, but most EB writes (UpdateEnvironment, deploys, restarts) aren't cancellable server-side — only the local dispatch ack can be dropped, which limits the operational meaning of an "abort" action. Without abort, `:queue` collapses to `:pending --in-flight` (one line of filter logic). Defer until the abort semantics are designed honestly.
### Console parity — polish (2026-05-21)

- [ ] **Mouse: column resize via drag + right-click row menus** — PARTIAL: drag already exists for the events-panel divider (`input.rs` `drag_origin`), so the interaction pattern is proven; what's missing is table COLUMN resize and right-click menus. Wheel + click-to-select is the current floor. Operators coming from console expect drag + right-click. TBD whether this is worth the design cost for a primarily-keyboard tool.
### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. ~~Blocker: TIER column is `Constraint::Length(7)`~~ — STALE: TIER is `Length(11)` now and `pill_chain` already renders wedge-capped pills in the header, so the machinery exists and the width objection is gone. What remains is the design call about applying it per-row. Old note: and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.

- [ ] **`icons = "ascii"` still draws non-ascii text.** Found while
  moving the chrome helpers onto `tui_common::chrome` (2026-10-19), which
  made the cursor, separators and corners ascii-safe. Panel titles and
  labels still carry literal `·` and `—` (`ui/events.rs:43`,
  `ui/detail.rs:307/851/1565`, `ui/help.rs:31/336`, the version title),
  and every border is box-drawing (`│─┌`) even in ratatui's Plain set.
  First decide what the mode promises: "no glyphs a programming font
  lacks" (box-drawing and `·` pass) or "7-bit ascii" (needs
  `BorderType`-free borders). Then a guard that renders each screen in
  ascii mode and checks every cell against that promise, instead of
  fixing literals one at a time.

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

- **The `lint --fix` freeze re-check (2026-09-25)** — design fork: see
  the open item.
- **Unifying the pre-deploy lint (2026-09-25)** — two rulings needed:
  see the open item.

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
