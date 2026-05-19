# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`:settings` modal form** to edit `~/.config/ebman/config.toml` interactively. Pre-fills from the live config; submit writes the file back and live-applies what it can (theme, icons, refresh interval). Fields: theme, icons, refresh interval, redact-by-default, group-by-app-by-default, notification bell, required tags, extra regions, webhook URL.
- **`icons = "auto"` config value** that probes the terminal at startup with a one-cell Powerline triangle (`U+E0B0`) and picks `powerline` if the font renders it in a single cell, `unicode` otherwise. The probe runs before the alt-screen is entered, so the glyph never reaches user scrollback.

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

[Unreleased]: https://github.com/tombaldwin/ebman/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/tombaldwin/ebman/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tombaldwin/ebman/releases/tag/v0.1.0
