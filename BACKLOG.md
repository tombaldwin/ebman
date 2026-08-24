# ebman backlog

Living list of done / pending / dropped work. New entries get added at the bottom of their section. Priority tiers below are loose — pick what fits.

---

## Done

### Core foundation
- Listing & live refresh: `DescribeEnvironments` with pagination, every 15 s
- Manual refresh (`Ctrl-R` / `F5`) with debounced loading indicator
- Generation counter so stale results from previous contexts are dropped
- Crash-safe panic hook that restores the terminal
- File logging to `~/.cache/ebman/ebman.log` with `RUST_LOG` env support
- Boot splash while STS resolves

### Identity & context
- `Account`, `Region`, `Profile`, `Caller` shown in header
- `STS GetCallerIdentity` runs async after rebuild (off the hot path)
- In-app profile picker (`p`) — parses `~/.aws/config` + `~/.aws/credentials`
- In-app region picker (`r`) — 30 commercial regions + user-defined extras
- `:region NAME` / `:profile NAME` command-bar entries
- SSO-friendly hint: rewrites `ExpiredToken` errors into `aws sso login --profile X` instructions
- Strict-validate profile/region on startup; falls back to env defaults if invalid

### Table view
- Columns: NAME / APPLICATION / TIER / STATUS / ● / TREND / PLATFORM / VERSION / CNAME / AGE
- Sort cycling (`s` / `S`) — order matches column order in the UI
- Filter mode (`/`) — case-insensitive, multi-field
- Group-by-application (`Ctrl-G`) — coloured horizontal partitions between groups
- Stable, sequential per-app colour assignment from a 16-colour palette
- Zebra striping that counts Env rows only (separators don't break the cadence)
- Severity row tint (Red / Yellow envs get tinted backgrounds)
- Mouse: wheel scroll, left-click to select, hover tints the row
- In-memory health sparkline per env (last 20 samples; oldest 1/3 dim)
- Status / tier rendered as coloured "pills"; health rendered as `●` dot (or `*` in ASCII)
- View-mode toggle (`Ctrl-D`) cycles default / compact

### Drill-down
- `Enter` opens a per-env Detail view; `Esc` returns
- Tabs: **Events / Instances / Metrics / Queue (Worker only) / Config**
- `Tab`/`Shift-Tab` (or `h`/`l`) cycle tabs; refresh on switch
- Events tab: `DescribeEvents` filtered by env, regex search (`/`, `n`, `N`), match highlighting
- Instances tab: `DescribeInstancesHealth` with all attributes; cause lines indented
- Metrics tab: real `Chart` widget with braille markers; `EnvHealth / 4xx / 5xx / LatencyP90`; per-series `now / max / min / Δ` with direction-of-bad colouring
- Queue tab: queue URL + visible/in-flight/delayed with sub-character micro-bars
- Config tab: env metadata, no extra API call
- `Ctrl-R` re-fetches active tab

### Worker / SQS
- Tier badge (Web/Worker) parsed from `EnvironmentDescription.tier`
- Platform-family parser handles both solution-stack and platform-arn formats
- Worker queue URL discovery via `DescribeConfigurationSettings`
- DLQ URL derivation by convention if not explicitly configured
- `sqs:GetQueueAttributes` for visible / in-flight / delayed counts on both queues
- DLQ viewer (`d` in Detail's Queue tab): peeks messages, shows id / receive-count / age / body preview
- DLQ actions: per-message resend (send-to-main + delete-from-DLQ), bulk purge with strict typed confirm

### Actions
- Action menu (`a`): Rebuild / Restart / Swap CNAMEs / Terminate
- Rebuild / Restart: Y/N confirm
- Swap CNAMEs: filterable picker of env candidates in the same app, then Y/N confirm
- Terminate: strict — must type the env name exactly
- Status / error feedback in footer
- Auto-refresh after success

### Events panel (main view)
- `Ctrl-E` toggles a bottom panel listing the most recent events across all envs
- Severity-coloured (`ERROR/FATAL/WARN/INFO/DEBUG/TRACE`)
- `Ctrl-↑`/`↓` resize the panel
- Alert badge: count of envs currently in Red (recomputed per refresh)

### Command bar
- `:` enters command mode
- Commands: `:q / :quit`, `:region X`, `:profile X`, `:sort KEY [desc]`, `:group [on|off]`, `:redact [on|off]`, `:events [on|off]`, `:save NAME`, `:f NAME`, `:filter NAME`, `:filters`, `:drop NAME`, `:export`, `:refresh`, `:help`
- `Tab` / `Shift-Tab` completion: command names (first Tab lands on the first match, then cycles), and env-name arguments for `:diff` / `:config-diff` / `:rds-detach` (drawn from the loaded fleet; the trailing token completes, so `:diff ENV-A ENV-B` fills the second slot too). Which commands take an env-name arg is registry metadata (`CommandSpec::env_arg` via `cmd_env_arg`, alias-resolving), not a hardcoded allowlist — pinned by a test.

### Filters, sorting, persistence
- Named filters: `:save NAME` / `:f NAME` / `:filters` / `:drop NAME` — persisted across runs
- Filter / sort / grouping / redact / events-visible / selected env all persist in `~/.config/ebman/state.toml`
- Cursor restored to the same env across restarts

### Privacy & redaction
- Redact mode (`Ctrl-X`) blurs Account, Caller ARN, CNAMEs with `▓` blocks
- TSV export respects redact mode

### Yank / export
- `y` copies CNAME of selected env, `Y` copies the name (via `arboard`)
- `Ctrl-Y` copies the filtered view as TSV

### Tabs / scope
- `Tab` / `Shift-Tab` cycle scope: **Envs ↔ Apps**
- Apps scope lists `DescribeApplications`; `Enter` on an application filters Envs to that app

### Theming & visual polish
- `Theme` struct with `dark` and `light` presets (loaded from `config.toml: theme = "..."`)
- Icon style (`unicode` / `ascii`) with ascii fallbacks for spinner, tab icons, health dot, title decoration
- Rounded borders everywhere
- Decorated block titles (`[ ◆ name ◆ ]`)
- Active-panel vs idle-panel border colours
- Animated braille spinner during loads (ticker gated on `loading_since`)
- Sparkline fade (oldest samples dim)

### Configuration
- `~/.config/ebman/config.toml`: `refresh_interval_secs`, `extra_regions`, `redact_default`, `grouped_default`, `theme`, `icons`
- `~/.config/ebman/state.toml`: persisted app state (managed by app)

### Help & onboarding
- `?` opens help popup; scrollable with `j`/`k`
- Footer key strip mode-aware (Normal / Detail / Action / Picker / Filter / Command / Help / Dlq)

### Quality
- Unit tests (49 passing) across `util / state / config / theme / aws / app / ui`
- Generation/epoch invariants for refresh, identity, detail, DLQ message handlers

### CLI & distribution
- `ebman completions <bash|zsh|fish>` — shell completion scripts generated from a single CLI-surface source of truth (subcommands + global flags + per-subcommand flags/verbs; static, no live env names). Subcommand names pinned to `cli::SUBCOMMANDS` (shared with `main.rs` dispatch) by a test so completion can't drift from the real CLI.
- `ebman mcp setup [--allow-writes]` — prints MCP registration commands (`claude mcp add` + `.mcp.json` snippet + `AWS_REGION` pin) from the trusted local binary; the secure alternative to "point your agent at a remote file and run it". Print-only.
- `--version` / `-V` and `--help` / `-h` flags (exit before TUI)
- MSRV declared in `Cargo.toml` (1.94.1 as of 0.31.0; the AWS SDK sets the floor)
- README with feature summary, keymap, config, and "what's stored locally" section
- GitHub Actions CI: build + test on macOS and Linux, `cargo fmt --check`, `cargo clippy -D warnings`, MSRV gate (pinned to the declared `rust-version`)
- Cargo metadata: license, repository, keywords, categories

### Operator UX (Tier 1 & 2)
- Detail auto-refresh: `R` toggles a per-tab tick driven by the main 15s ticker; AUTO pill in the Detail footer
- Open env in AWS console (`b`) via `open` / `xdg-open`; works in Normal and Detail modes
- Describe overlay (`D`) — popup with the env dumped as pretty JSON; works in Normal and Detail modes
- Breadcrumb top-line in the header: `region / application / env` (env follows selection or the Detail snapshot)
- Frozen / paused mode (`f`) — halts auto-refresh; `FROZEN` pill in the header; manual `Ctrl-R` still works
- Quick-jumps: `1`-`9` select the env at that position in the current view
- Pin / star envs (`*` or `:pin`); pinned envs float to the top; `★` glyph in the NAME column; persisted across runs
- Local env aliases (`:alias NAME LABEL` / `:alias-drop NAME`); alias replaces the rendered name; filter search matches aliases too; persisted

### Safety & audit
- Read-only mode: `--read-only` CLI flag or `:readonly on`; disables Actions menu, DLQ resend, DLQ purge; green `READ-ONLY` pill in the header
- Local audit log at `~/.cache/ebman/audit.log` for every dispatched action (rebuild / restart / swap / terminate / dlq-resend / dlq-purge)
- Crash report writer: on panic, writes `~/.cache/ebman/crash-TS.log` with backtrace, panic location, and payload

### Exports
- JSON export (`:json`) — copies filtered view as a JSON array
- Markdown report (`:report` / `:markdown`) — copies filtered view as a Markdown table

### Themes & onboarding
- `Theme::high_contrast()` preset (`theme = "high-contrast"` / `hc` / `highcontrast`)
- Notification bell on increase in Red-env count (`notify_bell = true` in `config.toml`)
- `:whatsnew` embedded changelog popup
- Spacious view mode — third position in the `Ctrl-D` cycle (Default → Compact → Spacious); 2-row data rows + `Padding::horizontal(2)` on the table block; `SPACIOUS` pill in the header

### Workflow extras
- `Ctrl-W` yanks the equivalent `aws elasticbeanstalk describe-environments` command (POSIX-safe shell-quoting)
- Quick-jump by name: `'` enters a mini-mode; typing a prefix moves selection to the first matching env (matches alias too); `Enter` keeps, `Esc` cancels
- Anomaly highlight: red `▲` glyph in NAME column on envs that transitioned to Red since the previous refresh
- Saved views: `:save-view NAME` snapshots filter / sort / grouping / scope; `:view NAME` restores; `:views` lists; `:view-drop NAME` removes. Persisted across runs.
- Tag view: `ListTagsForResource` per env on Detail open; tags shown in the Config tab
- Metrics time-range selector: `[` / `]` cycles 15m / 1h / 6h / 24h while on the Metrics tab; re-fetches on change
- Cost annotation in Config tab: per-instance hourly rate (us-east-1 baseline table) × instance count → $/hr and $/mo; flags unknown instance types
- Required-tag policy: `required_tags = "Owner,Project"` in `config.toml`; Config tab shows a `⚠ missing required tag(s):` line when any are absent
- Recommendation surface in Detail header health line: `≥ Nm in Red` / `≥ Nm in Yellow` when the env has been in that state for ≥1 minute of consecutive samples
- `:history` overlay listing the last 50 status / error messages with timestamps
- Dry-run preview on destructive confirms: spawns `DescribeInstancesHealth` for Rebuild / Terminate; modal shows `impact: N instances across M AZs`
- Pre-flight events recap: confirm modal also fetches `DescribeEvents` for the env (last 3) and renders them under the impact line
- Configurable columns: `:cols list | hide NAME | show NAME | reset`; persisted in `state.toml` as `hidden_cols`; works on top of the default / compact / spacious view-mode presets; NAME is non-hideable
- Sticky filter row in header: `Filter: <text>` shown in header line 2 when a filter is active, so it stays visible above the table even when the footer is occupied by a status/error message
- In-app `:loglevel <level>` — live-reloads the tracing filter via `tracing-subscriber` reload handle. Bare levels (trace/debug/info/warn/error) auto-add `aws=warn,hyper=warn` so AWS noise stays capped; full directives (`my_crate=trace`) accepted as-is
- CloudWatch alarms list (`:alarms`) — `DescribeAlarms` filtered client-side to alarms whose dimensions reference the selected env; popup overlay with state colouring (ALARM red, OK green, INSUFFICIENT_DATA muted)
- Diff two envs (`:diff NAME`) — overlay showing field-by-field comparison between the currently-selected env and the named target; differing fields prefixed with `≠` in yellow; truncates long values; respects redact mode
- Saved Configurations list (`:saved-configs` / `:configs`) — popup listing EB saved-configuration templates grouped by application; pulled from `DescribeApplications.configuration_templates`
- Plugin commands — `~/.config/ebman/commands.toml` defines `[commands.NAME] template = "..."`; `:NAME` substitutes `{name}`/`{cname}`/`{application}`/`{tier}`/`{region}`/`{profile}` and yanks the rendered command to the clipboard; `:plugins` lists what's available
- Hover preview line — when the mouse hovers a row, the bottom-most row of the table overlays a dim full-detail summary (name + alias + app + status/health + platform + CNAME, untruncated, redact-aware)

### Slick UX pass
- ASCII-block boot splash: 5-line `ebman` logo + tagline + "connecting to AWS…" inside a rounded card while STS resolves
- Empty states with friendly hint lines: "no environments in this account / region — try a different region (r) or profile (p)" / "no events for this environment — ^R to re-fetch" / "no instance data — env may be terminating"
- Status delta indicator in header: per-refresh `▲N Bucket` / `▼N Bucket` chips on Envs line, colour-coded by bucket (Red/Yellow/Green/Updating/Terminating); silently omits unchanged buckets
- Toast notifications: bottom-right transient cards (rounded, kind-coloured border) replace the footer-only feedback for status / error events; up to 4 stacked; auto-dismiss (4 s info / 8 s error); animation ticker wakes the draw loop so toasts disappear on idle
- `Ctrl-K` command palette: fuzzy-search across `:` commands (no-arg / with-arg), env names (jump cursor), saved views, and user plugins; substring scoring with detail-match penalty; ↑/↓ navigate, Enter dispatches, Esc cancels

### Code review follow-ups (2026-05-18)
- Async per-env results carry `env_name`; `AppMsg::Alarms` now drops late results that don't match the requested env. Removed silent overwrite of overlay contents by stale results.
- Overlay rendering routed through the same code path for Detail / Dlq / main views — popups opened from Detail (`D` describe) now actually paint.
- Mouse events only steer the main table in Normal mode + Envs scope + no overlay open. Wheel scroll no longer silently moves selection while Detail / Dlq / Action / Palette is visible.
- Diff state (`prev_health`, `prev_status`, `prev_alerts`, `newly_red`, `health_delta`, `status_delta`) and any open overlay are cleared on profile/region switch. Prevents cross-account "newly red" toasts and ▲N spam on the first refresh after a switch.
- `bucket_delta` semantics tightened: only counts envs present in *both* prev and next. New envs and disappeared envs are not deltas. With an empty prev (post-clear) the delta is empty.
- `init_client` makes `verify_identity` best-effort — `sts:GetCallerIdentity` failure logs a startup warning instead of refusing to launch. EB describe permissions don't require STS.
- `status_message` race fixed: `apply_refresh` only clears messages that still match the snapshot taken at refresh kickoff. User actions during the round-trip (sort, alias, pin, …) survive.
- Audit log captures dispatch + outcome — `write_audit_outcome` writes a second entry once the SDK response lands, so the trail reflects success / validation error / timeout, not just the dispatch time.
- `hsl_to_rgb_clamps_to_valid_range` test asserts real properties: hue wrap, greyscale collapse on zero saturation. No more `let _ = r;`.
- Plugin name collisions surface — `plugins::parse` takes a reserved-name list; colliding entries are dropped with a warning logged via tracing and shown as a startup error in the UI.
- `flatten_err` helper logs the full SDK error chain via `tracing::error!` before flattening to Display for the toast / footer. The chain is no longer lost from `ebman.log`.
- Toast deduplication: identical (kind + text) toasts refresh the existing card's timestamp instead of stacking duplicates.
- Overlay enum: replaced six `Option<String>` fields and one `alarms_pending_for` correlation field with a single `current_overlay: Option<Overlay>`. Unified dismiss, render, and context-switch-close paths.
- `LICENSE-MIT` / `LICENSE-APACHE` files committed; `Cargo.toml` declares `readme = "README.md"`; `.gitignore` covers macOS / editor / cache patterns.
- Audit log + crash report rotation: `audit.log` rotates to `audit.log.1` at 1 MiB; crash hook prunes oldest `crash-*.log` files keeping the 10 most recent.

### Performance + reliability (post-review)
- Per-application colour HashMap memoized in `App.cached_app_colors`; rebuilt only on `rebuild_view` rather than every frame. New `assign_app_colors` pure helper has tests for stable first-appearance, palette wraparound, and empty-palette no-ops.
- Throttle / backoff for EB describe APIs: `is_throttling_error` recognises `ThrottlingException` / `RequestLimitExceeded` / `429`, and `throttle_backoff` doubles the next-refresh delay (capped at 5 min). The ticker skips spawn_refresh while `throttle_until` is in the future; `Ctrl-R` always overrides. Consecutive-throttle counter resets on the next success. State cleared on context switch.

### Tier 1 features (post-review)
- **Live log tail**: new Logs tab in Detail. `^R` triggers `RequestEnvironmentInfo("tail")`, polls `RetrieveEnvironmentInfo` up to 12× at 2s intervals, then fetches each instance's pre-signed S3 URL via `curl`. UI advances through Requesting → Polling (with attempt counter) → Fetching → Ready stages. Per-instance content is shown with a banner row; regex search (`/`) filters visible lines independently of the Events tab search. Requires `curl` on PATH.
- **Deploy a version**: `:versions` lists `DescribeApplicationVersions` for the selected env's app in an overlay, sorted newest-first. `:deploy <label>` calls `UpdateEnvironment(version_label)` and records a dispatched audit entry. The outcome flows through the existing `AppMsg::ActionResult` path so success/failure surfaces in the footer.
- **Multi-region overview**: `:region all` flips into multi-region mode and fans `DescribeEnvironments` across `extra_regions ∪ {current}` in parallel. Each env gets its origin region stamped, and a REGION column is conditionally inserted in the table. `:region off` returns to single-region. New `aws::list_environments_in_region` helper is shared with cross-account search and org-health.

### Tier 2 / 3 / 4 / 5 / 6 / 7 batch
- **Header signals**: SSO session expiry countdown pill (red/yellow/grey by TTL), update-available pill driven by a one-shot crates.io check via curl, `:minimap on|off` overlay of one coloured cell per env (health-driven), saved-filter chip bar appears when `named_filters` is non-empty.
- **Pre-flight traffic warning** in the confirm modal: `compute_traffic_warning` flags ACTIVE DEPLOY / RECENT CHANGE / currently-Red before authorising further actions.
- **Drift glyphs** in NAME column: ◆ for envs updated within 24h, ◇ (muted) for envs unchanged > 30d.
- **5xx / 4xx / p90 anomaly badge** in Metrics tab (`series_anomaly_label` flags last sample > 2x baseline for error rates / 1.5x for latency).
- **Webhook on Red transition**: `webhook_url` config option; `build_webhook_payload` emits a flat JSON object via curl POST.
- **Selectable + yankable events panel**: `events_cursor`, `J`/`K` move, `y` yanks the line; `▶` glyph on the cursor row.
- **Multi-select + batch actions**: space toggles selection (✓ marker in NAME), `:batch-rebuild` / `:batch-restart` dispatch non-destructive actions across the selection in one shot.
- **Mouse-drag panel resize**: drag the divider between events panel and table; height clamped to [4, 30].
- **Focused-panel model + per-panel key strip**: `Focus` enum (Table / Events), `Ctrl-]` / `Ctrl-[` cycle, j/k routes by focus, footer strip swaps to events keys when focused there.
- **Custom keybindings**: new `src/keys.rs` parses `~/.config/ebman/keys.toml`; F1-F12 and uppercase A-Z aliases to `:` commands. `App.lookup_custom_key` intercepts in Normal mode before built-in dispatch.
- **Saved Configurations full CRUD**: `:config-save`, `:config-delete`, `:config-apply` wired to `CreateConfigurationTemplate` / `DeleteConfigurationTemplate` / `UpdateEnvironment(template_name)`.
- **`:account NAME`** alias for `:profile NAME` (the standard AWS pattern of one profile per account). A real `sts:AssumeRole`-based account model is deferred to a dedicated session.
- **Cross-account search**: `:find-env <substring>` fans `DescribeEnvironments` across every profile in `~/.aws/{config,credentials}` (in the current region), reports hits in an overlay.
- **Org-wide health overview**: `:org-health` aggregates env / Red counts per profile across all configured profiles, surfaced in an overlay.
- **First-run wizard**: when no persisted state + no AWS creds, a welcome overlay walks through bare-minimum setup.
- **Metric chart hover**: `metrics_hover_col` + `metrics_body_rect` capture mouse position in Detail/Metrics; `hover_index` pure helper maps column → point index → `@cursor <value>` in each chart's title row.

### Non-interactive CLI
- **`ebman envs [--json]`** prints the env list as TSV or JSON.
- **`ebman action <rebuild|restart|terminate> --env NAME [--yes]`** dispatches an action without entering the TUI. Terminate requires `--yes`.
- `--help` updated to document subcommands; `--version`, `-h`, `-V`, `--read-only` flags continue to work.

### CW Logs auto-discovery on Detail open (2026-05-19)
- **Discovery on Detail open** — `discover_env_log_groups` fires once when the user opens an env (alongside tags / env-vars / instances). Result stored on `DetailState.cw_log_groups`. Logs-tab idle hint now renders one of three tailored strings (groups present → "press s to live-stream", groups absent → "CW Logs not configured (`:logs-stream on` to enable)", still loading → "checking…"). No auto-open of the streaming overlay — that's still triggered by `s`. Discovery errors swallowed silently and fall back to the "checking" hint, so a missing IAM perm on `logs:DescribeLogGroups` doesn't surface as a toast.

### More per-option commands (2026-05-19)
- **`:deployment-policy POLICY`** — sets `aws:elasticbeanstalk:command.DeploymentPolicy`. Accepts canonical names (AllAtOnce, Rolling, RollingWithAdditionalBatch, Immutable, TrafficSplitting) and lower-case aliases.
- **`:rolling-update on|off`** — toggles `aws:autoscaling:updatepolicy:rollingupdate.RollingUpdateEnabled`.
- **`:health-check-url /path`** — sets `aws:elasticbeanstalk:application.Application Healthcheck URL` (the path the ALB target group probes for HTTP 200).
- **Logs tab idle-state hint** — "press ^R for one-shot snapshot · s to live-stream CW Logs (needs `:logs-stream on`)" replaces the prior single-line hint so operators discover the streaming path without reading help.

### Follow-ons (2026-05-19)
- **`:deploy --from s3://bucket/key`** — sidesteps the local-read + upload steps; goes straight to `CreateApplicationVersion` against the existing S3 object. Pure `parse_s3_url` helper with happy-path + 4 malformed-input tests. `spawn_deploy_from_s3` shares the same audit/pending/finish plumbing as the local path.
- **`s` keybind on the Detail Logs tab** opens the CW Logs streaming overlay (`spawn_logs_tail`) over the existing snapshot view. One-keypress upgrade; closing the overlay returns to the snapshot. Updated global + per-context help.
- **Custom metric dimensions** — `:metric add LABEL NS NAME [STAT] [DIM=VAL,DIM=VAL]` accepts explicit dimension overrides. Pure `parse_metric_extra_args` handles the "stat or dims, in any order" heuristic; `CustomMetricSpec` now carries an optional 4th pipe-delimited field for persistence. Metrics in `AWS/EC2`, `AWS/ApplicationELB`, etc. now reachable with the right dimension keys. Tests: `parse_metric_extra_args_*` × 4, `custom_metric_spec_round_trips_with_dimensions`, `custom_metric_spec_parse_drops_malformed_dimension_pairs`.

### Deploy from local path (2026-05-19)
Last remaining Tier 1 blocker, shipped. Tests: `derive_version_label_*` × 3, `expand_tilde_only_replaces_leading`.

- **`:deploy --from PATH [--label LABEL] [--describe DESC] [--no-deploy]`** uploads a local `.zip`, creates an EB application version, and (by default) immediately deploys it to the selected env. Existing `:deploy LABEL` shape preserved for shipping known labels.
- New `aws::create_storage_location` / `put_application_bundle` / `create_app_version` helpers. S3 client added to `AwsClient`.
- Bundle uploaded to EB's managed bucket under `applications/<app>/<label>`; `CreateApplicationVersion` references it via `S3Location`.
- Pure helpers + tests: `derive_version_label` (filename stem + unix ts, sanitised to EB's `[A-Za-z0-9_.-]` charset); `expand_tilde` (only the leading `~/` form).
- Pre-validation in the synchronous path: file exists, non-empty, read into memory. Multi-stage errors surface with stage prefix (`storage-location:`, `s3-put:`, `create-version:`, `deploy:`) so operators know where the chain broke.
- Known limitations (now on backlog): no `s3://bucket/key` source yet; no multipart upload (5 GiB ceiling); whole bundle held in memory during upload.

### Custom metric selection (2026-05-19)
Operator-defined extra charts in the Metrics tab. Tests: `parse_custom_metrics`, `parse_custom_metric_drops_malformed_value`, `custom_metric_spec_round_trips`.

- **`:metric add LABEL NAMESPACE NAME [STAT]`** upserts a custom-metric chart; STAT defaults to Average. Persists to `state.toml` under `metric.LABEL = "namespace|name|stat"`. Auto-refreshes the Metrics tab if it's currently open.
- **`:metric remove LABEL`** drops the entry + persists + refreshes.
- **`:metric list`** dumps the table into a TextOverlay.
- `aws::fetch_custom_env_metrics` generalises the existing GetMetricData path; runs concurrently with the built-in fetch via `tokio::join!`. Results append to the fixed 4-chart set in operator-add order.
- Known limitation (in backlog): charts hard-scope to `EnvironmentName` dimension, so anything outside `AWS/ElasticBeanstalk` namespace returns empty until we support custom dimensions.

### CloudWatch Logs `tail -f` (2026-05-19)
The biggest remaining Tier-1 blocker, shipped. Tests: `pick_default_log_group_*` × 3.

- **`:logs-tail [LOG_GROUP]`** opens a streaming overlay that polls `cloudwatch:FilterLogEvents` every 2s and appends events. If no group specified, discovers groups under `/aws/elasticbeanstalk/{env}/` and auto-picks the most useful (web.stdout.log preferred, then eb-engine.log / eb-hooks.log / nginx access).
- New `Overlay::LogTail` variant with cap of 2000 events (oldest dropped), `following` auto-tail mode, regex filter (`/` activates, `n` clears), j/k scroll, G snap-to-tail, g jump-to-top.
- Polling task lifecycle: aborted on overlay close, on a second `:logs-tail` call, and on profile/region switch via `apply_rebuild`. Session id bumped at every teardown so late `LogTailOpened` messages from the aborted task can't re-open the overlay (abort + channel-send race).
- Pure `pick_default_log_group` helper for the default-group selection. Render gracefully handles plane-1 chars in messages via ratatui's existing Wrap.
- Late `LogTailEvents` arriving during a `?`-help round-trip route into `pre_help_overlay` so events aren't lost while reading help.

### Per-option commands + generic option escape hatch (2026-05-19)
Fills the Network + Security + miscellaneous-option gap without the modal-form abstraction. The new generic commands cover anything we don't have a friendly name for.

- **`:keypair NAME`** — set EC2 key pair (security tab equivalent).
- **`:service-role ARN_OR_NAME`** — set EB service role.
- **`:instance-profile NAME`** — set IAM instance profile attached to EC2.
- **`:public-ip on|off`** — toggle `AssociatePublicIpAddress`.
- **`:elb-scheme public|internal`** — set ELB scheme.
- **`:set-option NAMESPACE OPTION VALUE...`** — generic escape hatch for any option-settings namespace; VALUE tokens joined with single spaces.
- **`:unset-option NAMESPACE OPTION`** — generic clear back to the platform default.

All seven funnel through the shared `spawn_option_settings_update` helper, so read-only + audit + pending tracking are inherited.

### Env vars in Config tab (2026-05-19)
- **Env vars now render in the Config tab** — operators no longer need `:env list` for the common "what's set?" case. Loaded eagerly on Detail open via the same lazy pattern as tags (`fetch_env_vars` → `AppMsg::DetailEnvVars` → `detail.env_vars`). After `:env set` / `:env unset` succeeds the Config tab auto-refreshes (the OptionSettingsUpdate handler keys on the summary prefix). Same key-column auto-sizing + overflow-to-newline layout as tags; empty values render as `""` to distinguish "set to empty" from "not set".

### Instance-type + custom-platform-delete (2026-05-19)
- **`:instance-type TYPE`** — first slice of the "capacity profile beyond min/max" gap; sets `aws:autoscaling:launchconfiguration.InstanceType` via the shared option-settings helper. EB triggers a rolling launch-config replacement. Other capacity settings (spot %, scaling triggers, scheduled scaling) still need either a modal form or per-option commands.
- **`:custom-platform-delete <arn>`** — closes the create/delete loop for the `:custom-platforms` listing. EB rejects with a clear error if any env still uses the platform; otherwise async cleanup proceeds. Create still on backlog (slow Packer-build flow).

### Env-var editor (2026-05-19)
Console's most-used edit surface (env var changes), now reachable without leaving ebman or opening a modal form. Tests: `format_env_vars_aligns_on_equals`, `format_env_vars_handles_empty_input`.

- **`:env list` / `:env set KEY VAL...` / `:env unset KEY`** — single CLI surface for `aws:elasticbeanstalk:application:environment` namespace. List opens a TextOverlay of `KEY = VALUE` lines (sorted; empty values render as `""`). Set/unset funnel through the existing `spawn_option_settings_update` helper so read-only + audit + pending tracking are free. Value tokens joined with single spaces (`:tag` convention). Usage error documents that changes trigger an app-server restart per EB. Pure `format_env_vars` helper for the list rendering.

### Console-parity batch (2026-05-19)
Shipped as one block; all use the existing pending-actions + audit-log + read-only-gating machinery. Tests added: `parse_named_arg_picks_up_value_after_flag`, `alarm_kind_to_metric_covers_known_kinds`, `format_template_settings_groups_by_namespace`, `format_template_settings_handles_empty_input`.

- **Inspect saved config template contents** — `:config-inspect [APP] TEMPLATE` calls `DescribeConfigurationSettings(template_name)` and surfaces the option settings as a sorted text dump grouped by namespace; new `i` keybinding in the interactive `:saved-configs` overlay opens the inspection for the cursor row. Empty values render as `""` so operators can distinguish "set to empty" from "not set". Pure `format_template_settings` helper for the rendering.
- **Create / delete CloudWatch alarms** — `:alarm-create NAME KIND THRESHOLD [OP]` and `:alarm-delete NAME`. KIND is one of `health` / `4xx` / `5xx` / `latency` (matches the existing Metrics-tab chart set). Operator defaults: 5-min period, 1 evaluation period, kind-specific comparison operator (health = LE, others = GT). No SNS action wired — operators set notification topics via console or `:notify`. Pure `alarm_kind_to_metric` helper.
- **CloudWatch Logs streaming toggle** — `:logs-stream on|off [--retention DAYS]` flips the EB option settings under `aws:elasticbeanstalk:cloudwatch:logs` (StreamLogs / RetentionInDays / DeleteOnTerminate). Default retention 7 days. Prerequisite for the still-on-backlog "real `tail -f`" item to have anything to tail.
- **Notifications (SNS topic for env events)** — `:notify EMAIL_OR_SNS_ARN` accepts either an email address (EB creates a topic + subscription) or an existing SNS topic ARN (EB just attaches it). `:notify off` clears the endpoint via options-to-remove.
- **Managed platform updates window** — `:managed-window DAY HOUR | off`. Day accepts full or abbreviated names (mon|monday); hour 0-23. Generates EB's cron-style `Sun:04:00` PreferredStartTime + enables ManagedActionsEnabled. If the env's ServiceRoleForManagedUpdates isn't set, EB will reject with a clear error — operator can address via console or follow-up option-settings call.
- **`OptionSettingsUpdate` AppMsg + `spawn_option_settings_update` shared helper** — all three option-settings commands funnel through one place. New `parse_named_arg` pure helper for `--flag VALUE` style optional args (used by `--retention`).

### UX punch list batch B (2026-05-19)
- **`Overlay::SavedConfigs(String)`-as-generic-text-dump refactor** — new `Overlay::TextDump { title, body }` variant and matching `AppMsg::TextOverlay { gen, title, body }` (renamed from the misleading `CrossAccountSearch`). Every callsite passes its own title; `:pending`, `:resources`, `:find-env`, `:org-health`, `:upgrade`, `:custom-platforms`, `:versions` all surface accurate block titles instead of "saved configurations".
- **`:help` now opens the context-scoped help** — Command-mode `:help` infers `help_topic` from app state (Detail view live → Detail; action flow open → Action; DLQ open → Dlq; SavedConfigs overlay open → SavedConfigs; otherwise Global). Matches the `?` keybinding's behaviour so the two routes don't disagree.
- **`:tag` usage error documents the value-joining convention** — "value tokens joined with single spaces; no shell quoting — use a separate call to set values with literal multi-spaces". Tag editing without surprise.
- **`:delete-version` invalidates the `:versions` overlay** — on a successful delete the handler checks whether the current overlay is the matching `application versions — {app}` text dump and re-fetches if so. No more stale entries after a destructive op.
- **Interactive saved-configs overlay groups by application** — rows render under bold app-name headers instead of a flat `app/template` list. Cursor still indexes items, not headers.

### UX punch list batch A (2026-05-19)
Items from the drive-the-app review, shipped together because they share state. Tests added: `action_labels_are_distinct_and_non_empty`, `visible_window_anchors_to_top_when_items_fit`, `visible_window_slides_to_keep_cursor_visible`, `visible_window_handles_empty_and_zero_budget`.
- **Audit log + toast labelling fixed for `:config-*` and `terminate-instance`** — new `Action::ConfigSave / ConfigDelete / ConfigApply / TerminateInstance` variants with proper labels. Replaces the `Action::Rebuild`-as-placeholder reuse; audit entries now record the real action name. Added `stage=dispatched` audit lines for all three config-* commands (previously only stage=completed was written).
- **Tag / delete-version / config-* writes now appear in the pending-actions panel** — `spawn_tag_update`, `spawn_delete_app_version`, and the three config-* paths all call `push_pending` + the corresponding handler calls `complete_pending`. Header `⏳ N` chip and `:pending` overlay are now an accurate truth-source for in-flight work.
- **Terminate-instance pending-row never matched complete_pending** — pre-existing bug: dispatch wrote `"Terminate instance i-abc"` as the label, completion looked for `"Terminate instance"` (now `Action::TerminateInstance.label()`). Result: termination rows lived forever as "in flight". Fixed by aligning the label and carrying instance id in the target string instead.
- **Pressing `?` from Detail / Action / Dlq now returns there on close** — pre-existing bug: closing help dropped the user back to Normal mode unconditionally. `pre_help_mode: Option<Mode>` field is set at every `?` keypress and restored on close. Same treatment for an overlay open at `?` time via new `pre_help_overlay` field.
- **Per-context `?` help now visible from the footer** — Detail / Action / Dlq key-strips advertise ` ? help`. The feature was unreachable for new users before.
- **`HelpTopic::SavedConfigs` implemented** — pressing `?` inside the interactive overlay stashes the overlay, surfaces a Saved-Configs help renderer (`draw_help_saved_configs`), and restores the overlay on close. Replaces the prior doc-comment-lie behaviour.
- **`x` in interactive overlay now requires Y/N confirm** — armed-confirm state on the overlay variant; banner turns red and the cursor row tints red until y/Y/enter (dispatches) or n/N/esc (cancels). Asymmetric-with-Terminate gap closed for a less destructive op.
- **Interactive overlay scrolls when items overflow** — pure `visible_window(cursor, total, budget)` helper slides the visible window so the cursor stays in view; `↑ N more above` / `↓ N more below` hints render when the list is clipped.

### Write-path batch B (2026-05-19)
- **Saved-configuration overlay → editable** — new `Overlay::SavedConfigsInteractive { items, cursor }` variant replaces the text-dump for `:saved-configs` when any templates exist (falls back to the dump when none do). j/k/g/G/up/down/home/end navigate; enter/a apply selected template to currently-selected env; x deletes; c closes the overlay and prefills `:config-save ` for the user to type a template name. All three dispatch through the existing `:config-apply` / `:config-delete` / `:config-save` paths so they share read-only gating and audit trail. Read-only gating was missing on the underlying commands too — fixed in the same pass. Pure `collect_saved_configs` helper sorts (app, template) pairs deterministically; tested for sort stability + empty-input.

### Write-path batch (2026-05-19)
- **Tags editor** — `aws::update_tags` wraps `UpdateTagsForResource`; `:tag KEY VALUE` adds/updates a tag and `:untag KEY` removes one. Read-only mode blocks both; ARN-missing on the selected env errors out; the call writes a dispatched + completed audit entry; on success a toast fires and the Config-tab tags refresh automatically. Pure helper `parse_tag_args` handles the "value tokens joined with spaces" convention; tested for happy path, multi-token join, and rejection of missing-value input.
- **Application-version delete** — `aws::delete_application_version` wraps `DeleteApplicationVersion`; `:delete-version <label> [--force]` dispatches against the selected env's app. `--force` (alias `-f`) sets `DeleteSourceBundle=true` so the S3 zip is also removed. Read-only mode blocks; dispatched + completed audit entries written; outcome surfaced as a toast. AWS still rejects deletes of versions currently deployed to an env — those bubble up in the error toast.
- **Powerline-font glyph set (`icons = "powerline"`)** — opt-in via config.toml; `IconStyle::Powerline` variant joins Unicode/Ascii. Routes thin powerline separator U+E0B1 through `sep()`, U+E0B6/E0B4 tab caps through `titled_block`, Nerd Font MDI tab icons (flash, server, chart-line, email, text-box, cog) through `tab_icon`, and U+F111 dot through `health_dot`. Spinner reuses the braille frame set (Powerline-targeted fonts include braille). README config example updated; tests for sep glyph routing and tab-icon distinctness across all three icon styles.

### Operator-feedback batch (2026-05-19)
- **Pending-actions panel** — `PendingAction { label, target, started, completed }` queue (cap 20, completed entries expire after 60s); wired into `spawn_action` / `spawn_batch_action` / `spawn_terminate_instance` and the `AppMsg::ActionResult` handler. Header chip `⏳ N` while any are in flight; `:pending` / `:in-flight` / `:inflight` overlay lists label, target, age, and outcome.
- **Per-context help** — new `HelpTopic` enum (Global / Detail / Dlq / Action / Shell) on `App`; `?` in Detail / Dlq / Action modes scopes the help overlay to just the keys relevant to that screen, with a footer pointer back to the global keymap. Implemented as `draw_help_detail` / `_dlq` / `_action` / `_shell` helpers in `ui.rs`. Shell topic kept reachable-shaped but currently unreachable since `?` is forwarded to the subprocess.

### Small-wins batch (2026-05-19)
- **Dry-run preview for Deploy / Scale / Clone** — parameterised actions now run the same `spawn_dry_run` + `spawn_preflight_events` pre-flight that Rebuild/Terminate have, so the confirm modal shows the instance / AZ impact and last 3 events before the operator authorises.
- **`:resources` overlay** (`:resources` / `:res`) — `DescribeEnvironmentResources` dump (ASGs, instances, LCs, LTs, LBs, triggers, queues) in a single overlay. Useful "what's actually in this env" view; also caught the WorkerQueueURL-is-empty bug originally.
- **Crash-report 30-day TTL** — `prune_old_crash_reports` now drops any `crash-*.log` older than 30 days regardless of the count cap. Test `prune_old_crash_reports_drops_files_past_ttl` covers it.
- **Status-diff toast suppression** — `delta_toast_key` extracts a bucket name from text shaped `▲N Bucket`; `push_toast` collapses successive toasts with the same key into the latest value rather than stacking. Tested for happy path + negative cases.
- **Sortable Config-tab tags mini-table** — tags now render alphabetically (case-insensitive); key column auto-sizes to the longest key clamped at 12–40 chars; long keys overflow to their own line so values stay aligned.

### Remote control plane
- **`--control-socket PATH`** — when set, ebman opens a Unix socket at PATH with 0600 perms and accepts one-shot requests: `SCREEN` (plain-text dump of the current frame from the ratatui back-buffer), `KEY <spec>` (synthesised key event injected via `handle_event`; spec supports Ctrl/Shift/Alt + arrows / Enter / F1-12 / single chars / `Char(x)`), `CMD <text>` (runs a `:` command), `STATE` (flat JSON with mode / profile / region / account / envs / selected / load / sort / grouped / redact / focus).
- **`ebman ctl <op>` subcommand** — one-shot client; defaults to `~/.cache/ebman/control.sock`; override with `--socket PATH`. Examples: `ebman ctl screen`, `ebman ctl key Down`, `ebman ctl key Ctrl+R`, `ebman ctl cmd ":region eu-west-2"`, `ebman ctl state`.

### Mocked-AWS coverage: write path + error path (2026-05-20)
- **`update_env_option_settings_builds_correct_request_shape`** — pins the load-bearing write path used by `:capacity`, `:env`, `:tag`, `:subnets`, `:elb-subnets`, `:security-groups`, and every `:set-option`. Asserts environment_name + each option_setting tuple (namespace / name / value, in caller order) + options_to_remove all land on the UpdateEnvironment request. Uses `match_requests` as the assertion vehicle — a request shape that diverges fails the rule match and surfaces as a test error.
- **`update_env_option_settings_rejects_empty_input_before_dispatch`** — the "nothing to do" guard must short-circuit before any AWS call. Test mocks a tripwire rule and asserts `num_calls() == 0` after the guard fires.
- **`update_env_option_settings_surfaces_aws_errors`** — `then_error` returns `InsufficientPrivilegesException`; assert the wrapped error string carries the contextual prefix so the log is actionable.

### ELB-subnets picker (2026-05-20)
- **`:elb-subnets`** — sibling to `:subnets`, targets `aws:ec2:vpc.ELBSubnets` so the ELB attaches to a different subnet set than the instances. Web-tier-only. Added `MultiSelectFlavour::ElbSubnets` variant; `load_multi_select` reuses the existing `list_subnets_in_vpc` call but pulls the initial selection from the new `EnvVpcContext.elb_subnets` field. `fetch_env_vpc_context` extended to parse `ELBSubnets` from option settings; test updated to assert all three subnet/SG fields populate in one round-trip.

### Network + Security MultiSelect pickers (2026-05-20)
- **`FieldKind::MultiSelect` + helpers** — modal-form abstraction gained a multi-select field kind with comma-joined `value` (matches EB's option-settings format directly), per-field `option_cursor` for in-field row navigation, optional `option_annotations` for per-option display suffixes, and pure helpers `parse_multi_value` / `toggle_multi` / `is_multi_selected`. Up / Down (or j / k) moves between options when MultiSelect is focused; tab still moves between fields. Space toggles the option at the cursor. 5 unit tests.
- **`:subnets`** — opens a MultiSelect form with the env's EC2 subnets (filtered by VPC). Pre-fills with `aws:ec2:vpc.Subnets`, submits via the shared option-settings update path. Subnet rows annotated with `(AZ · CIDR · Name)`. Bound to the env table cursor; reports an error if no env is selected. Ordered by AZ then CIDR for stable picker rows.
- **`:security-groups`** — same shape, targets `aws:autoscaling:launchconfiguration.SecurityGroups` and lists EC2 security groups in the VPC. Ordered by group name.
- **`load_multi_select` shared async helper** — fans out to `fetch_env_vpc_context` + the right EC2 list call (DescribeSubnets or DescribeSecurityGroups), assembles options + annotations + initial selection, and lands as new `AppMsg::FormMultiSelectLoaded { gen, env_name, field_key, result }`. Handler matches by `field_key` so multiple MultiSelect fields in one form remain trackable.
- **`aws::fetch_env_vpc_context`** — single DescribeConfigurationSettings round-trip that returns `EnvVpcContext { vpc_id, subnets, security_groups }` from the relevant namespaces in one pass.
- **`aws::list_subnets_in_vpc` / `list_security_groups_in_vpc`** — EC2 inventory queries filtered by `vpc-id`, returning the wide rows the pickers need (id + AZ + CIDR + Name tag for subnets; id + name + description for SGs). Pure `split_csv` helper extracted for the CSV parsing.
- **Tests**: `split_csv_trims_and_drops_empties`, `fetch_env_vpc_context_pulls_vpc_id_subnets_and_sgs`, `list_subnets_in_vpc_filters_orders_and_extracts_name_tag`, `list_security_groups_in_vpc_orders_by_name`. All mocked via `aws-smithy-mocks` against the EB + EC2 SDK surfaces.

### Mocked-AWS test foundation (2026-05-20)
- **`aws-smithy-mocks` wired into the test build** — added `aws-smithy-mocks = "0.2"` plus the `test-util` feature flag on each AWS SDK crate as dev-dependencies. Production paths use the regular config; only the test build pays the extra crate cost.
- **`AwsClient::for_tests` constructor** — gated behind `#[cfg(test)]`, takes pre-built (typically `mock_client!`-backed) sub-clients so tests can swap in mocks for any single SDK surface without touching the others. Bare SdkConfig + a fixed `us-east-1` region keep behaviour reproducible; non-mocked sub-clients fail loudly on use, which is the signal we want for "unexpected AWS call from a code path we thought was pure".
- **Regression #1 pinned**: `worker_queues_resolves_via_describe_environment_resources_when_autocreated`. EB autocreated worker queues return `WorkerQueueURL = ""` from `DescribeConfigurationSettings`; the fix queries `DescribeEnvironmentResources` first. Test mocks both and asserts the primary path fires.
- **Regression #2 pinned**: `peek_messages_loops_and_dedupes_across_batches` + `peek_messages_stops_after_two_empty_batches`. SQS `ReceiveMessage` may return fewer than the requested batch; ebman loops with long-polling, dedupes by message id, and bails after two consecutive empty calls. Mocks a 4-call sequence to exercise both paths.
- **Happy-path lock-in**: `list_environments_maps_describe_environments_to_env_rows` — covers the most-used code path so refactors of `list_environments` can't silently break the table render. Verifies `tier` normalisation (WebServer → Web) and `platform_family` extraction.
- Foundation in place for future `aws.rs` tests; pattern is `mock!(Client::op).then_output(|| out)` + `mock_client!(crate, [&rule])` + `client_with_eb` / `client_with_sqs` helpers.

### UI polish pass 3 (2026-05-20)
- **Action-menu icons** — every entry in `:action` now leads with an icon-style-aware glyph. Powerline picks Nerd Font MDI icons (`F0450` refresh, `F0521` swap, `F01B4` trash, etc.); unicode falls back to `↻ ⇄ ✗ ↑`; ASCII gets fixed-width letter tags. Destructive actions render glyph in `theme.health_red`. New `Action::glyph(IconStyle)` method; test `action_glyph_is_distinct_per_action_per_icon_style`.
- **Version-label highlight** — pure `format_version_label` helper identifies the longest digit run in the version label (typically the build number) and renders it in `theme.app_palette[0]` BOLD; surrounding prefix / suffix dim to `theme.muted`. Operators scanning `build-10678` see the bright `10678` against the dim `build-`. Pure `longest_digit_run`; 5 tests.
- **Group-banner sub-totals** — per-app banner row now shows `3 envs · 2 web · 1 worker · 1 red` in the APPLICATION column. Empty buckets omitted; tier split only when both Web + Worker present in the group. Pure `summarize_group`; 3 tests.
- **Newly-added env marker** — new `App::newly_added: HashSet<String>` populated each refresh with env names absent from the previous `prev_health` (skips first refresh so startup doesn't flag every env). Table renders a transient `+` glyph in `health_green` on the NAME cell.
- **Health-transition pulse** — when an env is in `newly_red`, the rightmost sparkline cell renders as `█` (full block) with BOLD + SLOW_BLINK, drawing the eye to the just-landed transition. `sparkline_for` gained a `pulse_last: bool` arg.
- **Pending pill inline summary** — `⏳ 3` in the header chain replaced with `⏳ rebuild ×2, deploy`. Identical action stems collapse via `×N`; output truncated to 25 chars. Pure `summarize_in_flight` + `label_stem` mapping; 3 tests.
- **Context-aware footer hints** — when the status / error / filter footer slot is empty, surface a `💡 hint` in priority order: 2+ alerts (`:alarms`), 3+ in flight (`:pending`), SSO expiring within 15 min, new envs marked `+` this refresh. Reads only `App` fields; `None` when nothing's worth saying.
- **Form-field validation marker** — invalid fields in the modal form get a trailing `✗` glyph in `health_red` BOLD next to the value, in addition to the existing inline error line below. Eye-catcher for scanning long forms.
- **Confirm-modal env highlight** — destructive confirms (Terminate, Swap, AbortUpdate) render the env name as a red-on-row_red_bg chip inside the question line; non-destructive get a `title_alt` highlight. Pure `highlight_env_in_summary` helper; 2 tests.

### Settings menu + font auto-detect (2026-05-19)
- **`:settings` modal form** — interactive editor for `~/.config/ebman/config.toml`. Pre-fills nine fields from the live `App` state (theme, icons, refresh interval, redact-default, grouped-default, notification bell, required tags, extra regions, webhook URL). Submit writes the file back via the new `config::serialize` round-trip and live-applies theme / icons / refresh interval. `extra_regions`, `notify_bell`, `required_tags`, `webhook_url` update in place and take effect on the next refresh / event. Routed through the existing modal-form abstraction via new `FormSubmit::LocalConfig` variant; `open_form` short-circuits the AWS pre-fill for local-config forms; `submit_form` branches to a new `submit_local_config` path. Pure `Form::apply_to_config` helper merges form values onto a baseline `Config`; pure `config::serialize` round-trips through `config::parse`. Tests: `parse_icons_auto_is_preserved`, `serialize_round_trips_full_config`, `serialize_round_trips_default_config`, `apply_to_config_updates_known_fields`, `apply_to_config_unknown_keys_are_ignored`, `local_config_submit_yields_no_option_settings`.
- **`icons = "auto"` config value + cell-width probe** — new `src/font_probe.rs` writes a single Powerline triangle (`U+E0B0`) at startup, reads the cursor column back via `crossterm::cursor::position`, and resolves to `"powerline"` on a one-cell advance / `"unicode"` otherwise. Probe runs before `enter_tui()` so the glyph never reaches user scrollback; raw mode is enabled briefly via a `Drop`-based guard and torn down regardless of outcome. Pure `classify_advance` + `resolve_icons_setting` helpers keep the probe testable. Non-TTY stdout short-circuits to `false`. Tests: `classify_one_cell_advance_is_supported`, `classify_other_advances_are_unsupported`, `resolve_passes_through_non_auto_values`.

### Powerline polish pass (2026-05-20)
- **Lead-in arrow shape fix** — `pill_chain` and `render_tabs` switched the leading edge from `U+E0B0` (right-pointing) to `U+E0B2` (left-pointing) so the pill's coloured base sits flush with the body, mirroring the trailing `U+E0B0`. Previously the leading wedge read as much smaller than the trailing one (terminal-bg cell + tiny pink point vs. solid pink rectangle + pink wedge). Per-app group banner row picks up the same treatment so it reads as a symmetric `◀{app}▶` ribbon. Tests: `pill_chain_uses_left_wedge_for_lead_in_in_powerline_mode`, `pill_chain_no_powerline_glyphs_in_unicode_mode`.
- **Header pill-chain spacing** — two leading spaces injected before the chain in Powerline mode so the wedge has visual breathing room from the preceding `Sort: ...` text.
- **Loading-indicator linger fix** — refreshes whose round-trip lands just past the 300 ms display threshold no longer flash the spinner on and off in a single frame. New `LOADING_INDICATOR_LINGER` (500 ms) keeps the `loading…` indicator visible after completion if it became visible during the load. Pure `compute_loading_linger_target` helper; tests `loading_linger_target_none_when_no_load`, `loading_linger_target_none_when_under_threshold`, `loading_linger_target_arms_past_threshold`. Anim ticker condition includes the linger window so the spinner keeps advancing.
- **Theme-correct colours (~100 sites)** — removed every hardcoded `Color::Yellow/Cyan/Gray/Red/White` foreground in the footer, breadcrumb, kv() helper, DLQ overlay, action menu, confirm modal, Detail tabs (Events/Instances/Queue), and all six help screens. `help_line()` now takes a `&Theme` argument; ~106 call sites updated. Light + high-contrast themes finally render footer / help / DLQ correctly.
- **Breadcrumb separator** switches to `U+E0B1` (thin Powerline divider) in Powerline mode, matching the rest of the header chain. Falls back to ASCII `/` otherwise.
- **Powerline filter chips** — saved-filter chip bar in the header renders as a `pill_chain` ribbon (active chip in `title_alt`, inactive in `row_alt_bg`) in Powerline mode. Plain pill+bullet style preserved for unicode/ascii.
- **README font section** — Install section gained a "Fonts (optional)" subsection with `brew install font-meslo-lg-nerd-font` / `font-jetbrains-mono-nerd-font`, terminal-font setup paths (iTerm2 / Terminal.app / Ghostty / Alacritty / WezTerm / VS Code), and the `icons = "auto"` follow-up.

### UI polish pass 2 (2026-05-20)
- **Cursor row marker** — new `cursor_marker(theme)` helper. Powerline mode picks up `U+E0B0` as the highlight glyph; unicode/ascii keep the half-block `▌`. Applied to all 5 ratatui List/Table `highlight_symbol` sites (palette, env table, scope table, DLQ list, action menu). Test: `cursor_marker_swaps_per_icon_style`.
- **Empty-state polish** — when no envs match, the centred hint echoes the live filter text back (`no environments match \`prod-\``) so the operator can see what's hiding their rows. Heading in `title_alt` for contrast, properly centred horizontally and vertically. Three copy variants: empty-account, filter-hides-everything, saved-view-hides-everything.
- **Detail-header pills** — env-header line now renders Status as a coloured pill via new `status_pill` helper (extracted from `status_cell`) and Health as `health_dot` + label, matching the main env table aesthetic. Name + Application stay as kv text.
- **Toast notification glyphs** — info / ok / error toasts gained a leading severity glyph in both title and body. Glyph set varies by icon style: Powerline gets Nerd Font (`F05A` / `F058` / `F057`), unicode gets `ⓘ` / `✓` / `✗`, ascii falls back to `i` / `+` / `!`.
- **Splash minimum** bumped from 1 s to 2 s so the gradient pass has time to land before the table replaces it.
- **Region persistence fix** — `persist_state` was writing `override_region`, which is `None` when the user never explicitly `:region`-ed (they were on the AWS_REGION env default). Result: state.toml had no `region =` line and the next restart followed whatever the shell env pointed at *now*, feeling like ebman "forgot" the previous region. Switched to persisting the *effective* `context.region` (and analogously the effective profile). Restart now returns to the last-seen region regardless of how the user got there.
- **Frame consistency (G)** — every overlay border now flows through `rounded_block()`. Action confirm modal, action running modal, Detail Events/Instances/Queue/Logs tab outer frames, embedded shell pane, and the minimap previously used raw `Block::default().borders(Borders::ALL)` without rounded corners.
- **Caret glyph upgrade (H)** — new `caret_glyph(theme)` helper. Unicode + Powerline modes pick up `U+258E` (a thin vertical block that reads as a real terminal cursor) in place of the underscore. ASCII keeps `_`. Applied to all 10 blinking-cursor sites: command bar, filter bar, quick-jump bar, palette input, picker prompt, Detail Events search, DLQ purge confirm, action swap-target picker, Detail Logs filter, type-name terminate confirm. Test: `caret_glyph_falls_back_to_underscore_on_ascii`.
- **Toast accent stripe (F)** — every toast now gets a chunky `▎` severity-coloured stripe on the left edge of the body, Slack / VS Code notification-card style. Truncation budget bumped by 1 cell.

### 0.4.0 release (2026-05-22)

The feature batch built on top of 0.3.5, shipped as **0.4.0**
(`Cargo.toml` bumped, CHANGELOG `## [0.4.0]` written). Order of
landing:

- **Undo window extended to batch ops** (`4a6f8b2`) — `:batch-rebuild` /
  `:batch-restart` / `:batch-deploy` / `:batch-tag` / `:batch-untag` /
  `:batch-set-option` now route through the same 5s cancel window as
  single-env confirms from 0.3.5. `PendingDispatch` refactored into a
  kind-enum (`Single` + four `Batch*` variants); `cancel_pending_dispatch`
  drops the whole batch on `U`. Apps-scope per-app action menu's
  `BatchRebuild` / `BatchRestart` pick up the window for free. +2 tests.
- **Apps-scope multi-select + pin** (`80aee4e` + `274cec3`) — `space`
  toggles app in/out of `apps_selected`; `*` toggles pin into
  `pinned_apps` (persisted to state.toml's new `pinned_apps` key).
  Pinned apps sort to the top via `resort_applications()`. Per-row
  prefix: `★ ` pinned / `▶ ` selected / two-space gutter. Esc clears
  apps-selected when no envs-selected. Help-screen entries. +3 tests.
- **`:apps-info` overlay** (`2eb1114`) — surfaces app metadata that
  doesn't fit in the apps-table columns (description / created /
  updated / template count / env list). Resolves the target from
  cursor in either scope. Removes the `#[allow(dead_code)]` on
  `Application::date_created` (now consumed). Registry entry under
  `Category::Inspection`.
- **Cost Explorer integration** (`bfb33f4` + `8bf732c`) — opt-in
  `:cost on` adds a COST column to the env table showing $/month per
  env via Cost Explorer (`Tag: elasticbeanstalk:environment-name`,
  30d trailing). 24h on-disk cache at
  `~/.cache/ebman/cost-{account}-{region}.toml`. Cost Explorer
  client pinned to `us-east-1` (global service). Bucketed cell
  colours (green < $50, text $50-$500, red ≥ $500). `cost_enabled`
  persists in state.toml. `:cost status` shows cache age. +4 tests.
- **`:listeners` ALB config overlay** (`1aa3358`) — fetches the env's
  `aws:elbv2:listener:*` namespaces via DescribeConfigurationSettings
  and renders one block per port (default first, then numeric asc).
  Web-tier only — Worker envs error out. Visibility-only; edit
  follow-up tracked as task #111.
- **`:rds` dbinstance config overlay** (`23e9221`) — fetches
  `aws:rds:dbinstance.*` option settings and renders them.
  `DBPassword` always redacted to "(redacted)" regardless of the
  global `:redact` toggle. Empty-state shows a usage example for
  bare `:set-option`. Visibility-only; attach/detach follow-up
  tracked as task #110.
- **`:report-bug` overlay** (`737048d`) — operator-driven bug reports
  with no outbound HTTP. New `src/report_bug.rs` module builds a
  scrubbed payload (version / OS / icons / theme / last 30 log lines
  / last 10 on-screen messages / latest panic backtrace). Scrubber
  redacts ARNs, env names (longest-first), app names, CNAMEs,
  12-digit account IDs, profile name (skipping the generic
  "default"). Operator picks `y` (copy to clipboard) / `b` (open
  GitHub issue draft in browser, body pre-filled via URL params,
  truncated at ~7900 chars for the 8k limit). README "Privacy /
  telemetry" section documents the design. +8 tests.

**Follow-on landings (all in 0.4.0):**

- **`:options [NAMESPACE]` settable-option vocabulary overlay** (task
  #113) — closes the biggest console-parity gap. Two parallel SDK
  calls (`DescribeConfigurationOptions` for vocab + `DescribeConfigurationSettings`
  for current values), merged by `(namespace, name)`. Renders one
  block per namespace with `▸` (operator-set) / `•` (default)
  markers, default value, `value_type`, `change_severity`,
  `min`/`max`/`max_len`, and the first 5 `value_options` enums.
  Optional `NAMESPACE` arg filters; bare `:options` shows the full
  list (slow but exhaustive). +3 tests.
- **`:` Tab autocompletion** (task #114) — Tab inside `Mode::Command`
  cycles forward through registry matches; Shift-Tab cycles back.
  Origin fragment captured so repeated cycling restores the prefix
  cleanly on each press. Footer hint advertises the binding.
- **"Did you mean?" on unknown commands** (task #115) — Levenshtein
  distance against `commands::all_names()`; `:restrt` → "did you
  mean `:restart`?" toast. Distance threshold of 2 keeps false
  positives down. +2 tests.
- **First-run nudge** (task #116) — `state::file_exists()` check at
  boot sets `app.first_run_hint = true`; sticky footer row hints
  at `?` / `:` / `Ctrl-K` until first input clears it. Footer
  height grows from 2→3 only on first run. +1 test.
- **`:resources` hierarchical tree** (task #117) — replaces the
  flat dump with an indented ASG → instances → ELB → target-group
  tree (Worker envs show ASG → instances → queue tier). Pure
  `render_env_resources_tree` keeps the rendering testable;
  `describe_env_resources` refactored from `String`-returning to
  `EnvResources`-returning. +1 test.
- **`:explain` IAM diagnosis** (task #118) — `:explain` (no arg)
  scrapes the last `AccessDenied:` toast and runs
  `iam:SimulatePrincipalPolicy` for that principal+action;
  `:explain ARN ACTION` evaluates explicit pairs. Renders allowed
  / explicit-deny / implicit-deny rows with matched / missing
  statement IDs and an SCP/permissions-boundary blocker flag when
  the simulator surfaces one. +2 tests.
- **`:env-edit` bulk env-var editor** (task #122) — drops the alt
  screen, shells out to `$EDITOR` (defaults to `vi`) with the
  current env's vars rendered as `KEY=VALUE` lines, diffs on save
  via pure `diff_env_vars(before, after)`, dispatches the
  resulting OptionSettings update through the existing 5s undo
  window. New `PendingDispatchKind::Single` variant + `pending_env_edit`
  main-loop handoff so the terminal blocking happens off the
  tokio runtime. +3 tests.
- **`:secrets` + `:secret NAME` Secrets Manager browser** (task #123) —
  region-scoped browser for the bulk-edit flow above. `:secrets [FILTER]`
  paginates `ListSecrets` and renders metadata only (name / ARN /
  description / changed / rotated / KMS key) so an accidental
  `:secrets` never dumps credentials. `:secret NAME` is the
  opt-in value reveal — JSON-shaped values pretty-print via a
  dependency-free recursive walker; `:redact on` replaces the
  value with `<redacted; N chars, fingerprint XXXXXXXX>` using a
  non-crypto FNV-1a fingerprint so the operator can confirm
  "same secret as before" on a screen-share without leaking it.
  CloudTrail logs `GetSecretValue` on the AWS side; ebman additionally
  writes its own audit line at dispatch. +12 tests covering the
  empty states, metadata-only rendering, redact path, JSON
  pretty-printer (incl. strings-with-braces), and age buckets.

- **Event timestamp display modes** — Events panel + Detail/Events
  tab timestamps are now switchable between `Utc` (default —
  `YYYY-MM-DD HH:MM:SSZ`, matches the EB / CloudWatch API),
  `Local` (operator wall-clock), and `Age` (the prior compact
  `5m` / `2h` relative form). New `EventTimeFormat` enum cycles
  `Utc → Local → Age`; switchable via `:event-time [utc|local|age]`
  (no arg cycles) or the `T` key in both the main table and the
  Detail view. Persists to state.toml as `event_time_format`.
  Pure `format_event_time` + `event_time_width` keep both
  renderers aligned. +6 tests.

- **Events tab — scroll clamp + severity / time-window filters** —
  Three fixes to the Detail/Events tab:
  - **Scroll no longer runs off the bottom.** `draw_detail_events`
    now returns the max legal scroll offset (filtered line count
    minus body height); the renderer clamps the display offset and
    the `j`/`k` handler clamps `events_scroll` against the stored
    `events_max_scroll`. Same `help_max_scroll` pattern.
  - **Severity filter** — `L` cycles a minimum-severity floor
    (`all → info+ → warn+ → error`). `severity_rank` maps EB's
    `TRACE/DEBUG/INFO/WARN/ERROR` (+ `WARNING`/`FATAL` synonyms;
    unknown → INFO) to a comparable rank.
  - **Time-window filter** — `w` cycles a window
    (`all → 1h → 6h → 24h → 7d`); events older than the cutoff are
    hidden. Events with no timestamp always pass (can't be excluded
    by a cutoff they have no value for).
  - Both filters are client-side over the already-fetched event
    list (no re-fetch). Title shows `[shown/total]` + active
    filter labels; a dedicated empty-state fires when filters hide
    every event. `n`/`N` search-jump rewritten to walk the
    *filtered* set so jump targets stay valid. +6 tests.

- **Config tab — cursor navigation + in-place value editing
  (section 1 of a sectioned editor)** — The Config tab was a
  read-only paragraph dump. Now:
  - `j`/`k` / arrow keys move a `▶` cursor over the *editable* rows
    (tags + env vars); read-only metadata rows are skipped.
  - `enter` opens an in-place value editor on the selected row;
    `enter` saves, `esc` cancels. Key is fixed (value-only edit) —
    renaming is a later section. The editor is a real text field:
    Left/Right/Home/End move a char-indexed caret, Backspace/Delete
    act at the caret, and the caret renders at its position
    (multi-byte-char safe).
  - Commit dispatches through the existing `:env set`
    (`UpdateOptionSettings`) / `:tag` (`UpdateTags`) paths, so the
    audit log + in-flight pill + auto-refetch all apply for free.
    Unchanged values are dropped without a dispatch. (Note: those
    paths dispatch immediately — they do *not* go through the 5s
    `PendingDispatch` undo window, which today only wraps lifecycle
    `Action`s + batch ops. Wiring option-settings updates into it
    is a separate follow-up.)
  - New `ConfigItem` / `ConfigItemKind` / `ConfigEdit` types; pure
    `config_editable_items` builds the cursor's index space in the
    exact order the renderer draws (tags sorted case-insensitively,
    then env vars natural order) so navigation + render agree by
    construction. +4 tests.
  - Section (d) **scroll-follow** shipped: pure `config_scroll_follow`
    keeps the cursor inside the viewport on long lists.
  - Sections (a) **add-new-row** (`n` — inline `KEY=VALUE` editor,
    kind from the cursor's section) and (b) **delete-selected-row**
    (`x`, `y`-confirmed) shipped. Both dispatch through the same
    `UpdateOptionSettings` / `UpdateTags` paths. Pure
    `parse_new_config_row` parses the add buffer. Only **key
    rename** (section c) remains — and value-edit + add + delete
    already cover it the long way.

**Net for the 0.4.0 batch**: 309 → 392 tests. Shipped as 0.4.0.

**Follow-ups parked**

- Task #110 — RDS attach / detach modal form (snapshot+modify+wait
  orchestration for detach; 10-field attach form).
- Task #111 — ALB listener edit form (LB tab + ACM cert picker).
- Task #119 — Form-based edit for the top 3 config namespaces (the
  `:env-edit` flow handles env vars; the long-tail namespaces still
  need a modal form per family).
- Task #121 — Per-env runbook hint surfaced in `:why`.

### Post-0.3.0 UX punch list (2026-05-21)
Twelve UX fixes from the v0.3.0 critical review, shipped as one batch (tasks #92–#103):

- **`Action::wants_preflight()`** in `mode_action.rs` — single source of truth for the "show impact preview + last-3 events" gating. Replaces three duplicated allow-lists (`open_parameterised_action`, `advance_action_flow::Terminate`, `advance_action_flow::Rebuild` hand-roll). `Rebuild` now routes through `open_parameterised_action` like every other lifecycle action.
- **`:swap` routes through `open_parameterised_action`** — was building `ActionFlow::Confirm` directly with `loading_dryrun: false`, so `:swap candidate` from the command bar silently skipped the preflight that `a → Swap` runs. Added `swap_with` to `ParameterisedAction` to support the single entry path.
- **`Esc` clears multi-select in Normal mode** — the multi-select status message advertised "esc = clear" but Normal had no Esc handler; a silent footgun for operators who multi-selected and walked away.
- **Multi-select active pill** — persistent "▶ N selected" pill while `multi_selected` is non-empty. Replaces the one-tick status-message hint that disappeared on the next refresh.
- **Pill foreground colours through `theme.contrast_text(bg)`** — WCAG-luminance-based black/white picker. The chain was hardcoded `Color::Black` (with one `Color::White` outlier for alerts) which broke any non-dark theme. Light + high-contrast themes now render readable pills.
- **Pill priority + width-aware elision** — `prune_pills_to_width` trims trailing low-priority pills under width pressure and marks the survivor with `+N` so elision isn't silent. Ordered most-critical-first (alerts > pending > multi-select > read-only > update > SSO > frozen > redact > grouped > view-mode).
- **ASCII glyph fallbacks** — new `warn_glyph` / `hint_glyph` / `stripe_glyph` helpers gate `⚠` / `💡` / `▎` (plus pill `⏳` / `▶`) so `icons = "ascii"` no longer renders box-tofu in the pending pill, footer hints, warnings, and toast accent stripe. Twelve sites swept.
- **Detail-Health tab now shows alarms + recent deploys** — was missing two of the four sections `:why` shows. New `DetailState::cw_alarms` / `recent_versions` fields + `spawn_detail_alarms` / `spawn_detail_recent_versions` helpers + `AppMsg::DetailAlarms` / `AppMsg::DetailRecentVersions` variants populate them when the Health tab opens. Triage surfaces no longer disagree.
- **Help screen completeness** — ~40 commands added across new sections (Multi-account, Lifecycle actions, Env config, Versions/configs/alarms/platforms, Bulk ops, Setup/discovery). Previously stale by half the v0.3.0 surface.
- **`:capacity` in action menu** — `Action::Capacity` variant + menu entry; `a → Capacity` opens the modal form. Was command-bar-only in v0.3.0.
- **`flatten_err_to_string` token coverage** — adds `AccessDenied`, `NotFound`, `Conflict`, `ExpiredToken` prefix-classifiers alongside the existing `ThrottlingException` set. Operators bouncing profiles hit AccessDenied constantly; now it's a labelled prefix instead of a buried SDK chain.
- **`FROZEN` pill goes yellow after 5 min staleness** — frozen auto-refresh during an incident is operationally important to not forget. Pill now reads `FROZEN (stale)` against a yellow bg when `last_refresh` is more than 5 min old.
- **Empty-state hint at no-envs-match** corrected from `views` → `:views`; footer context-hint at `app.alerts > 0` points at `:why` (v0.3.0 triage tool) instead of the stale `:alarms` / `:org-health`.

**14 new tests** covering `prune_pills_to_width` (3), the ASCII glyph helpers (3), `theme.contrast_text` (3), and `flatten_err_to_string` error-code classifiers (5). 282 → 296 total.

### `execute_command` split: final cut — task #66 complete (2026-05-20)
- **Three closing sub-modules in one go**:
  - **`src/app/cmd_alarms.rs`** (168 lines) — `cmd_alarm_create`, `cmd_alarm_delete`. Both still emit `AppMsg::AlarmOp` so the pending pill closes + toast fires; `alarm_kind_to_metric` reachable via `super::*`.
  - **`src/app/cmd_config_template.rs`** (129 lines) — `cmd_config_save`, `cmd_config_delete`, `cmd_config_apply`, `cmd_config_inspect`. `:config-save` keeps its inline `create_config_template` path (the only template arm that doesn't already have a `spawn_*` helper); the other three thunk into existing `spawn_config_*` plumbing.
  - **`src/app/cmd_misc.rs`** (330 lines) — the remaining cluster: `cmd_custom_platforms`, `cmd_versions`, `cmd_delete_version`, `cmd_pending`, `cmd_resources`, `cmd_custom_platform_delete`, `cmd_metric`. `Overlay::TextDump` reachable via `super::Overlay`; `humanize_short_age` / `parse_metric_extra_args` (pub fns) and `flatten_err` / `write_audit_line` (private parent-module fns) all wired via `super::*`.
- **Net for this cut**: `app.rs` 13,023 → 12,478 (-545 this cut; **-1,799 cumulative** from the original 14,277). Ten sub-module files (`cmd_action` 224, `cmd_alarms` 168, `cmd_config_template` 129, `cmd_misc` 330, `cmd_nav` 124, `cmd_option` 231, `cmd_overlay` 289, `cmd_settings` 285, `cmd_view` 206, `cmd_write` 174) total 2,160 lines.
- **`execute_command` is now pure dispatch** — every previously-inline arm body lives in one of the ten sub-modules; the match site reads as a column of one-liners (`"alarm-create" => self.cmd_alarm_create(&rest)`, etc.). The stale "Remaining categories" comment on the `mod cmd_*;` block in `app.rs` is updated to describe the finished layout.
- 282 tests pass; clippy `-D warnings` clean.
- **Task #66 closed**.

### `execute_command` split: seventh cut (2026-05-20)
- **`src/app/cmd_settings.rs`** — seven structured per-env write arms (`:tag`, `:untag`, `:env [list|set|unset]`, `:capacity`, `:logs-stream`, `:notify`, `:managed-window`) lifted into named methods. The big ones: `:env`'s 65-line sub-command tree (list/set/unset), `:managed-window`'s day-of-week + hour normalisation, `:capacity`'s 4-field modal form construction.
- **Net so far across seven slices**: `app.rs` 13,281 → 13,023 (-258 this cut; -1,254 cumulative). Seven sub-module files total 1,533 lines.
- 282 tests pass; clippy `-D warnings` clean. Pattern fully stabilised — `flatten_err`, `format_env_vars`, `parse_tag_args`, `parse_named_arg` all reachable via `super::*` from the sub-module.

### `execute_command` split: sixth cut (2026-05-20)
- **`src/app/cmd_nav.rs`** — six navigation / view-state arms (`:region` / `:r`, `:account`, `:profile` / `:p`, `:sort`, `:group`, `:redact`) lifted. Region multi-region toggle (`:region all` / `off`) preserved verbatim. `:account` keeps its AssumeRole-vs-profile-alias branching. `parse_toggle` helper imported via `super::*`.
- **Net so far across six slices**: `app.rs` 13,368 → 13,281 (-87 this cut; -996 cumulative). Six sub-module files total 1,248 lines. 282 tests still pass; clippy clean.
- This cut is smaller (~87 lines off) because navigation arms were already pretty compact compared to the 200+ line arms in earlier cuts, but the dispatch site is now uniformly one-liners across the entire nav+view+option+action+overlay+bulk-write spectrum — only structured-write + misc remain.

### `execute_command` split: fifth cut (2026-05-20)
- **`src/app/cmd_action.rs`** — eleven lifecycle action arms (`:deploy`, `:upgrade`, `:clone`, `:scale`, `:stop`, `:start`, `:abort`, `:rebuild`, `:restart`, `:terminate`, `:swap`) lifted into named methods. Most route through the existing `open_parameterised_action(action, ParameterisedAction { … })` path; `:terminate` keeps its strict-typed-name guard via the action menu; `:swap` builds the `ActionFlow::Confirm` directly because the swap shape doesn't fit `open_parameterised_action`'s API.
- **`:deploy`** preserves the two-form structure: legacy `:deploy LABEL [--preview]` and `:deploy --from PATH | s3://… [--label] [--describe] [--no-deploy]`. The path discriminator stays in the lifted method.
- **Net so far across five slices**: `app.rs` 13,552 → 13,368 (-184 this cut; -909 cumulative). Five sub-module files total 1,124 lines. 282 tests still pass; clippy clean.

### `execute_command` split: fourth cut (2026-05-20)
- **`src/app/cmd_option.rs`** — eleven per-option-settings arms (`:deployment-policy`, `:rolling-update`, `:health-check-url`, `:keypair`, `:service-role`, `:instance-profile`, `:public-ip`, `:elb-scheme`, `:set-option`, `:unset-option`, `:instance-type`) lifted into named methods. Each calls `spawn_option_settings_update` after its own canonicalisation / validation; the arms varied only in (namespace, name, value-shape) so lifting them turns a 200-line wall of repetitive `match rest.first().copied()` into a column of one-liners.
- **Net so far across four slices**: `app.rs` 13,743 → 13,552 (-191 this cut; -720 cumulative). Four sub-module files total 900 lines. 282 tests still pass; clippy clean.
- **`Some(s @ ("public" | "internal"))`** in `:elb-scheme` no longer needs the redundant `if s == "public"` re-mapping — the captured `s` already holds the matched string, removing 1 line of dead binding.

### `execute_command` split: third cut (2026-05-20)
- **`src/app/cmd_view.rs`** — view / filter / column management arms (`:cols`, `:save-view`, `:view`, `:views`, `:view-drop`, `:filter` / `:f`, `:save`, `:drop`, `:filters`) lifted into nine methods. All pure-state, no AWS, no async — lowest-risk slice yet. 162-line `:cols` arm dropped to one line.
- **Net so far across three slices**: `app.rs` 13,894 → 13,743 (-151 this cut; -529 cumulative). Three sub-module files total 669 lines. 282 tests still pass; clippy clean.
- **`encode_view` / `apply_view`** — private free functions in `app.rs` accessed via `super::*` from the sub-module, same visibility-via-descendants trick as `flatten_err_to_string` etc.

### `execute_command` split: second cut (2026-05-20)
- **`src/app/cmd_write.rs`** — bulk write-side arms (`:batch-rebuild`, `:batch-restart`, `:batch-deploy`, `:batch-tag`, `:batch-untag`, `:batch-set-option`) lifted into four methods (`cmd_batch_action`, `cmd_batch_deploy`, `cmd_batch_tag_or_untag`, `cmd_batch_set_option`). The union arms in `execute_command` collapse from 165 lines to 6 one-liners; `cmd == "batch-rebuild"`-style dispatch becomes an `Action` enum parameter passed in from the call site, cleaner than the in-arm string-check.
- **Net**: `app.rs` 14,052 → 13,894 (-158); `cmd_overlay.rs` 289 + new `cmd_write.rs` 174 = 463 lines in sub-modules. 282 tests still pass; clippy clean.
- **Same pattern as cmd_overlay** — private `spawn_batch_*` helpers stay in app.rs and remain reachable from the sub-module via parent-module visibility. `parse_tag_args` (pub fn) imported via `super::*`.

### `execute_command` split: first cut (2026-05-20)
- **`src/app/cmd_overlay.rs` extracted** — first slice of the long-pending `execute_command` refactor (task #66). The three heaviest multi-account-overlay arms (`:accounts`, `:org-health`, `:find-env`) — ~225 lines combined — moved into `impl App { … }` methods (`cmd_accounts`, `cmd_org_health`, `cmd_find_env`) in a new `app::cmd_overlay` sub-module. The dispatch arms in `execute_command` become one-line method calls.
- **Sub-module pattern**: `mod cmd_overlay;` declared inside `src/app.rs` resolves to `src/app/cmd_overlay.rs`. The new file's `impl App` block accesses App's private fields and methods via the parent-module visibility rule (private = visible within the defining module + descendants). `flatten_err_to_string`, `format_org_accounts`, `AppMsg`, and `crate::config::AccountSpec` imported via `super::*` paths.
- **Why these three first**: they're the heaviest overlay-only arms (each 50-100+ lines of `tokio::spawn` orchestration) and end at `tx.send(AppMsg::TextOverlay)`, so the refactor doesn't change any synchronous state transitions — lowest blast radius for the first cross-module split.
- **Net effect**: `app.rs` -225 lines (~14,277 → ~14,052); 282 tests still pass; clippy `-D warnings` clean. Pattern proven; the remaining write-side, navigation, and misc categories can follow the same shape in dedicated follow-ups.

### Organizations discovery: `:accounts` (2026-05-20)
- **`:accounts` overlay** — new command lists every child account in the active AWS org via `organizations:ListAccounts`. New `aws-sdk-organizations` dep; new `OrgAccount { id, name, email, status }` type; new `OrgClient` field on `AwsClient` (initialised in every constructor path including `with`, `assume_role`, `for_tests`). `list_org_accounts` paginates via `next_token`, sorts ACTIVE-first then by name, surfaces AccessDenied separately so the overlay can show a "no org access" hint with a config-toml workaround instead of an opaque SDK error.
- **Pure `format_org_accounts(accounts, configured)`** renders the overlay body. Each row: status marker (`●` ACTIVE / `⊘` SUSPENDED / `○` other) + name + 12-digit id + status. Email shown as a sub-line when populated. Most importantly: when a matching `accounts.NAME` entry exists in `config.toml` (matched on friendly name OR 12-digit id), the row gets a `:account NAME` suffix telling the operator exactly which keybind switches into it. Operators with no matching entry see informational data only and are pointed at the config workaround.
- **Switch-hint matching is case-insensitive name-or-id** so operators who key their `accounts.*` entries by account-id (e.g. `accounts.111122223333`) still get the hint. 3 tests cover happy-path-with-hint, empty-result hint, and id-based matching.
- **No interactive picker yet** — the overlay is read-only TextDump. Adding `Enter → :account NAME` requires the auto-AssumeRole-by-default-role path; logged for a follow-up. The current flow (configure `accounts.NAME` once, then `:account NAME` to switch) is fine and explicit.

### Cross-account `:find-env` (2026-05-20)
- **`:find-env` now scans AssumeRole accounts** — symmetric with the multi-account `:org-health` ship from earlier. Fans out over profile sources (existing) AND `accounts.NAME` entries via boxed dynamic futures into a single `join_all`. Hit lines for AssumeRole accounts carry the `(assume-role)` suffix so the operator can spot which credential path each hit came from. Status message updated to count both: `searching 'foo' across N profile(s) + M assume-role account(s) in REGION…`. Closes the Tier 4 AssumeRole-everywhere loop (switcher + org-health + find-env all consistent).

### Multi-account `:org-health` (2026-05-20)
- **`:org-health` now walks AssumeRole accounts too** — previously the fan-out only walked `~/.aws/{config,credentials}` profiles via `list_environments_in_region`. Now it also fans out across every `accounts.NAME` in `config.toml`, calling the new `aws::list_environments_for_account(name, &spec, Option<region>)` which assume-roles then lists. One ebman instance in the mgmt account surveys every child account in a single pass.
- **Unified rendering** — both kinds (profile + assume-role) feed into a single `join_all` via boxed dynamic futures. Assume-role rows get a `(assume-role)` suffix in the overlay so the operator can tell the kinds apart. Totals aggregate across both. Title bumped to "one row per profile / assume-role account".
- **Status message** updated to count both — `scanning N profile(s) + M assume-role account(s) in REGION…`.
- **Follow-up** (still open): extend `:find-env SUBSTRING` to also scan AssumeRole accounts. Same pattern; small.

### CW metric batching test + AssumeRole account switcher (2026-05-20)
- **CW batching mocked test** — `fetch_env_metrics_batches_and_reorders_by_canonical_id` pins four contract guarantees: (1) `fetch_env_metrics` dispatches exactly ONE `GetMetricData` call (batched, not fan-out), (2) all 4 canonical ids — `health` / `req4xx` / `req5xx` / `p90` — are requested, (3) the returned `Vec<MetricSeries>` is in canonical order even when AWS shuffles the response (which it has been known to do), (4) per-id labels map correctly. New `client_with_cw(cw)` helper extends the test-fixture family. Closes the last open mocked-AWS coverage gap.
- **AssumeRole account switcher** — new `AccountSpec { role_arn, source_profile?, external_id?, region? }` type + `Config.accounts: HashMap<String, AccountSpec>`; parsed from `accounts.NAME.field = "value"` lines in `config.toml` (mirrors the existing `metric.LABEL.field` shape, no TOML section parser needed). New `AwsClient::assume_role(target_name, &spec)` calls `sts:AssumeRole` with `source_profile`'s creds as the launchpad, captures the returned temp credentials, and builds a fresh `SdkConfig` carrying ONLY the assumed-role identity (no leaked source creds). New `aws-credential-types` dep (1 line in Cargo.toml; transitive via aws-config already).
- **`:account NAME` dispatch** — branches in two ways: (1) `accounts.NAME` configured → AssumeRole flow via new `spawn_assume_role_switch` (lands as `AppMsg::Rebuild`, same swap path as `:profile` so overlay tear-down / throttle reset / identity refresh are free), (2) otherwise legacy fallback to `:profile NAME` aliasing. The two paths coexist so operators with one-profile-per-account in `~/.aws/config` keep working.
- **Context breadcrumb** treats the friendly account name as the "profile" so the header reads `account=prod` rather than the source profile name. Account_id + caller_arn get filled in once `verify_identity` runs against the new client (existing path).
- **Session lifetime** defaults to AWS's 1h cap; the operator's refresh tick re-invokes when the session dies. No background refresh today — `spawn_assume_role_switch` is invoked again by `:account NAME` repeatedly if needed. Auto-renewal is a follow-up.
- 2 config-parse tests (`parse_accounts_collects_multiline_specs`, `parse_accounts_ignores_unknown_field`) lock the schema.

### UI integration test harness (2026-05-20)
- **`App::for_tests(aws, cfg)` constructor** — synchronous, no AWS round-trip, no disk read, no spawn_identity / spawn_refresh kickoffs. Builds the full App struct with sensible defaults so tests start from a known clean state and can mutate any field directly (struct is `pub`, fields are `pub`). Pair with `AwsClient::stub()` — a new `#[cfg(test)] pub` helper on `AwsClient` that returns a no-mocks client; AWS calls against it fail loudly, which is the signal we want for "test accidentally hit the network".
- **Harness pattern** inside `app::tests`: `test_app()` builds a fresh App; `press(&mut app, KeyCode, KeyModifiers)` synthesizes a `KeyEvent::Press` and dispatches via `handle_event`; `render(&mut app, w, h)` renders into a `TestBackend`-backed Terminal and returns the flattened buffer as a string for grep-style assertions. `mk_env(name, app, tier, health)` seeds the env list without going through async fetchers.
- **7 demo tests** cover the load-bearing keyboard flows: `tab_cycles_scope_envs_to_apps_and_back`, `question_mark_opens_help_and_escape_dismisses_it`, `colon_enters_command_mode_and_esc_cancels`, `slash_enters_filter_mode_and_text_lands`, `enter_on_red_env_opens_why_via_bang_keybind`, `render_main_table_includes_seeded_env_name`, `ctrl_x_toggles_redact`. These exercise: mode transitions, key precedence, overlay open/close, text-input mode, render-through, modifier handling.
- **Catches the regressions the pure-helper tests don't**: filter-input states, picker-vs-overlay precedence, mode transitions, overlay-shape-vs-dispatch desync. Build once, scales for every new key / overlay — adding a test for a new keybind is now `press(...); assert_eq!(app.X, Y)`.

### Drillable Health tab (2026-05-20)
- **Cursor on the Health tab** — j/k now walks the interactive items (severity-filtered events, severe instances, main/DLQ queue rows for workers); Enter drills based on item kind. New `pub enum HealthItem { Event{event_idx}, Instance{instance_idx}, MainQueue, Dlq }` and pure `health_items(detail, now) -> Vec<HealthItem>` enumerate the navigable items in render order. Both the renderer and the Enter dispatcher read from the same helper so a refresh that adds/removes items keeps the cursor position predictable.
- **Drill behaviours**: Event → opens the full message in a TextDump overlay (some EB events are multi-line so this gives operators readable text without scrolling the truncated Health row); Instance → switches to the Instances tab and seats the cursor on that instance (operator then has Enter / `i` / `s` / `y` / `x` for per-instance ops); Main/DLQ queue → switches to the Queue tab and positions the queue cursor on the corresponding row (Enter again opens the queue viewer).
- **Cursor glyph** uses the existing `cursor_marker(theme)` — `▌ ` in Unicode / ASCII, `\u{e0b0} ` in Powerline. Inactive item rows get two-space padding so cursor / non-cursor rows align. `detail_scroll` for the Health tab wraps the cursor over `health_items(detail).len()`; rem_euclid means j past the last item loops back to the first.
- **Footer keystrip** for the Health tab now reads `HEALTH  j/k move  enter drill  tab→ Events  a actions  ^R refresh  ? help  esc back`.
- **General principle** going forward: any rendered list in any view should be navigable + drillable. Health-tab implementation is the first sample.
- 6 new tests on `health_items`: event-severity filtering, 30-min recency window, per-3-instance cap, worker-only queue rows, Web tier skips queues, render-order matches operator view.

### Updating-kind classification + alert-aware Ready pill (2026-05-20)
- **`Ready` pill muted on alerting envs** — when an env's health is Red/Severe OR it's a Worker with `DLQ > 0`, the STATUS-column `Ready` pill renders as dim "Ready" text instead of the bright green pill. `Ready` per EB means "no lifecycle op in flight", NOT "everything's fine"; muting it stops the green pill from competing with the health-dot / row-tint / `⚠N` chip for the operator's attention. New `status_pill_for(status, theme, muted)`; `status_pill(...)` is now a thin wrapper that defaults `muted=false` for callers (Detail header etc.) that don't track alerting state. Updating / Terminating pills are unaffected — they already signal "something happening".
- **Updating status blinks** — the Updating/Launching pill picked up `Modifier::SLOW_BLINK` so in-flight lifecycle ops draw the eye away from idle rows. Modern terminals support it; legacy ones silently fall back to a static pill.
- **`classify_update_kind(events)` pure helper** — EB's `status` is generic ("Updating") regardless of cause, but the recent events expose what's happening. Returns a `UpdateKind` enum: `Deploy { version_label }`, `Config`, `Scale`, `Platform`, `Generic`. Walks events newest-first (matches the EB API order), returns the kind from the first matching message. Deploy extracts the version label from `'…'`-quoted strings (`Updating environment to use version label 'build-142'`). 8 unit tests cover each kind, label extraction, label-missing fallback, empty events, and ordering (newest match wins).
- **Health-tab annotation** — when status is Updating, the Health tab's status line gains a `→ deploying build-142` / `→ config change` / `→ scaling instances` / `→ platform update` suffix in `theme.status_updating` bold. Generic (unrecognised events or events not yet loaded) suppresses the suffix rather than guessing.

### Health tab (default Detail landing) (2026-05-20)
- **`DetailTab::Health` as the default tab** — pressing Enter on an env now lands on a rollup view rather than the Events tab. The Health body shows: (1) status pill + health dot + worker-DLQ chip on the top line; (2) recent ERROR / WARN events from the last 30 min (top 10); (3) instance summary with per-colour counts + inline detail for Severe rows (top 3, with up to 2 causes each); (4) main + DLQ queue depths for Worker envs (DLQ tinted red when > 0). Closing line points the operator at the per-source tabs for drill-in.
- **Data sources reuse existing fetchers** — `detail_refresh_active_tab` spawns events + queues on Health-tab visit; instances are eagerly fetched on `open_detail` so the summary is already populated by the time the user sees the tab. No new aws.rs surface required.
- **Tab icons**: `♥` (Unicode) / `\u{f02d1}` heart-pulse (Powerline) / `H` (ASCII). New per-tab keystrip line in the footer. Detail-scroll arm has Health alongside Metrics/Config as "no scroll cursor".
- **Companion to `:why`** — the in-app `:why` overlay still works (and now has its richer worker-queues + DLQ-peek section), but the Health tab is the default visual landing so the operator gets triage context before navigating; `!` still pops the overlay on demand from anywhere.

### Worker DLQ feeds Red alerts (2026-05-20)
- **DLQ-aware Red status check** — `apply_refresh` now fans out `describe_worker_queues` for every Worker-tier env via the new `spawn_worker_queue_check`. Results land as `AppMsg::WorkerQueueCheck { gen, results: Vec<(env, dlq_visible)> }`; the handler rebuilds `App.worker_dlq_depths` from scratch (so DLQs that drained back to zero reflect on the next draw) and recomputes the alert count.
- **New pure `compute_red_alerts(envs, dlq_depths)`** combines EB-health-Red + Worker-with-DLQ>0; a worker that's both is counted once. 3 unit tests cover the EB-only, Worker-DLQ-only, Web-with-spurious-cache-entry, and zero-DLQ cases.
- **Visual surfacing**: Worker rows with `dlq > 0` tint with `theme.row_red_bg` even when EB reports Green — distinctive "EB thinks it's fine but DLQ disagrees" look. STATUS column appends a small `⚠N` chip (3 cells) so the operator can spot the DLQ count without opening the Queue tab.
- **`:why` worker-queues section** — `Overlay::WhyRed` gained `tier`, `queues`, `dlq_messages` fields. For Worker-tier envs, `open_why_red` spawns a 5th fetcher (`describe_worker_queues`); the handler kicks a second-stage `peek_messages(dlq_url, 3)` only when DLQ depth > 0, so healthy workers don't pay the SQS visibility-timeout cost. Renders a new "worker queues" section in the overlay: main + DLQ stats (visible / in-flight / delayed), DLQ counts tinted red when > 0, and a peek of up to 3 DLQ message bodies (truncated to 100 chars) with sent-age + receive-count. Web envs skip the section entirely. Two new AppMsg variants (`WhyRedQueues`, `WhyRedDlqMessages`); all gated on `session_id` so reopening on a different env drops late results.

### Bulk ops + per-profile theme + deploy preview (2026-05-20)
- **Bulk operations** — `:batch-deploy LABEL`, `:batch-tag KEY VALUE`, `:batch-untag KEY`, `:batch-set-option NAMESPACE NAME VALUE` over the existing multi-select set (`space` to toggle). Each dispatches per-env in parallel via a dedicated `spawn_batch_*` helper that funnels through the same pending-pill + audit + `AppMsg::{ActionResult, TagUpdate, OptionSettingsUpdate}` paths as the single-env commands, so toasts / read-only gating / audit-log entries are free. Pre-flight validations: `:batch-deploy` refuses if the selection spans more than one application (the label can't possibly resolve across apps); `:batch-tag` skips envs whose ARN isn't loaded yet and reports the skipped names in the status footer.
- **Per-profile theme override** — new `profile_themes = "prod:high-contrast,staging:dark"` key in `config.toml` parses to a `HashMap<String, String>` on the App. New `maybe_apply_profile_theme()` swaps `self.theme` to the override (or back to the base) whenever `self.context.profile` changes — called from `apply_rebuild` (every `:profile` / `:account` / `:region` switch) and once at App::new bottom so the initial frame is already correct. Theme swap clears `cached_app_colors` so the palette regenerates cleanly. `base_theme_name` field tracks the configured baseline separately from the running theme so `current_config_snapshot` (used by `:settings`) doesn't accidentally persist a profile-overridden theme as the new default. Pure `parse_profile_themes` helper with 4 tests (happy path, malformed/blank skipping, empty input, end-to-end via `parse`); serialize round-trip test extended.
- **`:deploy LABEL --preview`** — opens a TextOverlay showing `env`, `current` version + age, `candidate` version + age + description, and a `⚠ rollback` warning when the candidate predates the current version. Settings-diff would be the natural ask but EB application versions don't carry option settings (settings live on the env), so the preview is "informed deploy" rather than "settings drift". Pure `format_deploy_preview` helper with 3 tests (happy path, rollback warning, unknown-label).
- **`:why` / `:diagnose` unified diagnostic overlay** — single command opens a four-section scrollable overlay aggregating the data an operator needs during triage of a Red env:
  - **Recent events** — last 30 min from `list_events_for_env`, severity-tinted (ERROR red, WARN yellow), top 15 entries.
  - **Alarms** — `list_alarms_for_env` sorted ALARM-first, then INSUFFICIENT_DATA, then OK; state reason rendered as a sub-line for active alarms. Top 10 entries.
  - **Instance health** — `list_instances` with per-instance health colour + causes; up to 3 cause lines per instance.
  - **Recent deploys** — `list_application_versions` top 5, label + relative age + description (truncated to 60 chars). Age suffix uses the same three-bucket `age_color` as the apps view.
- New `Overlay::WhyRed { env_name, events, alarms, instances, deploys, session_id }` variant with each section as `Option<Result<…, String>>` — `None` renders as `fetching…` placeholder; results stream in via four parallel tokio tasks (`spawn_why_red_{events,alarms,instances,deploys}`). Stale-session guard: `why_red_session` counter bumps on each open; late results for a prior invocation drop on arrival.
- Four new `AppMsg::WhyRed{Events,Alarms,Instances,Deploys}` variants carry per-section results.
- New `truncate_for_display(s, max)` pure helper for the deploy-description column; 4-case test (under/at-cap/over-cap/multibyte).
- Discoverability: `:why` / `:diagnose` in `BUILTIN_COMMANDS`, palette description, per-context help line. Bound to `!` in Normal mode + envs scope so the operator can open the diagnostic with one key on the selected row.

### Apps view + header / table polish (2026-05-20)
- **`LATEST` column in the apps view** — new `Application.latest_version_label` + `latest_version_created`. `spawn_app_latest_versions` fans out `DescribeApplicationVersions` per app in parallel via `join_all` once the apps list lands; results merged by name. UPDATED stays for the AWS-metadata timestamp (description / templates / lifecycle); LATEST shows the actual newest version label + relative age. Pure `merge_app_latest_versions(prev, next)` carries values across refreshes so the column doesn't flicker to "—" on every refresh tick — and only fills slots that are currently `None`, so a hypothetical pre-populated `next` isn't stomped. Tests: `merge_app_latest_versions_carries_previous_values_by_name`, `merge_app_latest_versions_does_not_overwrite_already_populated_slots`, `merge_app_latest_versions_handles_app_disappearance`.
- **Highlighted-row contrast preserved** — `Table::row_highlight_style` switched from `.bg(row_selected_bg)` to `Modifier::REVERSED | BOLD` in both `draw_table` and `draw_apps_table`. Pill cells (Worker yellow / Ready green) now keep their colour identity on selection — fg/bg swap to "yellow text on black bg" rather than getting masked by the dark selection bg. Plain text cells get a standard terminal-style inversion as the selection cue.
- **Header pill chain merges onto info row when wide enough** — new `header_layout(app, area_width) -> (rows, merge_pills)` decides per-frame whether the contextual pill chain (`! 1 alert`, `SSO 12m`, in-flight, etc.) fits alongside `Sort · Status · Envs · Last · Caller` on line 2. Wide terminals collapse to 5 header rows; narrow terminals keep the dedicated chain row so pills never clip. Pure `header_dimensions(info_w, chain_w, inner_w, has_filters)` is the testable kernel; `build_chain_pills` extracted as a pure builder so layout + render agree on the chain. Tests: 5 covering merge / split / no-pills / filters-row / boundary.
- **AGE column colour tinting** — three-bucket tint via pure `age_color(updated, now, theme)`: fresh (<24h) gets `title_alt` to pair with the `◆` drift glyph, normal (1–30d) gets `text` (promoted from muted), stale (>30d) keeps `muted`. Clock-skew durations (negative) treated as fresh, not stale. Tests: 6 covering all three buckets, missing, future-clock-skew, and the 24h boundary.
- **Group-separator banner in non-Powerline mode** — previously the per-app divider row in Unicode/ASCII mode was a homogeneous 200×`─` fill with no app name and no visible break. Now: NAME cell shows `── ▶ {app-name} ──` with the app's colour for the chevron + name and `theme.muted` for the dashes; the second cell carries the existing `summarize_group` summary; remaining cells keep the dash fill so the row still scans as a divider. Powerline mode keeps its E0B2/E0B0 ribbon banner. Pure `separator_glyph(icons)` picker (`>` ASCII, `▶` otherwise) with one test.
- **Powerline splash pills** — `font_probe::resolve_icons_setting` runs before `draw_splash`, so the splash now knows whether the user has a Nerd Font and can use PUA glyphs without risk of tofu on first launch. Tagline + byline render as rounded-cap pills (`\u{e0b6}` left + body bg + `\u{e0b4}` right) in Powerline mode, with the tagline prefixed by `\u{f0c2}` (fa-cloud, stable across Nerd Font releases). Unicode / ASCII keep the existing plain-text lines. `draw_splash(terminal, frame, icons)` signature extended with the icons setting; captured in `main.rs` before `cfg` is moved into `App::new`.
- **Powerline splash card tab** — Powerline mode now embeds a `\u{e0b6} v{VERSION} \u{e0b4}` rounded-cap pill on the splash card's top border (centre-aligned via `Block::title_alignment`) so the whole card reads as a labelled tab. A first attempt at swapping the N letter's stair-step diagonal for `\u{e0be}` slants was reverted — the half-cell wedge against full `█` blocks read as a broken / floating stroke rather than a smooth angled edge. Real letter-diagonal smoothing needs visual prototyping in a real terminal before re-attempting.
- **Tab-icon cell-width probe** — `font_probe` already probes `U+E0B0` for the Powerline triangle; extended with a second probe for `U+F048B` (mdi-server, the codepoint used by the `Instances` tab icon — representative of the whole Nerd Font MDI block used by `tab_icon`). When `icons = "auto"` resolves to `"powerline"` but the MDI probe fails, `resolve_icons_setting` logs a `tracing::warn!` pointing at the tab-strip misalignment with a suggested fix (install a Nerd Font or pin `icons = "unicode"`). Advisory only — the rest of Powerline mode still works. Pure `classify_auto(powerline, tab_icons) -> AutoResolved` decision is unit-tested for all 4 cases.
- **Logs auto-open reverted + group picker** — the auto-open of the CW Logs streaming overlay on Logs-tab entry (shipped earlier as task #69) was confusing because it jumped past the tab's own snapshot path. Reverted in `detail_cycle_tab` — `s` is back to being the explicit opener. To make group choice discoverable, `Tab` inside the streaming overlay now opens a `PickerKind::LogGroup` picker over the env's discovered `cw_log_groups`; selecting one calls `spawn_logs_tail(env, Some(group))` which aborts the existing poller and reopens the overlay against the chosen group. The event dispatcher now skips overlay key handlers when `Mode::Picker` is active so the picker's keys aren't swallowed by the underlying LogTail overlay. Footer hint + per-context help updated.
- **Lazy apps-versions fetch** — `spawn_app_latest_versions` no longer fires from every `AppMsg::Applications` landing. The fan-out happens only when `self.scope == Scope::Apps`, so accounts where the operator lives in the envs view all day don't pay N extra `DescribeApplicationVersions` calls per refresh tick. New `set_scope(new)` helper kicks the fetch on demand when transitioning Envs → Apps (Tab / BackTab), so the LATEST column populates on entry rather than waiting for the next periodic refresh. Persisted-via-saved-view scope=apps still works — first refresh tick lands and triggers the fetch since scope is already Apps at that point.
- **Apps view age tinting** — applied the existing three-bucket `age_color` to the CREATED / UPDATED / LATEST cells in the apps view so the stale / active / fresh signal reads consistently with the envs table. LATEST's "  Xh ago" suffix uses `age_color(latest_version_created, …)` (separate from the bold version label).
- **Throttling-error contract test + flatten fix** — new aws-smithy-mocks test `list_environments_throttling_error_is_recognised_by_predicate` mocks `DescribeEnvironments` returning a `ThrottlingException`-coded error and asserts the full path (SDK error → `flatten_err_to_string` → `is_throttling_error`) recognises it. Caught a real bug: `eyre!("OP failed: {e}")` *flattened* the SDK error chain so the structured `ThrottlingException` code never reached the predicate — refresh back-off would have stayed disarmed on real throttling. Fixed two ways: (a) `flatten_err` now also peeks at the eyre `Debug` form for known rate-limit tokens and surfaces a clean `"ThrottlingException: …"` prefix on the user-facing string (so toasts stay readable but predicates fire); (b) `list_environments` migrated from `map_err(|e| eyre!(…))` to `wrap_err(…)` so the SDK error stays the source of the eyre Report and its Debug dump (with code metadata) appears in the chain. **Limitation:** the other ~38 `map_err(|e| eyre!(…))` sites in aws.rs still flatten — back-off only fires for the refresh path today. Migrating them is a small mechanical follow-up.
- **`:deploy --from` multi-stage mocked test** — new `deploy_from_path_chain_dispatches_each_stage` exercises the four-stage flow (CreateStorageLocation → S3 PutObject → CreateApplicationVersion → UpdateEnvironment) in one test. Each mock asserts the upstream stage's output threaded into the downstream stage's request — bucket+key from CreateStorageLocation reaches PutObject + CreateApplicationVersion, version label reaches UpdateEnvironment. `num_calls()` asserts each rule fired exactly once. New `client_with_eb_and_s3(eb, s3) -> AwsClient` helper extends the existing `client_with_*` family. This is the most multi-step pure-AWS path in the project — a refactor that drops or reorders a stage now fails loud.
- **`map_err(|e| eyre!(…))` → `wrap_err(…)` across aws.rs** — all 38 remaining sites migrated in one mechanical pass via a one-shot Python script. Each `.map_err(|e| eyre!("OP failed: {e}"))?` becomes `.wrap_err("OP failed")?`; one site with runtime interpolation (`S3 PutObject {bucket}/{key} failed`) became `.wrap_err_with(|| format!("…"))?`. Effect: SDK error chains are preserved as eyre Report sources across every AWS operation, so `flatten_err_to_string`'s Debug-peek for throttling tokens now fires on all paths (not just the refresh / `DescribeEnvironments` path) — `:deploy`, `:tag`, `:scale`, `:logs-tail`, etc. all install the back-off horizon on rate limits.
- **Expired-token surfacing test** — new `list_environments_expired_token_surfaces_clean_user_message` mocks `ExpiredTokenException`-coded `DescribeEnvironments` failure; asserts (a) `is_throttling_error` does NOT fire (expired ≠ rate-limit), and (b) the user-facing toast string stays free of SDK Debug noise (`StatusCode`, `Extensions`, `SdkBody`). Pins a known shape for the auth-failure path so a future SDK stringification change can't silently dump the whole Debug dump into the toast.
- **`:history` overlay account-context header** — `format_message_log` now prepends `context: account=… · profile=… · region=…` before the recent-messages list so the operator can see, when scanning toasts after `:account` / `:profile` / `:region` switches, which account the messages were emitted under. Account is redacted with full-block shaded chars when `redact` is on. New pure `redact_for_log` helper (duplicates the ui module's private version to avoid an unrelated cross-module change); test covers the four paths (redact-on / redact-off / em-dash placeholder / empty).

### Distribution + remaining bits
- **Custom Platforms list**: `:custom-platforms` (alias `:platforms`) fetches `ListPlatformVersions` filtered to `PlatformOwner=self` and surfaces ARN / branch / version / status / lifecycle in an overlay.
- **GitHub Actions release workflow**: `.github/workflows/release.yml` triggers on `v*` tags, builds `x86_64-unknown-linux-gnu` / `aarch64-apple-darwin` / `x86_64-apple-darwin` release binaries, tarballs each with README + LICENSE files, attaches them + SHA-256 checksums to a draft GitHub Release.
- **Homebrew formula template**: `Formula/ebman.rb` installable via `brew install --formula ./Formula/ebman.rb`. The `sha256` fields are stubs — maintainer will need to bump them per release (the release workflow emits the checksums alongside each tarball).
- **`cargo install` smoke test**: verified locally that `cargo install --path . --locked` builds and produces a `--version`-reporting binary on stock toolchain. The crates.io publish step is still maintainer-driven.

### Architecture + code quality (2026-08-21)
- **`app.rs` decomposed: 22,648 → 2,981 lines.** Four passes, all behaviour-preserving, green (`clippy --all-targets -D warnings`, 944 tests) at each step.
  1. *Bulk out of the god-file* — `tests.rs` (6.8k), `types.rs` (918), and seven pure-logic modules: `render.rs` (overlay body renderers), `text.rs` (string/parse/format helpers), `config_diff.rs`, `deploy_math.rs`, `cost.rs`, `env_edit.rs`, `saved_views.rs`.
  2. *`impl App` split across 24 modules* — `input.rs` (keymap + mouse), `dispatch.rs` (the `:command` router), `action_flow.rs`, `spawn_refresh.rs`, `spawn_deploy.rs`, `spawn_tail.rs`, `forms.rs`, `view.rs`, `detail_nav.rs`, `config_edit.rs`, `palette.rs`, `open_overlay.rs`, `apps_menu.rs`, `export.rs`, `shell_session.rs`, `safety.rs`, `cmd_inspect.rs`, `cmd_ops.rs`, `cmd_cost.rs`. What's left in `app.rs` is the struct, construction, the event loop, toasts, and state persistence. The `commands::tests` drift detector now parses `app/dispatch.rs`.
  3. *`ViewState`* — see the entry below.
  4. *Module doc* — `app.rs` opens with the three compiler-unenforced invariants and a categorised module map instead of accreted header comments.
- **`ViewState` makes the stale-cache rule structural.** `App`'s eleven view fields moved into `src/app/view_state.rs`. The derived slices (`filtered` / `display` / `app_colors` / `stale_platforms`) are private; reading one asserts the cache was rebuilt since its inputs changed (`debug_assert` + `tracing::error` in release — panicking inside the alt screen is worse than one wrong frame). `filter` and `grouped` are private too, and their only mutators mark the cache stale, so you can't forget. Inputs `ViewState` doesn't own (`environments`, `aliases`, `latest_stacks`, the theme palette) call `invalidate()` explicitly. `store()` is the only thing that clears the flag and `rebuild_view` is its only caller. **Found a real bug:** `:alias` / `:alias-drop` mutate the map `rebuild_view` matches the filter against but never rebuilt — with an active filter, aliasing an env left the table showing stale rows. Both fixed, regression test added. +10 tests.
- **Panic-safety audit.** 401 `unwrap`/`expect`/`panic!`/`unreachable!` sites in `src`, of which 390 are in `#[cfg(test)]`. Of the 11 reachable from the shipped binary, all were locally provable but seven stated the proof in a comment; those are now total (`build_lineage`'s filter-then-unwrap became one `filter_map`; `palette`'s `rfind`-after-`find`; the rollout advance's `expect("checked by done")` became one `match`; `ui/detail`'s match-on-projection-then-unwrap; `cli/action`'s bounded `pop_front`; the MCP events clamp; the len-1 saved-config arm). Four remain: two exhaustiveness markers whose restructuring would duplicate a branch, a const date in the demo fixture, and `main.rs`'s startup handoff.
- **`aws.rs` split by service: 6,114 → 510 lines.** Prerequisite for ever supporting a second AWS service, and worth it on its own. Every `AwsClient` method turned out to touch exactly one SDK client, so the cut was clean: `aws/eb.rs` (1,763 — the Elastic Beanstalk domain: environments, applications, versions, option settings, templates, platform upgrades) plus twelve service modules — `cloudwatch` 356, `logs` 399, `s3` 216, `ssm` 167, `sqs` 164, `iam` 147, `cost` 143, `secrets` 133, `ec2` 130, `org` 63, `acm` 45, `waf` 22 — and `tests.rs` 2,031. Each holds its own types *and* the calls that produce them, glob-re-exported so every `crate::aws::Foo` path is unchanged. What's left in `aws.rs` is client construction, `AwsContext`, and the credential-hint rewriter. **The point:** of the fourteen AWS services ebman talks to, only one is the domain — that's now visible in the file list rather than buried, which is the seam a sibling tool for another service would cut along. `aws.rs`'s module doc says so explicitly.
- **`ARCHITECTURE.md` + `CONTRIBUTING.md`.** Module map, the keystroke → AWS call trace, the four rules, and where to start reading; plus setup, the pre-PR checklist, and the write-path rule. Both linked from `README.md` and `docs/development.md`.

---

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

### MCP discovery & setup follow-ups (Tier 6)
- `ebman mcp setup --client <claude|cursor|vscode|windsurf>` — detect the client and *write* its MCP config (currently print-only). Print-by-default, write only on the explicit `--client` flag, since it mutates the user's config. Confirm/merge rather than clobber an existing `ebman` entry.
- ~~Publish a `server.json` to the official MCP Registry for passive discovery.~~ **Wired (0.29.x):** `server.json` (cargo package, `io.github.tombaldwin/ebman`, validated with `mcp-publisher`), a visible `mcp-name:` ownership marker in `README.md`, and a `mcp_registry` job in `release.yml` (GitHub OIDC, `needs: crates_io`) that auto-publishes on release. First registry entry lands on the next release whose crate README carries the marker.

Items list `Depends on:` only when another backlog or done item is a real prerequisite.

### 0.10 candidates (2026-05-25)

Lineup for the next minor. Theme is **complete the 0.9 auto-rollback story + reduce CLI friction for CI/CD-style use**. Each item is ranked tier (HEADLINE / SUPPORT / BONUS) by expected operator value. Pick the top 3-4 to ship; the rest can wait for 0.11.

#### Auto-rollback observability — HEADLINE
- [x] **Armed-watchdog visibility in the UI.** SHIPPED (`3a81329`). Header countdown pill + `:rollbacks-armed` (alias `:rb-armed`) overlay; pure renderers tested.
- [x] **`:abort-rollback [ENV]`** — SHIPPED (`0293fd3`). No-arg drains all; named env drains just that one. Audit-logged.
- [x] **`:rollback --to LABEL [--auto-rollback Nm]`** — SHIPPED (`021127c`). Operator-named target composes with the watchdog flag.

#### CI/CD ergonomics — SUPPORT
- [x] **`:deploy LABEL --wait-for-green Nm`** — SHIPPED. Watcher armed at dispatch; apply_refresh pins success on Green or timeout error on deadline. Distinct header pill (`👁 watching`) from the armed-rollback pill. Composes with `--auto-rollback`.
- [x] **`ebman action deploy --env X --version Y --wait-for-green Nm --auto-rollback Mm`** — SHIPPED. Polls every 5s; pure decision helper `decide_poll()` covers the four-state matrix (KeepPolling / Success / WaitForGreenTimeout / DispatchRollback). Distinct exit codes (0/1/2/4/5) for CI branching.

#### Operator polish — BONUS
- [x] **Pre-deploy diff inline in the confirm modal.** SHIPPED. Every Deploy confirm modal now auto-fetches `list_application_versions` + inlines the `format_deploy_preview` body (candidate label / age / description / rollback-warning when older). The standalone `:deploy LABEL --preview` overlay still exists for explicit diff-only review.
- [x] **EB CLI `.elasticbeanstalk/config.yml` reader.** SHIPPED. New `eb_cli` module walks up from cwd to find `.elasticbeanstalk/config.yml`, parses YAML, exposes `profile` / `region` / `application`. Precedence: `.ebman/` > EB CLI > persisted state. Application name falls in as a soft filter prefill when `.ebman/` hasn't set one.
- [x] **`notify_webhook` outbound integration.** SHIPPED. `config.toml`'s `notify_webhook = "https://..."` arms a fire-and-forget POST on every audit line. Body is Slack-incoming-webhook-shaped (`text` + structured `at`/`account`/`profile`/`region`/`detail` siblings). Shells out to curl (10s cap) so we don't pull in an HTTP-client dep. Webhook failures don't alarm — local audit file remains source of truth.

#### Skipped on purpose
- **Watchdog UI as a graph / chart.** A countdown bar visualisation was considered but a text countdown ("4m 22s") is denser and reads at a glance. Defer unless an operator asks.
- **Cross-region rollout (`:rollout LABEL --regions ...`).** Real value but big — multi-region coordination is its own design problem (parallel vs sequential, abort-on-first-Red, regional health threshold). Tracked as a "0.11 or 0.12 candidate" rather than committed.

### 0.11 candidates (2026-05-25)

Surfaced by a post-0.10 review of the command surface + recent themes. Recent direction: **safety nets, composable deploy guardrails, CI/CD ergonomics, observability pills**. These items extend that arc. Each is sized for a single autonomous-mode block; build dependencies are noted where they bite.

**Note (2026-05-25)**: the two HEADLINE items below shipped early and were bundled into 0.10.0 rather than held for a separate 0.11 release — the deploy-story narrative read more naturally as one release. They're left in this section with [x] markers so the planning history is preserved; the actually-pending 0.11 work is the SUPPORT + BONUS tiers below.

#### Deploy-story completion — HEADLINE (landed in 0.10.0)
- [x] **`:promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm]`** — SHIPPED (`a1f3b7b`, bundled into 0.10.0). Version-label promotion via new `open_parameterised_action_on(env, …)` escape hatch; targets a named env rather than the table cursor. Composes with both watchdog flags. Option-settings delta promotion is a follow-on with its own design surface (still tracked below).
- [x] **Pre-deploy health-check probe** — SHIPPED (`04e4eac`, bundled into 0.10.0). At confirm time, every Deploy modal fetches the env's `Application Healthcheck URL` option-setting (defaults to `/`), composes a probe URL against the env's CNAME, and HEADs it via curl with a 2s + follow-redirect cap. Silence on 2xx (modal stays clean); yellow `⚠ health-check probe: <reason>` line on non-2xx / timeout / connect error. Pure helpers `build_health_check_probe_url` + `classify_health_check_status` are unit-tested. Skipped in `--demo` mode (synthetic CNAMEs would always fail).
- [x] **Pre-deploy "estimated unavailability"** — SHIPPED. New line in the Deploy confirm modal renders `deploy plan: POLICY → max N/M instances unavailable` (yellow if any unavailability, green if none). Pure math via `compute_unavailability_count` + `compute_batch_count` + `format_unavailability_line` + `extract_unavailability_inputs`, all unit-tested. Sourced from `aws:elasticbeanstalk:command` (DeploymentPolicy / BatchSize / BatchSizeType) + `aws:autoscaling:asg` (MaxSize) via a parallel option-settings fetch alongside the health-check probe. Skipped in `--demo` mode.

#### Drift + observability — SUPPORT
- ~~**`:config-diff --at 1h|24h|7d`** — point-in-time config diff. Scans the env's event history for `ConfigurationChange` events inside the window, replays the deltas backward from current option-settings state, shows what changed.~~ Withdrawn (2026-05-26). Re-audit shows EB's event API only carries free-text messages ("Environment configuration was updated successfully"), not structured before/after option-settings deltas. The "replay backward" mechanic the entry implies isn't implementable against EB's API surface. The honest reshape (a `--window` flag on the existing `:changes` command) duplicates 80% of `:changes` for marginal operator value. Operators who want "what's drifted in the last hour" today run `:changes` (which is already config-event-filtered) and compare against `:config-diff PROD-PEER` — same answer, two short commands. Drop unless a new design is proposed.
- [x] **`:freeze-deploys [reason]` / `:thaw-deploys`** — SHIPPED. Session-scoped fleet-wide write-lock; new `DeployFreeze { reason, frozen_at }` layered above per-env / per-account safety pins in `is_read_only_for`. Refusal toast surfaces the operator-supplied reason + age ("deploys frozen (3m ago): incident #1234 — :thaw-deploys to unfreeze"). Audit-logged. Re-issue replaces the reason in place. Cleared by `:thaw-deploys` or by exiting ebman (no state.toml persistence — intentional, freeze is a session-safety gesture not durable policy).
- ~~**OSC 8 terminal hyperlinks**~~ Withdrawn (2026-05-26, verified in 0.12). Re-attempted with an actual experiment in `ui::tests::osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation` — ratatui 0.29's `Buffer::set_stringn` path treats each byte of an OSC 8 escape sequence as a 1-cell-wide printing character, so a 24-byte opener consumes 24 cells of layout space and pushes the visible text past the buffer width. The regression test pins the broken behavior so a future ratatui upgrade that adds zero-width control handling will fail it and prompt us to revisit. Shipping today would require a custom widget that bypasses the diff renderer per-line — too invasive for the value when modern terminals (iTerm, etc.) already auto-detect URLs in pasted output, which the existing `y`-to-yank flow already produces. **Update 2026-08-24 (ratatui 0.30):** the prediction came true and the answer did not change. The bump failed this test, which is exactly what it was written to do. ratatui 0.30 no longer gives each escape byte a cell — it strips the ESC and renders the remainder as literal text, so the layout damage is gone but the escape is now unrecoverable from the buffer, which is *worse* for this feature: not even a custom widget could reassemble it. Test renamed to `ui::tests::osc8_still_cannot_round_trip_through_ratatui` since it no longer pins a 0.29-specific mechanism. Still withdrawn.

#### Operator polish — BONUS
- [x] **`:undo` for the last config write** — SHIPPED. Captures before-state on every `spawn_option_settings_update` (covers `:set-option` / `:keypair` / `:deployment-policy` / `:rolling-update` / `:health-check-url` / `:env-edit` / `:capacity` / `:scaling-triggers` / `:listener-edit` / etc.) via an extra DescribeConfigurationSettings call BEFORE the write; pushes a reverse-action `UndoEntry` onto a 10-entry ring buffer (`App.undo_history`) on successful completion. `:undo` pops the back, refuses if the captured env is no longer in view, and re-dispatches the reverse via the same spawn — which captures ITS own undo, so `:undo`+`:undo` = redo. Empty-string-prior values reverse via `to_remove` (not empty `to_set`) since EB doesn't distinguish unset from empty. Cross-context cleared on `apply_rebuild`. Config writes only (per BACKLOG design call) — deploy/terminate are out of scope; `:rollback` covers that.
- [x] **Custom command aliases in `config.toml`** — SHIPPED. `alias.NAME = "command line"` entries in `config.toml` get expanded in `execute_command` before the dispatch match. Single-level expansion (no transitive chaining → no cycle-detection complexity). Args after the alias name append to the expansion (`alias.dp = "deploy --auto-rollback 5m"` + `:dp build-900` → `:deploy --auto-rollback 5m build-900`). Pure `expand_command_alias(line, aliases)` helper unit-tested. Named `command_aliases` on Config + App to disambiguate from the existing `:alias <env> <label>` env-rename feature.

#### Skipped on purpose
- **Inline scheduled-actions surface (`:schedule add/remove/list`).** EB supports CloudWatch-event-driven scheduled scaling/restarts but most teams configure it once and forget. Defer until an operator asks for it.
- **Health-history sparklines on the main table.** Already shipped — the TREND column at `ui.rs:2925` renders the existing `sparkline_for(...)` glyph row from `App.health` history. Caught by review before this was tracked as a feature.
- **Cross-fleet event tail (`:tail-events`).** Different from `:logs-tail` (log lines) — would tail EB events across all envs in the current context. Real but lower-leverage than the drift items above. Track if operators request it.

### 0.12 candidates (2026-05-26)

Theme: **workspace polish — saved views as real tabs + ergonomic gap closures**. Picks up the long-deferred saved-views unification and tightens a few rough edges from the 0.11 batch.

#### Workspace polish — HEADLINE
- [x] **Saved views unified** — SHIPPED (`bb7547b`). `named_filters` and `saved_views` collapsed into one store; `]` / `[` cycles full views (filter+sort+group+scope, not just filter); chip bar renders saved_views; legacy `filter.NAME = "..."` state.toml lines auto-promote via the filter-only encoding. `:save` / `:filter` / `:drop` / `:filters` and `:save-view` / `:view` / `:view-drop` / `:views` all operate on the same store. Pure helpers `encode_filter_only_view` + `view_filter_value` unit-tested.

#### Ergonomic gap closures — SUPPORT
- [x] **`:batch-set-option` captures undo** — SHIPPED (`76e54b6`). Closed the multi-env undo gap from 0.11: `spawn_batch_set_option` now does the same pre-write option-settings read + `build_undo_entry` + `AppMsg::UndoCaptured` dispatch as its single-env sibling, so each env in a batch contributes its own undo entry. Repeated `:undo` walks the batch backwards. Self-review caught a context-switch race (env terminated mid-batch); guarded with an upfront fleet-presence check + audit-logged skip.
- ~~**OSC 8 terminal hyperlinks**~~ — Re-attempted with an actual experiment (vs the 0.11 assumption-based skip). Verified that ratatui 0.29 splits each escape byte into its own 1-cell-wide printing cell — a 24-byte OSC 8 opener eats 24 cells of layout space, pushing visible text past the buffer width. Regression test at `ui::tests::osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation` pins the broken behavior; a future ratatui that adds zero-width control handling will fail the test and prompt us to revisit. **Update 2026-08-24 (ratatui 0.30):** the prediction came true and the answer did not change. The bump failed this test, which is exactly what it was written to do. ratatui 0.30 no longer gives each escape byte a cell — it strips the ESC and renders the remainder as literal text, so the layout damage is gone but the escape is now unrecoverable from the buffer, which is *worse* for this feature: not even a custom widget could reassemble it. Test renamed to `ui::tests::osc8_still_cannot_round_trip_through_ratatui` since it no longer pins a 0.29-specific mechanism. Still withdrawn.

#### Skipped on purpose — held for 0.13
- **Cross-region rollout (`:rollout LABEL --regions r1,r2,r3 [--auto-rollback Nm]`)** — Held (2026-05-26). Real value but needs careful design: same-name vs explicit-mapping env discovery across regions, sequential vs parallel dispatch, partial-failure handling (region 1 ok, region 2 listing failed), per-region AwsClient construction, audit-log shape. Multiple reasonable shapes; warrants a dedicated session rather than tail-end of an autonomous run.

### 0.13 CLI charter (2026-05-26)

Lock this before adding new subcommands. The shape is **flat verbs for reads + `action <verb>` for writes + `ctl` for control plane + `mcp` reserved for server-mode futures.** Symmetric in that all are top-level; the differentiation is intent.

```
ebman                                  → TUI
ebman envs                             → read: list envs
ebman lint                             → read: diagnostic
ebman drift                            → read: terraform drift
ebman explain ISSUE_ID                 → read: LLM-backed explainer (future)
ebman versions                         → read: app versions (future)
ebman events                           → read: recent events (future)
ebman audit                            → read: audit log (future)
ebman cost                             → read: cost report (future)
ebman action <verb>                    → write: rebuild/restart/terminate/deploy/rollout
ebman ctl <op>                         → control plane (drive a running ebman)
ebman mcp serve                        → server mode (future: MCP for Claude Code)
```

**Locked conventions** (apply to every subcommand):

| Flag | Purpose |
|---|---|
| `--env NAME` | scope to one env |
| `--json` | structured machine-readable output |
| `--quiet` | suppress text output (paired with --json, or for exit-code-only use) |
| `--watch [--interval 60s]` | monitoring-tool loop |
| `--regions r1,r2,r3` | scope to regions (rollout, drift) |
| Duration grammar | `5m` / `30m` / `1h` / `2d` (same as TUI) |

**Locked exit-code convention** (consistent across all subcommands; CI scripts branch on these):
- `0` clean / success
- `1` AWS-layer error
- `2` usage error (missing flag, malformed duration, env not found)
- `3` issues / drift found (lint warnings, drift detected)
- `4` `--wait-for-green` timeout (deploy)
- `5` `--auto-rollback` fired (deploy)

**Non-zero on issues by default** — no `--exit-code` flag. CI gets natural `ebman lint && deploy` semantics; interactive users see `$? = 3` but can keep reading.

**Reads don't get `--yes`; writes do.** `--yes` is the destructive-confirm gate, not a general convention.

**Out of scope for CLI surface:**
- Local-state mutations (saved views, pins, runbooks) — these are operator gestures bound to a TUI session; scripting them invites footguns. Keep them TUI-only.
- Anything that requires a long-running TUI process — use `ctl` for that.

**Future-proofing test passed:** LLM explainer (`ebman explain`), MCP server (`ebman mcp serve`), cron-driven monitoring (`ebman lint --watch`), git pre-commit hooks (`ebman drift`), GitHub Actions integration (`ebman action deploy`), audit-stream consumption (`ebman audit --tail --json | jq`) all fit without restructuring.

### 0.28 candidates (2026-08-20)

Theme: **agents that can act — MCP v2 writes, safety-first.** Panel-shipped 0.27.0 the same day this was spec'd; 0.27 soaks while this spec holds the decisions so the build session doesn't re-litigate.

#### MCP v2: `ebman mcp serve --allow-writes` — HEADLINE — SHIPPED 2026-08-20 (spec locked + built same day; v3 tool-set + cross-process freeze folded in and shipped — see CHANGELOG Unreleased)

Demand: the same agent sessions that drove v1 (fleet upgrade + release ops) need `deploy`/`restart`/`rebuild` without shelling out — and the CLI shell-out path they'd otherwise use has *weaker* ergonomics for confirmation than a purpose-built two-phase tool. v1's year of hardening (redaction contract, degradation contract, bounded concurrency, protocol battery) is the foundation.

**Locked decisions — do not reopen in the build session:**

1. **Opt-in is flag-only.** `--allow-writes` on the command line, never a config key — enablement must be visible in the process table and `.mcp.json`, not hidden in a dotfile. Composes with `--demo` (the write layer's e2e harness).
2. **Five write tools** (v3's widening folded in, 2026-08-20): `deploy` (env + version label), `restart`, `rebuild`, `terminate`, `set_option`. Extra rigor for the two folded-in tools: **`terminate`'s phase 2 requires the token AND `confirm_name` equal to the env name** — the MCP equivalent of the TUI's strict-typed confirm; a mismatch is isError, token stays live for one retry within TTL. **`set_option` caps at 10 settings per call**, its plan lists every namespace/name with old→new values (old values redacted per the standing contract when env-var/DBPassword; the new value is echoed verbatim — the agent supplied it), and it refuses namespaces outside the env's own config (no cross-env blast). Rollout stays OUT — not deferred, RESOLVED: a rollout tool would violate the dispatch-only/30s/serialized-writes decisions below by construction; agents compose it from `deploy` per region + read polling, which is strictly more inspectable in a transcript anyway.
3. **Every write is two-phase, uniformly.** Phase 1: the write tool validates (env exists, pin check, version exists for deploy) and returns `{"pending": true, "confirm_token": "...", "plan": {...}}` — the plan carries env, current→target version, health, and the 3 most recent events, so the agent's transcript SHOWS what's about to happen. Phase 2: a `confirm_action` tool takes the token and dispatches. Tokens: random 128-bit, single-use, in-memory, 60s TTL; expired/reused/unknown → `isError` with "re-plan required". One mental model for all three verbs — no "restart is safe enough to skip confirm" carve-outs.
4. **Safety stack at the tool layer:** `Config::pin_reason(env, ambient AWS_PROFILE)` refusal (the shared check, isError with the pin name — this is what pin_reason was built for); writes SERIALIZED server-wide (one in flight; concurrent phase-2 → isError "another write is in flight"); dispatch-only semantics (no wait-for-green — the agent polls reads; keeps every call inside the 30s bound).
5. **Audit + webhook parity with the CLI:** `--allow-writes` (and only it) calls `audit::init_from_config_disk()`; dispatched/completed pairs via the typed writers with extras `via=mcp client="<clientInfo.name>"`; drain webhooks on shutdown (the existing drain machinery). Demo mode writes NO audit lines and fires NO webhooks — synthetic success only.
6. **tools/list is honest:** write tools + `confirm_action` appear ONLY under `--allow-writes`. Descriptions state dispatch-only semantics + the two-phase contract explicitly (v1 rule: the caveat lives IN the tool).
7. **Registry unification lands as the enabling refactor** (the 0.26 architecture review's design, gated on exactly this): one static table (name → schema, handler, is_write) feeding tools/list, the existence check, and dispatch; `cli/mcp.rs` splits to `cli/mcp/mod.rs` (protocol/loop) + `cli/mcp/tools.rs` at the same time. Do this FIRST — the write tools then slot into the table.
8. **Cross-process freeze visibility** (v3's freeze-file folded in, 2026-08-20): the TUI persists a freeze marker at `~/.cache/ebman/freeze.json` (0600) on `:freeze-deploys` / `:incident START` — `{pid, reason, incident, at}` — removed on `:thaw-deploys` / `:incident END` and on clean exit. **Session-scoped semantics are preserved via the pid**: readers treat the file as active only when the owning pid is alive, so a crashed TUI can't leave a permanent phantom freeze (stale file with a dead pid is ignored AND cleaned up by the next reader). Enforced by every cross-process write path — the MCP write tools AND, fix-the-class, the CLI (`ebman action`, `audit replay`, `lint --fix`), which have had the same blind spot all along. Refusal is isError / exit 3 carrying the freeze reason and `:thaw-deploys` / `:incident END` as the remedy. Demo-mode TUI does not persist (demo freeze is play-acting). Single file, last-writer-wins across concurrent TUI sessions — documented, not defended against. New pure seams: `freeze::write_marker / read_active / clear` in a small `src/freeze.rs` with pid-liveness injectable for tests.

**Tests (minimum):** golden tools/list with and without the flag; demo two-phase happy path (plan → confirm → synthetic ok); expired + reused + unknown token; pin refusal via injected config; write serialization (second phase-2 while one in flight); deploy plan carries current→target + events; no audit lines in demo; **terminate `confirm_name` mismatch refuses with token surviving one retry; set_option >10 settings refuses; set_option plan redacts old env-var values and echoes new; freeze marker round-trip (write/read/clear), dead-pid marker ignored AND cleaned, live-pid marker refuses across MCP + all three CLI paths, demo TUI writes no marker.**

**Docs:** headless.md gains the v2 section (two-phase example transcript incl. terminate's `confirm_name`); safety-and-privacy.md updates the "reads-only" bullet to the opt-in + two-phase + pin + freeze-marker story, and the freeze docs drop the "not persisted / in-session gesture" phrasing for the new pid-scoped marker semantics; commands.md `--help` entry; configuration.md notes freeze.json's location and that deleting it is the manual unfreeze of last resort.

#### 0.29 queue — 0.28 pre-tag review deferrals (2026-08-20)

The write/freeze pre-tag review (2 lenses) fixed 2 Critical + 2 Important + 1 Minor before tag (see CHANGELOG). Deferred, non-blocking:
- [x] **Unified MCP tool registry** (arch I1) — DONE 2026-08-22, as a PIN rather than a restructure. The two sides already agreed (8 read + 6 write descriptors, 14 dispatch arms) so there was no live bug to fix; what was missing was anything making them agree. Mirrors what `src/commands.rs` does for the TUI registry: a test reads the `call_tool` match arms from source and asserts the two sets are equal in both directions — a descriptor with no arm is a tool an agent calls and gets nothing from, an arm with no descriptor is dead, because `tools/call` refuses names absent from the table. A second test pins that no write tool is advertised without `--allow-writes`, which the same membership check turns into a write-surface property rather than a listing cosmetic. Restructuring into `&[ToolDef{...}]` would buy nothing further: the schemas are `json!` literals that have to live somewhere, and the pin already catches the drift the entry was worried about. Original note:  — the spec's `&[ToolDef{name, schema, is_write, handler}]` single table; today name/schema/handler live in three sites (tool_table descriptors + call_tool match + RPC existence check) with no compile-time or test link. A coverage test that every `tool_table(true)` name resolves to a real handler was NOT added this run — add it OR the full slice refactor (~half day). Drift is currently a runtime `isError`, not a panic.
- [x] **Shared verb-dispatch helper** (arch M2) — DONE 2026-08-22 as `CliVerb`, ~40 lines. And it immediately paid for itself: the test asserting the CLI's audit label matches the TUI's for the same verb FAILED, because the CLI wrote `Restart` while the TUI writes `RestartAppServer`. Two consequences, both real — `ebman audit --action` matched half the history either way, and `audit replay` accepted only `Restart`, so **the most common restart in any log, the TUI's, was unreplayable**. The CLI writes the canonical name now and replay accepts both spellings. Original note:  — `dispatch_write` (writes.rs) and `action::run` (cli/action.rs) both hand-map verb→method + the audit pair; a shared `dispatch_verb()` removes the drift surface (~2h).
- [x] **pin/freeze check-order + freeze-message rendering unified** (arch M3) — DONE 2026-08-22. `freeze::refusal_message` is the one sentence; the CLI's two sites go through `refuse_write`, which does freeze-then-pin like the MCP gate. The order lives in one function now rather than being two bare calls in sequence, which is how it came to differ. Original note:  — CLI checks pin-then-freeze, MCP checks freeze-then-pin; the freeze refusal string is rendered in two places. Cosmetic inconsistency for an operator comparing outputs.
- [x] **Superseded-token message** (bugs/arch M1) — FIXED 2026-08-22. `WriteState::install` retires the token it replaces (bounded at 8), and `mismatched_token_message` tells a superseded token from an unknown one so the agent knows whether to re-read the newer plan or re-send what it holds. Past the cap it falls back to "unknown", which is honest. Was: CONFIRMED STILL REAL 2026-08-22 (`writes.rs:468`): there is one `pending` slot, so a newer plan replaces the old one and confirming the old token returns "unknown confirm_token", indistinguishable from a typo; distinguish "superseded by a newer plan".
- [x] **`lint --quiet` can exit non-zero with nothing to explain it.** FIXED 2026-08-24.
  `src/cli/lint.rs:866` sets `cycle_degraded = true` when a probe could
  not run, then gates only the `eprintln!` on `!quiet`. So `--quiet`
  suppresses the reason while keeping the failing exit code — a red CI
  step with an empty log. Either `--quiet` should also stop degrading
  the exit, or the degrade reason needs a channel `--quiet` doesn't
  silence. Noted 2026-08-24; verified still live at 0.33.0.

- [x] **`lint --json` has no degraded field.** FIXED 2026-08-24 — same root cause as the item above (the degrade reason had no reliable channel), so both were fixed by funnelling all four degrade sites through one `degrade()` helper that prints, records for `--json`, and sets the flag together. Guarded against a fifth site being written the old way. `coverage_warnings`
  reaches `eprintln!` only (`src/cli/lint.rs:866-872`) and never enters
  the JSON payload, so a machine consumer cannot tell a clean run from
  one where a probe was skipped on AccessDenied — which is exactly the
  distinction `ProbeOutcome::Unknown` was introduced to preserve. The
  human output makes it; the JSON output flattens it back. Noted
  2026-08-24; verified still live at 0.33.0.

- [ ] **CLI rollout/auto-rollback freeze is start-only** (bugs M3) — a freeze declared mid-rollout doesn't stop later regions; matches the pin start-gate semantics, flagged as a conscious choice.

#### Also queued for 0.28
- [x] **Live-verification sweep** — DONE 2026-08-20. All three SDK-compiled-unverified calls confirmed against real resources in us-west-1: EBL020 `simulate_principal_policy` against Uflexi-prod's `aws-elasticbeanstalk-ec2-role` (returned EvalActionName/EvalDecision `allowed`); EBL015 `describe_platform_version` against a managed platform ARN (returned a real DateCreated); EBL018 `web_acl_for_resource` against a throwaway bare ALB (empty response → Ok(None) → rule fires; ALB + SG torn down clean, no orphans).
- [x] **Demo-mode poller quieting** — SHIPPED: `DEMO_QUIET_AWS_ERRORS` downgrades the stub's expected failures to debug in `--demo` (real mode keeps the loud contract).
- [x] **Cost Explorer page-loop cap** — SHIPPED (20-page bound).
- [x] **`:custom-platform-create` DROPPED** (2026-08-20) — slipped three cycles with no real demand; removed from the backlog rather than carried. Reopen with a fresh spec if custom platforms enter use.

### 0.27 queue — 0.26 max-depth review remainders (2026-08-20) — RETIRED same-day (all four phases shipped; see CHANGELOG Unreleased). Wontfix by decision: unicode display-width math, serde_yml alias hardening, audit --tail text alignment. ui.rs submodule split SHIPPED 2026-08-20 (ui/overlays + ui/detail + ui/help; root 9,400 -> 5,000 lines). The round-2 review remainders (log-tail boundary dedupe, DLQ staleness marker, TUI quit drain, id-keyed delete confirm, -32600 test) also shipped same-day. Only remaining deferral: MCP registry unification (gated on v2 --allow-writes).

Six-lens full-codebase review post-0.26.0. All 8 Critical-class + the quick Important findings were fixed same-day (see CHANGELOG Unreleased). Remaining, each deferred with a reason:

#### `aws/` correctness — max code review, 2026-08-21

Found by a max-effort review of the `aws.rs` split. All pre-existing (the split was a byte-faithful move); the split is what made them legible. **All fixed 2026-08-21**, each with a regression test.

- [x] **`list_events_inner` didn't paginate** — `:event-tail` advanced its watermark past events left behind a dropped `next_token`, so a rolling deploy larger than one batch lost its own start/warning lines. Now follows up to five pages, but only on the watermarked call: the two display callers want "the newest N" and chasing tokens there would spend API calls on events nobody renders. A test pins each direction.
- [x] **`upload_bundle_with` skipped the abort on a missing ETag** — bare `?` returned without `AbortMultipartUpload`, leaving parts of a >64 MiB bundle billed with no visible object. Also factored the four hand-rolled abort blocks into one `abort_multipart` helper that warns if the abort itself fails (they discarded the result silently).
- [x] **Alarm matching ignored the dimension name** — matched on "some dimension's *value* equals env_name", so an RDS alarm with `DBInstanceIdentifier=payments` was attributed to the EB env `payments`. Now matches name *and* value. Namespace is deliberately not part of the match: an operator-authored alarm in a custom namespace dimensioned by `EnvironmentName` is genuinely about the env.
- [x] **`list_subnets_in_vpc` / `list_security_groups_in_vpc` didn't paginate** — truncated at the API default (1000 for security groups), so a shared VPC's picker showed an arbitrary subset and the operator created a duplicate.
- [x] **`simulate_principal_policy` ignored `is_truncated`** — `:explain` rendered a partial decision table on a surface where an action's absence reads as "not the problem". Follows the marker only when `is_truncated` is set, so a stale marker can't start a loop.
- [x] **`compare_versions` ranked a pre-release above its release** — now implements semver precedence (absent beats present; identifiers left to right; numeric below alphanumeric; fewer fields below more). Solution stacks unaffected and pinned by a regression test.
- [x] **`format_insights_results` took columns from row 0** — Insights omits absent fields per record, so a field the first record lacked was dropped for every row. Now the union across rows, first-seen order. Same function measured widths in bytes while padding counted chars.
- [x] **`assume_role` turned an unrepresentable expiry into "never expires"** — `secs() as u64` wrapped a negative to ~1.8e19, `checked_add` returned `None`, and `Credentials::new(.., None, ..)` means no expiry, so the refresh tick never re-assumed. Now a pure `sts_expiry_to_system_time -> Result`; the silent reading is unrepresentable.
- [x] **`list_environments_in_region` labelled rows with the requested region** — its sibling used the resolved one, and `AwsClient::with` already detects the case where the SDK ignores an explicit region. Both now share one `stamp_region` step so they can't diverge again.
- [x] **`run_shell_command` checked its deadline only per full cycle** — sequential polling cost one round trip per instance, so a large env could burn its wall clock on a single cycle and write every instance off as `TimedOut(local)` while the command ran fine. The cycle is now concurrent (bounded at 10); the test pins per-instance result attribution, which is the risk concurrency introduces.
- [x] **IAM and Cost Explorer hardcoded `us-east-1`** — a cross-partition endpoint for GovCloud and China operators, so `:explain` and `:cost on` could never have worked there. New `global_service_region`; one test tabulates it, another asserts the endpoint never leaves the operator's partition.
- [x] **Cost Explorer page cap was silent** — fell out of `MAX_COST_PAGES` with a token in hand and the caller cached the partial map for 24 h, so envs past the cap read as unknown cost. `fetch_env_costs` now returns `EnvCosts { rows, truncated }` and a truncated result is rendered but **not** cached, so the next refresh retries.
- [x] **Log-tail re-emitted the same lines forever** — the boundary dedupe set was carried only when `truncated`, but a truncated poll stalls the watermark and the *next* (quiet, untruncated) poll dropped the set while leaving the watermark put, so every poll after re-printed the same lines. Carry is now keyed on "did the watermark move". The regression test was verified to fail against the old code.

Also from the same review, initially filed as "not bugs" and then fixed anyway:

- [x] **On-demand clients were built eagerly** — `list_environments_in_region` constructs a whole `AwsClient` per region on every refresh tick, so anything eager is paid per region per tick. Six of the twelve sub-clients (Cost Explorer, IAM, Organizations, Secrets Manager, ACM, SSM) are only reachable from an explicit operator action and are now `OnceLock`, built on first use. **Measured before changing anything:** every SDK client costs ~0.6 ms to construct, near-identically across services — so all twelve came to ~7.3 ms and deferring six roughly halves it. The review's claim that Cost Explorer alone accounted for 8.3 of 9.8 ms was wrong; the cost is uniform, there are just twelve of them. Tests pin that the cells start empty, that touching one doesn't build its neighbours, and that a seeded mock wins over `get_or_init`.
- [x] **Seventeen copies of `split_csv`** — the review said four; the exact shape (`split(',')` → trim → drop empties → `Vec<String>`) appeared seventeen times across ten files: config parsing, saved state, CLI flags, form input, EB option settings, MCP tool args. Now one `util::split_csv` with its own tests, called from nineteen sites. Two of the sites collected into a `BTreeSet` rather than a `Vec` — the compiler caught that, which is why the migration was done type-checked rather than textually.
- [x] **Eleven unbounded pagination loops** — each hand-rolled the same walk and none bounded it, so an endpoint that keeps returning a token (a bug, a proxy, a hostile response) would spin that task forever with the operation stuck "loading" and no error. Now one `aws::paginate` helper with a 100-page runaway guard that warns when reached. Nine listings migrated; `list_events_inner` and `simulate_principal_policy` keep their own bespoke loops because they need tighter, per-call caps with their own warnings — as do the log and event tails and Cost Explorer. Tests cover page-walking order, the empty-token-means-done case AWS actually returns, error propagation, and the runaway cap itself.

#### `aws/` second review pass — 2026-08-21

A max review of the fix lineup itself. Most findings were defects in the *fixes*, which is the useful outcome. All fixed except the four below.

- [x] **`pages < max_pages` was `1 < 1` for the display callers** — so every ordinary `DescribeEvents` fetch took the "cap reached" arm and logged a WARN, on every refresh tick, deploy poll and `:event-tail` open — burying the one warning that means something. The same arm had a wrapped string literal missing its `\` continuation (26 literal spaces mid-message) and a dead `let _ = t;`. Test asserts through a tracing subscriber, because the bug was invisible to every other test.
- [x] **A truncated Cost Explorer walk still clobbered the live map** — the `truncated` flag protected the disk cache but the handler cleared and replaced `self.costs` and stamped `costs_fetched_at` first, so envs past the cap flipped from real numbers to `—` (identical to "untagged") and `:fleet-cost` under-reported. A truncated walk is now a failed refresh: keep the previous map, say so. With nothing cached, partial renders but is labelled and doesn't stamp a fetch time.
- [x] **`paginate` had no truncation signal** — the exact defect fixed for Cost Explorer one commit earlier. Now returns `Paged { items, truncated }`. Critically, migrating `list_alarms_for_env` and `list_secrets` onto it had made the cap bound the whole-account *scan* rather than the result — so a matching alarm past ~10,000 read as "no alarms" during triage. Those two now call `Paged::complete` and error rather than report a false negative.
- [x] **`global_service_region` missed the ISO partitions** — `us-iso-*`, `us-isob-*`, `us-isof-*`, `eu-isoe-*` all fell through to `us-east-1`, the cross-partition endpoint the function existed to prevent. Worse, its "property" test derived the partition with the same prefix logic as the implementation, so it was vacuously true and could never have caught it. The oracle is now a hand-written region→partition table.
- [x] **SSM deadline still only checked after the whole cycle** — concurrency shrank the overrun ~10x without bounding it; 200 instances at 10 at a time is still 20 waves, and retry backoff can stretch one past the wall clock. Now `timeout_at` around each response as it lands, which keeps what completed.
- [x] **`list_instances` didn't paginate** — it is `:ssm-run`'s target list and `spawn_dry_run`'s blast-radius count, so a truncated one meant the command silently never reached the missing instances while the overlay reported N/N success.
- [x] **`update_check::is_newer` was the crate's second version comparator** and never learned the pre-release rule, so a binary running `0.30.0-rc1` was never told `0.30.0` had shipped. `compare_versions` now lives in `util` and both use it.
- [x] **Six doc blocks asserted the behaviour this lineup removed** — eager construction and the unconditional `us-east-1` pin, including one pointing at a BACKLOG item the same commit ticked. Plus `format_insights_results` still citing the row-0 guarantee its body stopped trusting, `EnvCosts` inserted under `EnvCost`'s doc, and the reciprocal "same grammar" claims between `parse_replay_spec` and `parse_window_ms` (the latter also takes seconds). This is the class the lineup's first commit set out to fix; it recurred because the behaviour changed after the docs were corrected.

All four items previously deferred here are now done:

- [x] **Partition-aware ARNs and console URLs** — partition knowledge was in four places and agreed with none of them. One `util::PARTITIONS` table now carries region prefixes, ARN prefixes, global-service endpoint regions and console hosts. `parse_access_denied` handles any partition (it matched the literal `arn:aws:sts::`, so `:explain` in GovCloud reached the right IAM endpoint and then failed on its argument — the raw session ARN, which IAM refuses); the three console links follow the partition, and honestly return nothing for the ISO partitions rather than emitting a commercial-host link that can't resolve; `report_bug`'s ARN scrubbing reads the same table instead of a hand-kept list of three that missed ISO ARNs.
- [x] **`AwsClient` memoised per (profile, region)** — the fan-out ran `aws_config::load()` per region per refresh tick, re-reading `~/.aws` from disk and rebuilding the credential chain. Now cached; cleared on context switch, since that is also when the operator may have re-run `aws sso login`. Caching is what the SDK expects anyway: its credential providers refresh internally, so a long-lived client picks up a renewed token while a per-call one throws that away. Only the profile path is cached — `assume_role` sessions have a hard 1-hour cap and must not be reused past it.
- [x] **`:event-tail` gap marker** — the page cap was a log line, not a signal: the watermark advanced past the newest event received and, because `DescribeEvents` returns newest-first, everything behind the token was older and unreachable by any later poll. `list_events_since` now reports truncation and the tail inserts a visible marker ahead of the oldest fetched event. No `AppMsg` change needed — the marker rides in the event stream with `at: None`, which keeps it out of the watermark calculation.
- [x] **Alarm matching is configurable** — the strict `EnvironmentName` match stays the default (a wrong alarm during triage is worse than a missing one), but `alarm_dimensions` lets operators whose own alarms spell it differently list the spellings. The value must still equal the env name, so widening the names can't reinstate the RDS false positive. Documented in `docs/configuration.md`.

#### `aws/` third review pass — 2026-08-21

Reviewed the fixes for the second review plus the four deferred items. Fifteen findings, nearly all defects in those fixes. All fixed.

- [x] **`:settings` save destroyed config it had parsed** — `serialize` never emitted `alarm_dimensions` (new) or `accounts.*` (pre-existing), and a save rewrites the whole file. So one `:settings` save deleted every AssumeRole account definition and broke `:account <name>` with nothing on screen to say why. Found by a round-trip test that now guards every key: parse a full config, serialize, re-parse, compare.
- [x] **`alarm_dimensions` replaced rather than extended** — setting it to `Environment` dropped `EnvironmentName` from the match, so alarms `:alarm-create` creates became invisible to ebman, along with every EB-native one. Now additive: the canonical name is always matched.
- [x] **The `:event-tail` gap marker could never be seen** — inserted as the oldest row of a batch of up to 1500 into a 1000-entry ring, so it was evicted by its own batch; and with no env or application, any active filter dropped it. Now the batch is trimmed to fit (keeping the newest, which is what survives anyway) and the marker carries a severity the filter exempts.
- [x] **Three more scan-then-filter listings took `.items()`** — `list_environments`, `list_application_versions`, `list_instances`. Callers `.find()` in them and report absence: a halted rollout, an MCP deploy refused as "version doesn't exist", and `:ssm-run` reporting N/N success over a short target list. All now `.complete()`.
- [x] **`.complete()` could wall a large account** — the requests were never narrowed (no `max_records`), so the scan ceiling was a fraction of what it should be and a big account would hard-fail on the triage path. Now `max_records(100)` plus a separate `SCAN_PAGES` budget (50,000 records) distinct from the runaway guard; and `list_secrets` requires completeness only when a name filter is present, since an unfiltered browse is served fine by a partial list.
- [x] **`aws-eusc` was missing from the partition table** — the pinned SDK carries it; without it `report_bug` would leak European Sovereign Cloud ARNs into a public issue, and `:explain` / `:cost on` would use a cross-partition endpoint.
- [x] **The partition fallback was positional** — `PARTITIONS.last()`, so appending an entry (the "one edit" the table advertises) would silently redirect every unknown region into it. Now resolved by identity.
- [x] **`:explain`'s args guard was still commercial-only** — `starts_with("arn:aws:")`, so it refused its own documented argument form for every operator outside the commercial partition, one level above the rewrite fixed for the same reason.
- [x] **`parse_access_denied` regressed to `None`** — making the rewrite partition-generic moved its `?`s into an arm that now fires for every partition, so a non-assumed-role STS principal failed the whole parse instead of passing through. The rewrite is a helper returning `Option` now.
- [x] **Console links used the home region** — under a fan-out that opens the wrong region's console, and the new partition guard was evaluated against the home region too, so it could never fire for the row it existed for. Now the row's own region.
- [x] **`open_instance_in_console` had no Windows arm** — it hand-rolled `open`/`xdg-open` while its two siblings used the shared `open_url`. Also folded the "no console host" message onto the partition table instead of three copies of hardcoded prose.
- [x] **The client cache removed the only path that re-read `~/.aws`** — SDK providers refresh SSO and `credential_process` credentials, but static profile credentials have no expiry and are never re-resolved, so pasting fresh ones did nothing until a context switch or restart. Five-minute TTL.
- [x] **A cache clear during a build was undone by the in-flight builder** — the `AppMsg` generation guard drops stranded *results*, not cache writes. The cache now carries its own epoch.
- [x] **A partial cost map became permanent** — the `is_empty()` test meant the first truncated walk's data survived the session while every retry burned twenty metered Cost Explorer pages and discarded them. Tracked explicitly by `costs_complete`.
- [x] **Two operator messages shipped with 30-space holes** — wrapped literals missing their `\` continuation, a verbatim recurrence of a defect ticked as fixed one commit earlier. Now asserted on the *rendered* text, and recorded as a house rule in `CLAUDE.md`.

Also from the same pass: doc drift created in the same commits (three comments describing behaviour the diff had just changed), a nested executor inside a `#[tokio::test]`, a `MAX_PAGES` shadow, the process-global cache tests racing each other, `Paged`'s public field allowing a silent discard of `truncated`, and `map_platform` shipping as the one new pure helper with no test.

#### `aws/` fourth review pass — 2026-08-22

Reviewed the third-review fixes and the write-safety tests. Fifteen findings; the severe ones were all defects in those fixes.

- [x] **A test wrote into the developer's real cache** — confirmed on the machine: `~/.cache/ebman/cost-unknown-us-east-1.toml` held a fabricated `env."full" = 1.00` with a fresh timestamp, written by `cargo test`. The cost cache is stale only after 24 h, so the next real session would have rendered that fiction, shown `—` for every real env, and skipped the fetch that would correct it. `util::cache_dir()` now redirects under `cfg!(test)`.
- [x] **`list_environments` + `.complete()` made a region vanish** — the fan-out only reported an error when EVERY region failed, so a region hitting its page budget went from contributing a short list to disappearing silently. `AppMsg::Refresh` carries `partial_errors` and the handler names the missing regions — set *after* the auto-clear, since a successful refresh wipes `error_message`.
- [x] **The `accounts.*` emit destroyed real config** — `parse` materialises an entry before matching the field, so a typo left a phantom spec with an empty ARN, and the new emit wrote it back as `role_arn = ""` over the operator's real line. Phantoms are now skipped. Value escaping was added and then removed: `parse` reads with `trim_matches('"')` and has no escape handling, so an escape it can't decode is worse than none — the limitation is pinned by an `#[ignore]`d test.
- [x] **The cache epoch guard didn't close the race** — the epoch was read outside the lock the insert takes, so a clear could complete between them and the stranded builder repopulated the map a profile switch had just emptied. Now one `install_if_current` doing both under the same lock.
- [x] **The gap marker was made less visible by being made to survive** — the `GAP` sentinel fell through to the `_` arm, `muted`, indistinguishable from routine chatter. The severity→colour map was inlined in three places and had drifted; now one `ui::event_severity_style`. And the marker can still be evicted by its own batch, so a sticky `truncated_polls` counter in the overlay chrome carries the signal instead.
- [x] **Three of the new tests could not fail** — the only test for the console-region fix re-implemented the fixed expression inline, the trim test asserted arithmetic over two constants, and the epoch test only checked a counter increments. All three rewritten to call production code, each verified by mutation.
- [x] **`:alarm-add` doesn't exist** — the command is `:alarm-create`, and the invented name reached the config docs and three source comments. CI pins the registry to the dispatch arms but not to prose; a test now does, verified against the mistake.
- [x] **`.complete()` could wall a large account, and the rule was applied inconsistently** — the requests set no page size, and six listings that feed pickers or `.find()` lookups still took `.items()` while their own comments said a short result reads as absence. Page sizes set, the rule applied throughout, and a drift guard requires any remaining `.items()` to be named with a reason.
- [x] **`alarm_dimensions` accumulated and rewrote the operator's line** — a second `alarm_dimensions =` line unioned with the first instead of replacing it, and `serialize` emitted the full match set so a hand-written `"Environment"` became `"EnvironmentName,Environment"` on the first save. Now rebuilt from the canonical name and only the added names are written.
- [x] **`costs_complete` had no reader where it mattered** — `:cost status` reported "no data yet" while figures were on screen, and `:fleet-cost` rendered an under-reporting total with no marker. Both now say so, and the flag resets when the map is torn down.
- [x] **`yank_cli` still used the home region** — the sibling of a fix applied ten lines above, on the surface most likely to be pasted into a channel as evidence. One `App::region_for` accessor now serves all three sites. The instance-console link deliberately keeps the home region: `d.instances` is fetched with the home-region client, so the row's region would name a home-region instance ID in another region's console.
- [x] **Smaller** — a stray space rendering as "1000 -event buffer" (the guard added one commit earlier tested for three spaces, so it couldn't see it; it tests for two now), a `MAX_PAGES` shadow, the `PARTITIONS` header asserting an unsound ordering rule (now a checked cross-product test), and the process-global cache lock not covering the app-side tests that clear it.

Still open, recorded rather than fixed:

- [x] **Detail shows home-region data for a fan-out row** — FIXED 2026-08-22 (0.30.0). `spawn_detail_*` all used `self.aws`, whose region is `context.region`, so opening Detail on an environment from another region showed that region's name with the home region's instances, metrics and events. Detail carries its own client now. The instance-console link had been *worked around* to match the wrong data; that compensation became a bug once the data was right, and was fixed in the 0.30.1 review round.
- [x] **`SCAN_PAGES = 500` is worst-case 500 sequential round trips** — FIXED 2026-08-22. on three interactive triage paths, with no timeout, no cancel and no partial render — and `detail_nav` has no in-flight guard, so scans longer than the 15 s tick stack up.
- [x] **The client-cache TTL reaches one call path** — FIXED 2026-08-22. — only `list_environments_in_region` uses `cached_client`; everything else goes through `App::spawn_aws` → `self.aws`, which is replaced only by an explicit context switch. A single-region operator never reaches the cached path at all, so pasting fresh static credentials still does nothing until restart.
- [ ] **`WRITE_COMMANDS` is hand-written** — RE-SCOPED TWICE, 2026-08-22. **It is not derivable from a verb table** (see the design pass below: the test lists `:command` invocations, of which only 3 of 12 correspond to an `Action` variant — a different axis). Keep the list hand-written; the open question is only whether a reachability guard can flag a missing entry, and the attempt at that produced false positives in both directions. Earlier note follows. RE-SCOPED after an attempt. The plan was "add a `write` flag to `CommandSpec` and derive the test list". Deriving the *classification* automatically does not work, demonstrated in both directions: a transitive walk from each dispatch arm reports `:explain` and `:changes` as gated (they are not — zero `deny_write` calls; the walk reaches shared helpers) and `:rebuild` as non-mutating (it is *the* canonical write; it goes through the confirm modal into `spawn_action`, which the walk doesn't reach). 67 of 131 commands came back flagged, and the list is wrong at both ends.
  So the flag has to be set by hand for 112 commands, and a partially-correct one is WORSE than none: it puts a safety guarantee behind an unreliable classification. What makes it worth doing is the second half — a structural guard that no command outside the declared write set can reach a mutator, in the shape of `every_spawn_declares_whether_it_is_per_env`. That is the part that catches "someone added a write and forgot the flag", and it is the part a script can't fake. Needs its own session with the registry open, not a slot in a batch.
  Original note: roughly two-thirds of the write surface is unpinned by the property test. Every omission does reach `deny_write` today — verified 2026-08-22 by checking all 40 mutating call sites: 5 sit in ungated functions (`spawn_batch_*`, `spawn_ssm_run_impl`) and all 5 are gated by their callers (`deny_write_batch` at queue time in `cmd_write.rs`; `spawn_action`'s `is_read_only_for` for SSM). Coverage gap, not a live bypass. rather than derived from the registry, so roughly two-thirds of the write surface is unpinned. Every omission does reach `deny_write` today (checked), so this is a coverage gap rather than a live bypass; `CommandSpec` needs a `write` flag to derive from.

#### `aws/` fifth review pass — 2026-08-22

Reviewed the fourth-review fixes. Fifteen findings, again mostly defects in those fixes. All fixed, each with a mutation-verified test.

- [x] **A test overwrote the developer's real `state.toml`** — the `cfg(test)` redirect landed on `cache_dir()` and not on `config_dir()`, so the sibling of the bug fixed one commit earlier was still live; confirmed on the machine by mtime. Both now share one `test_or_home()`, and a guard test was verified to fail against the old code.
- [x] **The `accounts.*` emit still destroyed config** — `serialize` dropped a whole account block rather than an empty `role_arn`, and `parse` discarded every line it didn't recognise, so a `:settings` save deleted a mistyped key (the very line an operator would spot and fix) and any key a newer release adds. Unrecognised lines are kept verbatim in `Config::passthrough` and re-emitted under a header.
- [x] **A partially-failed fan-out neither backed off nor kept its message** — the throttle back-off was armed only on a wholly-failed refresh, and the region notice was written over whatever the operator was reading. Back-off now arms on throttling inside `partial_errors`, and the notice only fills an empty slot.
- [x] **A truncated Cost Explorer walk was terminal** — `spawn_cost_fetch` has exactly one caller, so the retry the truncated branch preserves by not stamping `costs_fetched_at` did not exist; `:cost on` answered "already on" and the partial map stayed for the session. `:cost on` now re-fetches when the walk was incomplete, and the INCOMPLETE messages name it as the remedy.
- [x] **`:cost status` called a partial result "cached"** — reachable when a truncated walk lands over a non-empty map: the previous timestamp stays, so the arm quoting it described the older complete fetch while the handler had explicitly refused to cache what was on screen.
- [x] **`simulate_principal_policy` returned a short list silently** — its own comment states the rule (`:explain` is the surface where an action's absence reads as "not the problem") and it warned to the log, which the operator staring at the overlay cannot see. Returns `Paged<IamSimResult>`; `:explain` prints an INCOMPLETE banner *above* the rows, since it changes how all of them should be read. The lint probe takes `.complete()`.
- [x] **The `.items()` drift guard never scanned outside `src/aws`** — `Paged` is `pub(crate)` and the callers that actually have to make the choice live in `app/` and `cli/`. It walks all of `src` now, skipping backticked prose mentions (one of which lives in a `#[must_use]` string, not a comment).
- [x] **`list_org_accounts` had a hard 2,000-account ceiling** — ListAccounts caps `MaxResults` at 20, so the shared 100-page runaway guard bounded it at 2,000, and because the walk `.complete()`s a larger org got an error rather than a list. Now `SCAN_PAGES`.
- [x] **The Detail health panel filtered FATAL out** — the recent-events filter took ERROR and WARN only, so the severity *above* ERROR was the one event the panel would not show, and with only a FATAL in the window it said "no error / warning events", which reads as calm. (It would also have rendered `muted`.)
- [x] **A federated STS ARN reached `SimulatePrincipalPolicy`** — `parse_access_denied` deliberately passes through an STS ARN it can't rewrite, but the API rejects a non-policy-source principal as `InvalidInput`, and `:explain` rendered that under its "you probably lack `iam:SimulatePrincipalPolicy`" hint — pointing the operator at a permissions gap they didn't have. Refused locally now, naming the fix. `:explain ARN ACTION` also rewrites a pasted assumed-role ARN, which the parsed-from-error path already did.
- [x] **A test wrote to the real system clipboard** — `row_region_is_used_for_links_and_cli_snippets` calls `yank_cli()`. `yank` is a no-op under `cfg(test)`; `:update`'s direct `arboard` call goes through it, and a guard keeps that the only door.
- [x] **`alarm_dimensions` lost the false-positive escape hatch** — making the key add-only (third review) fixed "the operator's own spelling is invisible" and reinstated the opposite: non-EB alarms carrying an `EnvironmentName` dimension could not be disowned. A `-Name` entry removes a name; an all-removals list falls back rather than blinding the panel; the `-` survives a `:settings` save.
- [x] **`docs/safety-and-privacy.md` cited `:env-vars`** — not a command; the env-var reader is `:env list`. The prose guard written for exactly this class was scoped to three named files and couldn't see that page. It walks every doc plus README now, with an allowlist for metasyntactic placeholders.
- [x] **`:explain`'s args guard accepted any `arn:<anything>`** — subsumed by the principal check above.
- [x] **Two doc comments had drifted off their items** — `flatten_err`'s onto `MultiSelectFlavour`, and `AwsClient::with`'s fused to the line above. Artifacts of scripted doc-boundary edits.

Working note, not a code finding: `git checkout <file>` on a file carrying uncommitted work destroys it — done once here while restoring a mutation experiment, costing the `principal_not_simulatable` helper and a `ResolvedConfig` field. Mutation experiments back up the file they edit and restore from that copy; commit before experimenting.

#### Wrong-region work + bounded scans — 2026-08-22

The first five items off the 2026-08-22 triage list. Two of the five turned out to be stale entries; two of the "already done" security items had a real gap behind them.

- [x] **Per-row work went to the home region** — every per-env background fetch AND every write dispatched against `self.aws`, whose region is `context.region`. Under a multi-region fan-out the selected row is routinely somewhere else, so Detail showed the environment's name beside another region's instances, metrics, events, alarms, logs and queues; `:why` answered "why is this red" with the wrong region's evidence; the DLQ viewer peeked, purged and replayed against SQS URLs that don't exist there. Worst, `spawn_action` sent restart / rebuild / terminate / deploy / swap-cnames / scale to the home region — "environment not found", or, with a same-named env at home (what a fleet with per-region copies looks like), a destructive action against the wrong environment, with the audit line agreeing with the mistake. New `RegionClient` resolves inside the spawned task; a resolution failure lands on the same `Err(String)` the operation would produce, so no handler needed a new arm. **Self-review caught the sibling defect in the fix**: under `:account NAME` the account name lives in `context.profile`, so a cross-region row would have looked for an AWS profile that was never a profile — it re-assumes into the same account pointed at the row's region instead, exactly as `:org-health` does.
- [x] **The client-cache TTL reached one call path** — only `list_environments_in_region` used `cached_client`; a single-region operator never touched it, so pasting fresh static credentials still needed a restart, which is the case the TTL was added for. The tick now ages out the home client onto a new `AppMsg::ClientRefreshed` — deliberately not `Rebuild`, which tears down the fleet, overlays and both tails. Carries `rebuild_epoch` so a real switch always wins; failures are logged, not toasted over what the operator is reading. Excluded: demo mode, a refresh in flight, and AssumeRole contexts (hard one-hour cap; re-assuming is a different operation).
- [x] **`detail_nav` had no in-flight guard** — the 15-second auto-refresh tick stacked a new fan of sequential AWS calls on every slow scan, for as long as it ran. Guard derived from the existing `loading_*` flags (already cleared by every result handler) plus an age cap so a lost result can't wedge the tab for the session.
- [x] **Page budgets bounded round trips, not the wait** — `SCAN_PAGES` is worst-case 500 sequential calls with no timeout, no cancel and no partial render. Every capped walk now carries a wall-clock ceiling; hitting it is the same signal as hitting the page cap. `paginate_until` takes the deadline as an argument *so the branch is reachable in a test* — the first version of that test passed by hitting the page cap instead, because `tokio::time::pause` doesn't move `std::time::Instant`.
- [x] **`write_atomic` wrote `config.toml` world-readable** — it came from the shared crate using `std::fs::write`, i.e. the umask default, and `config.toml` carries `notify_webhook` (a bearer credential) and `accounts.*.external_id`. Shadowed locally with the mode set on the temp file rather than chmod'd after the rename, since the temp holds the same secrets for the same duration.
- [x] **Four audit fields still interpolated raw** — `append_lint_fix`'s `target=` / `rule_id=` / `namespace=` / `name=`, `region=` in both raw writers, and the header's `profile=`. `parse_audit_line` reads an embedded newline as a new entry and `ebman audit replay` re-dispatches parsed entries, so a raw field is a forge path into a destructive command.

Stale entries closed without code changes (each confirmed against the source, then pinned by a test where one was missing): **rebuild-epoch ordering** (already guarded; only its Err path was tested), **control.sock chmod race** (closed by SO_PEERCRED), **drift cleartext** (fixed in 0.27; guard test added for the three call sites).

#### Per-env region sweep — 2026-08-22

Completing the class the wrong-region fix opened. ~50 further spawn sites moved onto the row's region, and the choice is now pinned by a drift guard rather than by care.

- [x] **Every per-env write** — auto-rollback, alarm create/delete, the four batch dispatches, terminate-instance, `:ssm-run`, `:env-edit`, `:rollback`, option-settings from both the form and `:set-option`, tag updates, config-template apply/save/delete, listener cert edits, app-version delete, deploy from S3 and local.
- [x] **Every read that informs one** — `:options`, `:config-diff-local`, `:rds`, `:listeners`, `:lineage`, `:changes`, `:drift`, `:lint`, `:resources`, `:versions`, `:logs-insights`, the log tail, and the form pre-fills. `:config-diff` gets a client per SIDE: comparing two envs is its whole purpose and under a fan-out they're often in different regions, so one client silently compared the left-hand env against itself.
- [x] **The three pickers** (subnets, security groups, ACM certificates) — these write region-scoped IDs straight into the env's option settings, so home-region inventory was offering IDs that don't exist where they'd land.
- [x] **The per-row fan-outs** — worker-queue DLQ depth and the INST column build a client per env; app-latest-versions one per application. All three fill columns on rows that span regions.
- [x] **Audit regions follow the work** — including the completion line, which `ebman audit` correlates with its dispatch by action + target. A mismatched pair reported an action that started in one region and finished in another.
- [x] **Drift guard** — `every_spawn_declares_whether_it_is_per_env` requires any remaining `self.aws` spawn site to be named with a reason. Nine are: the fleet listing, identity, Cost Explorer, Organizations, IAM, the Secrets Manager browse, the custom-platform catalogue, the account-wide event tail, and `spawn_aws` itself.

Recorded honestly rather than fixed: **`applications` and `latest_stacks` are single collections**, so those two catalogues are the home region's under a fan-out. Widening them is a data-model change, not a client change; the guard's entry says so instead of implying the current behaviour is right.

#### Post-0.30.0 review pass — 2026-08-22

Reviewing what actually shipped in 0.30.0 rather than what the commit messages claimed. Five findings, all fixed; the first is the serious one.

- [x] **The role cache was never read.** The pre-tag review added `cached_role_client` and reported the STS-storm fix as shipped — but the edit routing `RegionClient::resolve` through it silently failed (`cargo fmt` had collapsed the match arm to one line, so the `replace` matched nothing), and the accompanying test only asserted that the cache gets CLEARED. So a cache existed, was cleared correctly, and nothing read it: per-env work under `:account` still re-assumed on every call. Now routed through it, with a test that seeds the cache and asserts `resolve` returns the seeded `Arc` — the read path, which is the half that was broken. **Verified by a mutation that was itself verified to have applied**; the first attempt was a no-op against the reformatted line and reported a false pass.
- [x] **`list_environments_for_account` re-assumed per call** — one AssumeRole per region on every 15-second tick under `:account` + `:region all` (the path the pre-tag review had just created), and one per account in `:org-health` / `:find-env`. Routed through the same cache.
- [x] **`region_for_name` couldn't see Detail's snapshot** — it only searched `self.environments`, but Detail is not torn down when a refresh drops its row (a terminated env, or a region whose fetch failed under a fan-out). The action menu targets Detail's env, so a restart / terminate dispatched there fell back to the HOME region: the original wrong-region bug in a narrow window, and silently.
- [x] **`:alarm-create` / `:alarm-delete` resolved Detail-first while operating on the selection** — `current_env_client` is Detail-first (matching `:alarms` / `:alarm-history`), but those two commands take their env from `selected_env()`. A refresh that reorders the table moves the selection while Detail keeps its snapshot, and then the alarm goes to one region and the audit line names another. Both now resolve from the env they operate on; a test pins why the two accessors aren't interchangeable.
- [x] **`apply_client_refresh` asserted rather than checked** that the refreshed client resolved the same region. A client pointing elsewhere while `context.region` disagreed would make `client_for_region` hand out the home client for the wrong region — the exact bug 0.30.0 exists to fix. It now verifies and keeps the old client on mismatch.

All five are post-tag, so they land in 0.30.1.

**Review of those fixes** (same day) — two more:

- [x] **The Detail-snapshot fallback was a partial fix.** The confirm modal carries only a target NAME, and there is an undo window between confirming and `tick_pending_dispatch` firing — so an action started from the TABLE (no Detail snapshot) on a row that a refresh drops in that window still fell back to the home region. `App::env_regions` now remembers where each env was last seen, consulted after the live table and the snapshot. An environment can't change region, the live table always wins, and a context switch clears it.
- [x] **The per-tick fan-outs went quadratic.** `spawn_env_instance_counts` and `spawn_worker_queue_check` iterate `self.environments` and called `client_for_env(&e.name)`, which scans the whole fleet by name — O(n²) every 15 seconds. They hold the row already; they use `region_for(e)` directly now.

Checked and sound: the role cache's 5-minute TTL against the STS session (no `duration_seconds` is set, so sessions are the 1-hour default — ~55 minutes of margin); no `std::sync::Mutex` is held across an await on any new path; `:alarm-history` correctly keeps the Detail-first accessor because `:alarms`, which it follows, resolves its env the same way.

#### 0.30.1 review round — 2026-08-22

Reviewing 0.30.1's own fixes. One finding, and it is the most instructive of the day.

- [x] **A workaround outlived the bug it compensated for.** `open_instance_in_console` used the HOME region *deliberately* — Detail's instance list was fetched through the home client, so an ID in that list came from the home region whatever region the row lived in, and the link had to agree with the data it named. Its comment said exactly that, and pointed at the BACKLOG entry for the underlying coupling. 0.30.0 fixed the fetch and left the compensation behind, so a real eu-west-2 instance ID now pointed at the us-east-1 console: "does not exist". Exactly the "fix the class, not the instance" rule — the sweep fixed sixty call sites and missed the one place that had been *bent around* the old behaviour. The region choice is now a pure `instance_console_target()` so it can be pinned; `open_url` isn't observable from a test and the choice was the part that was wrong.

Checked and sound this round: `env_regions` surviving `:region all` / `:region off` (neither rebuilds, and a remembered region stays correct because an env can't move); no stale-role hazard from a config reload (`apply_config_live` doesn't touch `cfg.accounts`, and `:settings` can only change theme / icons / refresh interval / redact / grouped); the `apply_client_refresh` region check can't misfire on a context switch because the epoch guard drops those first.

Recorded, not fixed: **`:region all` / `:region off` don't bump `generation`**, and `spawn_refresh` skips while one is in flight — so switching mid-refresh leaves the previous mode's rows on screen until the next tick. Pre-existing, self-healing in 15s, and a different class (refresh ordering) from the region work.

#### Surviving-workaround sweep — 2026-08-22

The class the 0.30.1 review found by accident, searched for on purpose: code deliberately *bent around* a behaviour this lineup changed. Grep finds code that does the wrong thing; it doesn't find code written to be wrong on purpose, because that reads as correct and carries a comment explaining why.

Method: 86 comments in `src/` carrying justification language (`deliberately`, `on purpose`, `works around`, `compensat`, `for now`, `the deeper issue`), each checked against whether its stated premise still holds.

- [x] **The breadcrumb named the session's region beside another region's env.** `REGION / app / env` rendered `context.region` unconditionally. That was accidentally truthful while Detail showed home-region data, and became a lie the moment Detail started fetching from the row's — `us-east-1 / uflexi / api-prod` for an env in eu-west-2 is exactly the confusion this release exists to remove. It names the env's region now, falling back to the session's when no env is named.
- [x] **`open_instance_in_console`** — found in the 0.30.1 review round, same class.

Checked and still true (no change needed): the truncated-Cost-Explorer note about leaving `costs_fetched_at` unstamped; the partial-fan-out note about not overwriting an operator's message; `RegionClient`'s note that assumed-role sessions aren't cached like profile ones (they are now, separately, with a TTL well inside the session cap); the `--demo` write refusals; `SsmRun` opting out of the preflight; `deny_write` bypassing `push_pending`.

Found while sweeping, recorded not fixed:

- [x] **Detail shows no region at all.** SHIPPED 2026-08-23. The header's second row now leads with `Region:`, resolved through `App::region_for(env)` — the same expression `detail_client` uses to pick the client, so the label cannot disagree with where the pane's data came from. Placed first in the row so it survives truncation on a narrow terminal. Render test asserts the ROW's region shows and the SESSION's does not; mutation-verified both ways (swap to `context.region`, and drop the field). Original note follows.  It replaces the screen with its own header and draws no breadcrumb, so an operator deep in a fan-out row's Detail has nothing on screen saying which region the pane's data came from. Not a stale workaround — a gap — so it's a UI addition rather than part of this sweep. Directly on-theme for the release, and small.
- [x] **`:region all` / `:region off` don't bump `generation`** — SHIPPED 2026-08-23, but *not* by bumping `generation`. A fan-out change alters which regions the fleet listing covers, not the account or credentials, so bumping `generation` would also drop every in-flight per-env result that is still valid — including `ActionResult` for a dispatched write, whose `complete_pending` would then never run and leave the header's `⏳ N` chip stuck forever (`apply_rebuild` clears `pending_actions` for exactly that reason; there is nothing to clear here). Added the narrower `fanout_epoch` instead, mirroring `rebuild_epoch`: `spawn_refresh` stamps it on the `Refresh` message and `apply_refresh` drops a superseded listing. Dropping is only half of it — `spawn_refresh` returns early while `load_state` is Loading, so the drop must also clear the flag and re-spawn or nothing fetches the new mode's rows and it wedges. Three mutations verified (neuter the check; drop-without-respawn; remove the bump). Original note follows.  and `spawn_refresh` skips while one is in flight, so switching mid-refresh leaves the previous mode's rows on screen until the next tick. Pre-existing, self-healing in 15s, different class (refresh ordering).

#### Backlog verification sweep — 2026-08-22

The `:1068` line turned out 74% stale, so the rest of the open backlog got the same treatment: every entry that makes a **claim about current behaviour** was checked against the code. Feature entries ("we haven't built X") have nothing to verify and were skipped.

Ten verifiable claims. **Three were already fixed** and are struck above — the webhook drain before CLI exit, the events-panel cursor follow, and the `report_bug` multibyte mojibake. Each carries a comment describing its own fix, so again: fixed deliberately, record never updated.

**One was partly fixed**: the ascii icon-mode stragglers are down to six sites (header delta arrows, sort marker, one cursor `▶`, the Metrics anomaly arrows) from "table markers, header, form, cursors".

**Six confirmed still real**, all checked at the call site:

- `WRITE_COMMANDS` derivation (re-scoped separately above).
- **Superseded-token message** — `writes.rs:468`. One `pending` slot, so a newer plan replaces the old and confirming the old token gets "unknown confirm_token". Agent-facing, and the fix is small: remember the previous token so the message can say what happened.
- **Unified MCP tool registry** — `tool_table` / `read_tool_table` in `tools.rs` plus dispatch in `mod.rs`; descriptors and handlers are still separate.
- **Shared verb-dispatch helper** — `action.rs` hand-maps `"rebuild" => "Rebuild"` for audit AND `"rebuild" => aws.rebuild_env(...)` for dispatch; MCP has its own 13-arm `WriteVerb` map. Two verb tables.
- **pin/freeze check order** — CLI is pin-then-freeze (`action.rs:161-162`), MCP's `write_gate` checks freeze first. The freeze refusal string is rendered in both. Cosmetic, as the entry says.
- **CLI rollout freeze is start-only** — `refuse_if_frozen` runs once at `action.rs:607`, not per region. Already flagged as a conscious choice.
- **Form scrolling** — `draw_form` has no scroll handling at all; nine-field forms on 80×24 still leave fields below the fold.

Standing lesson from both sweeps: a backlog entry is a claim with a timestamp, and an unverified one is worth less than nothing — it makes settled work look pending and any estimate against it wrong. Verify before planning.

#### Pre-0.30.2 code review — 2026-08-22

Reviewing the eight unreleased commits before tagging. One finding, and it is the worst kind: a fix that inverted the bug it was fixing.

- [x] **EBL010's `Option` fix was undone by all three of its callers.** Making `env_tag_keys` an `Option` fixed the RULE — `None` skips, `Some(&[])` fires — but every call site collapsed `None` into an empty Vec before calling: `:lint` (`cmd_misc.rs`), the confirm-modal lint (`spawn_deploy.rs`) and `ebman lint` (`cli/lint.rs`) all did `tags_opt.unwrap_or_default()`. So a failed `ListTagsForResource` went from *silently skipping the rule* to *firing a false positive for every required key on every env* — strictly worse than the bug it replaced, and it would have shipped. All three now keep the `Option`, and `LintInputs::env_tag_keys` is `Option<Vec<String>>` with `bare()` defaulting to `None`.
  A structural guard rejects `unwrap_or_default` on the tag-keys binding at any of the three. **Its first version was too weak** — it checked one line, and the mutation used to verify it spanned two, so it reported a false pass. It reads the whole statement now.

Reviewed and sound: the SSM chunking (the audit's `ok_count` counts `status == "Success"` against the full instance count, so `SendFailed` rows correctly land outside the numerator, and `format_ssm_results` renders any status generically — no branch treats an unknown one as success); the breadcrumb region; `instance_console_target`; the JSON-parser switch, including `is_backend_pointer`, which still classifies a pointer as such and treats a malformed file as one.

#### Design pass: a unified write table — 2026-08-22, VERDICT: DON'T

Five open items (unified MCP tool registry, shared verb-dispatch helper, pin/freeze check order, start-only rollout freeze, `WRITE_COMMANDS` derivation) looked like one root cause: four independent enumerations of "what is a write". The design pass was to decide whether one table with three consumers closes them.

**It doesn't, and the premise was wrong.** The four enumerations are not four views of one set. They are overlapping sets at different *granularities*:

| Surface | Count | Contents |
|---|---|---|
| TUI `Action` | 15 | Rebuild, RestartAppServer, SwapCnames, Terminate, Deploy, UpgradePlatform, Clone, Scale, Capacity, AbortUpdate, ConfigSave/Delete/Apply, TerminateInstance, SsmRun |
| MCP `WriteVerb` | 5 | Deploy, Restart, Rebuild, Terminate, SetOption |
| CLI shared dispatch | 3 | rebuild, restart, terminate (`deploy` and `rollout` have their own paths) |
| Test `WRITE_COMMANDS` | 12 distinct | Mostly option-setters — `deployment-policy`, `keypair`, `public-ip`, `tag`… **only 3 correspond to an `Action` variant at all** |

The intersection of all three code surfaces is four verbs. Ten `Action` variants exist nowhere else. And the test list is a *different axis entirely*: `:command` invocations that mutate through option-settings, not action verbs. Deriving it from a verb table was never possible — that was the mistake in the original entry, and re-scoping it to "hand-classify 112 commands" only replaced one wrong shape with another.

A unified table would therefore be four shared rows, ten TUI-only rows, and a separate mechanism for the option-setter commands. That is not a unification; it is the current structure with a table around it, and it would touch MCP, CLI, TUI and tests to get there.

**What the pass does support — three small independent fixes:**

- [x] **CLI's two parallel maps for the same three verbs** — DONE (see M2 above; `CliVerb`, and it exposed the audit-name split). Was: (`action.rs:170` audit label, `action.rs:191` method) — a genuine drift surface: adding a verb means editing two matches and the compiler checks neither against the other. One `&[(&str, &str, fn)]` row per verb, ~20 lines. This is item M2 at its real size, which is much smaller than "shared verb-dispatch helper" implied.
- [x] **pin/freeze check order** — DONE (see M3 above; `refuse_write` + `freeze::refusal_message`). Was: CLI is pin-then-freeze, MCP is freeze-then-pin. A five-line alignment plus one shared refusal string. Cosmetic, as the entry always said.
- [x] **MCP tool registry** — DONE (see I1 above; pinned by test rather than restructured). Was: descriptors in `tools.rs`, handlers in `mod.rs`. Genuinely worth doing and **independent of the other two**; it is about MCP's own shape, not about the write surface.

**Not supported, and now recorded as such:** `WRITE_COMMANDS` cannot be derived from any verb table. If the safety property test is to cover more of the surface, the honest route is to keep the hand-written invocation list and add a guard that every `:command` whose handler reaches a mutator appears in it — a reachability check, which is exactly the analysis that produced false positives in both directions when attempted (`:explain` and `:changes` read as gated, `:rebuild` as non-mutating). So: keep the list hand-written, and stop filing it as derivable.

The lesson repeats: the plan was built on an unverified reading of the backlog, and checking the actual enumerations before designing killed it in ten minutes.

#### Max review of the post-0.30.2 lineup — 2026-08-22

Twelve commits reviewed. No functional defects found in them — the one thing the review changed is a release constraint, and it matters.

- **The next release carrying this lineup must be 0.31.0, not 0.30.3.** `series_anomaly_label` is public API (`pub mod ui` + `pub use detail::{…}`) and gained an `IconStyle` parameter. Under Cargo's 0.x rules the MINOR position is the breaking position, so a patch bump would ship a breaking signature change on a crates.io library. Nobody realistically depends on that function, but the version number is a claim and it would be a false one.
- [x] **The ascii render test didn't cover the anomaly badge.** Both existing tests stayed green when the glyph was hardcoded *inside* `series_anomaly_label` — the fleet-view frame never renders the Metrics tab, and the unit test passes its own `IconStyle`. So the CALL SITE was unpinned: hardcoding `IconStyle::Unicode` there would have passed everything. Added a test that renders the Metrics tab with a spiked series; verified by that exact mutation. This is the session's recurring failure mode found in my own work, one commit after writing the house rule about it.

Checked and sound: the pin/freeze order is now uniform across all four CLI write paths, not just the two in `action.rs` (`audit replay` and `lint --fix` already did freeze-then-pin with their own inline checks — no gap, and the docs' claim that both refuse pinned targets holds); both refusal paths exit 3, so reordering changed the message and not the exit code; `retired` holds only superseded tokens that no path will accept, so remembering them is diagnostic rather than an auth surface; the MCP source scan reads one flat match with no nested string arms, and its `arms.len() >= 10` floor catches a formatting change that would silently shrink it.

- [x] **`ebman action <unknown-verb>` validated too late** — FIXED 2026-08-22 on the user's call. It built an AWS client and ran the safety gates before noticing the verb was wrong, so with no credentials it reported a credential failure and on a frozen fleet it reported the freeze (exit 3) rather than a usage error. A malformed command is malformed whichever state the fleet is in. `CliAction::parse` routes `deploy` and the three `CliVerb`s before anything else is touched; verified end to end that an unknown verb now exits 2 with the usage line and no AWS call. A test also pins that `rollout` is NOT routable here — it is dispatched earlier, and routing it as a plain verb would run a single-env action for a fan-out command.

#### `ui.rs` split — 2026-08-22

5,046 lines → 199. Eight new modules, one per surface, plus the 1,040-line test module lifted to `ui/tests.rs`.

| module | lines | |
|---|---|---|
| `table` | 1,135 | environments + applications tables and their cells |
| `header` | 867 | pill chain, breadcrumb, width arithmetic |
| `action` | 677 | the confirm modal |
| `chrome` | 464 | blocks, pills, glyphs, colours — the shared vocabulary |
| `footer` | 259 | key strip, status line, health hint |
| `events` | 218 | events panel, severity + timestamp formatting |
| `dlq` | 187 | the DLQ viewer |
| `shell` | 129 | the embedded SSM pane |

Byte-faithful, and **proved so rather than asserted**: a whitespace-normalised, comment-stripped line census against the pre-split file shows 2,278 distinct code lines, of which 70 changed — all 70 the declaration lines that gained a `pub(crate)` prefix (68 exact, 2 rewrapped by rustfmt when the prefix pushed them past the width limit). The only additions are the `mod` declarations and the glob re-exports.

The re-exports are what keep this cheap: `pub(crate) use chrome::*` and friends mean every sibling's `use super::*` still resolves, which is the convention `detail` / `overlays` / `help` already relied on — so moving an item between view modules later doesn't touch its callers, and `ui/tests.rs` reaches its subjects through `super::` regardless of which module owns them.

- [x] **`draw_table` decomposition** — 695 → 512 lines, 2026-08-22. Two extractions, and the first is the point of the exercise:
  - **`visible_columns`** (75) — the whole column-set rule: view-mode presets, the fan-out-only REGION column, the `:cost on` opt-in inserted before AGE, and the per-column hide list with NAME exempt. Pure, four inputs, and now **tested** — previously the only way to ask "does `:cols hide NAME` do nothing?" was to render a frame and read it back. Eight assertions, two mutation-verified.
  - **`env_cell`** (163) — the per-column match, moved verbatim behind a `CellCtx`.
  - [x] **`Separator` branch extracted 2026-08-23** — `separator_row`, 137 lines out, `draw_table` 511 → 382. It needed only the env list, the colour map, three theme fields and the column slice, so the signature says that rather than taking `&App`. Verified as a *pure* refactor by diffing the rendered frame before and after: byte-identical. It also turned out to have had **no coverage at all** — stubbed to an empty row, all 1,135 tests passed — so a render test went in first. **Remaining: the `DisplayRow::Env` body (~200).** It captures ~30 `App` fields and would need a context struct like `CellCtx`; that is where the value per edit genuinely drops off. Both are still inline. Extractable the same way; stopped here because the value per edit drops off sharply after the column logic.

  Two things worth recording from the attempt. The arms shadow context fields with locals of the same name (`alert`, `color`), so the first version — a regex rewriting `alert` → `ctx.alert` — silently broke `let alert = …` inside an arm. Destructuring on entry keeps the arms byte-identical and can't do that. And holding `&App` in the context **does not compile**: the rows borrow `app.environments`, and passing the whole struct defeats the field-level split the borrow checker was using to allow `&mut app.table_state` at the render call. Six of the arms did a map lookup keyed by the row's env, so the context holds those six resolved values instead — cheaper per row, and the reason it builds.

#### Supply-chain + API gates — 2026-08-22

Three gates added to CI. Two of them found something on the first run, which is the argument for having them.

- [x] **`cargo-deny`** (advisories / licences / bans / sources). Nothing had ever checked 61 dependencies against RUSTSEC. First run: six advisories, one yanked crate, three licence rejections.
  - The licence rejections were **my policy being too narrow** — BSL-1.0, CDLA-Permissive-2.0 and BlueOak-1.0.0 are all permissive and compatible; added with a note that each earned its place from a real dependency rather than being pre-loaded.
  - `cargo update` cleared the yanked `spin` and one `h2` copy. It also **broke the build**: `serde_yml` 0.0.12 → 0.0.13 changed `Value::get` to take `&str` and changed mapping iteration to hand out the key as `&str` — two breaking changes inside a *patch* release. Four call sites in `lint.rs` got simpler as a result. It also stopped reading `""` as a null document, which broke the EB CLI config parser; that now states "empty means no settings" itself rather than depending on a YAML library's null handling — the same fix shape as the tfstate parse earlier today.
  - The six surviving advisories are waived with **dated, individually justified** entries: five are transitive through the AWS SDK's rustls 0.21 / hyper stack or ratatui's `paste`, none reachable from anything ebman does, all fixed upstream or harmless. A gate that always fails gets ignored, so the exceptions are in the open with reasons rather than the threshold being loosened.
- [ ] **Migrate off `serde_yml`** — RE-SCOPED 2026-08-23 after looking properly. Two findings changed the shape of this:

  **It was nine consumers, not one.** The entry said "only remaining use is `saved_config.rs`". In fact five more were live, and **four of them were JSON being parsed by a YAML parser** — including `parse_baseline`, whose own error message says "baseline JSON parse failed", and three round-trip tests asserting output *is valid JSON* while reading it with a YAML reader that accepts things JSON rejects. All five moved to `serde_json` (already a direct dependency), so the surface is now exactly two files: `saved_config.rs` (EB saved configurations) and `eb_cli.rs` (`.elasticbeanstalk/config.yml`) — both genuine YAML. The `json_surfaces_are_parsed_by_a_json_parser` guard was scoped to the two files I happened to be editing when I wrote it; it covers all five now.

  **There is no obviously-right replacement**, which is why this stays open rather than being done today. Every serde-integrated YAML crate in the ecosystem is stale: `serde_yaml` deprecated (Mar 2024), `serde_yaml_ng` last released May 2024, `serde_norway` Dec 2024. The only actively-developed option is `saphyr` (released 2026-08-18) — but it is 0.0.x and is a parser rather than a serde integration, so it means hand-writing the deserialisation for both files rather than swapping a crate name. That is a trade-off with no clear winner, so it wants a deliberate decision, not a drive-by.

  Meanwhile the waiver's blast radius is two files instead of nine, and neither parses anything an attacker supplies — EB writes the saved configs, the EB CLI writes the other.
- [x] **`cargo-semver-checks`** on pull requests. ebman is lib + bin on crates.io, so a signature change in the lib decides whether the next tag is a patch or a minor — 0.30.2 shipped one (`ui::series_anomaly_label` gained a parameter) that a human review caught, which is not a thing to notice by reading.
- [x] **Least-privilege workflow permissions.** Four open CodeQL alerts, one per CI job (`actions/missing-workflow-permissions`) — jobs inheriting the repository default rather than declaring what they need. `ci.yml` is `contents: read` throughout; `release.yml` drops from a blanket `contents: write` to read, with only the `publish` job opting up, since `build` uploads via `actions/upload-artifact` and `crates_io` authenticates with its own token.

#### candor as the fourth gate — 2026-08-23

`.candor/` had scan reports dated 1 August, no policy file and no CI job — a one-off scan, not a gate, and the reports predated the `app.rs`, `aws.rs` and `ui.rs` splits, so their callgraph described a codebase that no longer existed.

`.candor/policy` now encodes the boundaries `ARCHITECTURE.md` already claims but nothing enforced, and a CI job runs it on every push and PR. Verified by canary: a `draw_footer` that reaches `llm::dispatch` is blocked, naming both the function and the path.

What the policy asserts, and what it deliberately doesn't:

- **`deny Net Llm ui`** — the invariant that matters. A repaint happens on every keystroke, so a render function that calls out turns a redraw into a round trip. Passes today.
- **`forbid aws -> ui`** — the AWS boundary knows nothing about the TUI.
- **NOT `forbid aws -> app`**, though the layering doc implies it: the layer matcher is a prefix match, so `app` also matches `aws_sdk_elasticbeanstalk::…::application_name`, and the rule fired on nine honest EB calls. A rule that cries wolf nine times teaches people to skip the output.
- **NOT `deny Exec Ipc ui`** — and this is a real finding rather than a false positive. `ui::shell::draw_shell` renders the embedded SSM pane, which reads a live PTY: Exec to spawn, Ipc to read. The one render path that legitimately isn't pure, now declared rather than quietly true.
- **NOT a test-harness capability rule.** With `--include-tests`, candor reports 102 test functions reaching Clipboard — because it is a syntactic scanner and sees the `#[cfg(not(test))]` arm of `yank`, which is exactly the arm compiled out under test. A gate firing 102 times on day one gets switched off. The `cfg(test)` stub plus its guard test is what actually holds that boundary.

Operationally important: **the CI job must run the dylint lint, not a scan report.** `candor-scan` (syntactic, stable toolchain) reports `ui::draw` with no Exec/Ipc and omits `ui::shell::draw_shell` entirely; only the type-resolved lint finds them.

**Correction to how this was first written up:** that is not "the two backends disagreeing", and not a scanner defect. `draw_shell` reaches its effects through `vt100`, and the scan output says so in as many words — *"candor's classifier doesn't cover 7 dependencies this code calls into — their effects are INVISIBLE to the scan (absent from the report, NOT a claim they're pure): ... vt100 (2 calls)"*. It disclosed the gap; I skimmed past it. The never-lies contract worked as advertised. The operational conclusion (gate on the lint) stands; the implied criticism does not.

- [x] **`ui::overlays::draw_form` recomputes a config path every frame** — FIXED 2026-08-23. The banner is computed at open (`Form::banner_for`) and stored on the form. `deny Fs Env ui` is now IN the policy, and a canary that puts `config_path()` back in the draw is blocked by it — a finding becoming a rule once it's fixed, so the fix can't quietly come back. Was: — `config::config_path()` → `util::config_dir()` → reads `$HOME`, to render the "file: …" banner telling the operator where `:settings` will write. Not I/O in production, but the render layer reaching into config is the layering the doc forbids, and it happens per repaint. Hoist it to when the form opens; then `deny Fs ui` can go in the policy and the exception comment comes out.

#### Max review of the unreleased lineup — 2026-08-23

Four findings, all in my own work from the last two days.

- [x] **The candor CI job could report success without running.** `|| true` on the lint meant a crash, a missing lib or a compile error produced no AS-EFF lines, the grep found nothing, and the gate passed. The same failure as the gitignore catch the day before — a gate with nothing behind it — in the same file, one commit later. It fails on a non-zero exit now and separately asserts the output contains `Checking ebman`, since a silently no-opping lint emits no findings either. That second check needs `touch src/lib.rs src/main.rs` first: `rust-cache` restores the check artefacts and a cached `cargo dylint` prints `Finished` without re-running, so the gate would otherwise pass on results from before the change under review. Verified by running the exact job body locally, clean and against a canary.
- [x] **The STATUS pill's alert was unpinned.** Forcing `StatusAlert::None` at the table's call site — which strips the alert colour from every Red env and every backed-up worker, the thing that says *this one* at a glance during triage — passed all 1,097 tests. `status_alert()` had unit tests; nothing checked the table used the result. Now pinned. **The first version of that test also passed under the mutation**, because it asserted on the ROW and the HEALTH dot is red on its own; it hides HEALTH and TREND so the status pill is the only thing that can colour the row.
- [x] **A comment in `parse_baseline` still described `serde_yml`** after the function moved to `serde_json`.
- [x] **Five items left the public API by accident** — `event_severity_style`, `visible_window`, `StatusAlert`, `format_instance_counts`, `status_alert` were `pub` in the old `ui.rs` (a `pub mod`) and now sit in private modules behind `pub(crate) use`. None is used outside `ui/`, so the narrowing is right, but it was a side effect rather than a decision. Recorded as deliberate for 0.31.0 — and it is precisely what the new `cargo-semver-checks` job exists to flag.

Checked and sound: no name collisions across the eleven `ui` modules, so the glob re-exports are unambiguous; the two `alert` bindings in `env_cell`'s STATUS arm and in `name_cell` are genuinely distinct locals, so dropping the `CellCtx` field was correct; the `parse_baseline` identity round-trip does cover the fields map, mutation-verified.

Standing tally worth keeping: **four tests written this session passed under the mutation they were written to catch.** Every one was caught by checking the mutation applied AND that the test failed — not by writing the test carefully.

#### Pre-0.31.0 review — 2026-08-23

Focused on what a RELEASE decision turns on, rather than re-reading code already reviewed twice.

Checked and sound:

- **`release.yml` permissions.** No job writes to the repo except `publish`, which creates the GitHub Release via `softprops/action-gh-release` and has `contents: write`. `build` uses `upload-artifact` (job-scoped), `crates_io` authenticates with its own token, and `mcp_registry` edits `server.json` in the working copy only. **Untested until the next tag** — it is the one change in this lineup that cannot be exercised before it matters. Failure mode is a clear 403 at publish time, not a silent bad release.
- **`cargo-semver-checks` config.** `feature-group: default-features` is the only sensible setting: the crate has no `[features]` section.
- **`deny.toml` waivers.** Exactly six live advisories, exactly six waivers, no stale entries and no gaps — verified against a run with `ignore = []`.
- **Release binary smoke.** Builds clean, `--version` correct, unknown subcommand and unknown action both exit 2.

- [x] **`--demo` without a TTY reports a raw OS error** — SHIPPED 2026-08-23. Guard moved *inside* `enter_tui` rather than the `--demo` call site, so every path into the alt-screen is covered, not just the one the bug was noticed on. Verified before/after against the shipped 0.31.0 binary. The message is a pure `no_tty_message()` so its *rendered* form is testable — asserted to carry no embedded newline and no double-space indentation hole, the wrapped-literal defect this project has shipped twice; mutation-verified by removing the `\` continuations. Original note follows.  — "Device not configured (os error 6)" from `enter_tui`, rather than "ebman needs a terminal". Confirmed PRE-EXISTING and not a regression: `src/main.rs` is untouched in this lineup and the installed 0.30.2 behaves identically. Cosmetic, and it is what someone piping ebman in CI will see first.

Two methodology errors of my own, caught during the review and worth recording because they are the same class the candor session and I had just finished discussing:

1. I compared the waiver list against `cargo deny`'s output **with the waivers applied** — circular, and it made all six look stale.
2. The corrected run used `cargo deny --config X check` instead of `cargo deny check --config X`, which cargo-deny rejected as a usage error. I read the resulting empty output as "no live advisories". A proxy standing in for a signal, indistinguishable from success when it fails — ten minutes after writing that sentence to someone else.

#### Command-dispatch coverage sweep — 2026-08-23

Same method as the render sweep, new axis: neutralise each of the 131
registry commands at the top of `execute_command` (alias-aware — the
match arms carry aliases, so stubbing only the canonical name would have
produced false "uncovered" for anything a test reaches via its alias),
run the suite, see whether anything fails.

**43 caught, 88 uncovered.** Two-thirds of the command surface can be
made a complete no-op with the suite still green. A command that
silently does nothing is worse than a blank pane, because the operator
believes it worked.

The good news first: **all 29 declared write commands are covered.**
`WRITE_COMMANDS` / `BATCH_WRITE_COMMANDS` / `APPLICATION_SCOPED_WRITES`
are iterated by the safety tests, so stubbing any of them fails those.
The highest-stakes subset was already protected.

The gap was next door. `WRITE_COMMANDS` pins the option-setting
commands — the ones with no `deny_write` of their own. The
**confirm-modal actions** are gated elsewhere and so appear in no list:
`restart`, `rebuild`, `terminate`, `stop`, `start` were pinned by
nothing at all. Now covered, with the action each arms and the env it
aims at asserted.

- [x] **`:terminate` armed a confirm modal under `--deny-write`.** Found
  by writing that coverage. `cmd_terminate` calls `open_action_menu()`
  and then `advance_action_flow(Action::Terminate)` — and
  `advance_action_flow` has no gate of its own, because it is designed
  to be called from *inside* the menu that already gated. The menu
  refused and the next line armed the confirm anyway.

  **Not a safety hole, and worth being precise about why.** Probed it
  rather than assuming: `open_action_menu` returns before setting
  `mode = Action`, so the modal was unreachable by keyboard and never
  drawn, and confirming produced no dispatch (`pending_dispatch` false,
  `pending_actions` empty). What it did do was leave inert `Terminate`
  state in `action_flow`, which is enough to make `?` open the Action
  help instead of the global one. `open_action_menu` returns `bool` now
  and `cmd_terminate` honours it; the two keybind call sites discard it
  explicitly, since a refusal there has already set its own message.
  Fix mutation-verified by reverting it.

Then the rest, to **131 of 131**. The nine that came first were: `env-edit`, `delete-version`,
`custom-platform-delete`, `rds-attach`, `unset-option`, `abort`,
`rollout`, `swap`, `scale`.

**What that second pass found.** `WRITE_COMMANDS` pins the commands with
no `deny_write` of their own; everything that gates *inside* its own
handler was therefore in no list at all, so nothing pinned that it kept
gating. Sixteen such commands. Each was checked before being listed —
**none was a hole** — and they are now pinned by `GATED_COMMANDS` plus
two tests for the awkward ones.

Two of them needed care, and are the reason the first probe looked
alarming:

- `:swap` is turned away on its argument ("target not found") before the
  gate is consulted, so it needs a second env in the same application
  before the question can even be asked.
- `:ssm-run` needs cached instances from an open Detail pane for the
  same reason.

Both refuse correctly once given valid input. Read as "ungated" on the
first pass purely because the precondition fired first — a good reminder
that *absence of a refusal message* is not evidence of an absent gate.

- [x] **`:rds-attach` opened a seven-field form under `--deny-write`.**
  The write itself was gated on submit, so nothing could escape, but the
  operator filled the whole form in before being told no. `:env-edit` —
  the sibling command, also a form — refuses at open. Made consistent:
  `rds-attach` now gates at open too. Mutation-verified by removing the
  new gate and watching `GATED_COMMANDS` catch it.

Correctly **not** gated, checked rather than assumed: `:update` (prints
the upgrade command for the detected install channel — touches no AWS)
and no-arg `:upgrade` (lists compatible platforms, a read; the ARN form
gates).

#### Post-panel delta review — 2026-08-24

A fourth review, scoped to the three commits that landed AFTER the
release panel ran — the panel reviewed a lineup that no longer existed.
Verdict SHIP, one real defect.

- [x] **Deleting `cost_usd_per_month` left its doc comment attached to
  the NEXT field**, so `newer_stack_available` documented itself as
  "Cost in USD per month". The same class as the split doc comment the
  first panel's skeptic found in `freeze.rs` — a deletion that leaves
  prose behind, twice now. Fixed.
- [x] `pub use tui_common::overlay` was kept public alongside
  `font_probe`, but only `font_probe` is reached from `main.rs`. Now
  `pub(crate)`.

Confirmed rather than assumed, which is why the review was worth
running: the `with_events` / `with_cost` deletion was correct. The
reviewer checked three ways — no rule reads events or cost, EBL003
("Red for extended period") deliberately uses `env.updated` as its
duration proxy instead, `docs/lint-rules.md` has no such rule, and the
backlog's only forward pointer is generic. Scaffolding for rules never
written, and git history keeps it.

It also downloaded the pinned `mcp-publisher` artefact and verified the
sha256, the tar member name, that the job is ubuntu-only so `sha256sum`
exists, and that `-c -` is shell-correct — i.e. the untested release
step will work at tag time.

- [x] **`app` is still a wide surface.** DONE 2026-08-24 (0.34.0). Keeping the module public keeps
  `App`'s ~100 pub fields and the `mode_action` / `mode_detail` /
  `mode_dlq` re-exports (`DetailState`, `ConfirmModal`, `ViewState`,
  `TailView`) public with it — so a new field on `DetailState` will
  still trip semver-checks next cycle. The narrowing removed most of the
  tax, not all of it. Next step: `pub(crate)` inside `app.rs`, keeping
  only `App` and its three entry points public. Deliberately not done
  in this release — it is a second breaking change to an API with zero
  consumers, and it should be measured, not bundled.

#### ratatui 0.30 / crossterm 0.29, and the dependabot queue — 2026-08-24

- [x] **ratatui 0.29 → 0.30.2, crossterm 0.28 → 0.29**, with
  `tb-tui-common` 0.1.3 → **0.2.0 published to crates.io** so ebman
  builds against the registry rather than a path patch.

  Neither crate needed a code change. The 85 compile errors a naive bump
  produces are entirely a two-versions-in-one-tree clash — `tb-tui-common`
  0.1.3 pinned ratatui `^0.29`, and 0.30 relocated types into
  `ratatui-core`, so `Color` from 0.29 and `Color` from 0.30 are distinct
  types to the compiler. It reads as a large API migration and is nothing
  of the sort; the compiler says so plainly once you look ("there are
  multiple different versions of crate `crossterm` in the dependency
  graph"). Aligning the versions took it to zero errors without touching
  a line of either crate.

  Two real changes fell out. `ListState` is now `Copy`, so a `.clone()` in
  `ui/overlays.rs` became redundant — and *required* on 0.29, which is why
  the code is now 0.30-only rather than compatible with both. And the OSC 8
  behaviour change above.

  Verified beyond the suite, because "the tests pass" is weak evidence for
  a TUI library bump: rendered the same frame — three envs, two apps,
  grouping on, a selected row, health colours, the group-separator summary
  — on 0.29 and on 0.30 and diffed. Byte-identical.

  **Corrected in the pre-0.33.0 review:** that claim was broader than
  its evidence. The frame diffed was the *default* view mode. Spacious
  differs — ratatui 0.30's `visible_rows` gained "include a partial row
  if there is space" with no 0.29 equivalent, so an odd-height rows area
  renders a clipped half-height row at the bottom. Cosmetic, but
  "byte-identical" was asserted about the bump and tested on one mode.

- [x] **The candor gate in `tb-tui-common` moved underfoot.** It passed in
  June and failed on a commit that changed no code at all, because
  `cargo install candor-scan` was unversioned and a newer scanner
  classified the same source differently. Third instance of that pattern
  across these repos; pinned to 0.31.0. The finding itself was true and
  the *rule* was over-broad: `font_probe` writes a glyph and reads the
  cursor position back, which is Ipc because it is a conversation with
  another process — but that process is the terminal, and asking the
  terminal what it can render is what a shared TUI library is *for*.
  Exempted narrowly, with the reasoning written into both `.candor/policy`
  and `ci/candor-check.sh`.

- [x] **Dependabot queue cleared — all nine PRs closed.** #7 (ratatui) and
  #9 (crossterm) auto-closed once main passed them; neither could have
  worked alone.

  - **The four action bumps landed as one commit**, not four merges.
    `release.yml` hands built tarballs between jobs via
    upload-artifact → download-artifact, which is version-coupled: merging
    #2 without #4 leaves a v7 uploader feeding a v4 downloader, and the
    failure surfaces at the next tag, in the one workflow with no dry run.
    Merged in sequence main is briefly in exactly that state. Checked the
    inputs actually passed rather than assuming — `pattern`,
    `merge-multiple`, all five softprops inputs still exist. The
    asymmetric v7↔v8 pairing is deliberate upstream, confirmed from
    download-artifact's own end-to-end example.
  - Fixed while in there: `if-no-files-found` defaults to `warn`, so a
    staging step that produced nothing would warn inside a green job and
    `publish` would attach an **empty asset set to a real tag**. Now
    `error`. Inherited free from v8: `digest-mismatch: error`.
  - **`toml` 0.8 → 1.1, `sha2` 0.10 → 0.11, `aws-smithy-mocks` 0.2 → 0.3**,
    taken locally rather than merged so fallout could be fixed in the same
    commit. `toml` 1.1.4 carries `+spec-1.1.0` — a change to the
    *language*, so "it compiled" says nothing. What makes it safe is that
    TOML 1.1 is a widening, and `project::parse` is the crate's only
    `toml::` call site and nothing serialises TOML, so ebman cannot emit
    1.1 syntax an older ebman would reject. Pinned with two
    mutation-verified tests rather than left to be re-derived.

#### Structure + tooling review, acted on — 2026-08-24

An invited Rust review of project structure and tooling. Its headline:
**don't split the crate** — measured 13s incremental, 8s test build, 3.4s
suite, with cold-build cost dominated by 17 AWS SDK crates a workspace
split wouldn't touch. Shipped from it:

- [x] Two live wrapped-literal bugs, and a lexer-based guard (five
  hand-rolled scanners failed first).
- [x] `tokio-stream` + `aws-types` dropped; `cargo-machete` in CI.
- [x] MSRV job runs `cargo test`, not just `cargo build`.
- [x] One CLI write gate + a convergence guard.
- [x] AWS errors classified from the code AWS reported, not a `Debug`
      dump — which had a reachable false positive.
- [x] RAII terminal guard, best-effort restore, `panic = "abort"`.
- [x] Cached view indices checked.
- [x] Test suite stops mutating the process environment.
- [x] Lint probes distinguish "couldn't check" from "clean".
- [x] `unwrap_used` / `expect_used` denied.
- [x] Property tests for the four hand-rolled parsers.
- [x] Nightly `cargo-mutants`; release provenance attestation;
      dependabot with the AWS crates grouped.
- [x] Per-frame screen clone gated on the control socket.

**Deferred, because they are breaking changes** and the queued fixes
should ship as a patch first:

- [x] **Narrow the public API.** DONE 2026-08-24 (0.34.0) — **4565 items
  -> 212** (`cargo public-api`, no flags; 67 omitting auto-derived and
  blanket impls), and the set of public items exposing a bumped
  dependency's type from 38 -> 2.

  *Numbers corrected at the 0.34.0 release panel:* this first recorded
  "2430 -> 126", which was a filtered count from an ad-hoc grep of mine
  and reproduces only if you know the filter. A flagship figure has to
  survive someone running the tool. The reduction is the same either
  way; the citation was not.

  The estimate below said 500 items; the tool measured 4565, because a
  public module re-exports
  everything `pub` inside it and `pub mod app` alone held 1874.
  Narrowing *modules* was never going to do it — narrowing items did.
  Three fields with exactly one external reader became named accessors.
  `#![warn(unreachable_pub)]` now holds the line: it flagged 377 sites
  (`cargo fix` applied them) and surfaced two methods dead in production
  that `pub` had hidden from `dead_code`. Original estimate follows.
  — 500 pub items, 107 pub structs, 94% of
  named-field structs all-`pub`, serving a `main.rs` that touches 12.
  This is *why* 0.31.0 shipped an accidental breaking change, and it
  makes `cargo-semver-checks` a permanent tax rather than a safety net.
  The reviewer's highest-leverage 12-month item. ~2-4h, one big diff.
- [x] **Collapse the `Option<T>` + `loading_*: bool` pairs** — PARTLY
  DONE 2026-08-24.

  **Correction, same day:** I first recorded here that there were
  "eight pairs, not 14" and that the entry was wrong about the count.
  That was my error, not the entry's. I had surveyed `DetailState` (8)
  and never opened `ConfirmModal` (6). 8 + 6 = 14, exactly as written.
  The count was right; I published a correction to a claim that did not
  need correcting, which is worse than leaving it alone.

  What the entry *was* wrong about still stands, because that part was
  checked with a test rather than by counting: the states are not
  redundant, and `LogTailStage` is not a generic `Fetch<T>`. Details
  below.

  The 14 are in two different shapes.
  `LogTailStage` is a five-stage pipeline for one fetch, not a generic
  `Fetch<T>`. And the states are **not** redundant: all four
  combinations are reachable because the two fields encode two
  orthogonal facts — do we hold data, and is a request running. The
  combination that proves it is settled-and-in-flight (a refresh with
  the old result still displayed), which `spawn_detail_*` creates
  deliberately so a refresh doesn't blank the panel. A four-variant enum
  would have lost it and stopped the Health tab spinning on any refresh
  where data was present.

  So `Fetch<T>` shipped as a struct holding both facts. Converted the
  two pairs with that shape (`cw_alarms`, `recent_versions`).
  **Remaining: the six `Vec`-shaped ones** — they settle into a shared
  error slot instead of their own `Result`, so wrapping them adds an
  error arm nothing fills and changes what the footer shows. That is a
  design call (per-section errors vs one panel error), not mechanical.

  Reading them found a real bug, fixed separately in the same commit:
  four concurrent fetches shared one error slot and every handler
  cleared it on success, so a success erased an unrelated failure.

  Original entry follows.  — 14 of
  them, 4 representable states for 3 real ones. `enum Fetch<T>` exists
  one file over as `LogTailStage` and isn't reused. ~~Touches `pub`
  fields, so it lands with the above.~~ **No longer a breaking change**
  as of 0.34.0 — those fields are `pub(crate)` now, so this and the item
  below became ordinary internal refactors that can land in any patch
  release. That sequencing was the point of doing the narrowing first.
- [x] **`ConfirmModal`'s 11 action-gated `Option`s** — DONE 2026-08-24,
  but NOT as payloads on `Action`'s variants. Reading it first found the
  eleven existed *twice*: `ParameterisedAction` declared all eleven,
  `ConfirmModal` declared the same eleven, and the funnel copied them
  across a line at a time — so a new parameterised action was three
  edits, and missing the third silently dropped the parameter. Now
  `ConfirmModal` holds one `ParameterisedAction`; 28 fields to 18.
  Moving them onto `Action` was the bigger hammer (`Action` is named at
  244 sites, is `Copy`, keys the `ACTIONS` table) for the same win.

  **It also turned up a live safety hole, fixed in the same commit:** a
  CNAME swap writes to BOTH envs, but every `deny_write` on the path
  checked the source, so `safety.envs.TARGET.read_only` was defeated by
  selecting the other env first. Both entry points had it.

- [x] **`App::mode` + partner `Option`s** — DONE 2026-08-24, but NOT as
  `Screen` + `Modal`. The invariant is real and is now enforced; putting
  the state inside the enum is the wrong way to do it here, for two
  structural reasons.

  `Mode` is `Copy` and `shell_return_mode = self.mode` stores one to
  return to on F12 — a payload-carrying `Mode` would copy a 42-field
  `DetailState` on every shell attach and force a lightweight tag to be
  reinvented for the uses `Mode` already serves.

  And the invariant is **asymmetric**, which an enum would flatten.
  Being in a mode requires its state; holding the state does not require
  the mode — the background layer dispatches on `detail.is_some()` so a
  Help or action popup over Detail keeps Detail behind it rather than
  flashing the main table. Merging collapses a distinction the UI relies
  on. Pinned by `holding_state_without_the_mode_is_fine`.

  Enforced instead the way `ViewState::assert_fresh` enforces the other
  invariant the types miss: panic in debug, log once per surface in
  release. Six surfaces, one test each — which earned itself, because
  instrumenting the four draw functions had three of four firing
  (`draw_detail` is unreachable in the broken state) and a shared test
  would have shown the dead guard as covered.

**Not doing, with reasons** (the reviewer's own list, and I agree):
miri (one `unsafe`, FFI it can't execute), `cargo-hack` (zero features),
fuzzing (proptest covers the same ground for a tenth of the setup),
`clippy::pedantic` (1712 warnings), coverage as a gate ("coverage
answers *what's untested*; you're past that and into *are the tests
load-bearing*, which is mutation's question"), reproducible builds.

#### Terminal lifecycle — 2026-08-23

RAII guard over the alternate screen, a shared best-effort
`restore_terminal`, and `panic = "abort"` in release. Details in the
0.31.1 changelog.

**What is actually verified, and what isn't.** With permission to use
the real terminal, ran the built binary inside a `script`-allocated pty
and compared `stty -g` before and after a full alt-screen session:
byte-identical. That establishes the restore path works end to end
against a real termios — which is the part a unit test can't reach.

Two earlier attempts at that check were vacuous and worth recording:
the first ran `stty` outside the pty (it errored, and "intact" was
printed anyway); the second redirected stdout to `/dev/null`, which
tripped the new non-TTY guard, so ebman exited 2 without ever entering
the alt screen — a clean "restored" verdict for a session that never
started.

**Not verified directly:** the `App::new`-fails-after-entering path the
RAII guard exists for. ebman deliberately starts without credentials and
surfaces the error in-app, so it can't be triggered from outside. The
reasoning that covers it: Rust guarantees `Drop` on a `?` return, and
the function `Drop` calls is the same one the pty run exercised. That is
an argument, not a measurement, and it is written down here as such.

**Deliberately not tested in the suite:** `restore_terminal` itself.
Calling it would disable raw mode on whoever runs `cargo test`,
including CI and other contributors. The permission to touch one
terminal doesn't make that acceptable in a committed test. The
structural guard — no site may re-create `disable_raw_mode()?` — is the
part that ships.

#### Release panel on 0.31.1 — 2026-08-23

Three seats, all **SHIP**, four actionable findings between them. Every
one was a defect in my own work, and none was caught by my own review
pass beforehand.

**Operator.** I fixed `:rds-attach` to refuse at open and stopped there
— but six siblings still opened their forms on a read-only fleet and
refused only at submit: `:capacity`, `:scaling-triggers` (nine fields),
`:listener-edit`, `:subnets`, `:elb-subnets`, `:security-groups`. Not a
safety hole; every submit path gates. But it is precisely the annoyance
the changelog claimed had been fixed, and fixing one instance of a class
while writing it up as the class is the failure mode this backlog
already has a rule about. All seven refuse at open now, all four new
gates mutation-verified, and the changelog says what actually shipped.

**Release engineer.** `Cargo.lock` must be regenerated in the *same*
commit as the version bump — every CI and release job runs `--locked`,
so a forgotten lockfile fails all 8 CI jobs and all 4 release builds.
Also confirmed the semver gate will genuinely run on the bump commit
rather than skip, and flagged `softprops/action-gh-release@v2` — an
unpinned major tag running with `contents: write` — as the highest-value
SHA-pin candidate. Tracked, not done: pinning it is a change to the
release path and doesn't belong in the release it would first affect.

**Skeptic.** Re-verified the coverage claims independently by stubbing
four render surfaces and neutralising four commands, and they held. Its
three findings:

- [x] **No drift guard on the render side.** The command side had one; a
  42nd `draw_*` would have taken 41-of-41 back to 41-of-42 silently.
  `every_render_surface_is_accounted_for` pins the set now, with its own
  parse guarded — mutation-verified by adding a 42nd surface.
- [x] **My assert message overstated what it catches.** A *deleted* arm
  falls through to `other =>`, sets "unknown command", moves the
  fingerprint, and the test passes. Confirmed by removing the `:pin`
  arm: `every_command_moves_observable_state` passed,
  `every_registry_name_has_a_dispatch_arm` failed. The two cover both
  cases; neither covers both alone. Wording fixed to say so.
- [x] **`COVERED_INDIVIDUALLY` was honour-system.** Now checks the
  command name appears in the test tree at all. **The first version of
  that check was self-satisfying** — the list lives in a file the check
  reads, so a fictional entry validated itself, and it duly passed a
  deliberately fake entry. Caught only because I mutation-tested the
  check itself. Fixed by cutting the list's own declaration out of the
  searched text; re-verified against the same fake entry.

Worth recording: **two of the four fixes above were themselves wrong on
the first attempt**, and both were caught by mutating rather than
reading. A check that validates itself and a mutation that silently
fails to apply look identical to a passing test.

#### Command coverage completed — 2026-08-23

**131 of 131**, confirmed by a full re-sweep. The last 74 went in as one
table-driven test rather than 74 hand-written ones.

The mechanism: snapshot the state a command can move — mode, status,
error, overlay discriminant, the flow/form/picker/detail/dlq/shell
options, load state, toasts, help topic, scope, sort, filter, grouping,
pending count, saved views, hidden columns — run the command, assert the
snapshot changed. That is exactly the property the sweep measures, so a
deleted, renamed or short-circuited arm fails it, and unlike a string
needle it cannot be satisfied by chrome the command didn't draw.

Only **2 of 74** moved nothing observable, both pure spawns:
`:logs-tail` (pinned via the `log_tail_task` handle it leaves behind)
and `:config-inspect` (the one command with nothing on `App` to look at
— pinned by draining its message off the channel).

**Be clear about what this proves.** The bar is "the command does
*something*", not "the command does the *right* thing". It is a floor,
not a substitute for behaviour tests. What it buys is that no command
can silently become a no-op — which is the failure the sweep found 88
instances of, and which is worse than a visible error because the
operator believes it worked.

- [x] **A drift guard so completeness survives the next command.** Both
  sweeps hit 100%, and without a guard the next command added would
  quietly make it 131 of 132. `every_registry_command_is_covered_by_some_test`
  reads the registry out of `src/commands.rs` and requires every name to
  appear in one of the five bulk tables or in `COVERED_INDIVIDUALLY`
  *with a stated reason* — an entry with no justification is how a gap
  gets papered over. Mutation-verified by injecting a new command into
  the registry and watching the guard name it.

  Its own parse is guarded too (`registry.len() > 120`), which earned
  its keep immediately: the first version parsed **0** commands, because
  registry entries are multi-line — `cmd_with_aliases(` on one line and
  `"region",` on the next. Without that assertion an empty parse would
  have read as a clean pass, which is the same "gate with nothing behind
  it" shape this repo has now hit four times.

#### Render-coverage sweep — 2026-08-23

`draw_shell` was found uncovered by accident while writing the DLQ and
events smoke tests, so this time the whole surface got measured instead
of waiting to notice the next one: stub each of the **41 `draw_*` entry
points** with an early return, run the suite, see whether anything
fails.

**13 covered, 28 not.** Twenty-eight render surfaces could stop drawing
entirely and no test would say a word. All 41 are covered as of the
second pass below.

Closed six of them (all mutation-verified by stubbing the surface and
confirming the test fails):

- [x] `draw_shell` — recorded **twice** as "needs a PTY, can't test".
  That was a guess and it was wrong: `ShellSession`'s `writer` /
  `master` / `child` are all `Option` precisely so `--demo` can build a
  session with no subprocess, and `resize` is a no-op when `master` is
  `None`. `ShellSession::demo` + `tick_demo_typer` renders a full
  transcript in a unit test.
- [x] `draw_detail_events`, `draw_detail_instances`, `draw_detail_queue`,
  `draw_detail_logs` — the Detail tabs an operator reaches mid-incident.
- [x] `draw_form` — the whole form overlay. Worth calling out: earlier
  this cycle its scroll behaviour was recorded as "already fixed,
  verified" on the strength of *reading* the code, with nothing
  exercising it.

Two methodology notes, both the same class I keep hitting:

1. **The first sweep reported 0 caught and 14 "did not compile".** The
   classifier tested for `^error` before `test result: FAILED`, and
   `cargo test` prints `error: test failed, to rerun pass --lib` on an
   ordinary test failure — so every genuinely-covered surface was
   mislabelled as a build break. Zero caught was the tell; `draw_dlq`
   and `draw_shell` had been mutation-verified an hour earlier.
2. **The first `draw_detail_logs` test passed with its own surface
   stubbed out.** It asserted on the env name and a non-blank line
   count, both drawn by the Detail chrome around the tab. Re-pinned on
   the pane's own title (`Logs · N instance(s) · M lines`), which the
   tab strip doesn't draw.

**All 41 are covered now** — the remaining 23 were closed in a second
pass and a full re-sweep confirms **41 caught, 0 uncovered**. The
overlays went in as one table-driven test (they share a shape: an
`Overlay` variant carrying text, and a renderer that must put it on
screen), the six help topics as another, and the rest individually.

- [x] `draw_why_red_overlay`, `draw_log_tail_overlay`,
  `draw_apps_action_menu`, `draw_apps_table`, `draw_palette`,
  `draw_picker`, `draw_toasts`, `draw_about`, the six `draw_help_*`
  topics, and the nine body-carrying overlays.

**The recurring mistake, worth naming.** Four separate assertions in
this sweep passed with the surface under test stubbed out, because the
chrome *around* it drew the needle:

| test | needle | who actually drew it |
|---|---|---|
| Detail Logs tab | env name, line count | the Detail header |
| Apps table | the app name | the header breadcrumb |
| Help topics (1st try) | `esc` | the footer keystrip, in every mode |
| Help topics (2nd try) | "frame differs from Normal mode" | the footer, which changes with mode by itself |

The second help attempt is the interesting one: a *differential* test
looked like the robust answer to the needle problem, and it was still
wrong, because mode changes the chrome on its own. What actually works
is a needle taken from the surface's own source — each help topic's
pane title (`ebman — keybindings`, `Detail view — keybindings`, …) —
rather than one guessed from outside. **Every one of the four was
caught by mutation-verifying, not by review.**

#### app/tests.rs split — 2026-08-23

9,515 lines → 16 modules under `src/app/tests/`, one per surface
(`refresh` 1,928, `pure` 1,389, `region` 784, `dispatch` 697 … `audit`
81), with `support.rs` holding `test_app` / `mk_env` / the render
harness / the write-command tables. Root `tests.rs` is 27 lines of
module declarations and a map. Same pass `ui.rs` got, and lower risk:
the compiler plus 1,104 existing tests are a complete safety net.

Mechanically: split on column-0 anchors rather than brace-counting,
because 441 `fn` and 441 column-0 `}` matched exactly and test bodies
are full of `{}` inside string literals. One rewrite was unavoidable —
`super::` meant `crate::app` in the flat file and would mean
`crate::app::tests` a level deeper, so all 295 occurrences were
re-anchored; rustfmt then reflowed some lines because the new path is
longer. Confirmed first that the file had **no nested `mod`**, which is
what makes that rewrite exact.

Verification, since "the tests still pass" proves very little about a
move of this size:

- All 422 test names present, none renamed, none duplicated.
- Per-test body comparison with whitespace stripped and rustfmt's
  trailing commas normalised: **420 of 422 byte-identical**; the 2 that
  differ are the guards below, changed on purpose.
- Zero non-blank lines fell outside a captured item — checked before
  writing anything, since a splitter that silently drops a region
  between items is the obvious failure mode.

Two bugs found along the way:

- [x] **The splitter swallowed items.** A single-line `const X = &[..];`
  sent the end-of-item search hunting for a line starting with `];`,
  which it found at the end of a *later* multi-line const — taking
  everything between with it, and duplicating those items. Caught by
  `E0428 defined multiple times`, not by anything I did.
- [x] **Both source-scanning drift guards excluded themselves by
  filename** (`file_name() == "tests.rs"`), which stopped being true the
  moment the tests became a directory: each then matched its own
  assertion literal and failed. Generalised to a shared
  `is_test_source` that skips the whole test subtree — what they always
  meant — with its own test pinning that production paths are *not*
  excluded, since over-excluding would switch both guards off silently.
  Both re-verified against a planted violation: an `arboard::` call in
  `cmd_misc.rs`, and an allow-list entry removed. The first attempt at
  the second mutation didn't compile, so its "pass" was meaningless —
  redone with one that builds.

#### Post-0.31.0 batch — 2026-08-23

Four items, all demonstrated defects rather than judgement calls.

- [x] **`cargo-semver-checks` was PR-only.** Now runs on main pushes too,
  gated on whether the manifest version is already published on
  crates.io. The old comment's reasoning was half right and worth
  keeping: between tags manifest == published, so any API change reads
  as "same version, different API" and main would stay red until
  someone bumped — a gate that noisy gets switched off. But it also
  meant the gate never ran on the one commit whose version claim
  actually ships. Gating on "is there an unpublished version to
  validate" gets both: silent between tags, checks the release commit
  *before* the tag instead of after. Fails toward running the check if
  crates.io is unreachable, so a blip can't silently disable it. The
  gate script was extracted from the YAML and exercised over all four
  cases (published / unpublished / crates.io down / PR) rather than
  retyped into a test copy. Verified live on the first push: the job
  logged `manifest=0.31.0 published=0.31.0 → already published` and
  skipped the check.

  **Correction to how I first wrote this up.** I claimed the gate would
  have caught 0.31.0's undocumented `Form.banner`. It would not. At the
  release commit the version is *already* at the breaking position, and
  a declared major bump permits everything — `0.30.2 -> 0.31.0` reports
  `0 checks: 0 pass, 254 skip / no semver update required`, whatever
  broke. What the gate actually catches is the **under-bump**: an API
  break shipped as a patch, which is the version-claim error that hurts
  downstream users. Enumerating breaks for the changelog needs
  `--release-type patch` against the last published version, which
  can't be a build gate (it fails by construction on any legitimate
  major bump). That went into the release procedure instead, next to
  the docs audit and the code review — an honest checklist item rather
  than a CI step that can't fail, which is the shape this repo has
  already been bitten by three times.

#### 0.31.0 shipped — 2026-08-23

Tagged `v0.31.0` after CI went green. The push-before-tag order was the
release-engineer panellist's call and it earned its keep: the lineup had
never had a CI run, and the first one failed two gates.

- **msrv — a real defect.** The `cargo update` earlier in this lineup moved
  the AWS SDK to crates declaring rustc **1.94.1**, so `rust-version = "1.91"`
  had been false ever since; `cargo build --locked` on 1.91 fails to resolve.
  Raised in `Cargo.toml`, the CI pin and the README, and **verified on a real
  1.94.1 toolchain** rather than asserted. A user-visible change, so it is in
  the changelog under Changed.

- **candor — a false failure, in my own guard.** The "prove it actually
  analysed this crate" check greps for `Checking ebman`, but the workflow sets
  `CARGO_TERM_COLOR: always`, so cargo writes `Checking\e[0m ebman` and the
  literal never matches. Locally cargo drops colour when piped — which is
  exactly why it passed here and failed there. The gate itself had run clean.
  Fixed with `CARGO_TERM_COLOR: never` on that step plus a looser pattern.
  Worth noting the shape: this is the *third* consecutive defect in this one
  gate (`.gitignore` hiding the policy, then `|| true`, now the colour), and
  all three had the same failure mode — the gate not measuring what it claims.

- **The candor clone is now pinned to a commit** instead of tracking HEAD.
  The release engineer flagged it as a flake/supply risk before the run; a
  gate that can block a release should not change underfoot.

- **The changelog claimed four fixes that shipped the day before.** `:ssm-run`
  50-instance batching and EBL010 landed in `7551912` / `dbb1d80`, both
  ancestors of `v0.30.2`, and the breadcrumb and EC2-link region fixes with
  them. Caught by diffing `v0.30.2..HEAD` instead of trusting the draft.
  Rewritten to what git says shipped.

- **A sixth breaking change was undocumented.** `cargo-semver-checks` is
  PR-only, so it had never validated the version claim on main. Run manually
  against published 0.30.2 with `--release-type patch`, it confirmed the five
  documented breaks and found `Form.banner` — a new public field on a
  constructible struct. 0.31.0 was the right bump; the notes were incomplete.
  **Follow-up worth considering: run semver-checks on tag pushes too**, not
  only PRs, since the release is the moment the claim actually ships.

#### Important (need live verification or a design pass)
- [x] **Cost Explorer pagination** — Shipped. `fetch_env_costs` follows `NextPageToken` (`aws/cost.rs`). The `MAX_COST_PAGES = 20` cap now warns, returns `EnvCosts { truncated }`, and a truncated walk is neither cached nor allowed to replace a complete in-memory map; `:cost status` and `:fleet-cost` both say when what's on screen is partial.
- [x] **logs-tail `next_token` follow** — Shipped. `fetch_recent_log_events` follows `next_token` up to `MAX_PAGES_PER_POLL = 5` with boundary-millisecond dedupe (`aws/logs.rs`). The carry is keyed on whether the watermark moved rather than on `truncated`, so a stalled watermark keeps its skip set and the same lines aren't re-emitted every poll.
- [x] **worker-queue error conflation** — Shipped. `describe_worker_queues` returns `Result<WorkerQueues>` (`aws/eb.rs`), so AccessDenied no longer renders as "no worker queues".
- [x] **webhook drain before CLI exit** — ALREADY FIXED; verified 2026-08-22. `cli::mod.rs` drains in-flight audit-webhook POSTs with a 12s budget before exiting.
- [x] **rebuild-epoch ordering** — ALREADY FIXED; entry was stale. `spawn_rebuild` / `spawn_assume_role_switch` stamp a monotonic `rebuild_epoch` and `apply_rebuild` drops stale arrivals. Only its Err path was under test, and Ok is the arm that swaps the client, replaces the context and clears the fleet; pinned 2026-08-22.
- [x] **events-panel cursor follow** — ALREADY FIXED; verified 2026-08-22. `draw_events` drives `event_panel.scroll` through `config_scroll_follow`, and the comment names the old symptom (holding J walked the cursor below the fold, and every subsequent key including `y` operated on an invisible row).
- [x] **form scrolling** — ALREADY FIXED; my verification was wrong. `draw_form` lives in `src/ui/overlays.rs`, not `src/ui.rs`, so grepping the latter found nothing and I recorded it as confirmed-still-real. It has driven `form.scroll` through `config_scroll_follow` for some time, with a comment naming the exact case from this entry (the 9-field `:asg-trigger` on 80×24) and a second one explaining why it counts logical lines rather than wrapped ones.

#### Minor (batchable)
- [x] control.sock chmod-after-bind race — CLOSED by the SO_PEERCRED check on every connection (`control.rs`), which is stronger than file perms and needs no process-global umask change. Confirmed 2026-08-22; entry was stale.
- [x] 0600 perms on audit.log / ebman.log / crash logs / explain cache — done in 0.27 via `open_append_secure` / `write_secure`. The gap that remained: `write_atomic` (shared crate) used `std::fs::write`, so `config.toml` — which carries `notify_webhook` and `accounts.*.external_id` — was umask-default. Shadowed locally 2026-08-22 with the mode set on the temp file, not chmod'd after the rename.
- [x] audit-line escaping — `target=` / `version=` already went through `field_token`; the gaps were `append_lint_fix`'s four raw fields, `region=` in both raw writers, and the header's `profile=` (free text from `~/.aws/config`, and `\t` is the separator). All escaped 2026-08-22.
- [x] report_bug scrubber mojibake — ALREADY FIXED; verified 2026-08-22. `scrub_12_digit_numbers` walks chars. The one remaining `byte as char` is inside `url_encode`, where the byte has just been matched against the ASCII unreserved set — correct there.
- [x] ascii icon-mode stragglers — FIXED 2026-08-22. Five sites, not six: `ui.rs:3458` turned out to be `separator_glyph`, which already had an `IconStyle::Ascii` arm — my scan flagged its `_ =>` line for having no ascii context within three lines. The real five were the header delta arrows, the sort marker, and the Metrics anomaly badge, which had `▲` baked into its *message string* where a grep for glyph helpers would never find it. `series_anomaly_label` takes an `IconStyle` now. Guarded at both levels: the helpers in `ui.rs`, and a rendered frame in ascii mode carrying no `▲`/`▼` at all — because the pure helpers can be right while a call site still hardcodes.
- [x] drift redaction — ALREADY FIXED in 0.27 (`redact_drift_fields` reaches `ebman drift` text and `--json`, the MCP tool, and the TUI overlay); entry was stale. A guard test now names the three call sites so a fourth consumer can't skip it. 2026-08-22.
- [ ] **Minor bugs — verified 2026-08-22.** The old one-line batch of ~19 was checked item by item against current code; **eleven were already fixed** by the 0.29/0.30 work and are struck below. What survives:
  - [x] **Detail Logs tab scroll is unclamped upward** — NOT A BUG; my own verification was wrong. `scroll_apply` clamps only at 0, but the Logs call site in `detail_nav.rs` already clamps at the total line count. Checking the helper in isolation instead of its call site is the same mistake the `WRITE_COMMANDS` walk made.
  - [x] **`run_shell_command` doesn't chunk >50 instances** — FIXED 2026-08-22. Sends in chunks of 50 and keys the poll loop on a per-instance command id, since there is now more than one. A failed chunk no longer discards the successful ones: those instances come back as `SendFailed` rows by name, which is strictly more than the operator got before.
  - [x] **`derive_dlq_url` guesses** — FIXED 2026-08-23 via `DlqOrigin` + `dlq_absence_note`; see the 0.31.1 changelog. Original note follows.  — `format!("{trimmed}-dlq")`, right for the EB convention. Downgraded on inspection: a wrong guess IS detected (`NonExistentQueue` on the derived URL resolves to "no DLQ" rather than an error), so the gap is only that the operator isn't told the difference between "no DLQ configured" and "we guessed a name and it wasn't there". Observability, not correctness.
  - [x] **EBL010's tag-fetch failure is indistinguishable from "no tags"** — FIXED 2026-08-22. `env_tag_keys` is `Option<&[String]>`: `None` is "not loaded" and skips, `Some(&[])` is a successful fetch of an env with NO tags and fires for every required key — that env was invisible to the rule, and it is the worst case the rule exists to catch. Matches the `Option` shape its neighbours `dlq_depth` and `healthy_instance_count` already used.
  - [ ] **Unicode display-width column math** — WONTFIX unless something demands it. `pad_right` counts chars, so wide/combining characters misalign — but it is used in one place, the rollout overlay's region / env / version columns, and EB constrains env names to alphanumerics and hyphens. The only realistic non-ASCII input is an operator-chosen version label. Adding a `unicode-width` dependency for a cosmetic misalignment in that case is the wrong trade; recorded so the next reader doesn't re-derive it.
  - [x] **JSON was parsed by the YAML parser** — FIXED 2026-08-22, and wider than recorded. Three inputs ebman does not control went through `serde_yml` on the reasoning that JSON is a YAML subset: both LLM response bodies (carrying model-generated text) and `terraform.tfstate` (discovered by walking up from cwd). True but beside the point — it means every YAML feature, anchor/alias expansion included, applies to that input. `serde_json` had been a direct dependency the whole time, so the comment justifying the detour was stale as well. Two round-trip TESTS had the same hole in reverse: they verified a JSON writer with a YAML reader, which accepts output JSON would reject. A guard pins the parser choice per file; `saved_config.rs` stays on YAML because EB saved configurations genuinely are YAML.
    Behaviour change worth knowing: an empty `terraform.tfstate` is no longer valid input. It was a null YAML document deserialising to `resources: []`, so an empty or truncated file read as "no envs" and passed `drift --exit-code` green. It now reports "no terraform.tfstate found" — same class as the 0.27 fix for backend pointers parsing as zero envs.
  - [x] **The three "needs a look" items were all already fixed** — verified 2026-08-22, each at its call site rather than by reading a helper:
    - *saved-configs window* — `draw_saved_configs_interactive` counts group headers inside the window and re-windows once, with the single pass justified (a second could change the header count by at most one row, which is one row of cosmetic overshoot in a popup). Sound.
    - *DLQ opening with no row selected* — the message handler selects row 0 on a fresh load; its comment names the exact symptom (`unwrap_or(0)` masking `None`, leaving Enter/x/r inert and the first `j` skipping to row 1).
    - *help-restore ghost states* — `apply_rebuild` clears `help.pre_mode` / `pre_overlay` on a context switch, naming the ghost state it prevents. And the `pre_overlay` tail routing is NOT dead: it is the second slot that keeps log-tail events from being lost while help is open over the tail.
  - Fixed since the line was written (confirmed in code, not assumed): `p` purge armable from Main view; `DlqMessages` carrying queue identity; watchdog rollback against a vanished env (disarms with a message); MCP `fleet_cost` NaN (`is_finite` guard in `cost_cache`); `versions --json` empty created date (emits `null`); `ebman envs` unknown flags and typo'd subcommands (both exit 2 with usage); `audit --tail` two text formats (one render path); `rollout --regions` dedupe; `project.rs` silent drop (warns to log + stderr); watch interval drift (start-to-start); MCP `id: null`.

Also queued from the 0.26 pre-tag architecture review: rewrite_credential_error + probe helpers out of app.rs; ui.rs submodule split; MCP registry unification (gate on v2 writes); EBL015 warnings surface in MCP; per-tool client dedup.

### 0.26 candidates (2026-08-03)

#### MCP server (`ebman mcp serve`) — HEADLINE — SHIPPED 2026-08-03 (same-day as the spec; see CHANGELOG Unreleased)

Deferred 5× on "no operator demand"; demand surfaced 2026-08-03 (an agent session driving a fleet upgrade + release would have used it directly instead of shelling out and re-parsing). The charter already reserves the namespace; this locks the shape so the build session doesn't re-litigate it.

**What it is.** A standalone stdio MCP server exposing ebman's *read* seams as MCP tools, so Claude Code (and any MCP client) can query fleet state first-class. It is NOT a bridge to a running TUI — it constructs its own `AwsClient` per call exactly like the CLI subcommands do (`ebman ctl` remains the drive-a-running-TUI surface; a `ctl` bridge tool can come later if wanted).

**Transport + protocol.** JSON-RPC 2.0 over stdio (newline-delimited), the standard Claude Code MCP shape (`claude mcp add ebman -- ebman mcp serve`). Tools-only server: implement `initialize`, `tools/list`, `tools/call`, `ping`, and the `notifications/initialized` no-op — five methods, small enough to hand-roll; unknown methods answer JSON-RPC `-32601`. **Pin the claimed `protocolVersion`** (latest at build time), echo-negotiate down when the client offers older, and golden-test the `initialize` + `tools/list` responses so schema drift is caught by CI, not by a broken agent session. **Dependency decision:** add `serde_json` (serde+derive already in tree; incoming JSON-RPC must be *parsed*, which `util::json_string` can't do) and hand-roll the protocol loop in house style — do NOT take the `rmcp` SDK (pulls schemars + tokio-util machinery for five methods; version drift risk for a protocol this small).

**Two hard rules the implementation stands on (review 2026-08-03):**
- **stdout is protocol-only; stderr is diagnostics-only.** The CLI `run()` wrappers `println!` by design — MCP tools must call the underlying seams (`run_rules`, `parse_audit_line`, `aws::*`), NEVER the `run()` wrappers. One stray print corrupts the session. Pin with a test where feasible.
- **Concurrent tools/call, responsive loop.** A serial read-eval loop lets a 30s lint fan-out block `ping` and the client times out. Spawn each `tools/call` as a tokio task keyed by request id; bound every tool at ~30s; explicitly IGNORE `notifications/cancelled` in v1 (documented limitation).

**Tool surface (v1 — reads only, mirrors the CLI charter's flat read verbs).** Every tool takes optional `profile` / `region` (→ `AwsClient::with`, same as CLI); results are the existing `--json` shapes verbatim so consumers and docs stay aligned. **Every tool output is bounded** — agents consume results into finite context windows, so caps are part of each schema, not a retrofit. **Every tool description states its coverage caveats** — an agent treats tool output as authoritative, so "no findings" must not silently mean "input not wired" (the CLI lint path's `dlq_depth = None` means EBL011 can't fire; EBL016/EBL020 are probe-gated and never fire here — say so IN the description):
- `list_environments` — envs with health/status/version/cname (seam: `ebman envs --json` path).
- `lint` — rule-engine findings; params `env?`, `severity?`, `rules?` (seam: `cli/lint.rs` context assembly — extract the per-env builder first per the 0.26 refactor list, then both callers share it). No `--probe-live` in v1 (network side-effects from an agent tool need their own think). Description carries the EBL011/016/020 caveats above.
- `get_option_settings` — one env's resolved option settings; param `env` (seam: `fetch_env_option_settings`). **Env-var VALUES redacted by default** — `aws:elasticbeanstalk:application:environment` values are secrets (keys stay visible; `DBPassword` also redacted, matching the `:rds` precedent). `--no-redact` server-start flag to opt out. Added by the 2026-08-03 review: agent sessions want raw env config, and this is the one v1 surface where redaction is load-bearing rather than cosmetic.
- `drift` — terraform drift report; params `env?`, `tfstate_path?`. Description notes tfstate discovery walks up from the SERVER's cwd — correct for project-scoped `.mcp.json` (launches in the repo), surprising otherwise.
- `audit_log` — parsed audit entries; params `since?`, `env?`, `action?`, `limit?` (default 100) (seam: `audit::parse_audit_line` + `AuditFilter`).
- `recent_events` — fleet or per-env EB events; params `env?`, `max?` (clamped ≤200) (seam: `list_events` / `list_events_for_env`).
- `list_versions` — application versions for an env's app; `limit?` (default 50, newest first) (seam: `cli/versions.rs`).
- `fleet_cost` — cached Cost Explorer summary; reads the `cost-{account}-{region}.toml` cache only, never triggers a fetch (agents polling Cost Explorer is a bill, not a feature).
- Excluded from v1: `explain` (the MCP client IS an LLM — hand it `lint` output instead), all writes.
- **Tool errors route through `format_aws_error`** so an expired SSO token surfaces as the `aws sso login --profile X` hint inside the `isError` content, not opaque SDK noise the agent can't act on.

**Writes (v2, explicitly out of v1 scope).** If/when wanted: `action_deploy` / `action_restart` / `action_rebuild` behind a server-start flag (`ebman mcp serve --allow-writes`), never terminate. Safety pins + freeze enforced via the `Config::pin_reason` helper (0.26 refactor list — build it first), `--demo` refused, and every dispatch audit-logged with a `source=mcp` extra so the trail distinguishes agent writes from operator writes. The v1/v2 split is deliberate: read-only ships without any new safety surface to review.

**Safety posture (v1).** Reads only; no credentials handling beyond what the CLI already does; honours `AWS_PROFILE`; no state mutation, no audit lines written (reads never audit — matches `ebman envs`/`ebman audit` doing no `init_from_config_disk`). Redaction: `get_option_settings` redacts env-var values by default (above) — the one sensitive v1 surface; everything else (CNAMEs/ARNs in `list_environments`) already ships in `envs --json` and stays as-is.

**Structure.** `src/cli/mcp.rs` (dispatch arm `"mcp"` in main.rs matching the charter's `ebman mcp serve`), pure `parse_mcp_args` (`serve` sub-verb + `--demo` / `--no-redact` flags in v1, exit 2 otherwise), a `handle_rpc(request: Value) -> Option<Value>` pure-ish core that's unit-testable with synthetic JSON-RPC frames (tool dispatch itself needs the async AwsClient — test the protocol layer with a mocked tool table, same seam style as the CLI arg parsers). Tool schemas as static JSON (hand-written, pinned by the golden `tools/list` test).

**Testing: `ebman mcp serve --demo` is the e2e harness.** The synthetic demo fleet gives full protocol round-trip tests in CI with zero AWS — spawn the server, drive `initialize → tools/list → tools/call list_environments/lint/get_option_settings`, assert on real frames. Much stronger than the mocked-tool-table unit tests alone; both layers ship.

**Docs + estimate.** `docs/headless.md` gains an MCP section with the `claude mcp add` line + tool table; `docs/commands.md` CLI section; main.rs `--help`; `docs/safety-and-privacy.md` notes the `get_option_settings` redaction default. Estimate: protocol loop + concurrency model + 8 read tools + demo e2e harness + docs ≈ one focused session (~6–8h). Depends on: nothing hard; pairs well with the `cli/lint.rs run()` decomposition from the 0.25 refactor list (shared per-env context builder).

#### Also queued for 0.26
- The five refactor candidates from the 0.25 pre-tag review (see "Refactor candidates for 0.26" in the 0.25 section below) — `TailView` extraction, `Config::pin_reason`, `cli/lint.rs run()` decomposition (a soft prerequisite for the MCP `lint` tool), replay module split, LintContext probe-fields note.

### 0.25 candidates (2026-07-15)

Theme: **incident operations.** The three biggest pending items on the shelf all serve the same operator moment — something's on fire across the fleet — and they compose with machinery that already exists (freeze, runbooks, audit, `:logs-tail`). Verified against the code before listing: EBL014/015/016/018/020 are absent from `src/lint.rs`, no replay path exists in `src/cli/`, and the stale checkboxes from earlier candidate lists (lint input caching → 0.21, `:diff --ignore-keys` → 0.23, ARM64 tarball → 0.23) were fixed in this same pass.

#### Incident story — HEADLINE
- [x] **`:event-tail`** — SHIPPED (2026-07-15). Simpler than the sketch: `DescribeEvents` with no env filter is already fleet-wide and each event carries its env name, so no fan-out coordinator was needed — a single 5s poll with a `start_time` watermark (new `list_events_since`) plus a 1000-event ring buffer covers the rate-limiting story. Same session-id/generation/context-teardown pattern as `:logs-tail`; regex filter over env+app+severity+message; severity-tinted rows. +5 tests (watermark math, filter predicate, ring cap + stale-session drop, close-defeats-late-reopen, styled red-row render).
- [x] **`:incident START "headline" / :incident END`** — SHIPPED (2026-07-15). Minimal composite as designed: START sets the `:freeze-deploys` lock (reason = headline), pins a red `🚨 INCIDENT` header banner with running clock, audit-logs `IncidentStart`; END thaws, clears, logs `IncidentEnd` + duration. Re-issue updates the headline without resetting the clock; freeze refusal toasts point at `:incident END` while an incident is active. Auto-`:why` / auto-logs-tail deferred until the composite earns use. +5 tests.

#### Incident-review + lint completion — SUPPORT
- [x] **`ebman audit replay <line-id>`** — SHIPPED (2026-07-15). Line-id = RFC3339 timestamp prefix (timestamps aren't guaranteed unique, so ambiguous prefixes refuse with candidates listed). Rebuild/Restart/Deploy/Terminate replayable; rollout/lint-fix/skipped lines refuse with pointers; Deploy needs `version=` on the line. Replays against the line's original profile+region; enforces safety pins CLI-side (the `ebman action` path never did — replay is the first); Terminate gated on `--yes` (exit 3). Writes its own `replay_of=`-tagged audit lines. +10 tests.
- [~] **Lint rule batch** — EBL014 + EBL020 SHIPPED (2026-07-15); EBL015 + EBL018 held. EBL014 reshaped to "legacy NetworkOut/NetworkIn trigger on a scaling ASG" (EB's trigger namespace has no CW-namespace key — the original framing wasn't checkable). EBL020 via a CLI-only `iam:SimulatePrincipalPolicy` probe + new `instance_profile_role_arn` resolver (SDK-compiled, unverified live). **EBL015 held**: `ListPlatformVersions` carries no dates → needs per-platform `DescribePlatformVersion` + an account-level issue shape. **EBL018 held**: WAF association isn't in option settings → needs a new `aws-sdk-wafv2` dep. Registry 15 → 18. +5 tests.
- [x] **EBL016 live health-check probe behind `--probe-live`** — SHIPPED (2026-07-15). Real rule (not synthetic injection) with a CLI-populated `health_probe_failure` context field, so disable/baseline/`explain` work uniformly. Reuses the Deploy confirm modal's curl probe (2s cap). Failed probe *run* skips — never false-positives. +1 test.
- [x] **`ebman lint --watch --webhook URL`** — SHIPPED (2026-07-15). Fires through the `notify_webhook` body shape only when the issue set *changes* between cycles (keyed by `issue_identity`) — no re-paging every interval; dirty→clean sends an explicit all-clear. Requires `--watch`. +3 tests.

#### Console parity — BONUS
- [ ] **`:custom-platform-create <packer-config>`** — SKIPPED this run (2026-07-15): needs S3-bundle upload plumbing + minutes-scale `CreatePlatformVersion` polling with more than one reasonable shape (fire-and-forget vs poll), all unverifiable against live EB here. Was tagged "fine to slip to 0.26" — it slipped.

#### Refactor candidates for 0.26 (pre-tag architecture review, 2026-07-15)
Surfaced by the 0.25 pre-tag review; none blocked the tag. Ranked by leverage:
- [x] **`src/app/tail.rs` extraction with a shared `TailView` state struct** — SHIPPED 2026-08-20 (the dedicated session the 0.26-run skip asked for): `TailView` + `handle_tail_key` + `reap_tail_task` (six teardown sites unified) + `tail_window_start`, and shared `draw_tail_overlay_chrome` in ui.rs; ~240 duplicated lines retired, +5 tests, existing reap regression tests pass unchanged. Original scope: retires three duplications at once: `handle_log_tail_key` vs `handle_event_tail_key` (~95 near-identical lines each), the ~45 shared scaffolding lines in `draw_log_tail_overlay` vs `draw_event_tail_overlay`, and the two 4-line reap branches in msg.rs. Deliberately NOT done piecemeal in 0.25 — extracting only event-tail would split near-identical twins across files; the right cut covers both tails (4 modules).
- [x] **`Config::pin_reason(env, profile)` helper** — SHIPPED 2026-08-03 (0.26 run). — the safety-pin check now has three copies (cli/audit.rs `safety_pin_reason`, cli/lint.rs `--fix` inline, TUI `is_read_only_for`). One testable home in config.rs before a fourth copy drifts.
- [x] **`cli/lint.rs run()` decomposition** — SHIPPED 2026-08-03 (0.26 run): `fetch_env_lint_inputs` / `build_lint_context` / `run_rules_for_env`, shared with the MCP lint tool. — 548 lines; 0.25 added the X-Ray probe, live-probe, and webhook change-guard inline in a triple-nested loop. Extract per-env LintContext assembly.
- [x] **Split replay out of `cli/audit.rs`** — SHIPPED 2026-08-03 (0.26 run): `cli/audit_replay.rs`. — replay is ~470 of its 700 lines with clean seams; mechanical move to `cli/audit_replay.rs` next time either half grows.
- [x] **LintContext probe-fields design note** — recorded (this entry IS the note; act at ~6-8 probe fields). — the Option-field-per-probe pattern is fine at 4 probe fields / 18 rules; at ~6-8 probe fields, fold into a typed `Probes` sub-struct and unify the two tri-state encodings (`Option<bool>` vs `Option<&str>`) behind a `ProbeOutcome` enum.
- Watch item: `fire_webhook`'s signature is audit-line-shaped; lint's call stuffs `"multi"` into region and `None` into account. Fine at two callers, revisit at three.

#### Deliberately out of the lineup
- **Next `app.rs` structural pass** — app.rs is back to ~22k lines but 0.24 was refactor-heavy; let a feature cycle run, revisit in 0.26.
- **ebman lib refactor (share app logic with pgman beyond tb-tui-common)** — its own design session, not a lineup item.
- **`:queue` inspector** — stays held per the recorded 2026-05-24 decision; abort semantics still aren't honest.

### 0.21.0 release (2026-05-29)

Continuation push. Theme: lint caching + audit-migration finish + rule-dev guide + spawn_* proof-of-pattern.

- [x] Lint input caching (App.env_tag_cache + env_health_cache, 60s TTL, AppMsg::LintInputsCached side-channel). Real ~200ms-per-modal-open latency win when cache is fresh.
- [x] 10 more app.rs append_raw sites migrated to typed append_action_dispatched. The remaining DLQ/SSM/event/skipped/undone shapes stay raw deliberately (no natural completed stage; would need new typed helpers).
- [x] docs/rule-development.md full "how to add a new lint rule" guide.
- [x] spawn_why_red_* cluster (6 methods) moved to src/app/spawn_why_red.rs. Proof of pattern for the remaining 5 clusters.

Deferred to 0.22 (genuinely needs focused review):
- ResolvedConfig sub-struct (30+ field-access rewrites with subtle borrow patterns)
- Remaining 5 spawn_* clusters (spawn_detail_*, spawn_dlq_*, spawn_batch_*, spawn_rollout_*, adhoc)
- ebman audit replay (dispatch mapping design surface)
- EBL014/015/016/018/020 (each needs live-EB verification)

780 tests still pass (behaviour-preserving changes).

### 0.20.0 release (2026-05-29)

Continuation of the 0.19 deferred-items push. Theme: rule engine depth + small operator surfaces.

- [x] EBL013 launch-config ASG lint rule (Warn, Manual fix) + 3 tests
- [x] EBL019 AllAtOnce on multi-subnet env lint rule (Warn, SetOption fix) + 4 tests + parse_csv_value helper
- [x] :fleet-cost overlay (renders App.costs as total + by-app / by-tier / by-health) + 4 tests
- [x] Promotion lineage tracking + :promotions overlay (in-memory; state.toml persistence is 0.21+) + 1 test
- [x] docs/lint-rules.md single-page reference

Deferred to 0.21+ (each warrants focused review beyond autonomous mode):
- ResolvedConfig sub-struct
- spawn_* clusters → src/app/spawn_*.rs
- Remaining ~20 app.rs append_raw sites
- Lint input caching on App (needs AppMsg variant + careful TTL plumbing at 3 sites)
- ebman audit replay (audit→CLI dispatch mapping is sprawling)
- EBL014/015/016/018/020 (each needs live-EB verification)
- docs/rule-development.md (focused docs cycle)

767 → 780 tests (+13).

### 0.19.0 release (2026-05-29)

Autonomous slice through the 0.19 candidates list. The big foundation refactors (ResolvedConfig, spawn_* clusters, finish app.rs audit migration) deserve focused human review and stay deferred to 0.19.1/0.20. This release is the polish-and-ship slice.

- [x] EBL017 — Managed Platform Updates disabled (Info, Manual fix) + 4 tests
- [x] `:config-diff --ignore-keys "k1,k2"` flag with case-insensitive name + namespace-qualified match + 4 tests
- [x] `ebman versions --env NAME [--json]` CLI subcommand (mirrors TUI `:versions`)
- [x] Confirm-modal lint sorted by severity DESC then rule_id ASC
- [x] `Action::label()` exhaustiveness extended to include Capacity + 15-variant count assertion
- [x] `audit::append_extras` wire-format golden pin
- [x] `Rule` trait invariants on the entire registry (registry-size check + consistency)
- [~] `ebman lint --baseline-regenerate` — **redundant**, existing `--baseline PATH` already overwrites unconditionally
- [~] `ebman lint --explain` — **redundant**, the existing `ebman explain ISSUE_ID` already does this (since 0.14)

Deferred to 0.19.1 / 0.20 (each warrants focused review beyond autonomous mode):
- ResolvedConfig sub-struct
- spawn_* clusters → src/app/spawn_*.rs
- Remaining ~20 app.rs append_raw sites
- Lint input caching on App
- :fleet-cost, ebman audit replay, Promotion lineage tracking
- EBL013-016, EBL018-020 (each needs live-EB verification)
- docs/lint-rules.md, docs/rule-development.md (focused docs cycle)

757 → 767 tests (+10).

### 0.19 candidates (2026-05-29)

Theme: **foundation pass + close the lint loop + small operator wins.** Three big refactors have been deferred 3-4× now (ResolvedConfig, spawn_* clusters, app.rs audit migration) — the codebase is structurally taut and these don't bleed but the next non-mechanical change in any of those areas is friction. 0.19 is the cycle to land them. The lint engine matured fast in 0.13-0.18 (12 rules live, baseline support, auto-fix); 0.19 closes the adoption loop with docs, more rules, and CI-shaped polish. Plus 4-5 operator features as the counter-balance so the release doesn't read as pure plumbing.

Aim for 8-12 of the items below; the rest can shape 0.20.

#### Foundation pass — HEADLINE (the deferred refactors)
- [x] **`App.cfg_resolved: ResolvedConfig` sub-struct** — Done (2026-06-06). The 12 config-derived fields (notify_webhook, command_aliases, lint_disable, explain_settings, required_tags, cfg_icons_raw, profile_themes, runbooks, safety_envs, safety_accounts, accounts, base_theme_name) now live in `App.cfg: ResolvedConfig`; every read site goes through `self.cfg.x` / `app.cfg.x` (scripted rename, compiler-guided). The contiguous constructor blocks wrap into `cfg: ResolvedConfig { … }`. No behaviour change; 67 config round-trip tests green. (`deploy_freeze`/`lint_fix_disable` from the old note don't exist as fields — backlog "etc." was loose.) Deferred 4×; finally landed.
- [x] **`spawn_*` clusters → `src/app/spawn_*.rs`** — Done (2026-06-01). The five cohesive clusters are now their own modules: `spawn_why_red` (0.21) + `spawn_batch` / `spawn_dlq` / `spawn_rollout` / `spawn_detail` (this sweep, 10 scattered methods gathered). Each a pure relocation (`fn` → `pub(super)`, `use super::{…}` for parent items), gated build+clippy+826-tests green per cluster. `app.rs` 22,410 → 21,657 (−753 lines this sweep; the −4-5k estimate was optimistic — much of app.rs is the `App` struct + `AppMsg` handlers + the ~30 remaining inline `spawn_*`/`cmd_*` singletons that don't cluster cleanly). The adhoc singletons (`spawn_confirm_lint` / `spawn_ssm_run_impl` / `spawn_action` / `spawn_refresh` / etc.) stay inline — they don't form a cohesive module and moving them would just scatter the dispatch logic. Deferred from 0.15/0.16/0.17/0.18/0.19/0.20.
- [x] **Remaining `append_raw` action sites → typed helpers** — Done (2026-06-06). Only 9 raw sites remained (not ~20). Added `append_action_skipped` / `append_action_undone` / `append_dlq_op`; routed the 8 action/DLQ sites (batch tag/untag/set-option + skip, undo, 4 DLQ ops) through typed helpers. Wire format preserved byte-for-byte (batch dispatched now also quotes special values via the shared `append_extras`, matching every other action line — no parser change). The one remaining `append_raw` caller is the passive `stage=event kind=red_transition` health line, which is genuinely an event, not an action. +1 golden test. Also extracted the DLQ handler cluster (`open_dlq`/`open_dlq_from_why`/`close_dlq`/`handle_dlq_key`) into `src/app/mode_dlq_handlers.rs` (completes the spawn_* cut; app.rs −244 lines).
- [x] **Lint input caching on App** — Shipped in 0.21.0 (stale checkbox fixed 2026-07-15; `App.env_tag_cache` + `env_health_cache` with 60s TTL landed per the 0.21.0 release notes below). Original sketch: 0.18's parallel `list_tags` + `fetch_env_instance_counts` fetches add ~one round-trip's worth of latency to every confirm-modal lint. Cache the results on `App.env_tag_cache: HashMap<String, Vec<String>>` + `App.env_health_cache: HashMap<String, EnvInstanceCounts>`, populated lazily by the periodic refresh tick. `spawn_confirm_lint` reads from cache when fresh (< 60s old), falls back to live fetch when stale or missing. Modal-open latency drops from `max(t_opts, t_tags, t_health)` to just `t_opts`. Cleared on context switch alongside the other env-keyed state. ~2 hrs.

#### Test coverage — SUPPORT
- [x] **CLI subcommand unit tests** — Done (2026-06-01). Took the arg-parse seam rather than full `aws-smithy-mocks` end-to-end: each subcommand's inline argv parsing (which called `std::process::exit` directly — the reason it was untestable: `exit` kills the test process) was extracted into a pure `parse_*_args(&[String]) -> Result<_, String|Error>`, with `run()` mapping the error to `eprintln` + `exit`. Covered all 7 verbs: `versions` (+5), `drift` (+6, region-CSV resolution), `action` (+9, incl. the destructive→exit-3 gate distinct from usage-2 via an error type carrying the code), `lint` (+8, the full cross-flag validation matrix + `--interval` grammar + value-flag `--baseline --json` trap), `audit` (+4, deterministic `--since`→window_ms with `Utc::now()` left in `run`), `ctl` (+5, injected default-socket + request assembly), `explain` (+5, positional-vs-flag + EBL### check). `envs` skipped — its only parse is `any(== "--json")`, no error paths. +42 tests, all behaviour-preserving. Full `run()` AWS-path coverage (the mocked-AWS layer) remains a deeper future cut.
- [x] **Exhaustiveness test for `Action::label()` distinctness** — Already done in 0.19 (stale entry, confirmed 2026-05-29). `action_labels_are_distinct_and_non_empty` already includes `Action::Capacity` and asserts the full 15-variant count (`all.len() == 15`). The "still omits Capacity" note predated the 0.19 fix. No work needed.
- [x] **`audit::append_extras` wire-format pinning** — Shipped. `append_extras_golden_wire_shape` at `src/audit.rs:1096` golden-pins the `key=value` / `key="..."` encoding (verified 2026-06-01, code-review triage).
- [x] **Rule `fix()` shape exhaustiveness** — Done (2026-05-29). The entry's premise was wrong on three counts and the genuine gap was smaller than stated: (1) there is no `cmd_lint_fix` panic — auto-fix lives in `src/cli/lint.rs` and handles `fix() == None` with `let Some(action) = … else { continue; }`; (2) `applies()=Some, fix()=None` is *legal and intended* (EBL003 "env Red >4h" is a state, not a fixable config); (3) `FixAction` has only two variants (`SetOption` / `Manual`), no `Multiple`, so "returns one of the documented variants" is already type-guaranteed. The real invariant (`applies()=None ⟹ fix()=None`) was already pinned by `rules_satisfy_trait_invariants`; the genuine unguarded gap — a malformed `fix()` payload (empty namespace/name/value/description on `SetOption`, or empty `Manual` instructions) — is now asserted in that same test. Strengthened the existing test rather than adding a redundant one.

#### New lint rules — SUPPORT
Cheap to add (each ~50 lines + tests, all use the existing `LintContext` builder pattern). Pick 4-5 to lock the rule-engine maturity story.
- [x] **EBL013 — launch configuration ASG** (Warn). Shipped — `LaunchConfigurationLegacy` at `src/lint.rs:1238`, registered at `:1472` (verified 2026-06-01).
- [x] **EBL014 — deprecated CW namespace in `:scaling-triggers`** (Warn). Shipped 2026-07-15 (0.25), reshaped — see the 0.25 candidates section. `AWS/EC2 → MetricCollection_5Minutes` is a legacy mapping; newer envs should use `aws/applicationelb` or env-health metrics. Fix=Manual.
- [x] **EBL015 — custom platform with no published versions in 180+ days** (Info). SHIPPED 2026-08-20 as the first account-level rule (pure pass outside the registry, `env_name: None`, both CLI + MCP lint). Was held 2026-07-15: ListPlatformVersions carries no dates; needs DescribePlatformVersion + an account-level issue shape. Builds on the existing custom-platforms fetch. Operator probably forgot the platform exists. Fix=Manual.
- [x] **EBL016 — live health-check probe non-2xx** (Warn). Shipped 2026-07-15 (0.25) behind `--probe-live` — see the 0.25 candidates section. Reuses the `spawn_health_check_probe` + `classify_health_check_status` code already shipped for confirm modals. Fires the probe at lint time, not just at deploy. Latency cost is real (one HTTP roundtrip per env at lint time); gate behind `--probe-live` flag in CLI to keep default lint fast.
- [x] **EBL017 — managed actions disabled** (Info). Shipped — `ManagedActionsDisabled` at `src/lint.rs:1397`, registered at `:1473` (verified 2026-06-01).
- [x] **EBL018 — env without WAF + on prod tier** (Warn). SHIPPED 2026-08-20, demand surfaced same-day (live dogfood traced 2 days of health flapping to unfiltered scanner sweeps — the exact traffic a WebACL absorbs). Probe-gated on prod-name + ALB via new aws-sdk-wafv2 dep; classic ELBs out of scope. Registry 18 → 19. Was held 2026-07-15: WAF association isn't in option settings; needs a new aws-sdk-wafv2 dep. Detection: env's ALB has no `webacl_arn` listener association AND env name matches `prod` / `production` / `prd` (case-insensitive). Soft prod-detection — operators can disable per env via `lint.disable`. Fix=Manual (WAF setup is its own flow).
- [x] **EBL019 — `ConfigDeploymentPolicy=AllAtOnce` on multi-instance + multi-AZ env** (Warn). Shipped — `AllAtOnceMultiAz` at `src/lint.rs:1325`, registered at `:1474` (verified 2026-06-01).
- [x] **EBL020** — ALREADY SHIPPED; entry was a spec for something built. Verified 2026-08-22: `XrayEnabledButTracesDenied` is registered in `default_rules`, documented in `docs/lint-rules.md`, tested, and `ebman lint`'s IAM probe wires it (probing only when `XRayEnabled` is on, so the common path pays no IAM call). 19 rules registered + the EBL015 account-level pass = the "nineteen rules" `docs/commands.md` claims. Original spec: (Warn). Detection: `aws:elasticbeanstalk:xray.XRayEnabled = "true"` plus IAM probe (uses the existing `iam:SimulatePrincipalPolicy` from `:explain` IAM path). Silent gap — operators see "X-Ray on" in config but no traces appear. Fix=Manual.

#### Lint adoption polish — SUPPORT
Close the loop on the lint engine maturing. Now that 12+ rules fire reliably, operators need adoption ergonomics.
- [x] **`docs/lint-rules.md`** — Shipped (file present, 10 KB; verified 2026-06-01).
- [x] **`docs/rule-development.md`** — Shipped (file present, 9 KB; verified 2026-06-01).
- [x] **`ebman lint --watch --webhook URL`** — Shipped 2026-07-15 (0.25) with a change-guard so identical cycles don't re-page. Original sketch: for ops teams that don't want a tail process but do want periodic alerts. Hooks into the existing `notify_webhook` plumbing. Composes with `--watch --interval`. ~1 hr.
- [~] **`ebman lint --baseline-regenerate`** — Redundant (verified 2026-06-04): `ebman lint --baseline FILE` already `std::fs::write`s the path, truncating + overwriting unconditionally — re-running it *is* regeneration. No separate flag needed. (Duplicate of the `[~]` conclusion above.)
- [x] **Confirm-modal lint sorted by severity** — Done (shipped pre-0.22; `ui.rs` confirm-modal lint render already sorts severity DESC then rule_id ASC). This was a stale duplicate of the `[x]` entry above.

#### Operator features — BONUS (pick 2-3)
- [~] **`ebman lint --explain ISSUE_ID`** — Redundant (verified 2026-06-01): the standalone `ebman explain ISSUE_ID` subcommand (`src/cli/explain.rs`) already ships the CLI explainer. No separate `--explain` flag needed; withdrawn.
- [x] **`ebman audit replay <line-id>`** — Shipped 2026-07-15 (0.25) — see the 0.25 candidates section. Original sketch: given an audit-log line (or a timestamp-keyed ID), re-run the same command. Wire-format-aware now that 0.18 consolidated audit shapes. Refuses for ambiguous lines (multiple matches) and for destructive actions without `--yes`. Useful for incident review: "what would happen if I ran this again?". ~2 hrs.
- [x] **`:fleet-cost`** — Shipped — `cmd_fleet_cost`, dispatched at `src/app.rs:11388` (verified 2026-06-01).
- [x] **`:diff A B --ignore-keys "..."`** — Shipped in 0.23.0 (stale checkbox fixed 2026-07-15; see the Tier 0 done entry dated 2026-06-04). The `[diff] ignore_keys` config-default idea from this sketch did NOT ship — only the per-invocation flag. Track separately if wanted.
- [x] **Promotion lineage tracking** — Shipped — `:promotions` dispatched at `src/app.rs:11389` (`cmd_promotions`); verified 2026-06-01.
- [x] **`ebman versions --env NAME [--json]`** — Shipped — `src/cli/versions.rs` (verified 2026-06-01; arg-parse tests added this session).

#### Distribution + perf — SUPPORT
Smaller wins that don't fit the other buckets.
- [x] **ARM64 Linux tarball in release matrix** — Shipped in 0.23.0 (stale checkbox fixed 2026-07-15; see the Tier 0 done entry dated 2026-06-04). Original sketch: operators on ARM-based CI (AWS Graviton, Apple Silicon Linux VMs) currently `cargo install` from source because there's no prebuilt `aarch64-unknown-linux-gnu` tarball. Add to `.github/workflows/release.yml` + bump Homebrew formula's Linux branch to pick the right tarball per `Hardware::CPU.intel?`. ~30 min (mostly CI yak-shaving).
- [x] **Migrate `notify_webhook` from curl shell-out to `reqwest`** — Done (2026-05-29). `audit::fire_webhook` now POSTs via the async `reqwest::Client` (10s timeout) instead of `tokio::process::Command::new("curl")` piping the body over stdin. Kept the existing `tokio::spawn` + `Handle::try_current()` guard (the fn is already only reached from inside the runtime; the guard makes a stray non-runtime call a silent no-op rather than a panic). Async client, not `blocking` — we're already inside `tokio::spawn`, where `blocking` would panic. No new Cargo feature needed (reqwest's async client + `rustls-tls` were already wired for `llm.rs`). Non-2xx responses + transport errors now log a structured `tracing::warn!` (parity with the old curl stderr path). Closes the webhook curl call site; the crates.io update-check (`update_check.rs`) and CW-Logs S3 fetch (`aws.rs`) curl sites remain — different shapes, separate follow-ons.

#### Out of scope for 0.19 (track in Feature candidates below)
- `:event-tail` cross-fleet event tail — own design surface (fan-out coordination, output rate-limiting)
- `:incident START/END` — composite of freeze + banner + audit subsection; design call on what's in the auto-runbook
- EBL021+ rules — track as the rule engine matures and operators ask

### 0.18.0 release (2026-05-28)

Theme: **live the stubs.** EBL008/010/011/012 shipped in 0.17 as code but several couldn't fire because their inputs weren't plumbed. 0.18 closes the gap — all four rules now fire in TUI and (mostly) CLI.

- [x] EBL011 worker DLQ depth — plumbed via `App.worker_dlq_depths` at 3 TUI sites (`spawn_confirm_lint` / `cmd_explain_issue` / `:lint`)
- [x] EBL010 env tag keys — inline `list_tags(env.arn)` fetch at 4 sites, parallel with options/health via `tokio::join!`
- [x] EBL012 healthy instance count — inline `fetch_env_instance_counts` parallel with EBL010 fetch
- [x] CLI EBL008 wiring — per-region `list_solution_stacks` + `aws::latest_stack_versions(&s)`
- [x] Audit-shape migration: 11 `cmd_*.rs` sites → typed `append_action_*` (wire-breaking: completed lines now use `outcome=ok` / `outcome=err err="..."`)
- [x] Hash-value pinning test for `issue_identity_hash` (2 golden tests — with-env and no-env)
- [x] `Action::wants_preflight()` exhaustiveness test (15 variants)

Deferred to 0.19:
- `App.cfg_resolved: ResolvedConfig` sub-struct (touches 30+ read sites)
- `spawn_*` clusters → `src/app/spawn_*.rs` grouping (61+ methods)
- Remaining ~20 `append_raw` sites in `src/app.rs` (SSM/DLQ/CW Logs — need new typed helpers)
- CLI subcommand unit tests (needs `aws-smithy-mocks` integration setup)

### 0.17.1 patch (2026-05-28)

Post-0.17 bug + UX hunt fixed three operator-visible Importants.
Smaller polish items (Minors + UX) tracked for 0.17.2.

- [x] `pending_actions` not cleared on context switch — apply_rebuild now clears both `pending_actions` and `pending_dispatch` alongside the other env-keyed state.
- [x] `ebman lint` CLI plumbs `required_tags` (EBL010 precondition; env_tag_keys deferred to 0.18). Brings CLI-side LintContext construction to TUI parity.
- [x] `:rollout --parallel` JoinHandle failure attribution — `join_next_with_id()` + id→region map so panic/cancel still maps to its launched region.

#### 0.17.2 patch (Minor + UX from the hunt) — shipped 2026-05-28
- [x] `apply_view` filter-clear contract documented (snapshot semantics — filter always restores, sort/grouped/scope no-op when absent)
- [x] `Action::Scale` modal copy: scale-to-zero gets explicit SCALE-TO-ZERO warning + `:start` recovery hint
- [x] `"no env selected"` hint expansion — 45 sites updated with "press 1-9, click a row, or type ' to jump by name"
- [x] `U` row in `docs/keys.md` for cancel-pending-dispatch
- [x] `:profile NAME` pre-checks against parsed profile list before kicking rebuild
- [x] `--demo` refuses destructive dispatch at `spawn_action` + `deny_write` + `tick_pending_dispatch` (covers Single + Batch paths)
- [x] `format_aws_error` adds `InvalidClientTokenId` / `SignatureDoesNotMatch` arm with `aws configure --profile X` hint
- [x] `:ssm-run` Y/N confirm — landed in 0.17.3. `Action::SsmRun` variant + `ssm_run_command`/`ssm_run_instances` on ConfirmModal + ParameterisedAction. spawn_action short-circuits to spawn_ssm_run_impl (TextOverlay shape vs standard ActionResult). cmd_ssm_run rewritten as parse + resolve + open_parameterised_action.

### 0.17.4 patch (2026-05-28)

`/code-review max everything` surfaced 15 findings — 5 Important + 10 Minor. All 15 fixed in 0.17.4 (one as a documented design choice rather than code change).

**Important (5):**
- [x] `spawn_deploy_from_local` + `spawn_terminate_instance` route through `deny_write` (was: bare `is_read_only_for` → phantom audit lines in `--demo`)
- [x] `profiles::load_profiles()` + helpers honor `AWS_CONFIG_FILE` / `AWS_SHARED_CREDENTIALS_FILE`
- [x] `Action::SsmRun` audit-log canonical name `"SsmRun"` (was: 3 different identifiers across dispatched/completed/cancelled stages)
- [x] `apply_rebuild` clears `self.detail` + `pending_shell_target` + `action_flow` + `picker` + resets mode → no more cross-context dispatch via stale Detail snapshot
- [x] `apply_rebuild` clears `status_message` → no more lying "X dispatches in 5s" bar after context switch

**Minor (10):**
- [x] `cmd_account` fallback validates against profile list
- [x] `Action` enum is `#[non_exhaustive]` (closes the 0.17.3 SemVer-major)
- [x] Scale-to-zero modal renders full destructive styling (was: red body + non-red accent asymmetry)
- [x] SsmRun bypasses 5s cancel-window (was: fast probes paid 5s tax)
- [x] SsmRun bypasses `push_pending` — deliberate design choice with strengthened comment (won't-fix)
- [x] `format_aws_error` arm-ordering pinned by inline comment
- [x] SsmRun modal skips `traffic_warning` (was: noise on diagnostic shells against Red envs)
- [x] (covered by apply_rebuild status_message clear)
- [x] `deny_write` demo toast composes safety-pin reason when applicable
- [x] `action_destructive_covers_*` test exhaustive on all 15 Action variants

4 new tests (3 profile-path env-var, 1 deny_write compose). Suite at 754.

### 0.17 candidates (2026-05-28)

Theme: **make the stubs live + lint adoption ergonomics.** 0.16 shipped EBL007-010 but EBL008 (stale platform) and EBL010 (required tags) silently no-op in production because their context fields (`latest_stack_version`, `required_tags`) aren't plumbed through. 0.17 plumbs them — and lands two more high-signal rules built on the same plumbing (EBL011 DLQ depth, EBL012 instance-count divergence). Plus `lint --baseline` so teams onboarding lint on a noisy fleet can grandfather existing issues without declaring bankruptcy. Plus tail cleanup (LintContext builder pattern, run_inline_ssm removal, two quick UX wins).

#### Smart features — HEADLINE
- [x] **LintContext builder + plumb `latest_stacks` + `required_tags`** — Shipped 0.17.0 (builder pattern + EBL008 wiring) → 0.18.0 (EBL010 required_tags + env_tag_keys plumbing finished at all 4 sites).
- [x] **EBL011: worker env with DLQ depth > N (Warn)** — Shipped 0.17.0 (rule landed) → 0.18.0 (plumbing via App.worker_dlq_depths at 3 TUI sites). CLI side intentionally unwired.
- [x] **EBL012: env reports 0 healthy instances but status=Green (Warn)** — Shipped 0.17.0 (rule landed) → 0.18.0 (plumbing via `fetch_env_instance_counts` parallel fetch).
- [x] **`ebman lint --baseline FILE` / `--against-baseline FILE`** — Shipped 0.17.0.

#### Cleanup — SUPPORT
- [x] **Remove `run_inline_ssm` dead code** — Shipped 0.17.0 (commit `2029d9e`).
- [x] **`:undo` discoverability toast** — Shipped 0.17.0 (commit `a400f4b`).
- [x] **First-run identity_warning routing** — Shipped 0.17.0 (commit `a400f4b`).
- [x] **docs/configuration.md backfill** — Shipped 0.17.0 (commit `105c49c`).

#### Out of scope for 0.17 (track for later)
- **`App.cfg_resolved: ResolvedConfig` sub-struct** — Biggest cut to App's 12 mirror fields. Architecture-review item; ~3hrs lift; not bleeding. Hold for 0.18.
- **`spawn_*` clusters → `src/app/spawn_*.rs` grouping** — Deferred from 0.15 + 0.16. Counted at 60+ methods in 0.15, still 60+ in 0.16 (didn't compound, didn't shrink). Hold for 0.18 until it actually compounds or someone hits the pain.
- **24 remaining dispatched-only `append_raw` sites** — Migrate to `audit::append_action_dispatched`. Gets webhook fan-out for every destructive dispatch. Mechanical; ~2hrs. Hold for 0.18.
- **CLI subcommand unit tests** — 0 tests across `src/cli/*.rs`. Real coverage gap. Hold for 0.18 as a dedicated test-coverage release.
- **`:explain` IAM split** — `:why iam` / `:why role` for IAM AccessDenied path, leaving `:explain` for lint. UX win but disambiguation needs operator input. Hold pending feedback.
- **MCP server (`ebman mcp serve`)** — ~~Now deferred 5×. Stop tracking unless external demand surfaces.~~ Demand surfaced 2026-08-03; spec locked as the 0.26 HEADLINE (see "0.26 candidates" section).
- **`:queue` action-queue inspector** — Held; abort semantics still unsolved.
- **`ebman explain --env NAME` cross-issue synthesis** — Useful but bigger prompt-engineering surface. Re-evaluate post-0.17 once new rules land.

### 0.16 candidates (2026-05-27)

Theme: **continuation + smart-feature depth + rollout deepening.** 0.15 finished the major refactors but left tail work (incomplete audit consolidation, draw_splash in main.rs, duplicated JSON-escape helpers). 0.14 shipped lint/explain/audit; 0.16 adds the monitoring-loop flag (`--watch`) and more rules so the smart-features arc keeps gaining ground. 0.13 shipped sequential cross-region rollout; 0.16 adds the three operational shapes operators eventually want (parallel, continue-on-fail, staggered).

#### Continuation cleanup — SUPPORT
- [~] **Audit migration: ~30 `append_raw` sites → typed `append_action_*`** — Partial: 11 `cmd_*.rs` sites migrated in 0.18.0 (wire-breaking — completed lines now `outcome=ok` / `outcome=err err="..."`). ~20 remaining sites in `src/app.rs` are SSM / DLQ / CW Logs operations without a natural completed stage — tracked in 0.19 candidates with a "needs new typed helpers" note.
- [x] **`draw_splash` + `hsl_to_rgb` move to tui-common splash** — Shipped 0.16.x.
- [x] **Unify JSON-escape helpers** — Shipped: consolidated to `util::json_escape` / `util::json_string`.
- [x] **`decide_poll` shared between CLI + TUI** — Shipped 0.16.0 (`5896afbc`).

#### Smart features — HEADLINE
- [x] **`ebman lint --watch [--interval 60s]`** — Shipped 0.16.0.
- [x] **New lint rules EBL007+** — Shipped: EBL007-009 in 0.16.0, EBL010-012 in 0.17.0, all four now actually firing as of 0.18.0.

#### Rollout deepening — HEADLINE
- [x] **`:rollout --parallel [--max-concurrency N]`** — Shipped 0.16.0.
- [x] **`:rollout --continue-on-fail`** — Shipped 0.16.0.
- [x] **`:rollout --staggered Nm`** — Shipped 0.16.0.

#### Out of scope for 0.16 (track for later)
- **`spawn_*` clusters → `src/app/spawn_*.rs` grouping** — BONUS deferred from 0.15. Big lift (~3hrs) and purely organisational; doesn't bleed. Hold for 0.17.
- **MCP server (`ebman mcp serve`)** — still no operator demand signal.
- **`:queue` action-queue inspector** — held; abort semantics still unsolved.
- **`ebman explain --env NAME` cross-issue synthesis** — bigger prompt-engineering surface; per-issue explain still has road-time left.
- **EBL002 auto-fix** — needs interactive prompt for the path; Manual stays correct.
- **TOML parser migration (config.rs / state.rs)** — hand-rolled works; big lift.
- **Saved views structured-schema migration** — string-encoded shape works.

### 0.15 candidates (2026-05-27)

Theme: **foundation pass.** No new operator-facing features — pure structural cleanup driven by the 0.14.0 architecture review. `src/app.rs` is at 21,794 lines / 532 methods; `src/main.rs` is at 2,625 lines with seven inline `run_*_cli` async fns that have become a CLI grab-bag. The user codified the code-review-before-tagging step in 0.14.1 — this release acts on its findings before the cliff hits at ~0.18. Sets the table for 0.16+ feature work to land in cleaner modules.

#### CLI split — HEADLINE
- [x] **CLI subcommands → `src/cli/{audit,explain,lint,drift,envs,action,ctl}.rs`** — Shipped 0.15.0.

#### Audit + explain — SUPPORT
- [x] **Audit writers → `src/audit.rs`** — Shipped 0.15.0 (commit `260ec41`).
- [x] **`App.explain_*` → `App.explain_settings: llm::Settings`** — Shipped 0.15.0.

#### spawn_* grouping — BONUS
- [x] **`spawn_*` clusters → `src/app/spawn_*.rs`** — Done (shipped in 0.22.0). The clusters now live in `src/app/spawn_batch.rs`, `spawn_detail.rs`, `spawn_dlq.rs`, `spawn_rollout.rs`, `spawn_why_red.rs`. (Stale `[ ]` — the work landed across the commits ending `ab69aad`; marked done 2026-06-04.)

#### Out of scope for 0.15 (track for later)
- **MCP server (`ebman mcp serve`)** — speculative; no operator demand yet. Re-evaluate post-0.15 once foundation work has shipped.
- **`:rollout --parallel` / `--continue-on-fail` / `--staggered Nm`** — deepens 0.13 rollout; held pending operator feedback on what the real failure-handling patterns are.
- **`ebman explain --env NAME` cross-issue synthesis** — bigger prompt engineering surface. Held until per-issue explain has road-time.
- **EBL002 auto-fix (health-check URL)** — needs interactive prompt for the path; Manual fix stays in 0.14 shape.

### 0.14 candidates (2026-05-27)

Theme: **from diagnostic to remediation.** 0.12 surfaced issues (`:lint`, `:drift`). 0.13 made them fleet-wide (`--regions` everywhere). 0.14 makes them actionable — LLM-backed explanations turn structured `Issue` output into operator-readable next steps, opt-in auto-fix dispatches the obvious-correct-answer ones through the existing undo machinery, and the audit log gets a first-class CLI for monitoring / Slack-bot integration. The user's earlier directive: "claude code/api integration would be nice, but not this version [0.13]" — meaning the time is now. Plus: "smart features must be available as standalone arguments so they can be run as git hooks, CI, monitoring tools" — same constraint applies to every item below.

#### Actionability core — HEADLINE
- [x] **LLM-backed explainer: `ebman explain ISSUE_ID` + `:explain ISSUE_ID`** — Shipped 0.14.0.
- [x] **`ebman lint --fix` + `:lint --fix` auto-remediation** — Shipped 0.14.0. Auto-fix on EBL001/004/006/009.

#### Operationalisation — SUPPORT
- [x] **`ebman audit` CLI** — Shipped 0.14.0.

#### Out of scope for 0.14 (track for later)
- **MCP server (`ebman mcp serve`)** — exposes ebman's read ops as MCP tools so Claude Code can drive ebman. Speculative; only build if there's demand or if the LLM explainer surfaces a "this would be useful for Claude Code too" signal during 0.14 build.
- **`:rollout --parallel` / `--continue-on-fail` / `--staggered Nm`** — Deepens 0.13's sequential rollout primitive. Real ops patterns but no operator has asked for them yet; wait for the 0.13 rollout to be road-tested before extending.
- **`ebman explain --env NAME` cross-issue synthesis** — run lint + drift on an env, feed ALL issues into a single LLM call, get an integrated "here's what's wrong with this env" narrative. Useful but bigger prompt-engineering surface than the per-issue v1; track if v1 explain lands well.
- **Auto-fix for EBL002 (missing health-check URL)** — would require asking the operator for the path; not auto-fixable without interactive prompt. Stays in Manual category.

### 0.13 candidates (2026-05-26)

Theme: **smart features — rule-based diagnostics with both TUI + CLI surfaces.** Shared rule engine drives `:lint` (TUI), `ebman lint` (CLI for git hooks / CI / monitoring), and confirm-modal warning lines. Terraform integration detects drift between live EB state and the operator's tfstate. LLM-based explanation (Claude API) is designed-for but out of scope for 0.13.

#### Docs polish — SHIPPED
- [x] **README split into `docs/`** — SHIPPED. Trimmed 448 → 103 lines; reference material (keys / commands / configuration / fonts / headless / safety+privacy / development) moved to topic-grouped files under `docs/`. README now leads with hero + triage workflow + highlights + install + quickstart + a documentation index, instead of forcing new users to scroll past ~350 lines of reference tables before they hit "Install". Inline pipe-separated TOC + Install moved up directly under the demo gif in a follow-up edit.
- [x] **End-to-end docs review against shipped code** — Audited every file under `docs/` against the actual implementation: fixed `ebman ctl reload` reference (no such op), repaired malformed `[runbooks]` TOML example, added `]`/`[` (cycle saved views) and `T` (cycle event-time format) to `docs/keys.md`, backfilled ~30 missing commands in `docs/commands.md` (most notable: `:rollback`, which the README's triage workflow points at), added a Diagnostics section covering `:lint` / `:drift` / `:explain`, fixed stale "named filters + saved views" wording to match the 0.12 unified store, corrected `ebman ctl` "second binary" → subcommand framing, and added `ebman lint` / `ebman drift` + exit-code convention to `docs/headless.md`. Source-of-truth for command descriptions is `src/commands.rs` registry (CI-checked against dispatch arms).

#### Smart diagnostic core — HEADLINE
- [x] **Rule engine + `:lint` TUI overlay + `ebman lint` CLI** — Shipped 0.13.0.
- [x] **Terraform integration: `:drift` overlay + `ebman drift` CLI + tf-managed badge** — Shipped 0.13.0.

#### Smart diagnostic integration — SUPPORT
- [x] **Confirm-modal lint hooks at write time** — Shipped 0.13.0 (generalised via `spawn_confirm_lint`).
- [x] **Cross-region rollout: `:rollout LABEL --regions r1,r2,r3` + `ebman action rollout`** — Shipped 0.13.0.

#### Smart diagnostic polish — BONUS
- [x] **Config: per-rule severity overrides + project-local rule disables** — Shipped 0.13.0.

#### Out of scope for 0.13 (track for later)
- **LLM-backed explainer (`ebman explain ISSUE_ID`)** — Designed for: rule engine emits structured `Issue` with discrete `detail` + `suggestion` + `fields` that an LLM could ingest. Wire-up to Claude API (or local model) is 0.14+. Operator opt-in via config; no API calls without explicit consent.
- **MCP server (`ebman mcp serve`)** — exposes ebman's read operations as MCP resources/tools so Claude Code can drive ebman programmatically. Speculative; only build if there's demand.
- **Auto-remediation (`ebman lint --fix`)** — runs each rule's suggested fix. Powerful but dangerous; needs careful per-rule opt-in design.
- **`ebman audit --tail`** — surfaces the audit log for scripting. Plausible follow-on once the rule-engine CLI shape is proven.

### Feature candidates — post-0.18 (2026-05-29)

Nine ideas surfaced by a backlog review after the 0.17.x + 0.18 ship-sequence. Ranked by **operator-value-per-hour**. The 0.19 candidates list above already pulls the top three (lint-explain CLI / audit replay / fleet-cost); these are the medium-term shelf.

- [~] **`ebman lint --explain ISSUE_ID`** — Redundant; `ebman explain ISSUE_ID` already ships it (`src/cli/explain.rs`). Withdrawn 2026-06-01.
- [x] **`ebman audit replay <line-id>`** — Shipped 2026-07-15 (0.25) — see the 0.25 candidates section.
- [x] **`:fleet-cost`** — Shipped (`cmd_fleet_cost`, `src/app.rs:11388`; verified 2026-06-01).
- [x] **`:event-tail`** — Shipped 2026-07-15 (0.25) — see the 0.25 candidates section. Original sketch: like `:logs-tail` but for EB events across every env in parallel. "What's happening across prod right now" surface. Closes the gap between Detail/Events (one env) and the console's flat event firehose. Needs a fan-out coordinator + output rate-limiter so a noisy fleet doesn't blow out the overlay. ~4 hrs.
- [x] **`:incident START "headline" / END`** — Shipped 2026-07-15 (0.25) as the minimal composite (freeze + banner + audit lines) — see the 0.25 candidates section. Original sketch: single command sets up incident mode: freezes deploys, pins a banner, opens runbook overlay, starts an audit subsection. `:incident END` clears + writes a summary line. Builds on existing freeze + runbook + audit machinery. Design call on auto-runbook scope (just freeze + banner? or also: open `:why` on every Red env, start CW Logs tail on the noisiest one, dump current alarms-firing list to the audit?). ~3 hrs.
- [x] **Promotion lineage tracking** — Shipped (`:promotions`, `cmd_promotions` at `src/app.rs:11389`; verified 2026-06-01).
- [x] **`:diff A B --ignore-keys "..."`** — Shipped in 0.23.0 on the env-metadata `:diff` (stale checkbox fixed 2026-07-15; the 2026-06-01 "genuinely pending" note predated the 0.23 work).
- [x] **`ebman lint --watch --webhook URL`** — Shipped 2026-07-15 (0.25) — see the 0.25 candidates section. Original sketch: for ops teams that don't want a tail process but do want periodic alerts. Hooks into the existing `notify_webhook` plumbing. Composes with the existing `--watch --interval` shape. ~1 hr.
- [~] **`ebman lint --baseline-regenerate`** — Redundant (verified 2026-06-04): `ebman lint --baseline FILE` already overwrites the file unconditionally (`std::fs::write` in `src/cli/lint.rs`), so re-running it regenerates. Withdrawn — no separate flag.

### Feature candidates — competitive scan (2026-05-24)

Ten new ideas surfaced by a backlog/peer-TUI review after the 0.7.0 ship. Ordered roughly by operator-value-per-hour. None overlap with already-tracked items; the niche items already on the backlog (custom-platform create, topology graph, Route 53, etc.) stay where they are. Sized for a 0.9 batch — pick from the top.

- [x] **`:diff env-A env-B`** — Done (2026-05-24). Discovery: `:diff ENV` already existed (single-arg, selected-vs-arg, structured `Overlay::Diff` via the existing `diff_envs` renderer covering Name / App / Tier / Status / Health / Platform / Version / CNAME / Updated). The right shape was to extend that arm to also accept two args, not to add a parallel command — so the dispatch at `src/app.rs` now matches `(rest.first(), rest.get(1))` and routes the two-arg form to a path that names both envs explicitly with no selected-env fallback. Same-env-twice gets a clear "pick two different envs" error rather than silently comparing an env against itself (added to the single-arg form too as a small UX win). +3 tests (two-arg happy path, same-env rejection, unknown-env error). Help text + commands-registry description updated. **Scope note**: the BACKLOG entry originally suggested combining the env-metadata diff with the option-settings diff in a single overlay — that's a separate UX change to the overlay surface (would touch `Overlay::Diff` + `draw_diff_overlay`), not the "name both envs" change this entry described. Operators who want both diffs today run `:diff A B` then `:config-diff` separately. A combined view can be a follow-on if it's actually wanted.
- [x] **`:ssh [i-abc]`** — Done (2026-05-24). New `cmd_ssh` routes to the existing `pending_shell_target → open_embedded_shell` machinery (the same flow as pressing `s` on Detail/Instances), so the TUI-suspend/resume + alt-screen dance is shared code. With an arg, the instance ID is validated to start with `i-` (refuses typo'd env-names that would otherwise produce an opaque CLI error). No-arg form opens a new `PickerKind::SshInstance` populated from cached `Detail.instances` — if Detail isn't open with the Instances tab loaded, surfaces a clear error pointing the operator at the precondition rather than silently no-op'ing. **Scope note**: the BACKLOG entry originally also asked for `:ssm-run "<cmd>"` (cross-instance command runner via `ssm:SendCommand` + polling). That's a separate (bigger) feature — needs new SDK calls, polling state, and a multi-instance result aggregator. Tracked separately below.  +3 tests (arg happy path, typo'd arg rejection, no-arg-without-Detail error). Existing infrastructure used: `open_embedded_shell` (live), `run_inline_ssm` (kept dead-code as the "drop out fully" reference).
- [x] **`:ssm-run "<cmd>"`** — Done (2026-05-24). New `aws-sdk-ssm = "1"` dep, `SsmClient` wired alongside ACM / Secrets / IAM (region-scoped). `AwsClient::run_shell_command(instance_ids, command, wall_clock_secs)` fires `SendCommand` with `AWS-RunShellScript`, then polls per-instance `GetCommandInvocation` every 2s (matches `run_insights_query`'s cadence). Each invocation reaching Success / Failed / Cancelled / TimedOut drops out of the wait set; instances still pending after the wall-clock get a synthetic `TimedOut(local)` row so the operator sees which ones didn't finish. Results sorted by instance ID for determinism. `cmd_ssm_run` in app.rs reads target IDs from cached `Detail.instances` (same source as `:ssh` no-arg), strips surrounding quotes from the joined command tokens, gates via `deny_write` (treats SSM as a write because a shell command can mutate state), and lands the aggregated body via `format_ssm_results` — per-instance section headers `─── id [status, exit=N] ───` then `stdout:` / `stderr:` blocks, with 50-line + 200-char-per-line truncation so a verbose command doesn't blow out the overlay. Hard 60s wall-clock cap to keep the TextOverlay from hanging. +5 tests cover renderer happy path / empty stub / output truncation / no-args usage / no-Detail guidance. **Scope notes**: not adding a `--timeout` flag (60s default + SSM's own server-side TimeoutSeconds covers the read-probe use case); not following `standard_output_url` / `standard_error_url` for >24KiB outputs (operator can pipe to `head`/`tail`); not adding a multi-instance picker — `:ssm-run` always fans across all cached instances, just like the BACKLOG entry described.
- ~~**`:upgrade`**~~ Withdrawn (2026-05-24). The existing `:update` (`src/app.rs:9168`) carries an explicit design comment against auto-upgrade: "Doesn't actually upgrade — operators on AWS-touching tools prefer conscious upgrades, and self-replacing the binary across Cellar / cargo-bin / tarball layouts has too many platform footguns." That decision predates this BACKLOG entry; the entry was written without checking. `:update` already detects the install channel and yanks the right `brew upgrade ebman` / `cargo install ebman --force` command to the clipboard, so the gap is just "paste vs press enter." Not worth pushing against the existing design call without a fresh prompt.
- [x] **Cost overlay per env** — Done (2026-05-24). `app.costs: HashMap<String, f64>` is already populated by `:cost on` (Cost Explorer fan-out cached at `~/.cache/ebman/cost-{account}-{region}.toml`). Surfaced in two places: (a) `:why` overlay — new top-of-overlay row right after the runbook line, format `$NN/mo` with the same green/muted/red bucket palette as the envs-table COST column; (b) Detail/Health status line — appended as a `cost: $NN/mo` chip alongside status/health/DLQ so spend lives in the same scanline as health. Both sites no-op when `app.costs` is empty (operators who haven't enabled cost tracking see unchanged layout). No new state, no new fetch, no new dependency — pure rendering over the existing cache. Unit format is monthly (`/mo`) not hourly as the BACKLOG entry suggested — matched to what Cost Explorer actually returns + what the COST column shows, consistency wins. **Scope note**: bucket-threshold logic is now duplicated 3 sites (envs table / `:why` / Detail Health). Considered extracting `cost_bucket_color(cost, theme)` but the 3-module reach + the obviousness of the thresholds make the helper a wash. Worth revisiting if a 4th site shows up.
- [x] **Local config diff against `.elasticbeanstalk/saved_configs/*.cfg.yml`** — Done (2026-05-24). Took the YAML dep call — added `serde_yml = "0.0"` (actively-maintained successor to the archived serde_yaml). New `src/saved_config.rs` module: `parse_saved_config(yaml) -> Vec<ConfigOption>` walks the `OptionSettings: {namespace: {name: value}}` nested map and emits the same shape `fetch_env_configuration_options` returns, with YAML scalar coercion (`true` → `"true"`, `4` → `"4"`, `'4'` → `"4"`) so the diff stays consistent across quoted-vs-unquoted forms; `discover_saved_configs(cwd)` walks up to `.elasticbeanstalk/saved_configs/`, returning paths alphabetically sorted; `saved_config_name(path)` strips `.cfg.yml` / `.yaml` / `.yml` suffixes for the operator-facing name. New `:config-diff-local [NAME]` command in app.rs: no-arg auto-picks if there's exactly one saved config (lists names when there are multiple so the operator can rerun with one); reuses `diff_config_options` + `render_config_diff_overlay` so the diff UI is identical to `:config-diff`. +7 tests cover parse happy path / unquoted scalar coercion / missing-OptionSettings / garbage YAML / name extraction / discovery walk / empty-dir-returns-empty. **Scope notes**: read-only operation (no `:config-apply-local` to push the local YAML to the env — that's a separate destructive feature that needs its own confirm flow); also doesn't show env metadata diff (Description / Platform / Tags) — only OptionSettings, which is what operators actually diff.
- [x] **`:lineage`** — Done (2026-05-24). New `cmd_lineage` reuses the `list_events_for_env(_, 100)` fetch already used by `:changes` / `:rollback`, filters events that carry a non-empty `version_label`, and collapses consecutive same-label events into one row (one deploy generates multiple events: started / instance OK / env update completed). Pure `build_lineage(events) → Vec<LineageRow>` does the collapse + ordering (newest-first); pure `format_lineage(env, events)` renders the overlay with the deploy's span (`took`) and gap to the next-older deploy (`Δ since previous`). +3 tests cover collapse / version_label filter / span+gap rendering. Empty event window produces a stub matching the `:changes` style. **Scope note**: 100-event window same as `:changes` — high-frequency-deploy envs may need a deeper window; defer until anyone hits the cap.
- [ ] **`:queue` action-queue inspector** — Builds on `:pending`. Show currently-dispatched + recently-completed writes across *all* envs (not just selected), with per-row abort for cancellable ops (best-effort; most EB writes aren't cancellable but the dispatch ack can be discarded). Useful when running batch ops — operator sees what's still in flight without scrolling event tape. **Held (2026-05-24)** — `:pending` already shows the same data globally (iterates `self.pending_actions` across all envs). The genuinely new piece would be per-row abort, but most EB writes (UpdateEnvironment, deploys, restarts) aren't cancellable server-side — only the local dispatch ack can be dropped, which limits the operational meaning of an "abort" action. Without abort, `:queue` collapses to `:pending --in-flight` (one line of filter logic). Defer until the abort semantics are designed honestly.
- [x] **Saved views as tabs (gh-dash style)** — SHIPPED (2026-05-26, 0.12). Unified `named_filters` + `saved_views` into a single store (`App.saved_views`). `]` / `[` now cycles full views — filter+sort+group+scope all apply together. Chip bar at the top of the main view reads from `saved_views`. `:filter NAME` / `:save NAME` / `:drop NAME` / `:filters` all operate on the unified store with the filter-only encoded form; `:save-view NAME` / `:view NAME` / `:view-drop NAME` / `:views` use the same store with the full encoded form. Legacy `filter.NAME = "..."` lines in `state.toml` auto-promote into `saved_views` on first load using the filter-only encoding; explicit `view.NAME` wins on collision. First save after upgrade drops the legacy `filter.*` output. Pure helpers `encode_filter_only_view` + `view_filter_value` unit-tested. **Scope note**: the original BACKLOG framing imagined a structured `SavedView { filter, sort_key, sort_desc, grouped }` struct — the encoded-string form already shipped as part of `:save-view` does the same job and avoids the schema-migration scope.
- ~~**Profile / region quick-chord**~~ Withdrawn (2026-05-24) — already shipped, just not as Ctrl chords. `p` and `r` (plain keys in Normal mode at `src/app.rs:3311-3312`) open the Profile / Region picker overlays directly. Better than the Ctrl chords the BACKLOG entry proposed: no modifier required, and `Ctrl-R` would have clashed with the existing manual-refresh keybind anyway. The BACKLOG entry was written without re-grepping the existing keybinds — closing the loop honestly.
- [x] **CloudWatch alarm state timeline** — Done (2026-05-24). `:alarm-history NAME` fetches up to 50 entries via `cw:DescribeAlarmHistory`, surfaces them as a TextOverlay newest-first with timestamp + kind (`StateUpdate` / `ConfigurationUpdate` / `Action`) + summary. New `AlarmHistoryEntry` struct in `aws.rs` (at / kind / summary), new `fetch_alarm_history(alarm_name, max_records)` method on `AwsClient`, new `cmd_alarm_history` in `cmd_alarms.rs`, pure `format_alarm_history(alarm_name, entries)` in `app.rs`. Empty result shows the 90-day-retention hint so the operator knows whether the fetch succeeded. +2 tests (rendered entries / empty stub / missing timestamp). **Scope note**: the `H`-on-alarms-list-row drill-in keybind is deferred — the alarms-list overlay would need to become interactive (it's currently a static `TextDump`), which is a different piece of UX work. Command-from-`:` works today.

### Code review — 2026-05-23

Findings from a full review of the codebase against the 0.7.0 batch + recent trims. Three parallel surveys (ui.rs, app.rs / handle_event, aws.rs) cross-referenced with the BACKLOG and CHANGELOG. Items split into a **0.7.1 patch** bucket (real bugs + low-cost polish) and an **0.8 feature** bucket (new operator-value features not previously tracked).

#### 0.7.1 patch candidates — bugs and polish

- [x] **Paginate `DescribeApplicationVersions`** — Done (2026-05-23). `list_application_versions` now loops on `next_token` matching the `list_certificates` / `list_secrets` / `describe_alarms` shape. Mocked-AWS test `list_application_versions_pages_through_next_token` exercises two pages + asserts the loop terminates on the absent second-page next_token. Closes the truncated-`:versions` / broken-`:rollback` bug for orgs with hundreds of historical versions.
- ~~**Paginate `ListAvailableSolutionStacks`**~~ — Withdrawn (2026-05-23). The AWS SDK's `ListAvailableSolutionStacksOutput` has no `next_token` field — the API returns all stacks in a single response (AWS verified). The review-agent claim was wrong. Stale-platform check sees everything already.
- [x] **Theme-correctness sweep — hardcoded `Color::Black` / `Color::White` in pill rendering.** Done (2026-05-23). All ~10 production sites in `src/ui.rs` that hardcoded a foreground colour against a themed background now call `theme.contrast_text(bg)`: filter chip (2349/2364), scope pill (2392), group banner (3004), Worker/Web tier pills (3243/3251), Ready status pill (3391), Updating status (3401), Terminating status (3406), AUTO badge (4700), Powerline tab fg (4847), non-Powerline tab fg (4882). Test-only `Color::Black` / `Color::White` references are dummy inputs (not rendered); left alone. The lone remaining `5412` site is a search-match highlight against literal `Color::Yellow` (bright in every terminal) — not a theme bug. Light + high-contrast themes now render readable text in every pill.
- ~~**Help routing for `Picker` and `LogTail` overlays.**~~ Withdrawn (2026-05-23). Verified that neither footer actually advertises `?` — Picker's footer at `src/ui.rs:3690` and LogTail's at `src/ui.rs:1197` are both honest about their key surface. The review-agent claim was wrong. Adding help screens would be a feature, not closing an inconsistency; Picker's 4-key surface is too small to justify one, and LogTail's footer is already a serviceable one-liner.
- ~~**Drop vestigial `session_id` on `Overlay::WhyRed` and `Overlay::LogTail`.**~~ Withdrawn (2026-05-23). Re-audit shows the `session_id` field is load-bearing, *not* vestigial. The centralised `AppMsg::generation()` guard catches cross-context staleness; `session_id` discriminates between *same-generation* overlay re-opens (operator opens `:why` on env A → in-flight `WhyRedEvents` for A → operator closes and opens `:why` on env B → without the session_id check, A's fetcher result lands on B's overlay). The handlers in `src/app/msg.rs:534-540` compare the incoming `session_id` against the *overlay's* stored session_id, not `self.*_session`. Same shape for `LogTail`'s session_id, which additionally routes events to `current_overlay` vs `pre_help_overlay` based on session match (`msg.rs:776-784`) — a feature the generation guard can't provide. Keep both fields.
- [x] **Centralise overlay sizing.** Done (2026-05-23). New `OverlaySize` enum with four categories (`Small` / `Picker` / `Text` / `Wide`) and a `centered_overlay(category, frame)` helper. All 19 production `centered_rect(W, H)` call sites migrated to the helper — action-menu / action-confirm / apps-action-menu → Small; palette / saved-configs / picker / swap-target → Picker; form / text-dump / alarms / history / whatsnew / describe / help → Text; log-tail / diff / why-red / report-bug → Wide. Size table lives in `overlay_dims()` as the single source of truth so re-tuning is one-line. +2 tests (`overlay_dims_ordering_makes_sense`, `overlay_dims_are_within_legal_percent_range`).

#### 0.8 feature candidates — new operator-value features

- [x] **`:logs-insights QUERY`** — Done (2026-05-23). New `run_insights_query` in `aws.rs` starts a CloudWatch Logs Insights query against the env's discovered log groups, polls `GetQueryResults` every 2s, and returns rows + scan stats once the server reaches a terminal state (Complete / Failed / Cancelled / Timeout). Default time range is the last 1 hour. Multi-group is supported by Insights natively, so we pass every group discovered by the existing `discover_env_log_groups` call — no log-group picker needed. Result lands as a `TextOverlay`. Pure `format_insights_results` renders a column-aligned table with per-column width capped at 60 cells (long values get a `…` truncation marker so the overlay stays readable). The synthetic `@ptr` Insights field (a record locator, not operator content) is filtered out of every row consistently. The scan-stats footer surfaces `matched / scanned` so the operator can see the cost of broad queries. Empty results show a "(no rows matched the query)" stub. +3 tests covering happy-path table render, empty stub, and the 60-char truncation behaviour. Scope notes: query cancellation on overlay close isn't wired (AWS bills on data-scanned, so cancel-late doesn't save money; 15-min server-side timeout caps the wall-clock). `--window` flag for arbitrary time ranges is a possible follow-on but the default 1h covers the common post-incident triage case.
- [x] **`:envs-by-version LABEL`** — Done (2026-05-23). Fans out across every `~/.aws/{config,credentials}` profile plus every `accounts.NAME` AssumeRole entry; filters envs by exact `version_label == LABEL` match (case-sensitive — labels are identifiers, not search terms). Each hit row shows source / env / app / health / status so the operator can pivot to `:account NAME` or `:profile NAME`. Per-source errors collected separately so a single AssumeRole failure doesn't poison the whole scan. New `cmd_envs_by_version` in `src/app/cmd_overlay.rs`, registered in `src/commands.rs` under Navigation. Operational use case: bad build in prod, need fleet-wide blast radius in one call.
- ~~**`:deploy --dry-run`**~~ Withdrawn (2026-05-23). Re-audit shows this is already shipped as `:deploy --from PATH --no-deploy` (the `--no-deploy` flag runs `CreateStorageLocation → S3 upload → CreateApplicationVersion` but skips `UpdateEnvironment` — identical behaviour to the proposed dry-run). Renaming the flag would be a cosmetic improvement at best; not worth the churn. Operators who want the dry-run semantic already have it.
- [x] **Pre-deploy snapshot + auto-rollback safety net** — Done (2026-05-25, commits `9392f25` + `8a877f2` + `204903c`). Every `:deploy` now captures the env's current `version_label` into `App.deploy_snapshots` (in-memory + persisted to `state.toml` as `deploy_snapshot.ENV = "label|RFC3339-ts"` lines so cross-session rollback still works). New `:deploy LABEL --auto-rollback Nm` flag arms a watchdog that fires once at deadline: Green-env disarm + status toast; non-Green env + valid snapshot triggers an audit-logged `Auto-rollback` redeploy back to `previous_version_label` (respects per-env / per-account read-only safety pins via `deny_write`). New `AppMsg::AutoRollbackCheck` + handler in `app/msg.rs`. `:rollback` prefers the snapshot when present, falls back to the existing event-scan for envs without a captured snapshot. +5 tests (Green-disarm / non-Green-dispatch / missing-snapshot-error / persistence round-trip / malformed-line rejection). **Scope notes**: only the version label is snapshotted (not full option-settings), so rolling back a config-only change isn't supported by this path — that'd need a second `DescribeConfigurationSettings` fetch + a more elaborate restore step, deferred to a future session. Watchdog fires once at the deadline (not periodically) — "disarm if Green at any point" would need a heavier polling loop.
- ~~**`:env-diff-time ENV TIMESTAMP`**~~ Withdrawn (2026-05-23). Re-audit: EB doesn't store historical option settings. `DescribeConfigurationSettings` only returns the *current* state. `ConfigurationDeployment` events record *that* a deployment happened, not *what* the settings were before/after. Genuine post-mortem-time config diff would require ebman to snapshot option settings on every `:deploy` / config change and persist them locally — that's a different feature ("pre-deploy snapshot + auto-rollback" below already proposes part of this). The proposed shape isn't implementable against EB's API surface as-is.

### Architecture — sibling-project crossover (2026-05-23)

Surfaced by a deliberate review of architecture + the sibling pgman repo (`~/git/pgman`, k9s-style Postgres TUI by the same author, same ratatui+crossterm+tokio stack, same CLAUDE.md mandatory-loop pattern). pgman has explicitly lifted `theme.rs` / `util.rs` / `font_probe.rs` / `splash.rs` from ebman as copy-paste — a shared crate would let fixes flow both ways. None of these are urgent; ebman is shippable as 0.7 without them.

- [x] **ebman bin → lib+bin refactor.** Done (2026-05-23). New `src/lib.rs` declares every `pub mod` + the `Tui` + `LogReloadHandle` type aliases that other modules need to reach. Splash code (446 lines + 14 frame consts + 6 tests) lifted out of `main.rs` into its own `src/splash.rs` module. `main.rs` is now a thin bin: argv parsing, TUI lifecycle (enter_tui / leave_tui / panic hook), the `draw_splash` renderer that calls into `ebman::splash`, the three subcommand handlers (envs / action / ctl), logging setup. `main.rs` imports the lib via `use ebman::{app::App, aws, config, control, font_probe, splash, util, LogReloadHandle, Tui}`. Cross-module references inside the lib continue to use `crate::*` which now resolves to the lib crate root (e.g. `crate::Tui` from app.rs still works). Test count preserved: 443 = 436 lib + 7 bin. Cargo.toml version bumped to `0.8.0-dev` to mark we're past 0.7. Unblocks the `tui-common` workspace item below.
- [x] **Two-crate workspace — `tui-common` shared with pgman.** Done (2026-05-23). Workspace scaffold + five migrations landed. Root `Cargo.toml` has `[workspace] members = ["tui-common"]` + `default-members = [".", "tui-common"]`; the `tui-common/` crate is `version = 0.1.0, publish = false` with minimal deps (crossterm + ratatui + tracing). Modules now shared (16 tests across them): **`font_probe`** (Powerline probe, 6 tests), **`overlay`** (`OverlaySize` + centred-rect helpers, 2 tests), **`util::parse_bool` + `util::write_atomic`** (2 tests), **`theme::IconStyle` + `theme::contrast_text_for`** (3 tests), **`splash::render_frame`** (pixel→`██` rendering loop with palette closure, 3 tests). All re-exported from ebman so existing call sites stay unchanged. Sibling pgman can path-depend on `tui-common = { path = "../ebman/tui-common" }` for local dev. **Stopped here on purpose** — further candidates (full `Theme` struct via BaseTheme trait, full command-registry, control socket) hit either massive call-site churn (~386 `theme.text` accesses in `ui.rs` alone would all need to become method calls), marginal payoff (~20 lines saved on the command-registry shape vs. the EB-specific category enum + command list), or speculative scope (pgman doesn't have a control socket yet). Trim-line set: the genuinely high-leverage shared bits are in `tui-common`; the rest stays per-app.
- [x] **Mode handler split.** Done (2026-05-24). The six inline `Mode::X => match key.code { … }` blocks in `handle_key` (Filter / Help / Command / Palette / QuickJump / Picker) are now `Mode::X => self.handle_X_key(key)` one-liners; the bodies live in a new `src/app/mode_keys.rs` (203 lines, follows the `cmd_*` split pattern). The dispatch site shrank from a wall-of-matches to seven aligned one-liners; the bigger modes (`Detail`, `Action`, `Dlq`, `Form`, `Shell`) already had their own `handle_*_key` methods and stay where they were. `app.rs` 16,394 → 16,211 lines.
- [~] **Replace hand-rolled TOML parsers in `config.rs` / `state.rs`.** Partial: `project.rs` migrated (2026-05-24) to `serde` + `toml` derive as a proof of concept (no prior users, smallest schema). `serde = { version = "1", features = ["derive"] }` + `toml = "0.8"` added to Cargo.toml. The hand-rolled `parse` is gone; `toml::from_str` does the work, with `#[serde(default)]` for forward-compat against new schema fields. Empty-string→None still preserved via a small `deserialize_non_empty` adapter. Tests went 6 → 8 (added invalid-TOML and `[runbooks]` table-syntax cases). **state.rs / config.rs deferred** — they have format-collision issues that need a real plan: in `state.rs`, `filter = "foo"` (scalar) collides with `filter.NAME = "..."` (named-filter table); in `config.rs`, the CSV-in-string fields (`extra_regions = "a,b,c"`, `required_tags`, `profile_themes`) aren't natural TOML lists. Migration would need either renamed keys (breaking for users) or a hand-rolled legacy fallback path that reads the old format and re-writes in the new one on first load. Worth doing but its own focused session.
- [~] **Integration test coverage.** Partial (2026-05-24). 5 new tests on top of the existing 7 demos cover the core text-input / multi-select / pin / picker workflows: `space_toggles_multi_select_and_esc_clears_it`, `filter_mode_text_input_and_backspace_round_trips`, `esc_in_filter_mode_clears_the_filter`, `star_toggles_pinned_set_for_selected_env`, `picker_workflow_open_filter_enter_dispatches_choice`. Coverage now 12 demo workflows. The async-spanning flows (open Detail → drill into instance → terminate; multi-region fan-out) are harder against the `AwsClient::stub()` harness because spawned tasks fail silently — those would need mocked-AWS at the integration layer. Flagged as the next-deeper-cut for a future session.
- [x] **Per-env / per-account read-only overrides.** Done (2026-05-23 + 2026-05-24 follow-on sweep). Config-toml `safety.envs.NAME.read_only = true` and `safety.accounts.NAME.read_only = true` parse + round-trip; lifted onto `App.safety_envs` / `App.safety_accounts`. `App.is_read_only_for(env_name)` resolves global → per-env → per-account-by-profile-name; `App.read_only_reason` differentiates the cause. Single-call ergonomic helper `App.deny_write(env_name, verb) → bool` sets the toast + returns the gate. Wired into ~20 destructive sites across `app.rs` + `app/cmd_*.rs` (lifecycle actions, deploy, config edits, DLQ resend/purge/replay, tags, delete-app-version, option-settings updates, alarm create/delete, config-template apply/save). The 4 batch-op sites in `cmd_write.rs` (`:batch-rebuild` / `:batch-restart` / `:batch-deploy` / `:batch-tag`+`:batch-untag` / `:batch-set-option`) stay on the global flag for now — a per-env enforcement would need to refuse-some-keep-others inside the dispatch loop, which is a deeper batch-ops refactor than the safety pin work. +3 tests.
- [x] **Project-local `.ebman/ebman.toml`** — Done (2026-05-23). New `src/project.rs` module walks up from cwd looking for a `.ebman/` directory, reads `ebman.toml` if found. Schema: `profile`, `region`, `application` (filter prefill), `filter`, and `[runbooks]` (dotted `runbooks.ENV = "url"` form, same as `~/.config/ebman/config.toml`). Profile / region win over persisted state so a repo pins its working context; runbook entries merge with the user-level map with project-wins-on-collision. Empty values are skipped so a stray `profile = ""` doesn't mask the user default. 6 tests cover parse / discovery / unknown-key tolerance / empty values. Wired into `App::new` after `state::load` and before `init_client`, so the resolved profile / region propagate to the AWS SDK setup. README documents the file under the config-files section. Commit-into-the-repo design (no credentials in the file).
- [x] **Lift the single-line text-input widget into `tui-common`** (proposed + done 2026-06-06, from pgman). `TextInput` lifted into **`tb-tui-common 0.1.2`** (published to crates.io; 8 unit tests + `From` impls). **pgman fully migrated** — local `src/text_input.rs` deleted, imports `tui_common::TextInput`. **ebman fully migrated** — all append-only inputs now use the shared widget (cursor-aware: Left/Right/Home/End, mid-string edit, Ctrl-W word-delete) via a shared `input_caret_spans` renderer: `filter`, `command_input` (completion cycle preserved), `quickjump`, `palette`, `picker.filter`, DLQ `purge_typed` + `replay_input`, Detail search (Events + Logs), `LogTail.filter_input`, `ConfirmModal.typed`. 6 new ebman tests + the 3 existing completion tests still green. Search/filter footers keep a caret-at-end glyph (left-to-right entry; editing works via handle_key). **Optional follow-up:** consolidate the already-cursor-aware `ConfigEdit` (its own `caret`/`split_at_caret` logic) onto `TextInput` — pure dedup, no behaviour change.

### Top priority — console-parity + peer-TUI polish (2026-05-21)

Surfaced by a critical console-vs-ebman + ebman-vs-peer-TUI comparison. Ranked by user-value-per-hour. The smaller ergonomics items in particular (autocompletion, did-you-mean, first-run hint) are the gap that makes ebman look unpolished next to k9s / lazygit — high impact, low cost.

- [x] **`:options` — full settable-option vocabulary with current values** — Done (task #113). Two-call merge of `DescribeConfigurationOptions` (vocab/metadata) + `DescribeConfigurationSettings` (current values) keyed on `(namespace, name)`. `▸` operator-set / `•` default; emits `value_type` / `change_severity` / range / enum-options when EB returns them. Optional `NAMESPACE` arg filters.
- [x] **`:` autocompletion against `commands::COMMANDS`** — Done (task #114). Tab cycles forward, Shift-Tab cycles back; origin fragment cached on first press so repeated cycling restores the prefix cleanly.
- [x] **"Did you mean?" on unknown commands** — Done (task #115). Levenshtein against `commands::all_names()`, threshold 2.
- [x] **First-run nudge** — Done (task #116). `state::file_exists()` gate sets `first_run_hint`; sticky footer row hints at `?` / `:` / `Ctrl-K` until first input.
- [x] **Resource topology as hierarchical text** — Done (task #117). Indented ASG → instances → ELB → TGs (Worker tier shows ASG → instances → queue). Pure `render_env_resources_tree`.
- [x] **`:explain` IAM diagnosis** — Done (task #118). `:explain` no-arg scrapes the last `AccessDenied:` toast; `:explain ARN ACTION` evaluates explicit pairs via `iam:SimulatePrincipalPolicy`. Surfaces SCP / permissions-boundary blockers when the simulator flags them.

**Secondary** (same review, smaller payoff or design call needed):

- [x] **Form-based edit for the long tail of namespaces** — Done (task #119, 0.6). The "top-3 namespaces still need forms" premise had drifted: by 0.6 nearly every config family already had a dedicated command/form — `:capacity` (ASG), `:rds-attach`, `:listener-edit`, `:env-edit` (env vars), `:logs-stream`, `:notify`, `:managed-window`, `:deployment-policy`, `:rolling-update`, `:health-check-url`, `:subnets`, `:keypair`, `:service-role`, … — and the genuine remainder (`proxy`, `healthreporting`) is 1–2 settings each, well served by `:set-option`. The one real multi-field gap was metric-based autoscaling: `:scaling-triggers` is now a 9-field modal form over `aws:autoscaling:trigger` (metric / statistic / unit / period / breach duration / lower+upper thresholds / scale increments), pre-filling the env's current trigger.
- [x] **Config tab in-place editor — key rename** — Done. `r` on the Config tab opens an in-place editor for the row's *key*; commit dispatches set-new + remove-old in one `UpdateOptionSettings` / `UpdateTags` call, carrying the value across. `ConfigEdit.is_new: bool` refactored to a `ConfigEditMode` enum (`Value` / `NewRow` / `RenameKey`). The Config-tab editor now has every section: cursor nav, value edit, add, delete, rename, scroll-follow.
- [x] **Per-tab help-density polish** — Done (task #120). The Detail footer key strip is now structured `(key, label)` pairs (`detail_tab_keys`) rather than a flat string; `render_detail_keystrip` renders keys bold + bright against muted labels, separated by a thin `·`, so each pair is scannable without extra width. Global keys (`tab` / `?` / `esc`) are appended uniformly by a shared `DETAIL_GLOBAL_KEYS` const, fixing the prior inconsistency where only some tabs advertised tab-cycling. A drift test asserts no tab lists a key twice. +3 tests.
- [ ] **Mouse: column resize via drag + right-click row menus** — PARTIAL: drag already exists for the events-panel divider (`input.rs` `drag_origin`), so the interaction pattern is proven; what's missing is table COLUMN resize and right-click menus. Wheel + click-to-select is the current floor. Operators coming from console expect drag + right-click. TBD whether this is worth the design cost for a primarily-keyboard tool.
- [x] **Per-env runbook hint** — Done (task #121, 0.6). Config-file map, not a CLI command, as floated: `runbooks.ENV = "https://…"` lines in `config.toml` parse into `Config.runbooks` / `App.runbooks` and round-trip through `serialize` (so `:settings` save preserves them). The `:why` triage overlay shows a bold `runbook  <url>` line at the top when the selected env has one. +2 tests (parse incl. blank-URL skip, serialize round-trip).

### Console-replacement gap — items between "useful" and "indispensable"

- [x] **`:deploy --from` multipart upload + streaming** — Done (2026-05-23). `put_application_bundle(Vec<u8>)` replaced by `upload_bundle(&Path)` / `upload_bundle_with(threshold, part_size)`. Bundles below `MULTIPART_THRESHOLD` (64 MiB) stream via `ByteStream::from_path` — no whole-file Vec<u8>; bundles at or above the threshold use multipart upload in `MULTIPART_PART_SIZE` (16 MiB) chunks via `CreateMultipartUpload → UploadPart×N → CompleteMultipartUpload`, with `AbortMultipartUpload` on any failure (open / read / upload-part / complete) so S3 doesn't accumulate orphaned parts. Peak RAM is one part regardless of bundle size; the 5 GiB single-PutObject ceiling is gone (10,000 × 16 MiB = 160 GiB headroom, well above S3's 5 TiB object cap). Pure helpers `should_multipart` + `plan_part_lengths` with 4 tests; mocked-AWS test `upload_bundle_uses_multipart_when_size_meets_threshold` exercises the three multipart calls end-to-end with a 17-byte tempfile + 8-byte parts. Existing single-PutObject coverage preserved via the `deploy_from_path_chain_dispatches_each_stage` test (now uses a tempfile through the new API).

### Proposed (review 2026-05-22, post-0.4.1)

Ideas surfaced after the 0.4.x console-parity + config-editor work. Ranked by operator value. The config-editor *key rename* slice is tracked separately (the `[~]` item below); per-tab help-density is task #120.

- [x] **`:rollback`** — Done. Redeploys the env's previously-deployed version label. `cmd_rollback` fetches the env's recent events; `Event` gained a `version_label` field (populated from `EventDescription.version_label` — more robust than message-parsing); pure `previous_version_label` scans newest-first for the first label ≠ current. Opens the standard deploy confirm modal, so the operator sees + confirms the target and the 5s undo window applies. read-only / generation / selection-moved guards. +1 test.
- [x] **Config change timeline** — Done as **`:changes`**. Fetches the env's `DescribeEvents` history and renders the deploy + configuration-change events as a newest-first timeline (with the version label per row); routine health/scaling noise is filtered out by the pure `is_config_event`.
- [x] **Env config compare / drift** — Done as **`:config-diff ENV`**. Fetches the selected env's + `ENV`'s configuration options in parallel and shows every operator-set option-setting that differs, grouped by namespace. Pure `diff_config_options` over the two `(namespace, name) → value` maps (`Some("")` and `None` normalised to "unset"). Auto-drift-flagging for grouped apps remains a possible follow-on.
- [x] **Stale-platform surfacing** — Done. `ListAvailableSolutionStacks` is fetched once per context by `spawn_solution_stacks` and folded into `App.latest_stacks` (family-key → newest version) via the pure `latest_stack_versions`. `Environment` gained a `solution_stack` field carrying the raw stack name; the pure `stack_family_version` splits a stack into `(family_key, version)` by stripping the `vX.Y.Z` token, and `newer_stack_version` flags an env when a strictly-newer version exists in the same family. The envs-table PLATFORM cell recolours amber + appends an `↑` glyph when stale; the Detail Health tab shows `↑ vX.Y.Z available`. ARN-only / custom-platform envs (no solution stack) are never flagged. +4 tests.
- [x] **Worker DLQ time-windowed replay** — Done (task #141, 0.6). `R` in the DLQ viewer opens a replay prompt; the spec accepts `all`, a count (`20`), or a window (`1h` / `24h` / `7d`). Pure `parse_replay_spec` + `select_replay_indices` (oldest-first, undated messages excluded from a window) live in `mode_dlq.rs`; `spawn_dlq_replay_batch` sends each message to the main queue then deletes it from the DLQ, counting partial failures, and the result (`DlqOp::Replayed`) triggers a refetch. Scope note: "all" means all *currently-loaded* (peeked) messages — SQS has no cheap full-queue enumeration, so a deep DLQ replays a page at a time. +5 tests.

- [x] **Cost-Explorer integration** — Done (2026-05-21). `:cost on` adds a COST column with bucketed colours; opt-in + 24h cache at `~/.cache/ebman/cost-{account}-{region}.toml`. Real-account verification still TODO since I can only test the SDK request shape against the docs.

### Refactors — structural cleanup remaining

- [x] **Split `src/app.rs` `execute_command` by category** — Done (task #66). Ten sub-modules under `src/app/` (cmd_action / cmd_alarms / cmd_config_template / cmd_misc / cmd_nav / cmd_option / cmd_overlay / cmd_settings / cmd_view / cmd_write) total 2,160 lines; `app.rs` 14,277 → 12,478 (-1,799). Dispatch site is now pure one-liner routing. `app.rs` is still ~12.5k lines because the bulk is `App` state + `AppMsg` handlers + `spawn_*` helpers — splitting *those* would need a different cut (e.g. `app/handlers.rs`, `app/spawn.rs`) and is a separate, larger task to scope. **Followed through 2026-08-21** — see "Architecture + code quality" in Done: `app.rs` is now 2,981 lines across 34 `src/app/` modules.

### `app.rs` decomposition — code-review 2026-05-22

`src/app.rs` has grown to ~16.9k lines: `App` is ~95 fields, `AppMsg` ~46 variants, `handle_msg` is one ~1,140-line function, with 51 `spawn_*` helpers and 39 hand-copied `generation` checks. The patterns are sound but applied by copy-paste convention rather than enforced structurally. Five cuts, ranked by value/risk:

- [x] **Centralize the generation check** — Done (task #135). `AppMsg::generation() -> Option<u64>` returns the context generation a message carries (`None` for the context-independent `Rebuild` / `UpdateCheck`); `handle_msg` checks it once up front and drops superseded messages. The ~39 per-handler `if gen != self.generation { return; }` guards are gone, and `apply_detail_msg` lost its now-redundant `gen` parameter. The stale-result house rule is now a structural invariant a new variant can't forget. Session-id checks (`log_tail_session`) stay per-handler. +2 tests.
- [x] **Split `handle_msg`** — Done (task #136). The ~1,140-line `match` moved to `src/app/msg.rs`: `handle_msg` is now a thin router delegating each variant to a dedicated `handle_*` method, same cut as the `cmd_*` split. `app.rs` dropped from 16,932 → 15,846 lines; `msg.rs` is 1,315.
- [x] **Generic `spawn` helper** — Done (task #137). `App::spawn_aws(op_name, op, into_msg)` clones `aws`/`tx`/`gen`, runs `op` against the client off the UI thread, flattens any `eyre::Report` to a tagged string, and feeds `(gen, Result<T,String>)` to `into_msg` to build the `AppMsg`. 23 single-call `spawn_*` helpers were collapsed onto it (≈−150 lines). Multi-call fan-outs (`spawn_worker_queue_check`, `spawn_app_latest_versions`), pipelines (`spawn_logs_tail`, `spawn_detail_logs`, `spawn_detail_metrics`'s `join!`) and non-AWS spawns (`spawn_update_check`) stay bespoke as intended.
- [x] **Group `App` fields into sub-structs** — Done (tasks #138, #139; extended 2026-08-21 with `ViewState`). Three cohesive clusters lifted off `App` (16 fields → 3 nested structs): `CompletionState` (Tab-completion cycle: `origin` + `index`), `HelpState` (`scroll` / `max_scroll` / `topic` / `pre_mode` / `pre_overlay`), and `EventPanel` (`events` / `visible` / `time_format` / `for_env` / `scroll` / `area` / `drag_origin` / `cursor` / `height`). ~110 call sites updated across `app.rs` / `app/msg.rs` / `ui.rs`, all compiler-verified. The `EventPanel` field is named `event_panel` (not `events_panel`) so the bare `self.events` rename doesn't prefix-collide the suffixed fields; the few multi-line `self`\n`.field` accesses the literal `replace_all` missed were caught by the build and fixed by hand.
- **`AppMsg` shape consolidation — declined.** ~13 variants share `{ gen, env_name, result: Result<T,String> }`; genericising just relocates the enum and hurts grep-ability. The duplication that hurts was in the handlers, addressed by the two items above. Not a checkbox — a recorded decision, not pending work.

### UX punch list — drive-the-app review (2026-05-19)

Findings from walking through the surface as a daily operator. Ranked by likelihood of biting a real user. Cross-referenced with file:line so the next session can pick targets without re-discovering them.

### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. ~~Blocker: TIER column is `Constraint::Length(7)`~~ — STALE: TIER is `Length(11)` now and `pill_chain` already renders wedge-capped pills in the header, so the machinery exists and the width objection is gone. What remains is the design call about applying it per-row. Old note: and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.

### Tier 0 — distribution & hygiene
- [x] **Demo PROJECT IRONWOOD lore pass** — Done (2026-06-04). Rebranded the `--demo` fixture from `ledgerly` → `poly` (the Polymorphism company/AWS-profile from the sibling website ARG, `~/git/web/docs/ironwood.md`). Added an `ironwood` env in Grey/health-unknown state running `build-1420` (the 1420 MHz hydrogen line) — absent from the deploy history + cost/instance-count maps, so INST and COST render a muted `—` ("Beanstalk can't account for it"). Rewrote `canned_ssm_session` so the SSM pane resolves `hostname` → `edge-lhr-03`, `uptime` → 642 days (the 642 ly star-fix), and `who` reveals a second session: `ironwood` from `127.0.0.1` logged in since `1977-08-15 22:16` (the "Wow! signal" stamp). All canon facts echoed verbatim per `ironwood.md` §4; tone kept clinical (no narration). `demo.tape` updated to clear the `/staging` filter at the end so the Grey `ironwood` row is in the closing frame. New tests: `fixture_has_ironwood_health_unknown_easter_egg`, `canned_ssm_session_carries_the_ironwood_tell`, + Grey assertion in the health-tier test. **Follow-ups (now done 2026-06-04):** `demo.gif` re-rendered (`vhs demo.tape`); `ironwood` moved to a distinct platform (Go 1.4 / retired AL2018.03 vs the Node.js 20 fleet — canon tie: the relay is `relay.go`); cross-project cameo recorded in `~/git/web/docs/ironwood.md` §12. Rendering the gif also surfaced + fixed a real bug: `/` (open filter mode) cleared `self.filter` without `rebuild_view()`, leaving a stale filtered view — fixed in `app.rs` with regression test `slash_with_active_filter_rebuilds_to_full_fleet`.
- [x] **`:diff --ignore-keys "k1,k2"`** — Done (2026-06-04). The env-metadata `:diff` now accepts the same `--ignore-keys` flag `:config-diff` had, suppressing matching rows (field labels `name`/`application`/`tier`/`status`/`health`/`platform`/`version`/`cname`/`updated`, case-insensitive; `version_label` also matches `version`). Pure helper `diff_field_ignored` + tests (`diff_field_ignored_matches_label_and_version_label_alias`, `diff_envs_drops_ignored_rows`); existing `diff_envs` tests + dispatch tests updated for the new signature. Registry + `docs/commands.md` updated.
- [x] **ARM64 Linux tarball in release matrix** — Done (2026-06-04, pending CI verification). Added `aarch64-unknown-linux-gnu` on a native `ubuntu-24.04-arm` runner to `.github/workflows/release.yml` (native build, no cross-compile), an `OS.linux? && Hardware::CPU.arm?` branch to `Formula/ebman.rb` (placeholder sha, filled at release time), and a fourth `fetch_sha` + awk rewrite rule to `scripts/update-formula.sh`. **Unverified locally** — can't build aarch64-linux on this macOS host; needs a real tag push to confirm the runner + build + tarball attach. Formula's ARM-linux sha is a placeholder until the next release runs `update-formula.sh`.
- [x] **README screenshots / demo gif** — Done (2026-05-25, shipped in 0.8.1 as `demo.gif`). 25s VHS recording of the triage workflow (`/staging` filter → `:why` overlay → drill into Detail → `s` for SSM session) captured against `ebman --demo` (synthetic fleet, no AWS calls). Lives at repo root + wired into the README hero slot under the badges. `demo.tape` carries the VHS script so future regens are one `vhs demo.tape` away. See the `--demo` mode entry in CHANGELOG.md (0.8.1) for the spawn-site gates that back the recording.
- [x] **`cargo install ebman` smoke test** — Done (2026-05-24). 0.7.0 published to crates.io via the manual `gh workflow run release.yml -f tag=v0.7.0` fire after the `CARGO_REGISTRY_TOKEN` secret was added. Workflow logs confirm `Uploaded ebman v0.7.0 to registry crates-io` + `Published ebman v0.7.0 at registry crates-io`. `cargo install ebman` resolves against the registry. The automated `crates_io` job in `release.yml` keeps future tags in sync without the manual fire.
- [x] **Homebrew formula / GitHub Releases with binaries** — Done (2026-05-24). Three per-target tarballs (aarch64-darwin / x86_64-darwin / x86_64-linux) attached to the GH Release for v0.7.0 by the matrix in `release.yml`. `tombaldwin/homebrew-tap` already existed (stuck at 0.3.5 since the tap was first set up — 0.4.x / 0.5.x / 0.6.x never made it across); bumped to 0.7.0 with the three real SHA-256s. End-to-end verified: `brew tap tombaldwin/tap && brew install ebman` resolves, installs `/opt/homebrew/bin/ebman`, `ebman --version` reports `0.7.0`. New `scripts/update-formula.sh vX.Y.Z` automates future bumps — downloads the release tarballs via `gh`, computes SHA-256s, rewrites both `Formula/ebman.rb` files (this repo + sibling tap clone) idempotently. Bash-3.2-safe (macOS default). Stale "(until tap is published)" comments removed from both formula headers.
- [x] **Backfill crates.io 0.3.5 → 0.5.0 gap (or decide not to)** — Decided (2026-05-23): accept the gap. 0.5.0 was published to crates.io manually so the in-app update-check reports current; 0.4.0 / 0.4.1 tags exist on GitHub Releases regardless. Retro-publishing those tags would mean checking out old refs and running `cargo publish` against them, with no operational benefit (nobody is upgrading 0.3.5 → 0.4.0 anymore; the path is 0.3.5 → latest). The automated workflow below prevents recurrence going forward.
- [x] **Automate `cargo publish` in the release workflow** — Done (2026-05-23). New `crates_io` job in `release.yml` runs after the build matrix passes, gated on a `CARGO_REGISTRY_TOKEN` secret (skipped on forks via the `repository.fork` guard, and skipped at runtime if the secret is unset so scratch tags still produce GitHub artefacts). Runs `cargo publish --locked` so the resolved dependency graph matches the build matrix's lockfile pinning.

### Tier 1 — operator killer features (the daily-driver gap)
All previously listed Tier 1 items are now shipped:
- Option settings editor — `:env`, `:set-option`, plus per-namespace commands (`:capacity`, `:instance-type`, `:keypair`, `:public-ip`, `:elb-scheme`, `:service-role`, `:instance-profile`, `:deployment-policy`, `:rolling-update`, `:health-check-url`, `:logs-stream`, `:notify`, `:managed-window`).
- CloudWatch Logs streaming — `:logs-tail` overlay with regex filter + auto-tail.
- Deploy from local path / S3 — `:deploy --from PATH` and `:deploy --from s3://bucket/key`.

### Console parity — write-side gaps (operators currently open the console for these)

Gaps surfaced during the 2026-05-19 console-vs-ebman comparison. Each entry is a console feature with no ebman equivalent. Ordered by daily-operator frequency.

- [x] **Attach / detach RDS database** — Done (tasks #109 + #110, 0.6). `:rds` (2026-05-21) reads the env's `aws:rds:dbinstance.*` option settings (DBPassword redacted). `:rds-attach` is a 7-field modal form (engine / class / storage / master user+password / deletion policy / Multi-AZ) over `aws:rds:dbinstance`, pre-filling if a DB is already attached. `:rds-detach ENV` "safe-ifies" the coupled DB — sets `DBDeletionPolicy=Snapshot` so it survives env termination, behind a typed-name confirm (the `ENV` arg must repeat the env name). **Scope reality:** Elastic Beanstalk has *no* detach operation — an EB-created RDS instance lives in the env's CloudFormation stack and true decoupling needs an env rebuild; `:rds-detach` makes the data safe to keep, it doesn't move it (command help + toast say so). The separate immediate `rds:CreateDBSnapshot` from the original sketch was dropped: it needs DB-instance-id discovery via CloudFormation stack introspection plus an `aws-sdk-rds` dependency, neither verifiable here — and `DBDeletionPolicy=Snapshot` already guarantees a termination-time snapshot. Could be revisited if a point-in-time backup *before* termination is wanted.
- [x] **ALB listener + TLS cert config** — Done (tasks #108 + #111, 0.6). `:listeners` (2026-05-21) reads the env's `aws:elbv2:listener:*` namespaces grouped by port. `:listener-edit PORT` is a modal cert-rotation form: a single MultiSelect field whose options are the region's ISSUED ACM certificates (loaded live via a new `aws-sdk-acm` dependency + `acm:ListCertificates`), pre-selected with the listener's current `SSLCertificateArns`; submit writes the new cert set to `aws:elbv2:listener:<PORT>` through the option-settings path. Scope notes: delivered as a command (`:listener-edit 443`), not a Detail "LB tab" — a whole new tab was disproportionate to the feature. Protocol / SSLPolicy / ListenerEnabled / rules stay on `:set-option`; the form is scoped to cert rotation, the dominant edit. The ACM call shape is unverified against a live account (the SDK compiles against it).
- [x] **Capacity profile beyond min/max + instance type** — Done. `:capacity` modal form (MinSize / MaxSize / InstanceType / Cooldown) shipped in 0.3.0; `a → Capacity` menu entry shipped in 0.3.1. Multi-instance-type / spot-base / scheduled-scaling fleets still missing but those are niche enough to drop from this list — operators using them are mostly EB CLI / Terraform users.
- [ ] **Custom platforms — create** — delete shipped as `:custom-platform-delete <arn>`. Create still missing: console offers a wizard that builds a new custom AMI from a Packer template (slow — minutes — needs polling); ours would be `:custom-platform-create <packer-config>` via `elasticbeanstalk:CreatePlatformVersion`. Niche but a real gap for operators who maintain in-house base AMIs.

### Tier 4 — multi-account / child accounts

### Tier 6 — power-user / scripting
- [ ] **Embedded recorder** — record + replay sessions to `.cast` (asciinema). Deferred — needs its own input-capture + replay infrastructure.

### Tier 8 — maybe / unprioritised
- [ ] **Snapshot at a point in time** — "what envs looked like 1h ago" (would need local history).
- [ ] **Visual resource topology graph** — console shows a "Resources" graph linking ASG → EC2 instances → ELB → target groups. We have `:resources` as a text dump which most operators prefer; the graph is nice-to-have but rarely the reason someone opens the console.
- [ ] **Route 53 / custom DNS integration** — console offers a one-click "set up custom domain" wizard tied to a Route 53 hosted zone. Niche and easy to do via AWS CLI or the Route 53 console directly.

### Trim candidates — built, but probably over-served
Honest list of features that landed during expansion sprints but aren't earning their maintenance cost. Don't remove unilaterally; flag for review.

- ~~**Webhook on Red transition**~~ — Trimmed (2026-05-23). The `webhook_url` config option, the `:settings` form field, the `fire_webhook` `curl` shell-out, and the `build_webhook_payload` JSON encoder are all gone. Red-transition events now emit a `tracing::warn!` with structured fields (env / application / health / region) and write a `stage=event kind=red_transition env=… application=… health=…` line to the audit log at `~/.cache/ebman/audit.log` — operators can tail that file and pipe to whatever notifier they want (Slack, PagerDuty, pages, whatever). README documents the audit-log path under the `notify_bell = …` section. Net: −2 webhook tests, +0 (the tracing/audit emission is well-covered by the audit-log path already).
- ~~**Custom keybindings (`keys.toml`)**~~ — Trimmed (2026-05-23). `src/keys.rs` deleted; `mod keys`, `App.custom_keys`, `lookup_custom_key`, and its dispatch site in `handle_event` all gone. README's `keys.toml` config example and storage-list entry removed; feature-bullet's "custom keybindings" mention dropped. Need is served by `Ctrl-K` palette + per-context hints. Net: −4 tests (the keys-parse tests went with the module).
- **Multi-region overview / org-wide health / cross-account search** — useful in theory; most teams operate in one account+region day-to-day. The `aws::list_environments_in_region` fan-out helper is the real win, retain that.
- ~~**Embedded mini-map (`:minimap`)**~~ — Trimmed (2026-05-23). `App.show_minimap` field, `:minimap` command arm + commands-registry entry, and the `draw_minimap` renderer (50 lines) all removed. README entry dropped. Cute corner overlay with no operational signal beyond what the main table already shows.
- **Asciinema recorder (deferred in BACKLOG)** — keep deferred; standalone replay infrastructure is its own product.

---

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
