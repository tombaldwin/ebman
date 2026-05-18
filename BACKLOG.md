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

### Remote control plane
- **`--control-socket PATH`** — when set, ebman opens a Unix socket at PATH with 0600 perms and accepts one-shot requests: `SCREEN` (plain-text dump of the current frame from the ratatui back-buffer), `KEY <spec>` (synthesised key event injected via `handle_event`; spec supports Ctrl/Shift/Alt + arrows / Enter / F1-12 / single chars / `Char(x)`), `CMD <text>` (runs a `:` command), `STATE` (flat JSON with mode / profile / region / account / envs / selected / load / sort / grouped / redact / focus).
- **`ebman ctl <op>` subcommand** — one-shot client; defaults to `~/.cache/ebman/control.sock`; override with `--socket PATH`. Examples: `ebman ctl screen`, `ebman ctl key Down`, `ebman ctl key Ctrl+R`, `ebman ctl cmd ":region eu-west-2"`, `ebman ctl state`.

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

### Refactors — structural cleanup remaining

- [ ] **Split `src/app.rs` (4400+ lines, ~50 fields)** — `handle_key` is a flat dispatch across 10+ modes; the file is past the point where one branch can be changed confidently without reading the others. Extract per-mode handlers into their own modules (`mode_detail.rs`, `mode_dlq.rs`, `mode_action.rs`, …); action-flow state machine into `action.rs`; DLQ state machine into `dlq.rs`; persistence / `rebuild_view` into `view.rs`.

### Tier 0 — distribution & hygiene
- [ ] **README screenshots / demo gif** — text README shipped; capturing screenshots requires running the TUI in a real terminal (not this shell).
- [ ] **`cargo install ebman` smoke test** — verify the binary installs cleanly from a stock toolchain. (Local-only verification possible; full smoke test needs a crates.io publish.)
- [ ] **Homebrew formula / GitHub Releases with binaries** — macOS users won't `cargo install`. Depends on CI building release artefacts.

### Tier 1 — operator killer features (the daily-driver gap)
- [ ] **Option settings editor** — modal text input for the most-edited namespaces (`aws:elasticbeanstalk:application:environment` for env vars; `aws:autoscaling:asg` for min/max; instance type / proxy). Pre-fill current values; submit via `UpdateEnvironment(option_settings)`. *Deferred*: needs a generic modal-form generator; substantial enough to deserve a dedicated session.

### Tier 4 — multi-account / org
- [ ] **Account switcher with sts:AssumeRole** — the current `:account NAME` is a `:profile NAME` alias. A proper assume-role flow needs an `[accounts]` config schema in `config.toml` with role ARNs, an AssumeRole call, and credentials injection into the SDK. Deferred.

### Tier 6 — power-user / scripting
- [ ] **Embedded recorder** — record + replay sessions to `.cast` (asciinema). Deferred — needs its own input-capture + replay infrastructure.

### Tier 8 — maybe / unprioritised
- [ ] **Snapshot at a point in time** — "what envs looked like 1h ago" (would need local history).

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
