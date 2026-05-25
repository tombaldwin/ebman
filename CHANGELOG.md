# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`notify_webhook` outbound integration.** New opt-in `config.toml` setting `notify_webhook = "https://..."` (or `""` to disable) arms a fire-and-forget POST on every audit-log line. Body is Slack-incoming-webhook-shaped — a top-level `text` field renders the audit line for human-readable Slack channels, with structured sibling keys (`at`, `account`, `profile`, `region`, `detail`) for routing / filtering by other consumers. Shells out to `curl` (10s cap, same approach as `fetch_url_text`) so we don't add an HTTP-client dep. Webhook failures log via `tracing::warn!` but never alarm the operator — the local `audit.log` remains the source of truth.
- **EB CLI `.elasticbeanstalk/config.yml` reader.** New `eb_cli` module walks up from cwd looking for the EB CLI's project-config marker directory and parses `config.yml` for default `profile` / `default_region` / `application_name`. Most EB CLI users already maintain this file; reading it lets ebman pick up the working context without forcing a duplicate `.ebman/ebman.toml` entry. Precedence: `.ebman/ebman.toml` > `.elasticbeanstalk/config.yml` > persisted `state.toml`. `application_name` fills in as a soft filter prefill when `.ebman/` hasn't set one. Parse errors / null values / unknown keys all silently fall back to defaults — a corrupt EB CLI file can't refuse to launch ebman.
- **Pre-deploy preview inline in the confirm modal.** Every `:deploy LABEL` confirm modal now auto-fetches `list_application_versions` and renders the existing `format_deploy_preview` body (candidate label / age / description / rollback-warning when candidate is older than current) inside the modal — no separate `:deploy LABEL --preview` round-trip required. Loading placeholder while the fetch is in flight; failures inline as `version preview unavailable: <reason>` rather than leaving the slot blank.
- **`ebman action deploy --env X --version Y [--wait-for-green Nm] [--auto-rollback Nm]`** — non-interactive CLI parity with the typed-command `:deploy`. Polls EB every 5s for Green/timeout/rollback resolution; pure decision helper `decide_poll()` is unit-tested across the full four-state matrix. Distinct exit codes for CI branching: 0 = ok, 1 = AWS error, 2 = usage error, 4 = wait-for-green timeout, 5 = auto-rollback dispatched. Same duration grammar as the TUI path (`5m` / `30m` / `1h`). Refuses upfront if `--auto-rollback` is set but the env has no prior version to roll back to. (`<TBD>`)
- **`:deploy LABEL --wait-for-green Nm`** — pure-observability companion to `--auto-rollback`. Arms a watcher at dispatch; `apply_refresh` pins success (`✓ deploy reached Green: ENV`) when the env reaches Green or pins a timeout error if the deadline elapses while still non-Green. Different glyph + colour (`👁 watching ENV REMAINING`, theme.title) from the `⏱ rollback` pill so the operator can tell at a glance which kind of in-flight observer is on the env. Composes with `--auto-rollback` — both flags can be set on the same deploy.
- **`:rollback --to LABEL [--auto-rollback Nm]`** — operator-named rollback target. Skips the snapshot/event-scan detection and routes straight to the deploy confirm with the named label. Composes with `--auto-rollback Nm` so the operator can dispatch "roll back to build-820, auto-roll-forward to build-823 if Green doesn't land within Nm" in one command. Validates non-empty target + refuses idempotent same-version rollbacks. (`021127c`)
- **`:abort-rollback [ENV]`** — explicit disarm. No-arg drains every armed watchdog in the current context; with an env name, just that one. Audit-logged. (`0293fd3`)
- **Armed-watchdog visibility.** Header countdown pill (`⏱ rollback prod-api in 4m22s`) plus `:rollbacks-armed` (alias `:rb-armed`) overlay listing every armed watchdog with env / target / armed-ago / deadline-in. Pure renderers covered by tests. (`3a81329`)

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

[Unreleased]: https://github.com/tombaldwin/ebman/compare/v0.9.0...HEAD
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
