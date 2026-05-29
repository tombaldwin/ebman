# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.0] — 2026-05-29 — Test pinning + new lint rule + small operator wins

Autonomous slice through the 0.19 candidates list. The big foundation
refactors (ResolvedConfig sub-struct, spawn_* clusters, finish app.rs
audit migration) deserve focused human review and stay deferred to
0.19.1/0.20. This release is the polish-and-ship slice: 4 new tests
that pin invariants the codebase had been relying on by convention,
1 new lint rule, 3 operator features.

Theme: **polish the rule engine + close two CLI parity gaps.**

### Added — operator features

- **`ebman versions --env NAME [--json]`** — CLI mirror of the TUI `:versions` overlay. Useful for CI scripts validating a candidate label exists before `ebman action deploy`, or surfacing the candidate's description / age in a Slack notify. Default text mode marks the currently-deployed label with `*`; `--json` emits one object per version with `{label, deployed, created (RFC3339), description}`.
- **`:config-diff ENV --ignore-keys "k1,k2"`** — Suppresses noise in config-diffs. Operators routinely want to exclude `version_label` / `EC2KeyName` / etc. from the diff output. Match is case-insensitive; both bare names (`MinSize`) and namespace-qualified forms (`aws:autoscaling:asg:MinSize`) are supported. Pure helpers `parse_ignore_keys` + `filter_config_diffs` with 4 tests.

### Added — lint engine

- **EBL017 — Managed Platform Updates disabled** (Info). Detection: `aws:elasticbeanstalk:managedactions.ManagedActionsEnabled` is not `"true"` (or the setting is absent — EB defaults to disabled on most modern platforms). Op-sec gap: env doesn't receive automatic security patches during the configured maintenance window. Fix=Manual (operator may have a deliberate reason to disable). +4 tests covering the firing matrix (explicit-false / absent / explicit-true / mixed case).

### Changed — confirm-modal lint render

- **Lint issues now sort by severity DESC then rule_id ASC.** When 3+ warnings fire in a confirm modal, the `Error` ones land at the top so operators see them first. Was a 0.18 review item.

### Added — test pinning

- **`audit::append_extras` wire-format golden** — Pins the exact `key=value` / `key="..."` encoding so a future quoting-policy change becomes a deliberate decision rather than silent breakage for downstream audit consumers. Same pattern as the 0.18 `issue_identity_hash` golden.
- **`Action::label()` exhaustiveness extended** — Now includes `Action::Capacity` (missing since 0.6, caught by the 0.17.4 review). Exhaustive on all 15 variants with an explicit count assertion so future additions can't skip the distinctness check.
- **`Rule` trait invariants on the entire registry** — For every rule in `default_rules`: `id()` non-empty, `severity()` doesn't panic, and **consistency**: if `applies(ctx) == None`, then `fix(ctx) == None` (the inverse — `applies=Some, fix=None` — is allowed for non-auto-fixable rules). Catches the failure mode where `cmd_lint_fix` would propose a fix for an issue that doesn't exist. Pins the registry size at 13 rules.

### Deferred to 0.19.1 / 0.20

The 0.19 candidates list had 29 items; this release ships the polish + small-feature slice. The bigger items each warrant focused review beyond autonomous mode:

- `App.cfg_resolved: ResolvedConfig` sub-struct — touches 30+ read sites, deserves a dedicated cleanup commit
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping — 61+ methods, mechanical but invasive
- Remaining ~20 `append_raw` sites in `src/app.rs` → typed helpers — needs new typed helpers for SSM / DLQ / CW Logs shapes
- Lint input caching on `App` (env_tag_cache + env_health_cache) — touches refresh tick + multiple lint sites
- `:fleet-cost`, `ebman audit replay`, Promotion lineage tracking — each ~2 hr operator features
- `ebman lint --baseline-regenerate` — **redundant** (existing `--baseline PATH` already overwrites unconditionally)
- `ebman lint --explain` — **redundant** (the existing `ebman explain ISSUE_ID` already does this since 0.14)
- EBL013-016, EBL018-020 — each needs live-EB verification of the exact detection shape
- `docs/lint-rules.md` / `docs/rule-development.md` — better as a focused docs cycle

### Tests

767 tests green (up from 757 in 0.18.0). 10 new tests:
- `action_labels_are_distinct_and_non_empty` extended (+1 variant + count assertion)
- `append_extras_golden_wire_shape` (golden pin)
- `rules_satisfy_trait_invariants` (registry-wide consistency)
- `parse_ignore_keys_splits_and_lowercases` + `filter_config_diffs_*` (4 tests for the new pure helpers)
- `ebl017_*` (4 tests for the new rule)

### Docs

- `docs/headless.md` — `ebman versions --env NAME` entry
- `print_help()` — `versions` subcommand entry

## [0.18.0] — 2026-05-28 — Live the stubs + audit migration + test pinning

The 0.17 series shipped EBL008/010/011/012 as code but several rules
couldn't fire because their inputs weren't plumbed from `App`. 0.18
closes that gap — all four rules now fire in both the TUI and the
CLI (modulo a small CLI-side gap on EBL011 where the worker DLQ
isn't polled). Plus an audit-shape consistency pass on the cmd_*
files, and golden-pinned identity-hash tests so operators'
`--baseline` files don't silently break.

Theme: **"live the stubs."** No new operator-facing surface — the
rules + plumbing + tests that were sitting in code as "would fire
if wired" are now actually firing in production.

### Added — lint plumbing (the stubs now fire)

- **EBL011 (worker DLQ depth > 100) fires in production.** TUI sites
  (`spawn_confirm_lint`, `cmd_explain_issue`, `:lint`) plumb
  `App.worker_dlq_depths` through `LintContext::with_dlq_depth(...)`.
  CLI side intentionally left unwired (worker-queue polling isn't a
  CLI flow); the TUI is where operators see DLQ-stuck envs.
- **EBL010 (missing required tags) fires in production.** The
  `env_tag_keys` precondition is now fetched inline via
  `list_tags(env.arn)` at all four lint sites (3 TUI + 1 CLI),
  running in parallel with the existing option-settings fetch via
  `tokio::join!` so the modal-open latency stays at
  `max(t_opts, t_tags, t_health)` rather than the sum.
- **EBL012 (Green-but-0-instances divergence) fires in production.**
  `fetch_env_instance_counts` runs in parallel with the tags fetch
  above; `healthy` is plumbed through `LintContext::with_healthy_count`.
- **CLI EBL008 (newer-stack) wiring landed.** Per-region `list_solution_stacks`
  fetch + `aws::latest_stack_versions(&s)` builds the family map; the
  `:lint` overlay was already wired in 0.17.3 — `ebman lint` now
  matches.

### Changed — audit-shape consistency

- **`cmd_*.rs` files migrated from `append_raw` → typed
  `append_action_dispatched` / `append_action_completed`.** 11 sites
  across `cmd_alarms`, `cmd_misc`, `cmd_config_template`. **Wire
  format change**: completed-stage lines for `AlarmCreate`,
  `AlarmDelete`, `DeleteCustomPlatform`, `ConfigSave`/`Delete`/`Apply`
  now write `outcome=ok` / `outcome=err err="..."` (matching every
  other typed-action site) instead of the previous trailing `ok` /
  `err="..."` shape. Audit consumers grepping for that specific suffix
  need to update.
- ~20 remaining `append_raw` sites in `src/app.rs` stay un-migrated for
  now — most are SSM / DLQ / CW Logs operations that don't have a
  natural "completed" stage and the migration would force a synthetic
  one. Tracked for 0.19.

### Added — test pinning

- **Golden-pinned `issue_identity_hash`** with two pin points (one
  with env, one without). Catches future field-shape / separator /
  truncation changes that would silently invalidate every operator's
  `--baseline` file. Failing the golden requires a deliberate decision
  about whether the breaking change is worth shipping.
- **`Action::wants_preflight()` exhaustiveness test** parallels the
  0.17.4 `Action::destructive()` extension — every variant gets an
  explicit assertion so future accidental flips are caught.

### Internal — testing

757 tests green (up from 754 in 0.17.4). 3 new tests:
`action_wants_preflight_covers_all_variants`,
`issue_identity_hash_golden_pin`,
`issue_identity_hash_golden_pin_no_env`. The migrated audit sites
all compile against the typed `append_action_*` shape — no new
test for the wire format itself, since `audit.rs`'s existing tests
already cover `append_extras` encoding.

### Deferred to 0.19

- `App.cfg_resolved: ResolvedConfig` sub-struct — touches every read
  site of 12 migrated fields, deserves a dedicated cleanup release.
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping — deferred from
  0.15/0.16/0.17/0.18. 61+ methods now; doing this without a focused
  release was the right call but it should land in 0.19.
- Remaining ~20 `append_raw` sites in `src/app.rs`. SSM / DLQ / CW
  Logs paths need new typed audit helpers for their non-action shapes.
- CLI subcommand unit tests — needs `aws-smithy-mocks` integration
  setup that's bigger than a 0.18 feature.

## [0.17.4] — 2026-05-28 — Patch: max-effort code review findings (15 fixes)

`/code-review max everything` against the 0.17.x patch series surfaced 15 findings — 5 Important + 10 Minor. User asked to bundle all into one patch.

### Fixed — Important

- **`spawn_deploy_from_local` + `spawn_terminate_instance` bypassed the `--demo` gate** (src/app.rs:9222, 10287). Both called `is_read_only_for` directly instead of `deny_write`, so `--demo` mode could still write `stage=dispatched action=DeployFromLocal` / `action=TerminateInstance` audit lines to the real audit log. Switched to `deny_write` — picks up the demo gate alongside read-only / safety pins.
- **`profiles::load_profiles()` ignored `AWS_CONFIG_FILE` / `AWS_SHARED_CREDENTIALS_FILE`**. 0.17.2's new `:profile NAME` pre-check (and the `p` picker) refused legitimate profiles defined in custom config paths — broke aws-vault / corp-env / work-vs-personal-split users. Added `config_file_path()` + `credentials_file_path()` helpers that mirror the AWS SDK provider chain. `cmd_profile` / `cmd_account` error messages now surface the actual path being checked.
- **`Action::SsmRun` audit-log identifier drift** (3 different names across stages). Dispatched / completed lines in `spawn_ssm_run_impl` used literal `"SsmRunCommand"`; cancelled lines from `cancel_pending_dispatch` used `format!("{action:?}")` = `"SsmRun"`. Broke `grep action=` correlation across audit-log stages. Canonicalized on `"SsmRun"` (Debug format) to match every other Action variant's convention. **Audit-log consumers that grepped for `"SsmRunCommand"` need to switch to `"SsmRun"`.**
- **`apply_rebuild` leaked stale `self.detail` + `pending_shell_target` + `action_flow` + `picker` across context switch.** Most severe: `cmd_ssm_run` resolved env name + instance IDs from `self.detail`, so after `:profile X` switched the AwsClient, a `:ssm-run "cmd"` against a same-named env in the new context would dispatch against OLD instance IDs (from the previous account) using the NEW AwsClient. Cross-context silent dispatch. Now: detail / pending shell target / open modals / open pickers all cleared on rebuild, and the mode resets from Detail / Dlq / Action / Picker → Normal so the UI doesn't render stale overlays.
- **`apply_rebuild` nulled `pending_dispatch` but left stale `"X dispatches in 5s — press U to undo"` `status_message`.** Now: status_message is cleared alongside pending_dispatch so the bar stops lying about a queued state we just cancelled.

### Fixed — Minor

- **`cmd_account` fallback path bypassed `cmd_profile`'s new validation** (src/app/cmd_nav.rs:62). Now validates against `load_profiles()` and surfaces a concrete "account/profile 'X' not in {config_path} or accounts.*" toast on typos.
- **`Action` enum was a SemVer-major break in 0.17.3 (added `SsmRun` variant)**. Added `#[non_exhaustive]` so future variant additions are non-breaking for downstream consumers.
- **Scale-to-zero modal rendered red body text on non-red modal accent** (visual hierarchy lied). Now: `Action::Scale` with `min == 0 && max == 0` renders the full destructive styling (red border, red env-name highlight) — matches the SCALE-TO-ZERO body copy. `Action::Scale.destructive()` stays false at the type level so routine scales don't get the alarming accent.
- **`:ssm-run` 5s undo-window delay on fast probes.** 0.17.3 routed SsmRun through `queue_action_dispatch` which arms a 5s pending_dispatch cancel-window. Fast probes like `:ssm-run "uptime"` waited 5s before reaching the SDK. Now: `queue_action_dispatch` short-circuits to `spawn_action` for SsmRun — immediate dispatch after Y. Other destructive actions keep the cancel window for the "I just pressed Y, oh no" rescue path.
- **`format_aws_error`: pinned arm ordering inline** so future cleanups don't reorder the SSO arm AFTER the InvalidClientTokenId arm (the SSO arm's generic "unable to load credentials" tokens can mask real SSO-expired errors that ALSO carry InvalidClientTokenId).
- **SsmRun's modal fired `compute_traffic_warning`** even though operators frequently run diagnostic shells ON Red envs (it's how they diagnose Red). Now: SsmRun skips the warning so the modal doesn't suggest "env is Red, don't dispatch" against the very command being used to investigate.
- **`deny_write` demo-mode toast masked safety-pin diagnostics.** Operators iterating on `safety.envs.*` config in `--demo` couldn't tell whether their pin was wired correctly — every refusal said "demo mode". Now: when both demo_mode AND a safety pin apply, the toast appends `— would also refuse: safety.envs.X.read_only`.
- **`action_destructive_*` test omitted Capacity + 7 other variants.** Extended to all 15 Action variants so a future accidental `destructive()` flip is caught.

### Deliberate non-fix

- **SsmRun bypasses `push_pending` (header `⏳` chip)** — by design. The 60s wall-clock cap + full-screen TextOverlay landing is the operator's completion signal; the pending pill exists for "what writes are in-flight against the EB fleet" and SsmRun doesn't touch EB state. Strengthened the rationale comment in `spawn_ssm_run_impl`'s docstring so future reviewers don't re-find this.

### Tests

5 new pure-logic tests:
- `profiles::config_file_path` / `credentials_file_path` honor + HOME fallback (3 tests, env-var-touching mutex-serialized)
- `deny_write_demo_mode_composes_pin_reason_in_toast` (1)
- `action_destructive_covers_terminate_and_ssm_run` extended to all 15 variants (1 — same test, more coverage)

Suite at 754 tests, all green.

## [0.17.3] — 2026-05-28 — Patch: `:ssm-run` Y/N confirm

The remaining item from the 0.17 bug-hunt — `:ssm-run` Y/N confirm —
deferred from 0.17.2 with the reason "needs new `Action::SsmRun`
variant + modal-flow plumbing, outside patch scope". User asked
to do the refactor anyway. Touches 4 modules (`mode_action.rs`,
`app.rs`, `ui.rs`, `docs/commands.md`); contained because the new
variant flows through every existing `ConfirmModal` site naturally.

### Added

- **`Action::SsmRun` variant + Y/N pre-confirm for `:ssm-run`.** Pre-fix, `:ssm-run "<cmd>"` dispatched immediately — operators had no Y/N gate on a command that fans shell exec across N instances. Now it routes through the standard `open_parameterised_action` confirm flow with a modal showing the command, fan-out count, and env: `SSM-RUN: \`<cmd>\` on N instance(s) of '<env>' (treat as write)`. Same Y/N + cancel-window UX the rest of the destructive actions use. Operator can `U` to cancel within the deadline.
- `Action::SsmRun` is **destructive** (modal renders in red — operator-explicit shell exec is treat-as-write even when the command is a read-only probe like `uptime`, because the visual prominence beats the false-positive risk).
- `Action::SsmRun` **opts out of preflight** (no instance-count / events fetch — the operator already chose the instance set by opening Detail/Instances).
- Not added to the `:a` actions menu (`ACTIONS` const) — `:ssm-run` is command-only; no menu entry surfaces it.

### Internal — refactor

- **`ConfirmModal` + `ParameterisedAction` gained `ssm_run_command: Option<String>` + `ssm_run_instances: Option<Vec<String>>`.** Five existing `ConfirmModal {...}` construction sites updated with `None` defaults — same shape the previous `swap_with` / `deploy_version` etc. fields use.
- **`spawn_action` short-circuits to `spawn_ssm_run_impl`** before its standard `tokio::spawn` body. SsmRun's SDK call returns per-instance rows that surface in a `TextOverlay`, not the `Result<(), String>` shape every other action uses; the short-circuit keeps the standard pipeline clean.
- `spawn_ssm_run_impl` carries the SsmRunCommand audit "dispatched" + "completed" lines (same `("instances", N)` + `("cmd", trimmed)` extras the previous `cmd_ssm_run` body emitted, so audit-log consumers see the same shape).
- `cmd_ssm_run` rewritten as a parse + resolve + `open_parameterised_action_on(env, Action::SsmRun, ParameterisedAction { ssm_run_command, ssm_run_instances, .. })` shape. ~90 lines → ~35.

### Tests

3 new tests for `Action::SsmRun` (`destructive() == true` regression suite, `wants_preflight() == false`, label + glyph completeness across the three icon styles). `action_labels_are_distinct_and_non_empty` extended to include the new variant. Suite at 750 tests, all green.

### Docs

- `docs/commands.md` `:ssm-run` entry updated with the 0.17.3 Y/N modal note and the "treat-as-write" gating reference.

## [0.17.2] — 2026-05-28 — Patch: bug-hunt polish (Minors + UX)

Continuation of the 0.17 bug-hunt — Minors and UX items the 0.17.1
patch left for follow-up.

### Fixed

- **`compute_traffic_warning` / `Action::Scale` confirm modal didn't flag scale-to-zero.** `:scale 0` and `:stop` both open the standard confirm modal but the modal copy didn't surface "this drops all instances". Now: scale-to-zero gets explicit all-caps "SCALE TO ZERO" copy with the `:start` recovery hint. (`src/ui.rs`)
- **`:profile NAME` typos surfaced as deep SDK errors.** Previously `:profile prod-readonl` (missing y) kicked a rebuild that failed somewhere in the SDK; now `cmd_profile` pre-checks against the parsed profile list and surfaces a concrete "profile 'X' not in ~/.aws/{config,credentials}" with remediation (`p` to pick, or `aws configure --profile X`). (`src/app/cmd_nav.rs`)
- **`--demo` mode no longer dispatches destructive writes.** Pre-fix, a fat-fingered `:rebuild` / `:deploy` / `:terminate` during a demo / screencast would hit the synthetic AwsClient (silently fail at SDK layer) AND still write `stage=dispatched` lines to the real audit log. Now gated at three sites: `spawn_action` (modal path), `deny_write` (the central guard covering option-settings / SSM / tag writes), and `tick_pending_dispatch` (batch path bypasses spawn_action). Operator sees a yellow toast: "demo mode — writes are inert; press q to exit". (`src/app.rs`)
- **`format_aws_error` now routes `InvalidClientTokenId` / `SignatureDoesNotMatch` to a concrete remediation.** Pre-fix these flattened to "context switch failed: \<long CRT message\>" with no hint at what to do. Now: "credentials invalid for profile 'X' — run: aws configure --profile X (or press `p` to pick a different profile)". Distinct from the existing expired-token arm (which still routes to `aws sso login`). (`src/app.rs`)

### Internal — polish

- **"no env selected" toast now includes a hint** at 45 call sites: "press 1-9, click a row, or type \`'\` to jump by name". Operators discoverability win — the picker keys aren't obvious from the bare error. (`src/app.rs`, `src/app/cmd_*.rs`)
- **`apply_view` / `encode_filter_only_view` doc accuracy.** The previous comment claimed `apply_view` "ignores missing fields" but `app.filter` was always set (snapshot semantics — defaults to empty when `filter=` is absent). Docstrings now match the actual contract: sort/grouped/scope no-op when absent, filter restores (including to empty). (`src/app.rs`)
- **`docs/keys.md`** now has the `U` row (cancel pending dispatch / deploy countdown). Shipped in the help overlay since 0.11 but missing from the docs page. (`docs/keys.md`)

### Deferred (skip-and-continue per the autonomous-mode rules)

- **`:ssm-run` Y/N pre-confirm.** Adding a typed `Action::SsmRun(String)` variant + modal flow touches `Action` / `open_parameterised_action` / `spawn_action` — outside the patch scope. `deny_write` already gates it under read-only locks and the demo guard; the missing UX is the per-command confirm. Tracked for 0.18.

### Tests

5 new pure-logic tests covering `format_aws_error` (InvalidClientTokenId arm, SignatureDoesNotMatch arm, ExpiredToken regression) and `deny_write` (demo-mode gate, allow-path). Suite at 748 tests, all green.

## [0.17.1] — 2026-05-28 — Patch: bug-hunt review fixes (3 Important)

Post-0.17 bug + UX hunt surfaced three operator-visible Important
bugs. This patch fixes them; the smaller polish items from the
same review land in 0.17.2.

### Fixed

- **`pending_actions` queue not cleared on context switch.** `apply_rebuild` cleared `armed_watchdogs`, `watching_deploys`, `deploy_snapshots`, `undo_history`, and the other env-keyed state — but missed `pending_actions` and `pending_dispatch`. Spawned tasks from the previous profile/region get dropped at the generation guard in `msg.rs`, so their `complete_pending` never ran. Symptom: dispatch any write (`:rebuild`, `:deploy`, `:tag`) then run `:profile X` before completion → header `⏳ N` chip and `:pending` overlay show the previous-context op forever. Fix: clear both fields alongside the other env-keyed state.
- **`ebman lint` CLI didn't plumb `required_tags` (EBL010 precondition).** TUI's `:lint` overlay chained `.with_required_tags(...)` on the `LintContext`; the CLI built the context raw. Operator-visible CLI/TUI behaviour drift on `LintContext` construction. Fix: plumb `config::load().required_tags` through `LintContext::for_env(...).with_required_tags(...)` in the CLI. Note: EBL010 still won't fire from CLI in 0.17.1 because `env_tag_keys` plumbing is the other half of the precondition — that's deferred to 0.18 (same `DescribeTags` fetch that gates EBL010 from the TUI's lint command). This patch closes the CLI-side half so 0.18 only needs to wire the env-tags fetch. (`latest_stacks` plumbing — for EBL008 newer-stack — likewise needs a `ListAvailableSolutionStacks` fetch and stays deferred to 0.18.)
- **`ebman action rollout --parallel` misattributed join failures.** Loop used `joinset.join_next()`; when a `JoinHandle` failed (panic / cancel) the closure couldn't return its region, so the outcome was keyed by `""`. The "skipped (rollout halted)" calc at the bottom of `run_rollout` then matched the empty key against no input region — the *actual* affected region was then re-printed as "skipped (rollout halted)". Operators in the parallel path got a misleading failure report. Fix: switch to `join_next_with_id()` + a `HashMap<task::Id, String>` tracker, so a failed join still attributes back to its launched region.



0.16 shipped EBL007-010 but EBL008 (stale platform) and
EBL010 (required tags) silently no-op'd in production —
their context fields weren't plumbed from `App`. 0.17
plumbs them (EBL008 fully TUI-live, EBL010 implementation
landed pending env-tag-keys wiring), adds two more high-
signal rules built on the same context shape (EBL011 worker
DLQ stuck, EBL012 Green-but-0-instances divergence), and
ships `ebman lint --baseline` / `--against-baseline` so
teams adopting lint on a noisy fleet can grandfather
existing issues without declaring bankruptcy.

Theme: "Make the stubs live + lint adoption ergonomics."

Two-agent code review caught one Critical (EBL008 plumbing
sent the wrong shape — version token vs full stack name —
making the rule false-positive on every env) plus several
Minors. Critical + selected Minors fixed in `7b8d4ac`
before tag.

### Added — smart features

- **`ebman lint --baseline FILE` + `--against-baseline FILE`.** CI adoption pattern. `--baseline` snapshots today's issues to JSON (exit 0 regardless). `--against-baseline` diffs against the snapshot — exit 3 only on NEW issues; CLEARED issues are informational. Identity is `(rule_id, env_name, sorted_fields)` — title / detail / suggestion can drift across releases without churning the diff. Hand-rolled JSON output (consistent with the project's "no serde_json" convention). New `lint::issue_identity_hash` / `lint::issue_identity` / `lint::parse_baseline` / `lint::BaselineIssue` helpers; round-trip pinned by `parse_baseline_identity_matches_issue_identity`. (`b6fdf4b` + `7b8d4ac` arg-parse fix)
- **EBL011 — Worker env with DLQ depth > 100 (Warn).** Catches stuck SQS consumers — the headline failure mode for worker envs. Scoped to `ctx.env.tier == "Worker"`. Threshold hard-coded (`EBL011_DLQ_THRESHOLD = 100`) for v1; future config-tunable if operators ask. Fix=Manual (operator triages via DLQ sampling + worker logs before redrive/purge/scale). (`e5d8e24`)
- **EBL012 — `status=Ready + health=Green/Ok` with 0 healthy instances (Error).** Classic ELB-vs-EB health-check divergence: EB reports Green but ALB target-group reports no healthy targets → traffic failing silently. Skips during Updating (deploy-in-flight is not the divergence pattern) and when health is non-Green (EBL003 handles long-Red). Fix=Manual (operator drills into Detail/Health). (`e5d8e24`)
- **EBL008 now fires in the TUI.** Plumbed via `aws::newer_stack_version(&env.solution_stack, &app.latest_stacks)` at three call sites (`:lint`, `:explain`, confirm-modal). The Critical bug caught in review was here: pre-fix, the plumbing passed a version token while the rule compared against a full stack name → unconditional false-positive. Fix routes through the existing version-tuple helper. CLI `ebman lint` + `ebman explain` still no-op for EBL008 (no `App`); tracked for 0.18. (`dbb1d80` + `7b8d4ac` fix)
- **EBL010 implementation landed.** Rule now fires when `required_tags` is populated AND `env_tag_keys` is populated AND any required key is missing (case-insensitive). The `env_tag_keys` wiring is the 0.18 follow-up — needs a `DescribeTags` fetch per call site. (`dbb1d80`)

### Internal — refactors

- **`LintContext::for_env(env, opts)` + `.with_*()` builder pattern.** Pre-fix, the context was built in 6 places with identical skeleton — adding a new field meant editing all six. Now: `for_env` is the minimal constructor; `.with_events()`, `.with_cost()`, `.with_newer_stack_available()`, `.with_required_tags()`, `.with_env_tag_keys()`, `.with_dlq_depth()`, `.with_healthy_count()` populate optional fields. Six call sites collapsed to one-liners. Three new fields (`required_tags`, `env_tag_keys`, `dlq_depth`, `healthy_instance_count`) added without editing every site. (`1e6558e`)

### Internal — polish

- **`:undo` discoverability toast.** Successful option-settings writes now append `· press U to undo` to the toast — operators discover the session-scoped undo ring without reading help. Suppressed when `undo_history` is empty (e.g. immediately after a context switch). (`a400f4b`)
- **First-run identity_warning routing.** Pre-fix, a fresh-creds user (no SSO login, expired creds) hit a RED error banner — that's the EXPECTED state, not an error. Post-fix routes to `status_message` (yellow informational) + pinned + adds `aws sso login` / `:profile NAME` hints. Plugin-startup warnings still land in `error_message` (those ARE user misconfigurations). (`a400f4b`)
- **`run_inline_ssm` dead code removed.** ~90 lines of `#[allow(dead_code)]` reference impl at app.rs:2683-2763 with three `println!` calls that violated the no-stdout-in-TUI convention. Kept "as a reference" since 0.7 but never re-enabled — `open_embedded_shell` has served every SSM session since. Reference recoverable from git history if needed. (`2029d9e`)
- **docs/configuration.md: `command_aliases` section backfilled.** Shipped in 0.11 but only had an inline source comment. (`105c49c`)
- **docs/commands.md: `:lint` entry updated with EBL011/012 + --baseline/--against-baseline.** (`1902523`)

### Internal — review fixes (`7b8d4ac`)

- **Critical**: EBL008 false-positive on every env with a known family. Plumbing was passing version token to a field the rule compared against full stack name. Routed through `aws::newer_stack_version` + renamed `LintContext.latest_stack_version` → `newer_stack_available` to make the Some/None semantic explicit ("Some means fire").
- **Important**: `--baseline` and `--against-baseline` silently consumed the next arg without an error guard. Added the null-check + flag-prefix-check pattern.
- **Minor**: three "EBL010 fires" comments rewritten to reflect that `env_tag_keys` is unwired (0.18 follow-up).

### Deferred to 0.18

- Plumb `App.worker_dlq_depths` → `LintContext.dlq_depth` (makes EBL011 fire in production).
- Plumb env-tags fetch → `LintContext.env_tag_keys` (makes EBL010 fire). Needs a `DescribeTags` fetch per call site or a cached `App.env_tags`.
- Plumb instance-count → `LintContext.healthy_instance_count` (makes EBL012 fire).
- CLI EBL008 wiring (needs its own `ListAvailableSolutionStacks` fetch).
- Hash-value pinning test for `issue_identity_hash` (golden test to catch future `fields` shape changes that would silently invalidate operators' baselines).
- Migrate the ~22 remaining dispatched-only `append_raw` sites.
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping (deferred since 0.15 — third time).
- `ResolvedConfig` sub-struct (deferred from 0.16's review).

### Internal — testing

- 723 lib + 17 tui-common + 3 bin tests green (up from 700 / 17 / 3 in 0.16.0).
- fmt + clippy clean. No new deps; no behavioural change to existing operator surface beyond the bug fix (which changed a noisy false-positive into the intended diagnostic).

## [0.16.0] — 2026-05-28 — Smart features depth + rollout deepening + cleanup continuation

Mixed-shape release. Three tiers landed:

**SUPPORT** — continuation cleanup from 0.15: the
architecture review's "For 0.16" list shipped (audit
writer migration to typed APIs, splash relocation, JSON-
escape unification, decide_poll shared between CLI + TUI).

**HEADLINE** — smart features grow: `ebman lint --watch`
closes the CLI-charter monitoring-loop feature; four new
lint rules (EBL007 ELB without HTTPS, EBL008 stale
platform, EBL009 missing ASG health-check grace period,
EBL010 missing required tags) extend the diagnostic
surface from 6 to 10 rules.

**HEADLINE** — rollout deepening: `:rollout --parallel
[--max-concurrency N]`, `--continue-on-fail`, and
`--staggered Nm` give operators the three operational
shapes deferred from 0.13 / 0.14. All compose with
`--wait-for-green`.

Two-agent code review per the CLAUDE.md release procedure
caught zero Critical, two Importants, and several Minors —
all addressed in `f9109dc` before tag (parallel output
ordering was non-deterministic; EBL008 silently no-ops in
production and now has explicit ship-note + pinning test).

### Added — smart features

- **`ebman lint --watch [--interval 60s]`** — locked-charter monitoring loop. Polls the lint engine at `--interval` (default 60s, accepts seconds `30` or duration `5m / 1h`) until Ctrl-C. Each cycle emits a `--- {rfc3339} ---` header line + the full issue list. `--json` emits one `{issues:[...]}` blob per cycle for ingestion pipelines. Exit code reflects the LAST cycle's state (0 clean / 3 issues). Canonical monitoring shape: `ebman lint --watch --interval 5m --severity warn --json > alerts.jsonl`. Mutex with `--fix`. (`4cbd590`)
- **Four new lint rules (EBL007-010).** EBL007 ELB without HTTPS listener — fires on HTTP-only fleets; mixed HTTP+HTTPS (redirect-only) doesn't false-positive. EBL008 stale platform version — compares against `LintContext.latest_stack_version`; production wiring tracked for 0.17, pinned by `ebl008_currently_stub_does_not_fire_in_cli`. EBL009 ASG missing HealthCheckGracePeriod — fires on LoadBalanced envs with grace < 60s; auto-fix sets it to 300s. EBL010 missing required tags — registered stub (the `required_tags` wiring into LintContext is the 0.17 follow-up). 11 new unit tests. (`aae66f7`)

### Added — rollout deepening

- **`:rollout --parallel [--max-concurrency N]`** — concurrent fan-out via `tokio::JoinSet`. Default unlimited concurrency; `--max-concurrency N` caps the inflight wave. Implicit `--continue-on-fail` since in-flight regions can't be cancelled server-side. Same `rollout_id` correlation across audit lines. (`bd4d06b`)
- **`:rollout --continue-on-fail`** — sequential mode variant: attempt every region (no halt on first failure). Composes with `--parallel` (where it's implicit). (`bd4d06b`)
- **`:rollout --staggered Nm`** — wait Nm between regions in sequential mode (canary pattern). Requires `--wait-for-green` (staggering is timed from each region's Green observation). Mutex with `--parallel`. (`bd4d06b`)

### Internal — refactors (continuation from 0.15)

- **Audit writers: 6 outcome-pair sites migrated to typed `append_action_*`.** GetSecretValue, SsmRunCommand, UpdateOptionSettings (x2), DeleteAppVersion, UpdateTags, DeployFromLocal. `append_action_dispatched` / `append_action_completed` extended with `extras: &[(&str, &str)]` slice so callers can attach per-action context (summary, cmd, label) without falling back to hand-rolled detail strings. Wire-format identical except UpdateTags (was emitting unstructured "tag k1,k2" inline; now `summary="tag k1,k2"`). ~24 dispatched-only sites stay on `append_raw` for future migration. (`96ea83c`)
- **JSON-escape helpers unified.** 6 variants across `audit.rs` / `cli/mod.rs` / `lint.rs` / `app.rs` / `llm.rs` collapsed to `util::json_escape` (no quotes) + `util::json_string` (wrapped quotes). Modules import via `use crate::util::...`; legacy names like `cli_esc` / `json_str` re-exported locally to keep call sites unchanged. YAML-round-trip test validates spec compliance. (`75513d3`)
- **`draw_splash` + `hsl_to_rgb` moved out of `main.rs`** into `src/splash.rs` alongside the existing scene helpers. `main.rs`: 698 → 494 lines (-29%). (`495a22a`)
- **`decide_poll` + `PollDecision` shared via `src/deploy_poll.rs`.** Pre-0.16 lived in `cli/mod.rs` (CLI-only); TUI's `App::spawn_rollout_dispatch` re-implemented the wait-for-green case inline with a deadline-only loop. Promoted to a sibling lib module; both paths now share one state machine. Future `--auto-rollback` per region wiring is a 4-line change instead of a re-implementation. (`5896abf`)

### Internal — review fixes (`f9109dc`)

- **Important**: `:rollout --parallel` output ordering — outcomes now sorted by input regions order before emission (CI consumers get deterministic JSON ordering regardless of dispatch mode).
- **Important**: EBL008 ship-note acknowledgment + pinning test — flags the rule as a stub in production until `App.latest_stacks` is plumbed into LintContext.
- **Minor**: EBL007 doc comment fixed (previously contradicted its own code).
- **Minor**: `dispatch_one_region`'s dead `wait_timeout_emitted` binding removed; literal `false` passed inline.
- **Minor**: `splash::hsl_to_rgb` visibility tightened from `pub` to `pub(crate)` (no external callers).

### Deferred to 0.17

- Plumb `App.latest_stacks` → `LintContext.latest_stack_version` so EBL008 fires in production.
- Plumb `Config.required_tags` → `LintContext` so EBL010 fires.
- Migrate the ~24 remaining dispatched-only `append_raw` sites (continues the 0.15/0.16 consolidation; some still emit bare-trailing-`ok` which lossy-parses to a corrupted `target` field on historical `ebman audit --env NAME` filters).
- Extract `util::parse_duration_secs` to dedupe the `--interval` / `--wait-for-green` / `--staggered` / `--auto-rollback` parsing logic.
- `append_extras` quoting heuristic add `c.is_control()`.
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping (BONUS deferred from 0.15).

### Internal — testing

- 701 lib + 17 tui-common + 3 bin tests green (up from 689 / 17 / 3 in 0.15.0).
- fmt + clippy clean. No new deps; no behavioural change to existing operator surface.

## [0.15.0] — 2026-05-27 — Foundation pass: audit consolidation, App.explain_settings, CLI split

Pure structural cleanup driven by the 0.14.0 architecture
review. No new operator-facing features — the surface is
byte-identical. `src/app.rs` is at ~22k lines and
`src/main.rs` was 2,600+; both have been cooking since
0.10. This release pays down the debt before 0.16 ships
new feature work on top of cleaner foundations.

The user codified the code-review-before-tagging step in
0.14.1; this release's review (two parallel agents,
architecture + correctness) caught a stale docstring,
some pub-visibility nits, an unused-disk-read on
flag-only launches, and one config-round-trip wart — all
acted on in `4d5d70f` before the tag. No bugs.

### Internal — refactors

- **Audit-line writers consolidated into `src/audit.rs`.** Three pre-0.15 writer entry points (`write_audit_outcome` in app.rs, `write_rollout_audit_line` + `write_lint_fix_audit_line` in main.rs) collapsed into four typed public APIs (`audit::append_action_dispatched` / `append_action_completed` / `append_rollout` / `append_lint_fix`) plus `audit::append_raw` for ad-hoc sites. Writers + parser now co-located closes the format-drift risk that the 0.14.1 patch surfaced. Plus `audit::init_from_config_disk()` called from CLI dispatch (action + lint subcommands only) so audit lines emitted by `ebman lint --fix` / `ebman action rollout` reach the configured `notify_webhook` — pre-0.15 CLI audit lines never fanned out. (`260ec41`)
- **`App.explain_settings: llm::Settings` replaces six discrete fields.** `App.explain_enabled` / `explain_provider` / `explain_model` / `explain_api_key_env` / `explain_ollama_url` / `explain_max_tokens` (added in 0.14, all three-site-mirror copies) collapse into one. New `Settings::write_to_config` is the reverse of `from_config` — used by `current_config_snapshot` to write the resolved settings back. Sets the template for the next `[section]` block. (`545e935`)
- **`src/main.rs` 2,609 → 698 lines (-73%).** Seven inline `run_*_cli` async functions split into `src/cli/{action,audit,ctl,drift,envs,explain,lint}.rs`, each exposing `pub async fn run(args: &[String]) -> Result<()>`. Shared CLI helpers (`PollDecision`, `decide_poll`, `json_string`, `cli_esc`) live in `src/cli/mod.rs`. main.rs is now argv parse + dispatch + TUI lifecycle + crash report + logging init. (`b290e03`)

### Internal — review fixes (`4d5d70f`)

- Stale docstrings in `audit.rs` updated (referenced functions that moved during consolidation).
- `init_from_config_disk()` moved INSIDE the cli-dispatch match arms so flag-only launches (`--read-only`, `--demo`, `--version`, `--help`) don't read config.toml.
- `FIX_DISPATCH_FAILED` static moved from `cli/mod.rs` into `cli/lint.rs` (its sole reader/writer).
- `decide_poll` / `PollDecision` / `json_string` / `cli_esc` tightened from `pub` to `pub(crate)`.
- New test `config_with_explicit_defaults_collapses_on_round_trip` pins the operator-UX wart that an explicit `explain.provider = "anthropic"` (the default value) gets collapsed on `:settings save`. Behaviour preserved; flagged for revisit if operators complain.

### Deferred to 0.16

- Migrate ~30 hand-rolled `stage=dispatched action=X target=Y` call sites from `audit::append_raw` to the typed `audit::append_action_*` APIs (continues the consolidation that 0.15 started).
- `draw_splash` + `hsl_to_rgb` move from `main.rs` to `splash.rs`.
- `decide_poll` TUI sharing (currently re-implemented in `spawn_rollout_dispatch`).
- Unify the 6 JSON-escape helper variants across modules.
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping (BONUS deferred from 0.15 scope).

### Verified — pinned by experiment

- **Audit-line wire-format equivalence** — all three new `append_*` paths produce byte-identical lines to the pre-0.15 writers. Verified by the correctness review.
- **CLI audit lines now reach `notify_webhook`** — `init_from_config_disk` registers the URL before any `audit::append_*` fires under action/lint subcommands. OnceLock first-wins shape means App's later `set_notify_webhook` is a harmless no-op when both fire (impossible in practice; CLI dispatch returns before App::new).

### Internal — testing

- 683 lib + 6 bin + 17 tui-common tests green (up from 672 + 14 + 17 in 0.14.1; the gain is migration tests for the new APIs).
- fmt + clippy clean. No new deps; no behavioural change to the operator surface.

## [0.14.1] — 2026-05-27 — 0.14 code review: 2 Critical + 4 Important fixes

Same-day patch for 0.14.0. A full code review of the
shipped lineup surfaced two real bugs (a safety-pin
bypass in `lint --fix` and silent failure-loss in the
rollout audit) plus four important correctness fixes
(LLM cache poisoning across envs, newline corruption
in audit-log error strings, residual pre-0.14 bare-`ok`
call sites that corrupt the `target` field, and the
missing `stage=completed` line for rollouts). Bundled
as one patch since they share the same workflow surface
(audit-log integrity + `--fix` safety).

### Fixed — Critical

- **`ebman lint --fix` now honours `safety.envs.NAME.read_only` and `safety.accounts.NAME.read_only` pins.** The TUI's `App::deny_write` (`app.rs:8229`) gates every destructive action against these pins; the CLI was re-implementing the dispatch and skipping the check. An operator who pinned `safety.envs.prod.read_only = true` in `config.toml` (the standard way to lock production against destructive TUI actions) was still seeing writes when they ran `ebman lint --fix --yes`. New behaviour: per-env safety-pin check + per-account safety-pin check (using `AWS_PROFILE` env var, same source the SDK consults) before the `update_env_option_settings` dispatch; refusal goes to stderr with the pinning reason, and `FIX_DISPATCH_FAILED` is set so the CLI exits 1.
- **`ebman action rollout` now emits audit entries for failed regions.** `write_rollout_audit_line` was only called on the success branch — failed regions left no trail in `~/.cache/ebman/audit.log`. Now: `stage=dispatched` is written up front (before the AWS call) so the trail exists even if dispatch itself fails, and `stage=completed action=Rollout target=ENV outcome=ok|err` is written at the end of every iteration. Operators doing post-mortem `grep rollout_id=` now find the full per-region timeline.

### Fixed — Important

- **`llm::cache_key` now includes `env_name`.** `build_prompt` embeds the env name in the LLM prompt (so the model may personalise its response — "for prod-api, set MinSize to 4 because…"). The cache key didn't include env_name, so two envs hitting the same rule with the same `fields` (e.g. EBL005 single-instance on `dev-a` AND `dev-b`) would share a cache entry; the first env's personalised advice leaked to the second. Fix: env_name is part of the SHA256 input.
- **Audit-log writers now sanitize newlines + tabs in error strings.** Multi-line AWS errors (`InvalidRequest: …\n  caused by: …`) wrote a literal `\n` mid-line; the parser reads line-by-line, so the embedded newline corrupted the next entry. New centralised `audit::escape_value` helper (`"` → `'`, `\n`/`\r`/`\t` → ` `) used by every writer (`write_audit_outcome`, `write_rollout_audit_line`, `write_lint_fix_audit_line`).
- **Residual pre-0.14 bare-trailing-`ok` call sites migrated to `outcome=ok`.** Seven sites in `app.rs` (GetSecretValue, SsmRunCommand, UpdateOptionSettings ×2, DeleteAppVersion, UpdateTags, DeployFromLocal) still emitted bare `ok` after the detail. That format made the parser extend the `target` value to include ` ok`, breaking `ebman audit --env NAME` against historical entries. All migrated to `outcome=ok` / `outcome=err err="..."` shape, matching 0.14's `write_audit_outcome` convention.
- **`App.lint_fix_disable` dropped (was dead).** Never read in the TUI; `ebman lint --fix` reads via `config::load_lint_fix_disables()` directly. `App::current_config_snapshot` now re-reads from disk so `:settings save` preserves the existing `lint.fix_disable` line.

### Fixed — Minor

- **`ebman explain` consent-gate error wording is provider-aware** — points at API-key env var for Anthropic, points at Ollama URL for Ollama.
- **`audit::escape_value` is char-based** to avoid clippy's `consecutive_str_replace_calls` warning + run in one pass.

### Verified — pinned by experiment

- **Cache key cross-env collision regression test** (`cache_key_differs_by_env_name`) — pins the new env_name behaviour so future cache-key changes that drop it will fail the test.
- **`escape_value` round-trip test** — pins the newline / tab / quote sanitisation.

### Internal

- 672 lib + 17 tui-common + 14 main tests (up from 669 + 17 + 14 at 0.14.0).
- Same fmt + clippy clean. No new deps.

## [0.14.0] — 2026-05-27 — From diagnostic to remediation: explain, lint --fix, audit CLI

0.12 surfaced issues (`:lint` rule engine, `:drift`
terraform drift detection). 0.13 made every read
fleet-wide (`--regions r1,r2,r3` on every smart-feature
subcommand + `:rollout` cross-region deploy). 0.14 makes
the output **actionable** — LLM-backed explanations turn
the structured `Issue` shape into operator-readable next
steps for any rule that fires; opt-in auto-remediation
dispatches the obvious-correct-answer fixes through the
existing undo machinery; the audit log gets a first-class
CLI for monitoring / Slack-bot integration so operators
stop shell-parsing it themselves.

### Added — explain

- **`ebman explain EBL###` CLI + `:explain EBL###` TUI dispatch.** New `src/llm.rs` module ships a `Provider` trait with Anthropic and Ollama implementations. Anthropic uses `POST /v1/messages` (default model `claude-haiku-4-5` — ~$0.001 per explanation, <2s p50); Ollama uses `POST /api/generate` against the local server for operators on locked-down corporate networks. Both providers consume the same prompt template (`build_prompt`), so swapping providers doesn't materially change response quality. Backward-compat with the existing IAM AccessDenied `:explain` — dispatch arms on the `EBL\d+` prefix. CLI flags: `--env NAME` / `--json` / `--dry-run` / `--no-cache`. Responses cached by SHA256(rule_id || sorted fields) → 16-hex key at `~/.cache/ebman/explain/`; CI loops running `ebman lint --json | jq | xargs explain` don't burn API calls on identical issues. (`22904d9`)
- **Explicit consent gate.** `[explain] enabled = true` required in `config.toml`. Off by default. Presence of `ANTHROPIC_API_KEY` is NOT implicit consent — security-conscious orgs that export API keys for other tools shouldn't have ebman silently start making outbound calls. The error message points the operator at the config edit + env var when invoked without consent. (`22904d9`)
- **HTTP via `reqwest` with rustls-tls** — reuses the rustls aws-sdk already pulls in (no openssl dependency added). `json` feature deliberately off; request bodies are short + fixed, hand-rolled to match the project's existing JSON convention. Response parsing uses `serde_yml` (JSON is a YAML subset; already a dep). (`22904d9`)

### Added — lint --fix

- **Opt-in auto-remediation: `ebman lint --fix --yes` + `--dry-run`.** Each `Rule` grows an optional `fix(&LintContext) -> Option<FixAction>` method (default `None` preserves the report-only shape). v1 auto-fix set: EBL001 (DeploymentPolicy: AllAtOnce → Rolling), EBL004 (BatchSize → MaxSize), EBL006 (Cooldown → 360s). v1 Manual fixes: EBL002 (path is app-specific), EBL005 (capacity is workload-specific). EBL003 (env Red >4h) has no fix — it's a state, not a config issue. (`5cb2ab8`)
- **Safety + audit + per-rule opt-out.** `--fix` requires explicit `--yes` (or `--dry-run` to preview); `--yes` + `--dry-run` mutex. Only rules whose issue passed the `--severity` / `--rules` filters are eligible for fix dispatch — `--rule EBL001 --fix` won't apply EBL004's fix even if both fire. Each dispatched fix writes an audit line tagged `stage=fix action=SetOption rule_id=ID` so `ebman audit --rule EBL001` returns clean per-rule history. Per-rule opt-out via `lint.fix_disable = "EBL004"` in `config.toml` (or `fix_disable = ["EBL004"]` in project-local `.ebman/ebman.toml`). Exit-code regime differs from `--no-fix`: `--fix` mode exits 0 on success / 1 on AWS dispatch failure, NOT 3 — operator's intent is "fix the issues", so a clean apply is exit 0. (`5cb2ab8`)

### Added — audit CLI

- **`ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID] [--action NAME] [--json]`.** First-class scriptable surface for the existing `~/.cache/ebman/audit.log`. Default text mode renders pretty columns (TS / REGION / STAGE / ACTION / TARGET / OUTCOME); `--json` emits JSONL one entry per line. `--tail` polls the file at 1s intervals from EOF (Ctrl-C to exit); rotation detected by file shrink (resets offset). `--since 1h` filters to entries within the duration window. New `src/audit.rs` module with `AuditEntry` typed fields + `parse_audit_line` parser handling both the normal-action shape and the rollout-line shape uniformly via a `parse_kv_pairs` tokenizer (quoted values + naked spaces in unquoted values both supported). (`dda731d`)
- **Audit-line format change: `outcome=ok` as explicit key=value pair** instead of bare trailing `ok`. Pre-0.14 entries stay parseable but lossy (target value extends to include the trailing token); pinned in `parse_audit_line_pre_0_14_bare_ok_lossy_but_parses` so the soft-regression is documented. (`dda731d`)

### Internal

- **`reqwest` + `sha2` added as direct deps.** reqwest with `default-features = false, features = ["rustls-tls"]` to reuse the rustls already in the tree; ~1MB binary growth. sha2 was already transitive via aws-sdk; promoted to a direct dep so `src/llm.rs` can use it for cache keys.
- **Tests: 669 lib + 17 tui-common (up from 628 + 17 in 0.13).** +18 audit-parser tests, +8 lint fix() coverage tests, +13 llm prompt/cache/dispatch tests, +2 config round-trip tests for the new `[explain]` block, +1 project-local `lint.fix_disable` parse test, +2 `lint_fix_disable` round-trip tests. `cargo test` runtime unchanged (~3s on M1).

## [0.13.0] — 2026-05-27 — Multi-region everything: cross-region rollout + fleet-wide reads

ebman now treats the fleet as a unit. A new sequential
cross-region deploy primitive — `ebman action rollout` for
CI and `:rollout` for the TUI — pre-flights every region
(env existence, current version label) before dispatching,
shows a plan, then drives each region through Updating →
Ready+Green with a `decide_poll` state machine, halting on
the first failure with a single `rollout_id` correlating
every audit-log line. The smart-feature reads from 0.12
(`lint`, `drift`) gain matching `--regions r1,r2,r3` flags
for the same fan-out shape. Docs catch up with shipped
code: README gets an inline TOC + Install above the fold,
`docs/` gets an end-to-end review pass against the
`commands.rs` registry, and CLAUDE.md locks in a docs
audit as part of the release procedure so the drift
doesn't reaccumulate.

### Added — multi-region

- **`ebman action rollout` CLI — sequential cross-region deploy.** New top-level subcommand follows the 0.12 CLI charter (`action <verb>` for writes; `--regions r1,r2,r3` fan-out; `--yes` required; structured exit codes). Pre-flight phase builds a per-region `AwsClient::with(profile, Some(region))`, fetches the target env, records its current version label, and refuses to dispatch if any region fails the pre-flight (env-not-found, AWS error, profile mismatch). Plan rendered to stdout as a table (REGION / ENV / CURRENT → TARGET / STATUS). Dispatch phase drives each region in order: `UpdateEnvironmentRequest{version_label}`, then `decide_poll(status, health, elapsed, wait_for_green_secs, auto_rollback_secs)` state machine waits for `status=Ready AND health=Green|Ok` (closes the brief Updating+Green transition window — see 0.12 audit). Halts on first failure. Audit lines all share a single `rollout_id = rollout-YYYYMMDDTHHMMSSZ`. Headless surface for git-tag-triggered deploy automation. (`ee789db`)
- **`:rollout` TUI surface — plan overlay + state machine.** New `ActionFlow::Rollout(RolloutFlow)` variant in `mode_action.rs` drives the same engine from inside the TUI. `Planning` runs preflight in parallel across regions; `AwaitingConfirm` renders the plan table inline in the action modal (REGION / ENV / CURRENT / TARGET / STATUS columns via a `pad_right` helper, errors rendered on `↳` continuation lines so an early-return doesn't abandon later rows — bug caught in self-review of first cut); `Dispatching { next_index }` walks the regions sequentially with the same `decide_poll` state machine the CLI uses; `Done` shows the final outcomes. Operator can review the plan and `y` to dispatch, or `Esc` to bail before the first write. (`b9ad597`)
- **`ebman lint --regions` + `ebman drift --regions` — fleet-wide reads.** Both subcommands gain a `--regions r1,r2,r3` flag with the same fan-out shape as `action rollout`: per-region `AwsClient::with(...)`, iterate, tag the originating region into each `Issue.fields` map for JSON consumers, prefix the region into the text-mode output. Drift mode pairs the multi-region live read against a single tfstate (cross-region terraform workspaces). Empty `--regions ""` refused with exit code 2. The same engine that drove the 0.12 TUI overlay + single-region CLI now drives fleet-wide reads — no parallel implementations. (`73861fe`)

### Added — docs

- **README inline TOC + Install above the fold.** Hero section trimmed to lift the Install block within ~50 lines of the top so new visitors land on `brew install` / `cargo install` before the feature tour. Inline TOC anchors the workflow + reference sections so the README stays a navigable single-page entry-point even at its current scope. (`c8d5e65`)
- **`docs/` end-to-end review against shipped code.** Audit pass against the `src/commands.rs` registry caught: ~30 commands shipped without docs (mostly batch-* and config-*), a malformed `[lint]\ndisable = [...]` TOML example, a fabricated `ebman ctl reload` reference, stale wording from the 0.12 saved-views unification. All five `docs/*.md` reference pages updated to match shipped behaviour. (`3c652e2`)

### Internal

- **Release procedure: docs audit at tag time.** New CLAUDE.md section codifies the docs walk as a release-time check, enumerating which files to audit against which source (`commands.rs` ↔ `docs/commands.md`, every new `ebman <sub>` in `main.rs` against `docs/headless.md`, every new config key against `docs/configuration.md`, TOML examples must actually parse). Closes the only-caught-during-review-pass class of drift. The 0.13 audit found one `docs/headless.md` gap (`action rollout` + `--regions` not documented); fixed before tagging. (`0b627d0`)

## [0.12.0] — 2026-05-27 — Smart features: lint engine, terraform drift, workspace polish

ebman now understands your fleet. A rule-based diagnostic
engine surfaces operator-actionable issues (`:lint` TUI
overlay + `ebman lint` CLI for git hooks / CI / monitoring);
terraform integration detects drift between tfstate and live
EB state (`:drift` overlay + `ebman drift` CLI + `ⓣ` badge
in the env-name column + confirm-modal warning on writes
against tf-managed envs); confirm modals run all of the
above at write time so the operator sees rule-keyed risk
before authorising. Workspace polish: saved views unified
into a single store (`]`/`[` cycles full filter+sort+group
snapshots, not just filter strings); `:batch-set-option`
captures undo entries (closes a multi-env safety gap from
0.11); README split into hero + `docs/` so new visitors hit
"Install" inside ~100 lines instead of past ~350.

### Added — smart features

- **Rule-based diagnostic engine + `:lint` TUI + `ebman lint` CLI.** New `src/lint.rs` module with a `Rule` trait, structured `Issue` output (rule_id / severity / title / detail / suggestion / fields map), and 6 v1 rules: `EBL001` AllAtOnce on multi-instance env (downtime risk); `EBL002` Web tier with empty `/` health-check URL; `EBL003` env Red >4h (operational hygiene); `EBL004` Fixed `BatchSize > MaxSize` (deploy math broken); `EBL005` `MinSize=MaxSize=1` (no redundancy, Info); `EBL006` ASG `Cooldown < 60s` (thrashing, Info). Three surfaces share the engine: `:lint [ENV]` TUI overlay; `ebman lint [--env --json --severity --rules --quiet]` CLI with exit codes 0/1/2/3 (non-zero on issues for CI gating); confirm-modal warning lines at write time. Operator-tunable disables in both `~/.config/ebman/config.toml` (`lint.disable = "EBL001"`) and project-local `.ebman/ebman.toml` (`[lint]\ndisable = [...]`) — project disables extend the global set. Issue shape designed for an eventual `ebman explain ISSUE_ID` LLM call (out of 0.12 scope). (`6a96bb6` + `33ad2ee` + `f415526`)
- **Terraform integration: `:drift` TUI + `ebman drift` CLI + `ⓣ` badge.** New `src/terraform.rs` reads `terraform.tfstate` JSON directly (no shell-out to the `terraform` binary needed). Walks up from cwd for `.terraform/terraform.tfstate` (preferred — post-init location for S3 / Terraform Cloud backends) or a local `terraform.tfstate` (same discovery shape as `.ebman/` and `.elasticbeanstalk/`). Pure `compute_drift` compares tf-declared option_settings + version_label against live EB state, returns a structured `Vec<DriftField>` sorted deterministically (version_label first, then option_settings by ns+name). Direction-aware semantics: pairs PRESENT IN TF are compared; live-only settings aren't drift (could be EB defaults or operator-set additions); empty tf version_label skips the version drift check (operators using a deploy pipeline don't want drift alerts every deploy). Three surfaces: `:drift [ENV|refresh]` TUI overlay; `ebman drift [--env --tfstate --tfdir --json --quiet]` CLI; `ⓣ` (U+24E3) badge in the env-name column for tf-managed envs (O(1) `HashSet` lookup per row, refreshed on context switch + manual `:drift refresh`); confirm-modal yellow warning line on destructive actions against tf-managed envs (`⚠ env is terraform-managed — changes will drift on next plan/apply`). (`ba02e75` + `7bd6415` + `c7b4ff8`)
- **Confirm-modal lint hooks at write time.** Every confirm modal now runs the lint engine against the env's pre-write state and renders Warn+ issues as inline yellow warning lines. Generalises the health-check probe + unavailability pill pattern — same modal shows ALL relevant risks before the operator confirms, not just the two specialised ones. Same Issue shape `:lint` and `ebman lint` already render. Operator-tunable disables apply here too. (`6607ba8`)

### Added — workspace polish

- **Saved views unified.** Closed a long-deferred BACKLOG item: `App.named_filters` (filter-only) and `App.saved_views` (full filter+sort+group+scope) collapsed into a single store. `]` / `[` now cycles FULL views — sort + group apply alongside filter, the gh-dash-style "tabs" the BACKLOG had been promising since 2026-05-24. Chip bar reads from `saved_views`. `:filter NAME` / `:save NAME` / `:drop NAME` / `:filters` and `:save-view NAME` / `:view NAME` / `:view-drop NAME` / `:views` all operate on the unified store with different encodings (filter-only vs full snapshot). Legacy `filter.NAME = "..."` lines in `state.toml` auto-promote into `saved_views` on first load using the filter-only encoding; explicit `view.NAME` wins on collision. First save after upgrade drops the legacy output. (`bb7547b`)
- **`:batch-set-option` captures undo entries.** Closed the multi-env undo gap from 0.11 — `spawn_batch_set_option` now does the same pre-write `DescribeConfigurationSettings` read + `build_undo_entry` + `AppMsg::UndoCaptured` dispatch as its single-env sibling, so each env in a batch contributes its own undo entry. Repeated `:undo` walks the batch backwards. Self-review caught a context-switch race (env terminated mid-batch); guarded with an upfront fleet-presence check + audit-logged skip. (`76e54b6`)
- **README + `docs/` split.** 448-line README trimmed to ~103: hero + triage workflow + key highlights + install + quickstart + documentation index. Reference moved to topic-grouped files under `docs/` (keys / commands / configuration / fonts / headless / safety+privacy / development). New visitors hit "Install" inside ~100 lines instead of past ~350 of reference tables. (`166f42b`)

### Verified — pinned by experiment

- **OSC 8 terminal hyperlinks not viable in ratatui 0.29.** Re-attempted (vs the 0.11 assumption-based skip) with a real experiment in `ui::tests::osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation`. Confirmed: each byte of an OSC 8 escape sequence gets its own 1-cell-wide printing cell — a 24-byte opener consumes 24 cells of layout space, pushing the visible text past the buffer width. The regression test pins the broken behavior so a future ratatui upgrade that adds zero-width control handling will fail it and prompt us to revisit. (`4c717ad`)

### Internal

- **CLI charter doc in BACKLOG.** Locked conventions for top-level CLI shape (flat verbs for reads, `action <verb>` for writes, `ctl` control plane, `mcp serve` reserved for server-mode futures), per-command flags (`--env`, `--json`, `--quiet`, `--watch`, `--regions`, duration grammar), exit-code matrix (0/1/2/3/4/5). New `ebman lint` and `ebman drift` subcommands follow it; future commands stay symmetric. (`3bd681d`)

## [0.11.0] — 2026-05-26 — Triage ergonomics: freeze, undo, aliases, capacity preview

Small but focused minor — picks up where 0.10 left off and
extends the safety-net + observability themes into day-to-day
config edits and incident triage. Every option-settings write
is now reversible via `:undo`; mid-incident operators can lock
the fleet with `:freeze-deploys "incident #N"`; the Deploy
confirm modal grew a fourth signal (capacity impact) alongside
the inline preview + health-check probe; and `alias.NAME = "…"`
entries in `config.toml` let daily-driver shortcuts shrink to
one keypress.

### Added

- **`:undo` for the last option-settings write.** Automatic
  before-state capture on every `spawn_option_settings_update`
  dispatch (covers `:set-option` / `:keypair` /
  `:deployment-policy` / `:rolling-update` / `:health-check-url`
  / `:env-edit` / `:capacity` / `:scaling-triggers` /
  `:listener-edit` and the form-driven equivalents). 10-entry
  ring buffer (`App.undo_history`); `:undo` pops the back and
  re-dispatches the reverse-action via the same spawn — which
  captures ITS own undo, so `:undo`+`:undo` is redo for free.
  Empty-string-prior values reverse via `to_remove` (not empty
  `to_set`) since EB doesn't distinguish unset from empty.
  Cross-context cleared on `apply_rebuild`. Refuses cleanly when
  the captured env is terminated OR filtered out of the current
  view (in the latter case the entry is put back on the deque so
  the operator can retry after clearing the filter). Pure
  `build_undo_entry` helper unit-tested across the four
  reverse-action shapes. (`aa253bd`, follow-up display-row-index
  fix `7516200`)
- **`:freeze-deploys [reason]` / `:thaw-deploys`.** Session-
  scoped fleet-wide write-lock with an operator-supplied reason,
  layered above the per-env / per-account `safety.*` pins in
  `is_read_only_for`. Refusal toast surfaces reason + age
  (`"deploys frozen (3m ago): incident #1234 — :thaw-deploys to
  unfreeze"`). Audit-logged. Re-issue updates the reason in
  place. Not persisted to `state.toml` (intentional — durable
  pins use the existing `safety.*` mechanism). Useful during
  incident triage to prevent accidental deploys mid-investigation.
  (`f51168b`)
- **Pre-deploy "estimated unavailability" line in the confirm
  modal.** Fourth confirm-time signal alongside the impact
  preview, version metadata, and health-check probe. Renders
  `deploy plan: POLICY → max N/M instances unavailable` (yellow
  if any unavailability, green if none) computed from the env's
  deployment policy + batch settings + ASG max-size. Pure
  `compute_unavailability_count` + `compute_batch_count` +
  `format_unavailability_line` helpers covering all five EB
  deployment policies (AllAtOnce / Rolling /
  RollingWithAdditionalBatch / Immutable / TrafficSplitting)
  plus an unknown-policy worst-case fallback. Skipped in
  `--demo` mode. (`f1b5b2c`)
- **Custom command aliases in `config.toml`.**
  `alias.dp = "deploy --auto-rollback 5m"` entries expand on
  command-bar entry before the dispatch match — `:dp build-900`
  becomes `:deploy --auto-rollback 5m build-900`. Single-level
  expansion only (no transitive chaining → no infinite-loop
  footgun, no cycle-detection complexity). Pure
  `expand_command_alias(line, aliases)` helper unit-tested.
  Named `command_aliases` on Config + App to disambiguate from
  the existing `:alias <env> <label>` env-rename feature (which
  is state.toml-persisted and unaffected). (`c70c862`)

### Fixed

- **`:undo` table_state index mismatch under active filter or
  grouping.** Code-review caught: `cmd_undo` set
  `table_state.select(Some(idx))` using
  `environments.iter().position(...)` — the env's index in the
  unfiltered fleet vec — but `selected_env()` reads from
  `display_rows()` (filtered + grouped view). The two index
  spaces diverge whenever a filter is active or grouping
  inserts separators, so the dispatch could target the wrong
  env or hit out-of-bounds. Fix: look up the target in
  `display_rows()` space. Also split "env not in view" into
  two clearer refusals (terminated vs filtered out, with
  the entry put back on the deque in the filtered-out case).
  +2 regression tests. (`7516200`)

### Skipped on purpose

- **OSC 8 terminal hyperlinks.** ratatui 0.29 has no OSC 8
  support — its cell-based buffer mis-handles escape sequences
  and breaks width calculations. Workaround would require a
  custom widget that bypasses ratatui's diff renderer per-line
  (brittle). Design trade-off with no obvious winner. Revisit
  if upstream ratatui adds native support.
- **`:config-diff --at 1h|24h|7d`** (point-in-time config diff).
  Re-audit caught a false premise in the BACKLOG entry: EB's
  event API only carries free-text messages (`"Environment
  configuration was updated successfully"`), no structured
  before/after option-settings deltas. The "replay backward"
  mechanic isn't implementable against EB's API surface. The
  honest reshape (a `--window` flag on existing `:changes`)
  duplicates 80% of `:changes` for marginal value.

## [0.10.0] — 2026-05-26 — Deploy story complete: safety nets, observability, CI parity

This release completes the deploy story 0.9.0 started: every step
of "ship a build" is now operator-observable, composable, and has
a safety net. The auto-rollback watchdog (0.9.0) is joined by a
wait-for-green watcher, an operator-named rollback target, an
explicit abort, a header countdown pill, and a non-interactive
CLI surface for CI/CD pipelines. Two more guardrails fire at
confirm-time before a deploy commits: an inline version preview
and a live health-check probe. Promotion across envs ships as
`:promote-env STAGING PROD` so the daily staging→prod gesture is
one dispatch. Outbound notifications fan audit lines to a
Slack-shaped webhook; the EB CLI's `.elasticbeanstalk/config.yml`
slots in as a config source so existing EB CLI users get
working-context discovery for free.

### Added — confirm-modal safety net

- **Pre-deploy health-check probe.** Every `:deploy` confirm modal now fetches the env's `Application Healthcheck URL` option-setting, composes a probe URL against the env's CNAME, and HEADs it via curl (`-L --max-time 2 -I`) at confirm time — not dispatch. Silence on 2xx (modal stays clean); yellow `⚠ health-check probe: <reason>` line on non-2xx / timeout / transport error, plus a muted hint `(deploy will proceed; consider --auto-rollback Nm if this matters)`. Catches the canonical auto-rollback footgun (new version doesn't have the configured `/health` endpoint → instant Red after deploy) BEFORE the operator commits. Pure helpers `build_health_check_probe_url` + `classify_health_check_status` unit-tested across the path-normalisation + status-code matrix. Skipped in `--demo` mode so synthetic CNAMEs don't pollute screencasts. (`04e4eac`)
- **Pre-deploy preview inline in the confirm modal.** Every `:deploy LABEL` confirm modal now auto-fetches `list_application_versions` and renders the existing `format_deploy_preview` body (candidate label / age / description / rollback-warning when candidate is older than current) inside the modal — no separate `:deploy LABEL --preview` round-trip required. Loading placeholder while the fetch is in flight; failures inline as `version preview unavailable: <reason>` rather than leaving the slot blank. (`5ef0a97`)

### Added — deploy story

- **`:promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm]`** — one-command staging→prod promotion. Takes SOURCE's current `version_label`, opens the deploy confirm on TARGET (not the selected env), threads both watchdog flags. Routes through a new `open_parameterised_action_on(env, …)` escape hatch so the destination is named, not inferred from the table cursor. Refuses if SOURCE has no version, source==target, or SOURCE's version is already on TARGET. Config-options promotion (copying operator-set option-settings deltas alongside the version) is a deeper follow-on; today operators run `:config-diff SOURCE` first if they need to verify alignment. (`a1f3b7b`)
- **`:deploy LABEL --wait-for-green Nm`** — pure-observability companion to `--auto-rollback`. Arms a watcher at dispatch; `apply_refresh` pins success (`✓ deploy reached Green: ENV`) when the env reaches Green or pins a timeout error if the deadline elapses while still non-Green. Different glyph + colour (`👁 watching ENV REMAINING`, theme.title) from the `⏱ rollback` pill so the operator can tell at a glance which kind of in-flight observer is on the env. Composes with `--auto-rollback` — both flags can be set on the same deploy. (`2328c83`)
- **`:rollback --to LABEL [--auto-rollback Nm]`** — operator-named rollback target. Skips the snapshot/event-scan detection and routes straight to the deploy confirm with the named label. Composes with `--auto-rollback Nm` so the operator can dispatch "roll back to build-820, auto-roll-forward to build-823 if Green doesn't land within Nm" in one command. Validates non-empty target + refuses idempotent same-version rollbacks. (`021127c`)
- **`:abort-rollback [ENV]`** — explicit disarm. No-arg drains every armed watchdog in the current context; with an env name, just that one. Audit-logged. (`0293fd3`)
- **Armed-watchdog visibility.** Header countdown pill (`⏱ rollback prod-api in 4m22s`) plus `:rollbacks-armed` (alias `:rb-armed`) overlay listing every armed watchdog with env / target / armed-ago / deadline-in. Pure renderers covered by tests. (`3a81329`)

### Added — CI / CD ergonomics

- **`ebman action deploy --env X --version Y [--wait-for-green Nm] [--auto-rollback Nm]`** — non-interactive CLI parity with the typed-command `:deploy`. Polls EB every 5s for Green/timeout/rollback resolution; pure decision helper `decide_poll()` is unit-tested across the full four-state matrix. Distinct exit codes for CI branching: 0 = ok, 1 = AWS error, 2 = usage error, 4 = wait-for-green timeout, 5 = auto-rollback dispatched. Same duration grammar as the TUI path (`5m` / `30m` / `1h`). Refuses upfront if `--auto-rollback` is set but the env has no prior version to roll back to. (`fa6ba36`)
- **`notify_webhook` outbound integration.** New opt-in `config.toml` setting `notify_webhook = "https://..."` (or `""` to disable) arms a fire-and-forget POST on every audit-log line. Body is Slack-incoming-webhook-shaped — a top-level `text` field renders the audit line for human-readable Slack channels, with structured sibling keys (`at`, `account`, `profile`, `region`, `detail`) for routing / filtering by other consumers. Shells out to `curl` (10s cap, same approach as `fetch_url_text`) so we don't add an HTTP-client dep. Webhook failures log via `tracing::warn!` but never alarm the operator — the local `audit.log` remains the source of truth. (`71f4bd3`)
- **EB CLI `.elasticbeanstalk/config.yml` reader.** New `eb_cli` module walks up from cwd looking for the EB CLI's project-config marker directory and parses `config.yml` for default `profile` / `default_region` / `application_name`. Most EB CLI users already maintain this file; reading it lets ebman pick up the working context without forcing a duplicate `.ebman/ebman.toml` entry. Precedence: `.ebman/ebman.toml` > `.elasticbeanstalk/config.yml` > persisted `state.toml`. `application_name` fills in as a soft filter prefill when `.ebman/` hasn't set one. Parse errors / null values / unknown keys all silently fall back to defaults — a corrupt EB CLI file can't refuse to launch ebman. (`d8376e2`)

### Fixed

- **`Updating`+`Green` false-positive in deploy watchers.** EB briefly leaves `health=Green` while `status` flips to `Updating` right after `UpdateEnvironment`. Three watchers were checking health alone and false-positiving during that window: the auto-rollback watchdog (disarm before deploy rolled — safety net evaporates), the wait-for-green watcher (premature "✓ reached Green" pin), and the CLI `decide_poll` (exit 0 before deploy started — CI gates passing on rolling deploys). Fix: new pure `deploy_settled_green(status, health)` requiring BOTH `status=Ready` AND `health=Green/Ok`; all three sites now route through it. (`e705d54`)

## [0.9.0] — Pre-deploy snapshot + auto-rollback

Headline addition is `:deploy LABEL --auto-rollback Nm` — a
canary-style safety net for production deploys. Every `:deploy`
captures the env's pre-deploy `version_label` into a persisted
snapshot; the `--auto-rollback Nm` flag arms a watchdog that
redeploys that snapshot if the env hasn't reached Green by the
deadline. Eliminates the "operator dispatched a bad version,
walked away, came back to a Red prod" failure mode. Also folds
in a code-review pass + an `:about`-overlay sizing fix that
0.8.1 users would otherwise have to wait for.

### Added
- **`:deploy LABEL --auto-rollback Nm`** — arms a per-env watchdog after dispatch. Two outcomes: env reaches Green by the deadline → watchdog disarms with a `pin_status` toast; env still Red / Yellow at deadline → ebman automatically redeploys the captured snapshot's previous version. Same duration grammar as `:logs-insights --window` (`5m` / `30m` / `1h`). The auto-rollback respects per-env / per-account read-only safety pins (`safety.envs.NAME.read_only`) — if the operator pinned the env to read-only mid-window, the watchdog disarms with an error pointing at the lock rather than punching through. Audit-logged as `action=AutoRollback target=ENV version=LABEL health=HEALTH`. (`9392f25` + follow-ons)
- **Pre-deploy snapshots persist to `state.toml`.** Every `:deploy` captures the env's current `version_label` into `App.deploy_snapshots` (in-memory) AND serialises to `state.toml` as `deploy_snapshot.ENV = "label|RFC3339-ts"` lines. Cross-session `:rollback` falls back to this snapshot (much more reliable than the event-history scan which has a 100-event window cap). Snapshots are dropped on context switch (account / region change) so a same-named env in another context can't trigger a stale-target rollback. (`8a877f2` + `6b1e339`)
- **Early-disarm on Green refresh.** The watchdog used to wait the full N minutes even if the env recovered at minute 1. New `apply_refresh` decision pass drains the watchdog the moment the env crosses Green, with a `pin_status` disarm toast. Operator sees recovery acknowledged in real-time. (`204903c`)
- **AWS-mock tests for new 0.8/0.9 SDK calls.** `fetch_alarm_history` (CloudWatch `DescribeAlarmHistory`) + `run_shell_command` (SSM `SendCommand` + `GetCommandInvocation` polling loop) both have mocked-AWS coverage now. The SSM polling-loop test uses `#[tokio::test(start_paused = true)]` so the 2 s poll interval doesn't actually sleep. (`60bfe25`)

### Fixed
- **`:about` overlay no longer wraps text mid-word.** The Stacked layout sized the popup at `ABOUT_SCENE_W + 6 = 46` cols, but the text lines were designed for `ABOUT_TEXT_W = 58`. "Polymorphism Ltd builds operations tools..." wrapped to "operations tools f / Hire u / what's missing — happ" with everything truncated. Popup now sizes to `max(scene, text) + 6 = 64`; the layout-picker threshold bumps accordingly. +3 regression tests. (`a10c913`)
- **`armed_watchdogs` + `deploy_snapshots` no longer leak across context switches.** `apply_rebuild` (the account / region switch handler) now `.clear()`s both alongside the other context-scoped state. Previously a same-named env in the new context could be seen as "still armed" by the early-disarm pass, and the late deadline tokio — even though the generation guard drops it — could in principle have triggered a spurious rollback if timing aligned. (`6b1e339`)
- **Same-tick race between deadline message + refresh result.** Earlier shape had `handle_auto_rollback_check` dispatching inline using cached health. If the deadline message landed before the refresh result, the handler read stale (Red) health and dispatched — even though the refresh would have shown Green. Inverted: deadline message now just kicks a manual refresh; `apply_refresh` is the single decision point and reads the freshly-applied health. Trade-off: up to one refresh-roundtrip of extra latency past the deadline before dispatch, well below the human-noticeable threshold. (`ac95d58`)
- **`DeploySnapshot::parse_persisted` failures now emit a `tracing::warn!`.** Malformed state.toml entries were silently dropped before — confusing UX when the operator then ran `:rollback` and got "no snapshot" without knowing the file had one. (`6b1e339`)

### Changed
- **`:rollback` prefers the captured snapshot when one exists.** Snapshot lookup is O(1) in the in-memory map and more reliable than the existing event-history scan (which has a 100-event window cap that can miss the actual previous version on chatty envs). Falls back to the event scan when no snapshot exists. (`9392f25`)
- **`AwsClient::stub()` / `AwsClient::for_tests()` / `App::for_tests()`** tightened from `pub` to `pub(crate)`. Every caller lives in this crate (test code + `App::new_demo`); no need to expose to downstream consumers. SemVer hygiene. (`6b1e339`)
- **`DeploySnapshot` + `ArmedWatchdog`** are now `pub(crate)`. Used only as internal App state. (`6b1e339`)
- **`run_shell_command` uses `tokio::time::Instant`** for the deadline so paused-clock tests can exercise the timeout branch. Production behaviour unchanged. (`60bfe25`)

### Internal
- **New `dispatch_auto_rollback(env_name, health)` helper** in app.rs — shared between `apply_refresh`'s expired-deadline path and (formerly) the inline handler. Single source of truth for the rollback dispatch shape. (`ac95d58`)
- **New `pin_error(msg)` helper** symmetric to `pin_status`. Used by `dispatch_auto_rollback`'s "no snapshot" branch so the warning survives `apply_refresh`'s same-tick auto-clear. (`ac95d58`)
- **Pre-commit hook tightened** to run `cargo clippy --all-targets -- -D warnings` (was `cargo fmt --check` only). Catches the doc-lint class that bounced CI on the f60ba3c push. (`b240d4f`)

### Test foundation
- 491 tests + 16 in tb-tui-common. New since 0.8.1: 4 AWS-mock tests for SSM + CW (`run_shell_command_collects_per_instance_result_on_success`, `run_shell_command_synthesises_local_timeout_when_deadline_passes`, `fetch_alarm_history_extracts_kind_and_summary`, `fetch_alarm_history_tolerates_missing_optional_fields`); 5 auto-rollback tests covering the decision matrix (disarm-on-Green, dispatch-on-deadline-non-Green, no-snapshot error path, keep-armed-before-deadline, noop-when-no-watchdog); 1 context-switch clear test; 2 persistence round-trip tests; 3 :about overlay-sizing regression tests.

## [0.8.1] — Demo mode, hero gif, terminal title

A small point release on top of 0.8.0. Headline addition is
`--demo` — a synthetic-fleet runtime that backs the README's
animated demo gif and gives new users a no-AWS-required first
launch. Also folds in a code-review fix from a real data-loss bug
the demo mode would otherwise have introduced, plus the terminal-
title polish that mirrors what k9s and lazygit do.

### Added
- **`ebman --demo`** — runs against a hand-crafted six-env `ledgerly` fleet (Web + Worker envs across Green / Yellow / Red / Updating health tiers; one Worker with a 12-deep DLQ) with no AWS calls, no disk reads. The whole UI surface stays usable: filter, sort, `:why` overlay, drill into Detail (Health / Events / Instances / Queue tabs are all fixture-populated), and `s` on Detail/Instances opens a fake SSM session whose canned content (banner + `uptime` + `tail` on the EB engine log) types itself out at ~60 cps. Spawn-site gates in `aws.rs` short-circuit every `list_*` / `describe_*` / `:why` fetch to fixture data so nothing falls through to the stub AwsClient. New `src/demo_fixture.rs` carries the data; new `App::new_demo(config)` + `App.demo_mode: bool` carry the runtime state.
- **Terminal window title set to `ebman` on TUI entry.** Single OSC 2 escape via crossterm's `SetTitle` in `enter_tui()`. Honoured by xterm / iTerm2 / Terminal.app / Ghostty / Alacritty / WezTerm / VS Code's terminal; ignored silently by terminals that don't.
- **`demo.gif` rendered into the README hero slot.** Captured via VHS against `--demo`; the `demo.tape` script lives next to it so future renders are one `vhs demo.tape` away.
- **`scripts/update-formula.sh`** Homebrew bumper (already in 0.8.0; explicitly noted here because it now drives every release with no manual SHA fiddling).

### Fixed
- **`--demo` no longer clobbers `~/.config/ebman/state.toml` on exit.** The fixture sets a synthetic profile, region, env names, and `cost_enabled = true`. Pre-fix, `main.rs:122`'s `persist_state()` would unconditionally write that to the operator's real state file. Now `persist_state()` early-returns when `App.demo_mode` is true. +1 regression test.
- **`:ssh` and `:ssm-run` now write to the audit log.** Other write-class commands (deploy / terminate / DLQ purge / alarm CRUD) already write `stage=dispatched` + `stage=completed` lines to `~/.cache/ebman/audit.log`. SSM Session Manager and Run Command are also write-class (a shell command can mutate state), and they're now in the log too — including the per-instance ok=N/total summary for `:ssm-run`.
- **README L211's stale `:diff NAME` description** updated to advertise the post-0.8 two-arg form + the other 0.8 inspection commands (`:config-diff-local`, `:lineage`, `:alarm-history`, `:ssh`, `:ssm-run`).

### Internal
- **Pre-commit hook tightened** to also run `cargo clippy --all-targets -- -D warnings` (was: `cargo fmt --all -- --check` only). Caught the `+ producing output` doc-lint that bounced CI on the f60ba3c → f8ebc33 push and would otherwise have bounced future commits too. Lives at `.git/hooks/pre-commit` (local-only).
- **`AwsClient::stub()` + `AwsClient::for_tests()`** lose their `#[cfg(test)]` gates — `App::new_demo` needs them at runtime.
- **Cargo.toml workspace comment** updated to describe the shipped surface + the `tb-tui-common` rename + the pgman consumption path (was: "more migrations coming in 0.8" — anachronistic post-release).
- **Status-line config** (`.claude/settings.local.json`) for a `ebman <version>` claude-code status line, untracked + maintainer-local.

## [0.8.0] — Workspace refactor, eight new commands, per-env safety pins

Architecture: the bin became a lib+bin so a sibling `tui-common`
workspace crate can share splash / theme / overlay / font-probe / util
modules with another k9s-style TUI in the same shape (`pgman`). Operator
features: eight new commands (`:diff ENV-A ENV-B`, `:lineage`,
`:alarm-history`, `:ssh`, `:ssm-run`, `:config-diff-local`, and `]` / `[`
cycle for the saved-filter chip bar), plus a cost-aware `:why` /
Detail/Health view when `:cost on` is enabled. Safety: per-env /
per-account read-only pins via `safety.envs.NAME.read_only` and
`safety.accounts.NAME.read_only` in `config.toml`, swept across ~20
destructive sites via a new `deny_write(env, verb)` helper, plus a
project-local `.ebman/ebman.toml` so a repo can pin its working
profile / region / filter without leaking credentials.

### Added
- **`:diff ENV-A ENV-B`** — extends the existing single-arg `:diff` to also accept two explicitly-named envs, so an operator can compare `staging` ↔ `prod` without first selecting one of them. Reuses the existing `Overlay::Diff` renderer; same-env-twice and unknown-env get clear errors. (`9f76333`)
- **`:lineage`** — deploy-only chronological timeline for the selected env. Where `:changes` mixes deploys with config-change events, `:lineage` filters to events that carry a `version_label`, collapses consecutive same-label events into one row (a deploy emits multiple events: started / instance OK / completed), and shows the deploy's span (`took`) plus the gap to the next-older deploy (`Δ since previous`). Pure `build_lineage` + `format_lineage`. (`235d135`)
- **`:alarm-history NAME`** — recent CloudWatch alarm transition timeline. Fetches up to 50 entries via `cw:DescribeAlarmHistory`, surfaces them as a TextOverlay newest-first with timestamp + kind (`StateUpdate` / `ConfigurationUpdate` / `Action`) + summary. Empty result shows the 90-day-retention hint so the operator isn't left wondering whether the fetch succeeded. (`dc416b3`)
- **`:ssh [i-abc]`** — SSM Session Manager from the command bar. With an arg, targets the named EC2 instance directly (validated to start with `i-`); without, opens a picker over cached `Detail.instances` (same source as `:resources` and the `s` keybind on Detail/Instances). Routes through the existing `pending_shell_target → open_embedded_shell` machinery — same TUI-suspend/resume + alt-screen dance the existing instance-tab `s` keybind uses. Requires the AWS CLI + `session-manager-plugin` on PATH. (`39f0de7`)
- **`:ssm-run "<shell-command>"`** — fan a shell command across the env's instances via SSM Run Command (`AWS-RunShellScript`). Targets are read from cached `Detail.instances`; the result lands as a TextOverlay with per-instance `─── id [status, exit=N] ───` headers and stdout/stderr blocks (50-line + 200-char-per-line truncation so verbose output doesn't blow out the overlay). 60s wall-clock cap; instances still running past the deadline get a synthetic `TimedOut(local)` row. Gated by `deny_write` — SSM can mutate state, treat it as a write. New `aws-sdk-ssm` dependency. (`7a56316`)
- **`:config-diff-local [NAME]`** — diff the deployed env's current option settings against a local EB CLI saved config (`.elasticbeanstalk/saved_configs/<NAME>.cfg.yml`). No arg auto-picks the lone file; with multiple, lists names. Bridges EB CLI users into ebman: answers "is what I committed still what's deployed" without rerunning `eb config get` and eyeballing the YAML. New `src/saved_config.rs` module with `parse_saved_config` (coerces YAML scalars — `true` / `4` / `'4'` all become `"4"`), `discover_saved_configs`, `saved_config_name`. New `serde_yml` dependency. (`46a41f6`)
- **Cost in `:why` + Detail/Health** — when `:cost on` has populated `app.costs` from Cost Explorer, the `:why` overlay shows a new top-of-overlay `cost  $NN/mo` row right after the runbook line, and Detail/Health appends a `cost: $NN/mo` chip to its status line. Same green/muted/red bucket palette as the COST column. Both sites no-op when cost tracking isn't enabled — operators who don't care see no layout change. (`351bd7b`)
- **`]` / `[` cycles through saved-filter chips on the main view** — the chip bar above the env table has shown active named-filter membership since `named_filters` landed; the missing piece was a keybind to flip between them. `]` / `[` (guarded by `detail.is_none() && !named_filters.is_empty()` so Detail/Metrics' existing bindings keep working) now cycles through `named_filters` in BTreeMap sort order. (`b240d4f`)
- **Per-env / per-account read-only safety pins.** `config.toml` `safety.envs.NAME.read_only = true` and `safety.accounts.NAME.read_only = true` parse + round-trip; lifted onto `App.safety_envs` / `App.safety_accounts`. New `App.is_read_only_for(env_name)` resolves global → per-env → per-account-by-profile-name; `App.deny_write(env_name, verb) -> bool` sets the toast + returns the gate in one call. Wired into ~20 destructive sites (lifecycle, deploy, config edits, DLQ resend/purge/replay, tags, delete-app-version, option-settings updates, alarm create/delete, config-template apply/save, custom-platform-delete). The four batch-op sites in `cmd_write.rs` still gate on the global flag — refuse-some-keep-others inside the dispatch loop is a deeper refactor and stays on the BACKLOG. (`7a23333` + `9257d0a`)
- **Project-local `.ebman/ebman.toml`** — walks up from cwd looking for a `.ebman/` directory, reads `ebman.toml` if found. Schema: `profile`, `region`, `application` (filter prefill), `filter`, and `[runbooks]`. Profile / region win over persisted state so a repo pins its working context; runbook entries merge with the user-level map with project-wins-on-collision. Commit-into-the-repo design (no credentials in the file). Used to wire SSO-via-multi-account workflows where every repo wants a specific working context. (`5a8d35c`)
- **`scripts/update-formula.sh vX.Y.Z`** — long-promised Homebrew formula bump helper. Downloads the three release tarballs via `gh release download`, computes SHA-256s, rewrites both `Formula/ebman.rb` files (this repo + sibling `tombaldwin/homebrew-tap` clone) idempotently. Bash-3.2-safe (macOS default). Closes the recurring "remember to bump the SHA256s on each release" footgun. (`9ef1b15`)

### Changed
- **Splash holds on the final frame for ~2s before wrapping** — the new beanstalk-growing animation reaches a stable "bud" frame as its terminal state, but the splash was cycling back to frame 0 too aggressively, giving the impression the animation hadn't finished. `FINAL_FRAME_HOLD_TICKS = 67` (≈2 s at 30 fps) holds the bud frame so it reads as the closing pose rather than a fleeting in-between. (`867932c`)

### Internal
- **Lib+bin refactor.** New `src/lib.rs` declares every `pub mod` + the `Tui` + `LogReloadHandle` type aliases. Splash code (446 lines + 14 frame consts + 6 tests) lifted out of `main.rs` into its own `src/splash.rs`. `main.rs` is now a thin bin: argv parsing, TUI lifecycle (enter_tui / leave_tui / panic hook), the three subcommand handlers (envs / action / ctl), logging setup. Unblocks the `tui-common` workspace crate. Test count preserved: 443 = 436 lib + 7 bin. (`b582bc2`)
- **`tui-common` workspace crate.** Root `Cargo.toml` has `[workspace] members = ["tui-common"]` + `default-members = [".", "tui-common"]`; the `tui-common/` crate is `version = 0.1.0, publish = false` with minimal deps (crossterm + ratatui + tracing). Five modules migrated (16 tests across them): `font_probe` (Powerline probe), `overlay` (`OverlaySize` + centred-rect helpers), `util` (`parse_bool` + `write_atomic`), `theme` (`IconStyle` + `contrast_text_for` WCAG picker), `splash` (pixel-art `render_frame` loop with palette closure). All re-exported from ebman so existing call sites stay unchanged. Sibling `pgman` can `path-dep` on this for local dev. (`d9cefca` → `ec0621c`)
- **Mode handler split.** The six inline `Mode::X => match key.code { … }` blocks in `handle_key` (Filter / Help / Command / Palette / QuickJump / Picker) are now one-liners delegating to `handle_X_key(key)` in a new `src/app/mode_keys.rs` (203 lines, follows the `cmd_*` split pattern). `app.rs` 16,394 → 16,211 lines. (`fbb5bb6`)
- **`project.rs` migrated to serde + toml (proof of concept).** The hand-rolled parser is gone; `toml::from_str` does the work, with `#[serde(default)]` for forward-compat against new schema fields. Empty-string→None preserved via a small `deserialize_non_empty` adapter. `state.rs` / `config.rs` deferred — format-collision issues (`filter = "foo"` vs `filter.NAME = "..."` in state, CSV-in-string fields in config) need their own focused session. (`0cac196`)
- **Integration test coverage.** 5 new `App::for_tests` golden-path workflows (`space_toggles_multi_select_and_esc_clears_it`, `filter_mode_text_input_and_backspace_round_trips`, `esc_in_filter_mode_clears_the_filter`, `star_toggles_pinned_set_for_selected_env`, `picker_workflow_open_filter_enter_dispatches_choice`). Coverage 7 → 12 demo workflows. (`6fc6dc5`)
- **`release.yml` `workflow_dispatch` trigger.** Manual re-run of the crates.io job when the secret was missing on the first `release.published` fire; takes a `tag` input so the checkout pulls the right ref. (`ca83586`)
- **Homebrew tap caught up to 0.7.0.** `tombaldwin/homebrew-tap` had been stuck at 0.3.5 since first set up — 0.4.x / 0.5.x / 0.6.x / 0.7.0 never made it across. Bumped + verified end-to-end (`brew tap tombaldwin/tap && brew install ebman` resolves, installs, `ebman --version` reports `0.7.0`). The new bump script keeps future tags in sync.

### Test foundation
- 470 tests (was 443 at 0.7.0 → 0.8.0 opener). New: 3 per :diff two-arg, 3 per :lineage (build + format + edge cases), 2 per :alarm-history, 3 per :ssh dispatch, 5 per :ssm-run (renderer + edge cases + dispatch), 7 per saved_config (parse + discover + name extraction), 3 per cycle_named_filter (wrap forward / backward / empty). Plus integration tests above and tui-common's 16 sub-crate tests.

## [0.7.0] — Streaming bundle upload, automated publish, webhook trim

A short release focused on operator hygiene — lifts the 5 GiB deploy ceiling
without holding the bundle in RAM, automates the manual `cargo publish` step
that lapsed twice in the 0.4.x line, and trims the half-built single-URL
webhook in favour of a tracing + audit-log signal operators can wire to
whatever notifier they want.

### Added
- **`:deploy --from` streams from disk and uses multipart above 64 MiB.** The previous path read the whole bundle into RAM via `std::fs::read` and used a single `PutObject` capped at 5 GiB. Now `AwsClient::upload_bundle` streams via `ByteStream::from_path` below the threshold and switches to multipart in 16 MiB chunks above it (`CreateMultipartUpload → UploadPart×N → CompleteMultipartUpload`), with `AbortMultipartUpload` on every failure path so S3 doesn't accumulate orphaned parts. Peak RAM is one part regardless of bundle size; the 5 GiB ceiling becomes 10,000 × 16 MiB = 160 GiB headroom, well above S3's 5 TiB object cap. Pure helpers `should_multipart` + `plan_part_lengths`; a mocked-AWS happy-path test exercises the three multipart calls, and a separate abort-on-failure test pins the orphan-prevention invariant.
- **`cargo publish` runs from the release workflow.** A new `crates_io` job in `.github/workflows/release.yml` fires on the `release: published` event (not tag push) so the maintainer still reviews the draft GitHub Release before crates.io is updated — once published, crates.io can't be unpublished, so the human gate matters. Gated on the `CARGO_REGISTRY_TOKEN` secret; skipped on forks. Tag-push runs build the matrix + attach the draft release as before. Closes the recurring "forgot to `cargo publish`" gap that left 0.4.0 / 0.4.1 GitHub-only.

### Removed (breaking)
- **`webhook_url` removed.** The single-URL POST on Red transitions had been flagged as "too rigid for real ops workflows" in the BACKLOG; replacing it with a proper Slack block-kit integration would have been a whole new feature, while the audit log + `tracing::warn!` already carry the same data with timestamps. The `webhook_url = "…"` config key is now silently ignored (no warning — the parser drops unknown keys); the `:settings` form no longer surfaces the field; the `fire_webhook` curl shell-out and `build_webhook_payload` JSON encoder are gone. Replacement: a `stage=event kind=red_transition env=… application=… health=…` line written to `~/.cache/ebman/audit.log` for every Red transition, plus a structured `tracing::warn!` with env / application / health / region fields. Tail the audit log to drive a Slack / PagerDuty / pager notifier of your own.
- **`:minimap` removed.** The corner mini-map overlay (one coloured cell per env, driven by health) was operationally redundant — every signal it carried was already in the main table. `App.show_minimap`, the `:minimap on|off` command, and the `draw_minimap` renderer (50 lines) are gone. The setting is silently ignored if a saved state.toml has `show_minimap = true`.
- **Custom keybindings (`keys.toml`) removed.** `~/.config/ebman/keys.toml` parsed F1-F12 and uppercase-letter aliases to `:commands`, but the underlying need (discoverable per-key dispatch) is served by `Ctrl-K` palette + per-context help. The `src/keys.rs` module, the `App.custom_keys` field, the `lookup_custom_key` hook in `handle_event`, and the README's keys.toml example + storage-list entry are gone. If a `keys.toml` exists it's silently ignored.

### Added
- **`:envs-by-version LABEL`** — fleet-wide blast-radius lookup for a version label. Fans out across every `~/.aws/{config,credentials}` profile plus every `accounts.NAME` AssumeRole entry; filters by exact `version_label == LABEL` match. Each hit shows source / env / app / health / status. Operational use: bad build in prod, need to know everywhere it's deployed in one call. Reuses the existing `:org-health` / `:find-env` fan-out machinery.
- **`INST` column on the main env table** — shows `healthy/total` instance counts per env (e.g. `3/3`, `0/1`). Populated by a fan-out of `DescribeEnvironmentHealth` calls on every refresh tick (same shape as the existing Worker DLQ check; Terminated / Terminating / Launching envs skipped). Cell colour tiers by ratio: all healthy → green, partial → yellow, zero healthy with instances present → red, empty env → muted, no data yet → muted em-dash. Sits between `●` (health dot) and `TREND` in the default column order; honoured by the existing `:cols hide INST` machinery for operators who don't want it. Pure helpers `summarise_instance_health` (SDK summary → `(healthy, total)`) and `format_instance_counts` (counts → coloured text) with tests for every tier.
- **`:logs-insights [--window WINDOW] QUERY`** — CloudWatch Logs Insights query against the env's discovered log groups. `--window` accepts the same grammar as the DLQ replay prompt: `30m` / `1h` / `6h` / `24h` / `7d`; default is the last 1 hour. Multi-group is supported by Insights natively, so we pass every discovered group — no picker needed. Results land as a column-aligned `TextOverlay` with the per-row scan stats footer (`matched: N / scanned: M`) so the operator can see the cost of broad queries. Long values truncate at 60 chars with a `…` marker; the synthetic `@ptr` field is filtered out. Closes the real-EB-operator gap that `:logs-tail` doesn't (regex on a live stream isn't enough for "p99 latency for /checkout over the last 6h, grouped by path").

### Internal
- **Centralised overlay sizing.** Single `OverlaySize` enum (`Small` / `Picker` / `Text` / `Wide`) with a `centered_overlay(category, frame)` helper replaces 19 independent `centered_rect(W, H)` call sites. Size table lives in one place (`overlay_dims()`), so re-tuning every overlay is a one-line change. Visually the action confirm / picker / palette / form / log-tail / why-red / etc. now read with consistent proportions. Two tests pin the category invariants (size ordering, legal percent range).
- **Mocked-AWS coverage for previously-untested services.** Added request-shape mocked tests for `list_secrets` (Secrets Manager — field mapping + last-changed-desc sort), `list_certificates` (ACM — `Issued`-status filter + domain extraction), `list_org_accounts` (Organizations — Active-first then-by-name sort), and `fetch_env_costs` (Cost Explorer — Monthly granularity + UnblendedCost metric + tag-key prefix split). Pins the load-bearing field mappings against a future SDK rename. Added the `test-util` feature flag to `aws-sdk-acm` / `aws-sdk-organizations` / `aws-sdk-costexplorer` / `aws-sdk-secretsmanager` so the `mock_client!` macro can build against them.

### Fixed
- **`DescribeApplicationVersions` now paginates.** Orgs with hundreds of historical versions per app saw truncated `:versions` lists and broken `:rollback` against labels that fell past the first page. `list_application_versions` now loops on `next_token` matching the existing pattern. Mocked-AWS test `list_application_versions_pages_through_next_token` covers the two-page case.
- **Theme-correctness sweep — ~10 hardcoded `Color::Black` / `Color::White` foregrounds in `ui.rs` replaced with `theme.contrast_text(bg)`.** Filter chip, scope pill, group banner, Worker/Web tier pills, Ready / Updating / Terminating status pills, AUTO badge, and active-tab labels were rendering unreadable text on light + high-contrast themes (the corresponding `theme.title_alt` / `theme.title` / `theme.border_active` are dark colours in those themes). Pure rendering fix.

### Internal
- **`return Err(e).wrap_err_with(...)?` cleanup** in the multipart abort path — the `?` already short-circuits the function, so the explicit `return` was dead.
- **Existing deploy-from-path test migrates to a tempfile** so the same chain-of-stages assertions apply to the new streaming API.

### Test foundation
- 426 tests. New: `should_multipart_crosses_threshold`, `plan_part_lengths_exact_multiple` / `_partial_last_part` / `_zero_and_under_one_part`, `upload_bundle_uses_multipart_when_size_meets_threshold`, `upload_bundle_aborts_multipart_on_upload_part_failure`, `list_application_versions_pages_through_next_token`. Removed: `webhook_payload_escapes_quotes_and_backslashes` / `_handles_missing_account` (feature gone); 4 `keys.rs` parse tests (module gone).

## [0.6.0] — Interactive triage and form-driven config

User-facing: an interactive `:why` overlay (cursor + drill into every
section), three new modal forms over the long-tail config namespaces
(`:rds-attach` / `:listener-edit` / `:scaling-triggers`), a safe-ify
`:rds-detach`, time-windowed DLQ replay, a per-env runbook hint in
`:why`, and a STATUS pill that tells the truth about an env's health.
Internal: the form-submit path now safely round-trips pre-filled edit
forms so a blank field doesn't clobber an existing setting.

### Added
- **`:why` is interactive.** Cursor + drill across every section: ↑↓ / j k navigate the events / alarms / instances / deploys / queues / DLQ-peek rows; `Enter` drills in. Events, alarms, instances, and deploys pop open `Overlay::Describe` with formatted detail (full event text, alarm reason, instance health + causes, version description); queue / DLQ rows jump straight to the DLQ viewer (examine / `r` resend / `R` replay / `x` delete / `p` purge). `d` remains a shortcut for the DLQ drill. The overlay title now matches health — `why is X red?` for Red/Severe, `why is X amber?` for Yellow/Warning/Degraded, `X — recent activity` otherwise.
- **`R` in the DLQ viewer: time-windowed replay.** A spec prompt accepts `all`, a count (`20`), or a window (`30m` / `1h` / `24h` / `7d`); `spawn_dlq_replay_batch` sends each selected message to the main queue then deletes it from the DLQ, oldest-first, counting partial failures and refetching on completion. Scope-honest: "all" means all currently-peeked messages — SQS has no cheap full-queue enumeration, so a deep DLQ replays a page at a time.
- **`:rds-attach`** — modal form over `aws:rds:dbinstance` (engine / instance class / storage / master user+password / deletion policy / Multi-AZ). Pre-fills if a DB is already attached, so it doubles as an edit form: a field left blank is dropped from the update (see `Form::to_option_settings`), so editing the instance class without retyping the password is safe.
- **`:rds-detach ENV`** — typed-name confirm; sets `DBDeletionPolicy = Snapshot` so the database survives env termination (EB takes a final snapshot then). Honest about what it isn't: EB has no decoupling operation, and the command's help + toast say so.
- **`:listener-edit PORT`** — modal cert-rotation form for an ALB listener. A single MultiSelect loads live ACM certificates (new `aws-sdk-acm` dependency + `acm:ListCertificates`), pre-selected with the listener's current `SSLCertificateArns`; submit writes the new set through the option-settings path.
- **`:scaling-triggers`** — 9-field modal form over `aws:autoscaling:trigger`: metric / statistic / unit / period / breach duration / lower+upper thresholds / scale increments. Pre-fills with the env's current trigger; blank fields are kept as-is so the operator can tune one knob.
- **Per-env runbook hint.** `config.toml` `runbooks.ENV = "https://…"` lines parse into a map round-tripped through `Form::to_option_settings`; the `:why` overlay shows a bold `runbook <url>` line at the very top so the responder sees it first.
- **Stale-platform surfacing.** Envs running a superseded solution stack are flagged amber with an `↑` glyph in the `PLATFORM` column (and on the Detail Health tab). Driven by a one-shot `ListAvailableSolutionStacks` lookup; the comparison is precomputed in `rebuild_view` so the render path stays O(1) per row.

### Changed
- **`STATUS` pill colour-tiers by alert level.** `Ready` is EB's *operational* state ("no lifecycle op in flight"), not a health verdict, so the bright green pill on a Red-tinted row read as "everything fine" when it wasn't. New pure `status_alert(health, dlq)` returns `Red` (health Red/Severe), `Yellow` (health Yellow/Warning/Degraded or worker DLQ > 0), or `None`; `Ready` now renders in the health colour for the alerting tiers. `Updating` / `Terminating` are unchanged — they already carry their own strong signal.

### Internal
- **`Form::to_option_settings` drops empty text fields.** Previously only empty `allow_empty` integers were skipped; a blank text field would send an empty value to EB. Critical for pre-filled edit forms — EB redacts secrets like `DBPassword`, so they pre-fill blank and would be clobbered without this. A genuinely-required field left blank is still caught: EB rejects with a clear "option required". Fixes a real footgun in the `:rds-attach` edit path.
- Drill-in Describe text no longer duplicates the close hint — `draw_describe`'s title already says it.

### Test foundation
- 425 tests. New coverage: DLQ replay parsing + index selection (+5), `runbooks.ENV` config parse + serialize round-trip (+2), the empty-text form skip (+1), `why_overlay_title` health framing (+1), `status_alert` tiering (+1).

## [0.5.0] — Rollback, config drift, and a structural cleanup

The user-facing additions are config-drift and rollback tooling
(`:rollback`, `:config-diff`, `:changes`) plus a stale-platform nag and
an animated splash. Under the hood, a large decomposition of `app.rs` —
the generation guard, the message-handling layer, the spawn helpers, and
the `App` struct itself.

### Added
- **`:rollback`** — redeploys the environment's previously-deployed version label. Scans the env's recent events for the prior label, then opens the standard deploy-confirm modal so the operator sees + confirms the target and the 5-second undo window applies.
- **`:config-diff ENV`** — compares the selected environment's operator-set option settings against `ENV`'s, listing every namespace/key whose value differs (the two configs are fetched in parallel; `unset` is normalised so absent-vs-empty doesn't read as a diff).
- **`:changes`** — a newest-first deploy + configuration-change timeline for the selected env, drawn from `DescribeEvents` with routine health/scaling noise filtered out.
- **Config-tab key rename** — `r` on the Config tab edits a tag / env-var *key* in place; commit dispatches set-new + remove-old in one `UpdateOptionSettings` / `UpdateTags` call, carrying the value across.
- **Stale-platform surfacing** — environments running a superseded solution stack are flagged amber with an `↑` glyph in the `PLATFORM` column (and on the Detail Health tab), driven by a `ListAvailableSolutionStacks` lookup. The AWS console nags about this; ebman was previously silent.
- **Animated splash screen** — an 8-bit pixel-art scene (an angry giant chomping a beanstalk, four-frame animation) replaces the plain boot banner, and also appears on the About screen. The layout degrades gracefully on small terminals.

### Changed
- **Detail footer key strip** — restructured lazygit-style: keys render bold/bright against muted labels with a thin `·` separator, and the global keys (`tab` / `?` / `esc`) are appended consistently on every tab instead of being listed ad-hoc per tab.

### Internal
- **`app.rs` decomposition.** The stale-result generation guard is centralised — `AppMsg::generation()` is checked once in `handle_msg` instead of 39 hand-copied `if gen != self.generation` blocks. The ~1,140-line `handle_msg` is split into a thin router plus one `handle_*` method per variant in a new `app/msg.rs`. A generic `spawn_aws` helper absorbs the clone / spawn / map-err / send boilerplate of 23 single-call spawn helpers. 16 `App` fields are grouped into three sub-structs (`CompletionState`, `HelpState`, `EventPanel`).
- Stale-platform staleness is precomputed in `rebuild_view` (cached `env → newer version`) rather than re-parsed per row per frame.

### Test foundation
- 415 tests. New coverage: solution-stack family/version parsing + the stale comparison, the Detail key-strip builder, `AppMsg::generation()`, the `:rollback` previous-version scan, and the `:config-diff` namespace comparison.

## [0.4.1] — Config-editor polish

Two fixes from a post-0.4.0 code review of the Config-tab editor.

### Fixed
- **Config cursor no longer strands past the end of the list.** Deleting the last tag / env-var row left `config_cursor` pointing past the (now shorter) list, so the `▶` marker vanished until the next `j`/`k`. `DetailState::clamp_config_cursor` is now called when a tags / env-vars refetch lands, pulling the cursor back into range.
- **`:secret` now audit-logs completion.** It logged only `stage=dispatched`; the spawned fetch task now also writes a `stage=completed action=GetSecretValue … ok|err` line, matching the dispatched/completed pairing of the write paths.

### Also
- The Config-tab add-a-row editor scroll-follows correctly — on a long tag/env-var list the new-row line is brought into view instead of being typed blind below the fold. (Shipped just after 0.4.0; folded in here.)

## [0.4.0] — Console-parity push: discovery, diagnosis, and in-place config editing

A large feature release built on top of 0.3.5. The throughline is
closing the gap with the AWS console: surfacing the configuration
vocabulary operators didn't know to ask for, diagnosing failures
ebman previously just reported, and — the headline — editing an
environment's config without leaving the TUI.

### Added — discovery & diagnosis
- **`:options [NAMESPACE]`** — the full settable-option vocabulary for the env's platform, with current value / default / type / constraints / enum choices, grouped by namespace. Closes the biggest config-discoverability gap vs. the console.
- **`:explain`** — diagnoses the last `AccessDenied` via `iam:SimulatePrincipalPolicy` (`:explain ARN ACTION` for explicit pairs). Surfaces the matched/missing statement and flags SCP / permissions-boundary blockers.
- **`:resources`** — environment resources as an indented ASG → instances → ELB → target-group tree (was a flat dump).
- **`:listeners`** / **`:rds`** — read-only overlays for the env's ALB listener and RDS dbinstance option settings (`DBPassword` always redacted).
- **`:secrets [FILTER]`** / **`:secret NAME`** — region-scoped Secrets Manager browser. `:secrets` lists metadata only (never values); `:secret` is the opt-in value reveal, JSON pretty-printed, redaction-aware, CloudTrail- + audit-logged.
- **`:apps-info`** — app metadata overlay (description / created / updated / template count / env list).
- **`:cost on|off`** — opt-in COST column ($/month per env via Cost Explorer; 24h on-disk cache).
- **`:report-bug`** — scrubbed bug-report overlay (no outbound HTTP; PII redacted; copy-to-clipboard or pre-filled GitHub issue).

### Added — config editing
- **`:env-edit`** — bulk env-var editor: opens the current vars in `$EDITOR` as `KEY=VALUE`, diffs on save, dispatches the delta.
- **Editable Config tab** — `j`/`k`/arrows move a cursor over the tags + env-var rows; `enter` edits a value in place (full text-field caret: Left/Right/Home/End, Backspace/Delete); `n` adds a new row (`KEY=VALUE`); `x` deletes the selected row (with `y` confirm). All changes dispatch through the existing `UpdateOptionSettings` / `UpdateTags` paths — audit log, in-flight pill, and auto-refetch included. The body scroll-follows the cursor.

### Added — events & ergonomics
- **`:event-time` / `T`** — event timestamps switchable between UTC (default), local, and relative age; persists.
- **Events-tab filters** — `L` cycles a minimum-severity floor (`all → info+ → warn+ → error`); `w` cycles a time window (`all → 1h → 6h → 24h → 7d`). Events-tab scrolling is now clamped so `j`/`k` can't run off the bottom.
- **`:` Tab autocompletion** — Tab / Shift-Tab cycle command-registry matches in the command bar.
- **"Did you mean?"** — Levenshtein suggestion on an unknown command.
- **First-run nudge** — a one-time footer hint (`?` / `:` / `Ctrl-K`) when no `state.toml` exists yet.
- **Apps scope** — `space` multi-select, `*` pin (pinned apps sort to the top, persisted).

### Changed
- The 5-second undo window now also covers the batch operations (`:batch-rebuild` / `:batch-deploy` / `:batch-tag` / …), not just single-env confirms.

### Test foundation
- 309 → 392 tests. New coverage spans the option-vocabulary merge, IAM-simulation rendering, the env-var diff, secret scrubbing + JSON pretty-printer, event filters, and the Config-tab editor (cursor, caret, `KEY=VALUE` parse, scroll-follow).

## [0.3.5] — Undo: 5-second cancel window after action dispatch

Safety feature called out by the v0.3.0 UX review. After authorising
an action (Y on Y/N confirm, or typing the env name on a typed-name
confirm), the AWS call no longer fires immediately — ebman holds the
dispatch for 5 seconds in a cancel window. Pressing `U` in Normal
mode aborts before the deadline. After the deadline, the SDK call
goes out as before.

### Added
- **`U` keybind in Normal mode** — aborts the pending dispatch within the cancel window. Capital U so it can't be hit accidentally (lowercase `u` is unbound).
- **Pending-dispatch pill** in the header chain — red bg, format `Rebuild env Ns — U undo`. Re-renders every 100ms via the existing `anim` ticker so the countdown is smooth.
- **`PendingDispatch` struct + `pending_dispatch: Option<PendingDispatch>` field** on `App`. Holds the captured `ConfirmModal` + deadline `Instant` + cached action / env labels for the toast.
- **Audit-log line for undone actions** — `stage=undone action=… target=…`. So an operator who aborted mid-incident can find the breadcrumb later.

### Changed
- **Y / TypeName confirm path no longer calls `spawn_action` directly** — routes through `queue_action_dispatch` which closes the action flow and queues the modal. `tick_pending_dispatch` (called from the main loop) fires `spawn_action` when the deadline passes.
- **`anim` ticker gate** extended to wake when `pending_dispatch.is_some()` so the per-frame countdown stays accurate even with the operator idle.
- **Only one pending dispatch at a time**. A second Y-confirm while one is mid-dispatch errors out with "press U to undo or wait".

### Removed
- **`ActionFlow::Running` variant** + its render code. Used to show a "dispatching…" modal between Y-confirm and the SDK call landing. The pending-dispatch pill subsumes the signal; no operator-facing change but ~30 lines of UI deleted.

### Test foundation
- 5 new tests (`queue_action_dispatch_holds_action_for_cancel_window`, `cancel_pending_dispatch_clears_field_and_emits_status`, `second_queue_attempt_errors_while_first_pending`, `tick_pending_dispatch_fires_after_deadline`, `capital_u_cancels_pending_dispatch_in_normal_mode`). 304 → 309 total.

## [0.3.4] — Terminology sweep: env vs environment

Pure-text sweep — no behaviour changes. The v0.3.0 UX review flagged
30+ user-facing strings mixing `env` and `environment` inconsistently
(pills + scope tab + `:env` command all use `env`; error messages,
confirm-modal copy, and the action menu used `environment`). This
commit picks `env` as canonical and sweeps every user-facing string.

### Changed
- **Error / status messages** — `"no environment selected"` → `"no env selected"` (32 sites), plus `"no environments in this account / region"` → `"no envs in this account / region"`, `"no environments match the active view"` → `"no envs match the active view"`, etc.
- **Confirm modal copy** — `"Rebuild environment 'X'?"` → `"Rebuild env 'X'?"`; same for `"TERMINATE environment 'X'..."`, `"Restart app server on environment 'X'?"`, `"Deploy version 'Y' to environment 'X'?"`.
- **Action menu labels** (`Action::label()`) — `"Rebuild environment"` → `"Rebuild env"`, `"Terminate environment"` → `"Terminate env"`, `"Clone environment"` → `"Clone env"`.
- **Detail-view header** — `"environment: {name}"` → `"env: {name}"`.
- **Empty-state hints** — `"no DLQ for this environment"` → `"no DLQ for this env"`, `"no CloudWatch alarms reference this environment"` → `"no CloudWatch alarms reference this env"`, etc.

### Kept as-is
- **AWS namespace strings** (`aws:elasticbeanstalk:environment`, `aws:elasticbeanstalk:application:environment`) — these are EB API identifiers, not display copy.
- **AWS console URL paths** (`...environment/dashboard?environmentName=...`).
- **AWS CLI command examples** (`aws elasticbeanstalk describe-environments --environment-names ...`) — the CLI itself expects that spelling.
- **AWS SDK type names / function names** (`Environment`, `list_environments`, `environmentnotfound` classifier token).
- **AWS event-message test fixtures** (`"Updating environment to use version label 'build-142'."`, `"Adding instance 'i-abc123' to environment."`) — these are literal AWS API outputs ebman parses, must stay verbatim.

No new tests; the existing 304 still pass.

## [0.3.3] — Apps scope as a real surface

Builds out the previously-half-finished Apps scope that the v0.3.0 UX
review flagged as undersold. `Tab` into Apps view now has its own
operational rollup, its own action menu, and its own browser-open
keybinding — the scope is a real mental model now, not a vestigial
table.

### Added
- **Apps table rollup columns** — ENVS / RED / UPDATING replace the previous CREATED column. Counts walk the live env list every refresh; RED merges EB-reported Red health with worker-DLQ-depth alerts so an env where the DLQ is filling up but EB still calls it healthy counts as alerting. Red / Updating cells render bold in the corresponding severity colour when non-zero.
- **Apps-scope `a` opens a per-app action menu**. Five items (`Drill into envs` / `Rebuild all N envs` / `Restart all N envs` / `Deploy version label to all N envs` / `Open application in AWS console`); j/k navigates; Enter dispatches; esc closes. Batch actions seed `multi_selected` with the app's envs and route through the existing `cmd_batch_*` helpers so audit log + pending pill + toasts all work the same way the env-scope batch ops do.
- **Apps-scope `b`** — opens the EB applications-page console URL in the browser. Mirrors envs-scope `b` but for the application overview, not a specific env.
- **Help screen "Apps-scope keys" section** — documents `enter` / `a` / `b` / `j-k-g-G` so the keys aren't discovered by accident.
- **`app_rollup(envs, app_name, dlq_depths)`** pure helper — testable rollup of env count / red count / updating count / worker-DLQ alerts. Three new tests covering happy path, worker-DLQ-only alerting, and the empty / unknown-app case.

### Changed
- **Apps table CREATED column dropped** to make room for ENVS / RED / UPDATING. `Application::date_created` field kept (still populated by the SDK conversion) but marked `#[allow(dead_code)]` — re-surface in a future `:apps-info` overlay if an operator asks.

### Test foundation
- 3 new tests (`app_rollup_*`). 301 → 304 total.

## [0.3.2] — Command registry

Internal-only refactor; no user-visible behaviour changes. Pulls the
`:command` metadata into a single `src/commands.rs` registry so the
palette (`Ctrl-K`), the global help screen (`?`), and the plugin-collision
detector all read from the same list. Adding a new command now means
one entry in `commands.rs` plus the dispatch arm — the help and palette
update automatically.

### Changed
- **New `src/commands.rs` module** with `pub const COMMANDS: &[CommandSpec]` carrying every built-in `:command`'s name, aliases, one-line help, category, and palette behaviour (`ZeroArg` vs `Prefill`).
- **`build_palette_items`** (palette source) iterates `COMMANDS` instead of the two hand-maintained `zero_arg_cmds` / `prefill_cmds` lists. ~150 lines deleted from `app.rs`.
- **`draw_help`** (global `?` screen) iterates `COMMANDS` grouped by `Category::ORDER` instead of the 100+ hand-written `help_line(...)` calls. ~100 lines deleted from `ui.rs`.
- **`app::BUILTIN_COMMANDS`** const replaced with `app::builtin_commands()` fn that flattens `COMMANDS` (names + aliases). Same callers, same data, single source.

### Test foundation
- **`commands::tests::registry_covers_every_dispatch_arm`** — parses `app.rs::execute_command` at test time and asserts every `"name" =>` arm appears in `COMMANDS` (as name or alias). Fails fast if someone adds a dispatch arm without a registry entry.
- **`commands::tests::every_registry_name_has_a_dispatch_arm`** — reverse direction. Fails if a registry entry has no real dispatch.
- **`commands::tests::every_name_is_unique`** — no duplicate name/alias collisions across the registry.
- **`commands::tests::every_command_has_nonempty_help`** — every entry has a one-line description.

Five new tests, 296 → 301 total. The agent's "three sources of truth for what `?` should say" pattern of concern from the v0.3.0 UX review — keybindings in `app.rs`, dispatch in `cmd_*.rs`, help in `ui.rs` — is now a single source of truth backed by CI.

## [0.3.1] — UX punch list

Twelve UX fixes + supporting plumbing surfaced by a critical post-0.3.0
review. No new features; everything addresses rough edges that operators
actually hit.

### Added
- **Multi-select active pill** in the header chain — persistent `▶ N selected` while `multi_selected` is non-empty. Replaces the one-tick status-message hint that disappeared on the next refresh.
- **`Action::Capacity` in the `a` action menu** — `Capacity (min/max/instance/cooldown)` entry that opens the modal form. Previously command-bar-only (`:capacity`).
- **Detail-Health tab now renders alarms + recent deploys** to match `:why` — the default Detail-view landing tab was missing two of the four sections the `:why` overlay showed. Triage surfaces no longer disagree.

### Changed
- **`Esc` clears multi-select in Normal mode** — the status message after `space`-selecting has always said "esc = clear", but Normal mode had no Esc handler. Silent footgun for operators who multi-selected and walked away. Now actually clears.
- **`:swap TARGET` routes through `open_parameterised_action`** — was building `ActionFlow::Confirm` directly with `loading_dryrun: false`, skipping the impact-preview + last-3-events preflight that `a → Swap` runs.
- **`Action::wants_preflight()`** in `mode_action.rs` is now the single source of truth for the "show preflight" decision. Replaces three duplicated allow-lists (`open_parameterised_action`, `advance_action_flow::Terminate`, `advance_action_flow::Rebuild`).
- **Pill foreground colours go through `Theme::contrast_text(bg)`** — WCAG-luminance black/white picker. The chain hardcoded `Color::Black` (with one `Color::White` outlier for alerts) which broke light + high-contrast themes. Now readable on every theme.
- **Pill chain elides under width pressure** — `prune_pills_to_width` drops trailing low-priority pills and marks the survivor with `+N` so elision isn't silent. Pill order: alerts > pending > multi-select > read-only > update > SSO > frozen > redact > grouped > view-mode.
- **ASCII glyph fallbacks** for `⚠` / `💡` / `▎` / `⏳` / `▶` (twelve sites across form errors, footer hints, toast stripe, traffic warnings, DLQ confirm, tag policy, header pills). `icons = "ascii"` no longer renders box-tofu.
- **`FROZEN` pill turns yellow + reads `FROZEN (stale)` after 5 min staleness** — frozen auto-refresh during an incident is operationally important to not forget.
- **Global help (`?`) gained ~40 commands across 7 new sections** (Multi-account, Lifecycle actions, Env config, Versions/configs/alarms/platforms, Bulk ops, Setup/discovery, Detail-tab keys). Was stale by half the v0.3.0 surface — the footer was advertising `:help` but help didn't know about `:why`, `:capacity`, `:account`, `:accounts`, `:find-env`, `:org-health`, `:settings`, `:about`, `:update`, `:metric`, `:notify`, `:managed-window`, `:logs-stream`, `:set-option`, `:tag`, `:env`, etc.
- **Context-aware footer hint** at `app.alerts > 0` points at `:why` (v0.3.0 triage tool) instead of the stale `:alarms` / `:org-health`.
- **`flatten_err_to_string` classifies AccessDenied / NotFound / Conflict / ExpiredToken** SDK errors with clean prefixes. Was throttling-only; operators bouncing profiles hit AccessDenied constantly — clean prefix instead of a buried SDK Debug chain.

### Fixed
- **Empty-state hint at "no envs match the active view"** referenced a nonexistent `views` keybind; corrected to `:views` command.
- **`tracing::info!` on `persist_state` downgraded to `debug!`** — fires every state-changing keystroke; INFO-level telemetry on each one was too noisy in `~/.cache/ebman/ebman.log`.

### Test foundation
- 14 new tests covering: `prune_pills_to_width` (3), the ASCII glyph helpers (3), `Theme::contrast_text` luminance picker (3), `flatten_err_to_string` error-code classifiers (5). 282 → 296 total.

## [0.3.0] — Red-env triage, multi-account, bulk ops, default Health tab

### Added
- **Red-env diagnostic overlay** — `:why` (or `!` on any env row) opens a four-section overlay aggregating recent events, alarms, instance health, and recent deploys for the selected env. Worker envs also get a main + DLQ peek so an env that's Red because the DLQ is filling up surfaces the queue depth + most-recent message metadata without bouncing through the Queue tab.
- **Health tab as default Detail landing** — `Enter` on an env now opens to a Health rollup tab summarising the same data as `:why`, with `j`/`k` to walk items and `Enter` to drill into the source tab (Events / Instances / Queue). Existing tabs (Events / Instances / Metrics / Queue / Logs / Config) all still reachable via `Tab` / `Shift-Tab`.
- **Updating-state classification** — `Updating` envs are now labelled with the kind of update in flight (deploy / config / scale) so an in-flight deploy is visually distinct from a routine option-settings push. Alert-aware Ready pill: an env with active CW alarms no longer renders as a plain green "Ready".
- **Worker DLQ feeds Red alerts** — Worker envs go Red when their DLQ depth > 0 even if EB itself reports the env healthy.
- **AssumeRole account switcher** — `accounts.NAME.role_arn` entries in `config.toml` define cross-account targets; `:account NAME` switches via `sts:AssumeRole` from a base profile. Fresh `SdkConfig` carries only the assumed-role identity so source-profile creds never leak into request signing. `:account NAME` falls back to `:profile NAME` aliasing when the name has no `accounts.` entry.
- **AWS Organizations discovery** — `:accounts` overlay calls `organizations:ListAccounts` against the active profile and lists child accounts; rows with a matching `accounts.NAME` entry get a `:account NAME` switch hint.
- **Multi-account `:org-health`** — fans across configured AWS profiles **and** AssumeRole accounts in parallel; aggregated env / red counts per identity.
- **Cross-account `:find-env`** — substring scan across every profile in `~/.aws/{config,credentials}` plus every configured AssumeRole account; hits in AssumeRole accounts are annotated.
- **Bulk write commands** — `space` multi-select now drives more than `:batch-rebuild` / `:batch-restart`: added `:batch-deploy LABEL`, `:batch-tag KEY VALUE`, `:batch-untag KEY`, `:batch-set-option NAMESPACE OPTION VALUE`. Each fans out in parallel with per-env audit + pending pill rows.
- **Deploy preview** — `:deploy LABEL --preview` opens a side-by-side overlay of the currently-deployed version vs the candidate (label, description, S3 source, timestamp), with a rollback hint and traffic warning when the env is mid-deploy or recently changed.
- **`:capacity` form** — one modal that edits Min / Max / Instance type / Cooldown in a single shot, pre-filled from `DescribeConfigurationSettings`.
- **Per-profile theme override** — `profile_themes = "prod:high-contrast,staging:dark"` in `config.toml` pins a theme per AWS profile so a glance at the screen says "you're in prod" without reading the breadcrumb.
- **`:about` / `:credits` overlay** — version, license, attributions.
- **`:update`** — surfaces (and yanks) the right upgrade command for the install channel that's live (Homebrew cellar / cargo-bin / tarball).
- **Apps scope** — `Tab` / `Shift-Tab` cycle the main table between Envs and Apps view; Apps view shows per-application rollup (env count, red count, latest version, latest activity).
- **Relative refresh time in header** — header age says "12s ago" instead of an absolute timestamp.
- **Age-column tinting** — AGE cell colour-graded by bucket so stale envs (>7d) stand out.

### Changed
- **`execute_command` refactor** — the previously-monolithic dispatch match in `src/app.rs` is now split across ten category sub-modules (`src/app/cmd_*.rs`) totalling 2,160 lines; `app.rs` shrank by ~1,800 lines. Dispatch site is pure one-liner routing. No operator-facing behaviour changed.
- **AWS error context preserved across the chain** — every `map_err(|e| eyre!(...))` site was migrated to `wrap_err(...)` so the SDK's `ProvideErrorMetadata` chain (`ThrottlingException` codes, etc.) reaches `is_throttling_error` and the SSO-login hint correctly. Throttling-detection test added.
- **`tracing::info!` → `tracing::debug!`** on `persist_state` — state writes happen on every meaningful interaction; INFO-level telemetry on each one was too noisy in `~/.cache/ebman/ebman.log`. Bump `RUST_LOG=debug` to recover.
- **Splash screen** — Powerline mode gets a cloud icon + tagline; version chip rendered as a Nerd-Font tab pill; the N glyph in the logo gets proper diagonal wedges.
- **Header pill chain** — refresh / account / region / profile pills resized + colour-graded against the active theme.
- **Lazy `app-versions` fetch** — `:versions` and the deploy-version dropdown only call `DescribeApplicationVersions` when the operator asks; startup no longer pays for it.
- **Logs tab `:logs-tail`** — when the env has multiple log groups and `:logs-tail` is called without an explicit group, a picker opens instead of silently auto-selecting.

### Test foundation
- **UI integration test harness** — `App::for_tests(aws, cfg)` + `AwsClient::stub()` give synchronous, no-network, no-disk construction of the full `App` state so UI behaviour can be exercised at the keystroke level. 282 tests pass against the harness + mocked AWS layer.
- **CW metric batching test** — covers the `GetMetricData` request shape + multi-series response demuxing.
- **Mocked-AWS error-path coverage** — every spawn_* helper now has at least one error-path test asserting the error label that reaches `AppMsg::*Result` callers.



### Added
- **`:settings` modal form** to edit `~/.config/ebman/config.toml` interactively. Pre-fills from the live config; submit writes the file back and live-applies what it can (theme, icons, refresh interval). Fields: theme, icons, refresh interval, redact-by-default, group-by-app-by-default, notification bell, required tags, extra regions, webhook URL.
- **`icons = "auto"` config value** that probes the terminal at startup with a one-cell Powerline triangle (`U+E0B0`) and picks `powerline` if the font renders it in a single cell, `unicode` otherwise. The probe runs before the alt-screen is entered, so the glyph never reaches user scrollback.
- **`:subnets` / `:elb-subnets` / `:security-groups` MultiSelect pickers** — `FieldKind::MultiSelect` in the modal-form abstraction. Pre-fills with the env's current selection, lists available subnets / SGs from the env's VPC, submits via the shared option-settings update path.
- **Action-menu glyphs** — each entry in `:a` leads with an icon-style-aware glyph (Nerd Font / unicode / single-letter ascii).
- **Group-banner sub-totals** — `xi · 3 envs · 2 web · 1 worker · 1 red` summary in the APPLICATION column when grouped.
- **Newly-added env marker** — transient `+` glyph on the NAME cell for envs that first appeared on the current refresh.
- **Health-transition pulse** — rightmost sparkline cell pulses BOLD + SLOW_BLINK on a Red transition.
- **Pending pill inline summary** — `⏳ rebuild ×2, deploy` instead of `⏳ 3`.
- **Context-aware footer hints** — `💡` suggests `:alarms` / `:pending` / `aws sso login` etc. when the status slot is empty.
- **Form-field validation marker** — invalid fields show a trailing `✗` next to the value.
- **Confirm-modal env highlight** — destructive confirms render the env name in a red chip.

### Changed
- **Powerline lead-in arrow** now uses `U+E0B2` (left-pointing) so the pill's coloured base sits flush, mirroring the trailing `U+E0B0`.
- **Theme-correct colours** — every footer / kv() / DLQ overlay / action menu / confirm modal / Detail tab / help screen foreground now resolves through the theme; ~100 hardcoded `Color::Yellow/Cyan/Gray/Red/White` removed.
- **Splash screen** stays for a minimum 3 s.
- **Cursor marker** swaps to `U+E0B0` in Powerline mode.
- **Caret glyph** swaps `_` to `U+258E` (thin vertical block) in unicode + Powerline modes.
- **Toast notifications** gained a severity glyph in title + body and a chunky `▎` accent stripe on the left edge.

### Fixed
- **Region persistence on restart** — `persist_state` now writes the operator's intent (`override_region`) with `context.region` as a fallback, and runs from `main.rs` after `run()` regardless of Ok / Err so a cargo-watch SIGTERM-driven shutdown can't drop the latest state.
- **Loading-indicator flicker** — once the `loading…` indicator becomes visible, it stays for at least 500 ms. The rendering slot is fixed-width so line 2 no longer jumps horizontally on transitions.

### Test foundation
- **`aws-smithy-mocks`** wired into the test build. `AwsClient::for_tests` constructor; 9 mocked-AWS tests covering the worker-queue auto-create regression, peek_messages loop + dedupe, EC2 subnet / SG listing, VPC-context pre-fill, and the `update_env_option_settings` write path (request shape, empty-input guard, error propagation).

## [0.1.1] — post-release fixes

### Fixed
- **Wedged terminal after cargo-watch restart (or any `kill <pid>`)**. ebman now traps SIGINT / SIGTERM / SIGHUP and runs the normal cleanup path (`leave_tui`) before exiting, instead of being killed abruptly by the default OS handler with raw mode + alt-screen still set. Previously the parent shell ended up with broken `\r\n` translation and only `pkill` from another terminal could unstick it.
- **Action menu and Y/N confirm modal now accept `q` to cancel**, not just `Esc`. Brings them in line with every other overlay; `TypeName` confirms (Terminate) and the SwapTarget filter intentionally stay `Esc`-only since `q` can appear in user-typed input.

### Changed
- **Instances tab cursor row gets a full-row background highlight**, matching the main env table instead of only marking the row with `▶` + an ID colour change.
- **Detail mode now has per-tab footer key strips.** Each tab advertises its real keys (Instances: `s ssm shell · i info · y yank · x terminate · b … in browser`; Metrics: `[ / ] range`; Logs: `^R snapshot · s live-stream`; etc.) instead of the generic one-size-fits-all line.
- **Enter on an instance opens a non-intrusive info overlay** (id / type / AZ / health / causes / launched + uptime). The previous behaviour — launching the EC2 console in a browser — moved to `b`. `i` is also wired as an Enter alias for symmetry with other modes.

## [0.1.0] — first public release

Initial public release. Headline surface:

### Listing & inspection
- Live environment table with sort / filter / group-by-app, health sparkline (auto-labelled trend window), severity tints, mouse support.
- Per-env drill-down: Events (regex search), Instances (with health causes + embedded SSM shell), Metrics (CloudWatch with custom-metric overlay), Queue (Worker tier; main + DLQ), Logs (snapshot or live CW Logs tail), Config (tags, env vars, cost estimate).
- DLQ viewer with peek / resend / strict-typed purge / bulk delete.
- Side-by-side env diff (`:diff`).
- CloudWatch alarms list (`:alarms`).
- Application versions list + delete (`:versions`, `:delete-version`).
- Saved-configurations interactive overlay (`:saved-configs`) with apply / inspect / delete / create-from-env.
- Custom platforms list + delete.
- Cross-account search (`:find-env`) and org-wide health (`:org-health`).

### Write surface
- Action menu: Rebuild / Restart / Swap CNAMEs / Terminate / Deploy / Upgrade platform / Clone / Scale / Abort.
- Deploy from local zip or `s3://` source (`:deploy --from PATH | s3://bucket/key`).
- Env-var editor (`:env list|set|unset`).
- Tag editor (`:tag` / `:untag`).
- CloudWatch alarms create / delete (`:alarm-create`, `:alarm-delete`).
- Per-option commands: `:notify`, `:managed-window`, `:logs-stream`, `:instance-type`, `:keypair`, `:service-role`, `:instance-profile`, `:public-ip`, `:elb-scheme`, `:deployment-policy`, `:rolling-update`, `:health-check-url`.
- Generic option-settings escape hatch: `:set-option NAMESPACE OPTION VALUE` / `:unset-option`.

### Safety
- `--read-only` CLI flag + `:readonly on|off`.
- Strict-typed confirms for Terminate and DLQ purge.
- Pre-flight dry-run + last-3-events preview in confirm modals.
- Traffic warnings for active-deploy / recent-change / currently-Red envs.
- Audit log (`~/.cache/ebman/audit.log`) with rotation.
- Crash reports (`~/.cache/ebman/crash-*.log`) with 30-day TTL.

### Power-user
- Fuzzy command palette (`Ctrl-K`).
- Named filters, saved views, custom keybindings (`keys.toml`), plugin commands (`commands.toml`).
- Local env aliases (`:alias NAME LABEL`).
- Saved configurations as a structured overlay.
- Multi-select + batch actions (`:batch-rebuild`, `:batch-restart`).
- TSV / JSON / Markdown export of the filtered view.

### Multi-account / multi-region
- `:region all` fans across configured regions in parallel.
- Cross-profile env search (`:find-env`) and org health (`:org-health`).

### Headless / scriptable
- `--control-socket PATH` exposes a Unix-socket interface (key / cmd / screen / state).
- `ebman ctl <op>` one-shot client.
- Non-interactive CLI: `ebman envs [--json]`, `ebman action rebuild --env NAME`.

### Distribution
- GitHub Actions CI (Linux + macOS, fmt / clippy / MSRV gate).
- Release workflow attaches binaries for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` to draft GH releases on `v*` tags.
- Published to crates.io as `ebman`.
- Homebrew tap at `tombaldwin/homebrew-tap`.

[Unreleased]: https://github.com/tombaldwin/ebman/compare/v0.17.0...HEAD
[0.17.0]: https://github.com/tombaldwin/ebman/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/tombaldwin/ebman/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/tombaldwin/ebman/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/tombaldwin/ebman/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/tombaldwin/ebman/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/tombaldwin/ebman/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/tombaldwin/ebman/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/tombaldwin/ebman/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/tombaldwin/ebman/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/tombaldwin/ebman/compare/v0.8.1...v0.9.0
[0.3.5]: https://github.com/tombaldwin/ebman/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/tombaldwin/ebman/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/tombaldwin/ebman/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/tombaldwin/ebman/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/tombaldwin/ebman/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/tombaldwin/ebman/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tombaldwin/ebman/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tombaldwin/ebman/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tombaldwin/ebman/releases/tag/v0.1.0
