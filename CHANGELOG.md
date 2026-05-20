# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/tombaldwin/ebman/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/tombaldwin/ebman/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tombaldwin/ebman/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tombaldwin/ebman/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tombaldwin/ebman/releases/tag/v0.1.0
