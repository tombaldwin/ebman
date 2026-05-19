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

### Console-replacement gap — items between "useful" and "indispensable"

The three things still keeping users in the AWS console day-to-day. Highest-leverage backlog block; ordered by impact.

- [ ] **Multi-select pickers for subnets / security-groups** — `:set-option` works as an escape hatch but the operator has to know IDs and namespace strings. A multi-select picker (similar to the existing region picker but allowing space-toggle) would close the remaining Network + Security gaps. Modal-form abstraction is in; needs a new `FieldKind::MultiSelect`.
- [ ] **Auto-open the streaming overlay on Logs-tab navigation** — discovery now runs on Detail open and the Logs-tab idle hint reflects whether CW Logs are configured. The remaining piece is auto-opening the streaming overlay (instead of showing the snapshot path) when groups are present. Operator preference signal — current behaviour is explicit (press `s`); auto-open is convenience.
- [ ] **`:deploy --from` multipart upload + streaming** — `:deploy --from PATH` and `:deploy --from s3://bucket/key` both shipped. Bundles >5 GiB still need multipart on the local-upload path; the s3:// path already sidesteps the limit. Whole bundle held in memory during upload is the other follow-on (stream from disk).

### Refactors — structural cleanup remaining

- [ ] **Split `src/app.rs` (6000+ lines, 60+ fields)** — `handle_key` is a flat dispatch across 10+ modes; the file is past the point where one branch can be changed confidently without reading the others. Extract per-mode handlers into their own modules (`mode_detail.rs`, `mode_dlq.rs`, `mode_action.rs`, …); action-flow state machine into `action.rs`; DLQ state machine into `dlq.rs`; persistence / `rebuild_view` into `view.rs`. Stop-conditioned in autonomous mode; needs a focused session.
- [ ] **Mocked-AWS test coverage** — currently only pure helpers are tested. Two real regressions this week were caught only by the user (DescribeConfigurationSettings returning empty WorkerQueueURL when EB autocreates the queue; peek_messages returning <max without long-polling and a loop). Adopt `aws-smithy-mocks` (or hand-rolled `ReplayingClient`) to assert on observed request shapes and exercise response variants. Foundation for confidently changing aws.rs without breaking silently.

### UX punch list — drive-the-app review (2026-05-19)

Findings from walking through the surface as a daily operator. Ranked by likelihood of biting a real user. Cross-referenced with file:line so the next session can pick targets without re-discovering them.

**Medium — discoverability / consistency**
- [ ] **`status_message` overwrites on rapid dispatch** — back-to-back `:tag` / `:delete-version` calls each clobber the previous footer message. Pending panel + toasts cover the gap now that those ops are tracked, but the footer-strip is still misleading mid-flight when N>1 ops are racing. Either drop status_message in favour of the pending chip + toasts, or queue messages and cycle them every ~1s. `src/app.rs` (multiple spawn helpers).

**Low — polish**
- [ ] **Powerline tab-icon glyphs (U+F048B …) are plane-1 codepoints** — Nerd-Font terminals render them width-1; some terminals or fallback fonts may render width-0 (no advance, overlap) or width-2 (double-width, misalignment). Tab strip will misalign silently. Add a startup probe (cell-width sanity check) or a config-time warning encouraging the user to verify in their terminal. `src/ui.rs:tab_icon`.

### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. Blocker: TIER column is `Constraint::Length(7)` and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.
- [ ] **Refresh-time format in header** — `21:46:27 (every 15s)` could shorten to a relative `12s ago · next 3s` (Grafana-style). Cheaper visual scan; signals when the next refresh is due.
- [ ] **Group-separator glyph in non-Powerline mode** — currently the per-app banner row is `─xi────` (dashes in app colour). Could mix in a thin glyph like `─ ▶ ─` mid-banner to break up the dash run.
- [ ] **Age-column colour tinting** — old envs (>30d since last update?) render `age` in muted; fresh ones in normal text. Visual recency without an explicit sort. Pure helper + 2 tests.

### Tier 0 — distribution & hygiene
- [ ] **README screenshots / demo gif** — text README shipped; capturing screenshots requires running the TUI in a real terminal (not this shell).
- [ ] **`cargo install ebman` smoke test** — verify the binary installs cleanly from a stock toolchain. (Local-only verification possible; full smoke test needs a crates.io publish.)
- [ ] **Homebrew formula / GitHub Releases with binaries** — macOS users won't `cargo install`. Depends on CI building release artefacts.

### Tier 1 — operator killer features (the daily-driver gap)
All previously listed Tier 1 items are now shipped:
- Option settings editor — `:env`, `:set-option`, plus per-namespace commands (`:capacity`, `:instance-type`, `:keypair`, `:public-ip`, `:elb-scheme`, `:service-role`, `:instance-profile`, `:deployment-policy`, `:rolling-update`, `:health-check-url`, `:logs-stream`, `:notify`, `:managed-window`).
- CloudWatch Logs streaming — `:logs-tail` overlay with regex filter + auto-tail.
- Deploy from local path / S3 — `:deploy --from PATH` and `:deploy --from s3://bucket/key`.

### Console parity — write-side gaps (operators currently open the console for these)

Gaps surfaced during the 2026-05-19 console-vs-ebman comparison. Each entry is a console feature with no ebman equivalent. Ordered by daily-operator frequency.

- [ ] **Attach / detach RDS database** — console exposes a Database tab where you can attach a new RDS instance to an env (creates the security-group + IAM linkage automatically) or detach an existing one. Needs `UpdateEnvironment(option_settings: aws:rds:dbinstance.*)` for create/attach, and a different code path for the post-creation "decouple" workflow (since EB-created RDS instances are pinned to the env's lifecycle by default).
- [ ] **ALB listener + TLS cert config** — list and edit ALB listeners (port, protocol, default action, attached cert) for the env's ALB. Adds an "LB" tab in Detail. Needs `aws:elbv2:listener.*` option settings + an ACM cert picker. Web-tier-only.
- [ ] **Capacity profile beyond min/max + instance type** — `:scale N` sets min=max; `:instance-type TYPE` sets the launch-config InstanceType. Console exposes a full Capacity tab with fleet composition (multi-instance-type list, on-demand base + spot %), scaling triggers (custom CW metric / threshold / cooldown), and scheduled scaling actions. New `:capacity` command opening a modal-form (depends on the option-settings editor abstraction from Tier 1) — or per-option commands like `:trigger metric LATENCY threshold 0.5`.
- [ ] **Network / VPC editor (subnet picker)** — common cases (`:public-ip`, `:elb-scheme`) shipped as per-option commands. Subnet selection (EC2 subnets + ELB subnets) is the remaining gap — needs a multi-select picker since envs typically use 2-3 subnets across AZs. Plain `:set-option aws:ec2:vpc Subnets "subnet-a,subnet-b"` works as an escape hatch today.
- [ ] **Security tab — EC2 security groups picker** — service role, instance profile, and key pair shipped as per-option commands (`:service-role`, `:instance-profile`, `:keypair`). Security-group list is the remaining gap — similar multi-select shape as the subnet picker. `:set-option aws:autoscaling:launchconfiguration SecurityGroups "sg-a,sg-b"` works today.
- [ ] **Custom platforms — create** — delete shipped as `:custom-platform-delete <arn>`. Create still missing: console offers a wizard that builds a new custom AMI from a Packer template (slow — minutes — needs polling); ours would be `:custom-platform-create <packer-config>` via `elasticbeanstalk:CreatePlatformVersion`. Niche but a real gap for operators who maintain in-house base AMIs.

### Tier 4 — multi-account / org
- [ ] **Account switcher with sts:AssumeRole** — the current `:account NAME` is a `:profile NAME` alias. A proper assume-role flow needs an `[accounts]` config schema in `config.toml` with role ARNs, an AssumeRole call, and credentials injection into the SDK. Deferred.

### Tier 6 — power-user / scripting
- [ ] **Embedded recorder** — record + replay sessions to `.cast` (asciinema). Deferred — needs its own input-capture + replay infrastructure.

### Tier 8 — maybe / unprioritised
- [ ] **Snapshot at a point in time** — "what envs looked like 1h ago" (would need local history).
- [ ] **Visual resource topology graph** — console shows a "Resources" graph linking ASG → EC2 instances → ELB → target groups. We have `:resources` as a text dump which most operators prefer; the graph is nice-to-have but rarely the reason someone opens the console.
- [ ] **Route 53 / custom DNS integration** — console offers a one-click "set up custom domain" wizard tied to a Route 53 hosted zone. Niche and easy to do via AWS CLI or the Route 53 console directly.

### Trim candidates — built, but probably over-served
Honest list of features that landed during expansion sprints but aren't earning their maintenance cost. Don't remove unilaterally; flag for review.

- **Webhook on Red transition** — single-URL POST is too rigid for real ops workflows. A proper Slack block-kit integration (with env context, action buttons) would replace it; otherwise consider trimming back to a `tracing::warn!` and a status toast.
- **Custom keybindings (`keys.toml`)** — supports F1-F12 and uppercase aliases to `:commands`. Almost no one will write this file; the underlying need (palette + per-context hints) is served by `Ctrl-K`.
- **Multi-region overview / org-wide health / cross-account search** — useful in theory; most teams operate in one account+region day-to-day. The `aws::list_environments_in_region` fan-out helper is the real win, retain that.
- **Embedded mini-map (`:minimap`)** — cute corner overlay; no operational signal beyond what the main table already shows.
- **Asciinema recorder (deferred in BACKLOG)** — keep deferred; standalone replay infrastructure is its own product.

---

## Skipped — needs retry

Populated by autonomous runs per `CLAUDE.md` stop-conditions. Each entry: one-line reason. Drop the entry once retried (successfully or with the user's deliberate decision to defer further).

- **README screenshots / demo gif** — autonomous shell has no real TTY; can't render the TUI for capture. Retry from an interactive session.
- **Option settings editor (Tier 1)** — requires a modal text-input form generator and a category-tree of namespaces; defer.
- **Split `src/app.rs`** — refactor spans 10+ mode handlers + state machines, exceeds the CLAUDE.md "touches > 3 modules" stop condition. Pick up in a focused session.
- **Embedded asciinema recorder (Tier 6)** — needs its own input-capture/replay infrastructure; defer.
- **`sts:AssumeRole`-based account switcher (Tier 4)** — needs an `[accounts]` config schema with role ARNs + credentials injection; defer. The current `:account NAME` aliases `:profile NAME` for the standard one-profile-per-account pattern.

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
