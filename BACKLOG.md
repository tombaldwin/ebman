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
- `--version` / `-V` and `--help` / `-h` flags (exit before TUI)
- MSRV declared (`rust-version = "1.82"`)
- README with feature summary, keymap, config, and "what's stored locally" section
- GitHub Actions CI: build + test on macOS and Linux, `cargo fmt --check`, `cargo clippy -D warnings`, MSRV gate on 1.82
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
- ~~**OSC 8 terminal hyperlinks**~~ Withdrawn (2026-05-26, verified in 0.12). Re-attempted with an actual experiment in `ui::tests::osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation` — ratatui 0.29's `Buffer::set_stringn` path treats each byte of an OSC 8 escape sequence as a 1-cell-wide printing character, so a 24-byte opener consumes 24 cells of layout space and pushes the visible text past the buffer width. The regression test pins the broken behavior so a future ratatui upgrade that adds zero-width control handling will fail it and prompt us to revisit. Shipping today would require a custom widget that bypasses the diff renderer per-line — too invasive for the value when modern terminals (iTerm, etc.) already auto-detect URLs in pasted output, which the existing `y`-to-yank flow already produces.

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
- ~~**OSC 8 terminal hyperlinks**~~ — Re-attempted with an actual experiment (vs the 0.11 assumption-based skip). Verified that ratatui 0.29 splits each escape byte into its own 1-cell-wide printing cell — a 24-byte OSC 8 opener eats 24 cells of layout space, pushing visible text past the buffer width. Regression test at `ui::tests::osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation` pins the broken behavior; a future ratatui that adds zero-width control handling will fail the test and prompt us to revisit.

#### Skipped on purpose — held for 0.13
- **Cross-region rollout (`:rollout LABEL --regions r1,r2,r3 [--auto-rollback Nm]`)** — Held (2026-05-26). Real value but needs careful design: same-name vs explicit-mapping env discovery across regions, sequential vs parallel dispatch, partial-failure handling (region 1 ok, region 2 listing failed), per-region AwsClient construction, audit-log shape. Multiple reasonable shapes; warrants a dedicated session rather than tail-end of an autonomous run.

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
- [ ] **Mouse: column resize via drag + right-click row menus** — Wheel + click-to-select is the current floor. Operators coming from console expect drag + right-click. TBD whether this is worth the design cost for a primarily-keyboard tool.
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

- [x] **Split `src/app.rs` `execute_command` by category** — Done (task #66). Ten sub-modules under `src/app/` (cmd_action / cmd_alarms / cmd_config_template / cmd_misc / cmd_nav / cmd_option / cmd_overlay / cmd_settings / cmd_view / cmd_write) total 2,160 lines; `app.rs` 14,277 → 12,478 (-1,799). Dispatch site is now pure one-liner routing. `app.rs` is still ~12.5k lines because the bulk is `App` state + `AppMsg` handlers + `spawn_*` helpers — splitting *those* would need a different cut (e.g. `app/handlers.rs`, `app/spawn.rs`) and is a separate, larger task to scope.

### `app.rs` decomposition — code-review 2026-05-22

`src/app.rs` has grown to ~16.9k lines: `App` is ~95 fields, `AppMsg` ~46 variants, `handle_msg` is one ~1,140-line function, with 51 `spawn_*` helpers and 39 hand-copied `generation` checks. The patterns are sound but applied by copy-paste convention rather than enforced structurally. Five cuts, ranked by value/risk:

- [x] **Centralize the generation check** — Done (task #135). `AppMsg::generation() -> Option<u64>` returns the context generation a message carries (`None` for the context-independent `Rebuild` / `UpdateCheck`); `handle_msg` checks it once up front and drops superseded messages. The ~39 per-handler `if gen != self.generation { return; }` guards are gone, and `apply_detail_msg` lost its now-redundant `gen` parameter. The stale-result house rule is now a structural invariant a new variant can't forget. Session-id checks (`log_tail_session`) stay per-handler. +2 tests.
- [x] **Split `handle_msg`** — Done (task #136). The ~1,140-line `match` moved to `src/app/msg.rs`: `handle_msg` is now a thin router delegating each variant to a dedicated `handle_*` method, same cut as the `cmd_*` split. `app.rs` dropped from 16,932 → 15,846 lines; `msg.rs` is 1,315.
- [x] **Generic `spawn` helper** — Done (task #137). `App::spawn_aws(op_name, op, into_msg)` clones `aws`/`tx`/`gen`, runs `op` against the client off the UI thread, flattens any `eyre::Report` to a tagged string, and feeds `(gen, Result<T,String>)` to `into_msg` to build the `AppMsg`. 23 single-call `spawn_*` helpers were collapsed onto it (≈−150 lines). Multi-call fan-outs (`spawn_worker_queue_check`, `spawn_app_latest_versions`), pipelines (`spawn_logs_tail`, `spawn_detail_logs`, `spawn_detail_metrics`'s `join!`) and non-AWS spawns (`spawn_update_check`) stay bespoke as intended.
- [x] **Group `App` fields into sub-structs** — Done (tasks #138, #139). Three cohesive clusters lifted off `App` (16 fields → 3 nested structs): `CompletionState` (Tab-completion cycle: `origin` + `index`), `HelpState` (`scroll` / `max_scroll` / `topic` / `pre_mode` / `pre_overlay`), and `EventPanel` (`events` / `visible` / `time_format` / `for_env` / `scroll` / `area` / `drag_origin` / `cursor` / `height`). ~110 call sites updated across `app.rs` / `app/msg.rs` / `ui.rs`, all compiler-verified. The `EventPanel` field is named `event_panel` (not `events_panel`) so the bare `self.events` rename doesn't prefix-collide the suffixed fields; the few multi-line `self`\n`.field` accesses the literal `replace_all` missed were caught by the build and fixed by hand.
- **`AppMsg` shape consolidation — declined.** ~13 variants share `{ gen, env_name, result: Result<T,String> }`; genericising just relocates the enum and hurts grep-ability. The duplication that hurts was in the handlers, addressed by the two items above. Not a checkbox — a recorded decision, not pending work.

### UX punch list — drive-the-app review (2026-05-19)

Findings from walking through the surface as a daily operator. Ranked by likelihood of biting a real user. Cross-referenced with file:line so the next session can pick targets without re-discovering them.

### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. Blocker: TIER column is `Constraint::Length(7)` and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.

### Tier 0 — distribution & hygiene
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

- **README screenshots / demo gif** — autonomous shell has no real TTY; can't render the TUI for capture. Retry from an interactive session (operator captures stills + gif; I edit the supporting copy).
- **Embedded asciinema recorder (Tier 6)** — needs its own input-capture/replay infrastructure; defer.

**Retried successfully** (kept here briefly so the history's discoverable):

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
