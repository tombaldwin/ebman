# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/tombaldwin/ebman/compare/v0.3.5...HEAD
[0.3.5]: https://github.com/tombaldwin/ebman/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/tombaldwin/ebman/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/tombaldwin/ebman/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/tombaldwin/ebman/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/tombaldwin/ebman/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/tombaldwin/ebman/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tombaldwin/ebman/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tombaldwin/ebman/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tombaldwin/ebman/releases/tag/v0.1.0
