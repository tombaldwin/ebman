# ebman

A k9s-style terminal UI for AWS Elastic Beanstalk.

Browse environments, drill into events / instances / metrics / queue / config, stream CloudWatch logs, edit env vars / option settings, deploy new versions — all without leaving the terminal.

<!-- screenshot here once the maintainer can capture a real frame -->

## Highlights

- **Live env table** with sort, filter, group-by-app, health sparkline (trend window auto-labelled), severity tints, mouse support.
- **Drill-down per env** — Events (regex-searchable), Instances (with health causes + embedded SSM shell), Metrics (CloudWatch line charts; custom metrics with arbitrary dimensions), Queue (Worker tier; main + DLQ stats with viewer), Logs (one-shot snapshot or real `tail -f` from CW Logs), Config (tags + env vars read-only, plus cost estimate).
- **Daily-driver write surface** — env vars (`:env set/unset`), tags (`:tag`/`:untag`), version deploy from existing label or local zip / S3 (`:deploy --from`), saved-config CRUD, CloudWatch alarms CRUD, log streaming toggle, notifications endpoint, managed-update window, ALB scheme, instance type, key pair, IAM roles, deployment policy, rolling-update settings, health-check URL, plus a generic `:set-option NAMESPACE OPTION VALUE` escape hatch.
- **Worker / SQS workflow** — DLQ viewer with per-message resend (`r`), strict-typed purge (`p`), bulk delete (`x`), peek-and-tail with long-polling.
- **Multi-region / multi-profile** — `:region all` fans out to every configured region in parallel, `:find-env` searches across every AWS profile in `~/.aws/{config,credentials}`, `:org-health` aggregates env counts per profile.
- **Safety** — `--read-only` CLI flag or `:readonly on` disables every write surface; destructive actions (Terminate, DLQ purge) require typed-name confirmation; pre-flight dry-run shows impact (N instances across M AZs) + last 3 events before authorising; recent-change / mid-deploy traffic warnings in the confirm modal.
- **Audit log** — every dispatched action and its outcome are appended to `~/.cache/ebman/audit.log`; rotates at 1 MiB.
- **Power-user ergonomics** — fuzzy command palette (`Ctrl-K`) across commands / envs / saved views / plugins, named filters + saved views, custom keybindings (`F1-F12` and uppercase letters via `~/.config/ebman/keys.toml`), plugin commands (`~/.config/ebman/commands.toml`), in-app `:loglevel` reload, `:diff` between envs.
- **Headless / scriptable** — `--control-socket PATH` exposes a Unix-socket interface; `ebman ctl <op>` is a one-shot client (`screen` / `state` / `key <spec>` / `cmd <:cmd>`).
- **Non-interactive CLI** — `ebman envs [--json]` / `ebman action rebuild --env NAME` / `ebman ctl ...` for scripts and CI.

## Install

**Homebrew (macOS / Linux):**

```bash
brew tap tombaldwin/tap
brew install ebman
```

**Pre-built binary:**

Download the tarball for your platform from the [GitHub Releases page](https://github.com/tombaldwin/ebman/releases), verify the `*.sha256` next to it, extract, and put `ebman` on your `PATH`.

**Cargo:**

```bash
cargo install --path .
# or, once published to crates.io:
cargo install ebman
```

Tested on Rust 1.91+. macOS (Apple Silicon + Intel) and Linux x86_64. AWS SDK uses the standard credentials chain (`AWS_PROFILE` / `AWS_REGION` env, `~/.aws/credentials`, instance role, etc.).

## Quickstart

```bash
ebman                                  # launch the TUI
ebman --read-only                      # disable all write surfaces (audit-friendly)
ebman --control-socket ~/.cache/ebman/control.sock   # expose the ctl interface
ebman envs --json                      # non-interactive: print env list as JSON
ebman action rebuild --env myenv --yes # non-interactive: dispatch a rebuild
ebman ctl screen                       # dump the current frame from a running instance
ebman --version
ebman --help
```

Once running, press `?` for a per-context keymap (Detail, DLQ, Action menu, Saved-configs overlay all have scoped help).

## Keys

### Normal mode (env table)

| Key | Action |
|---|---|
| `j` / `k` / wheel | Move selection |
| `g` / `G` | Top / bottom |
| `1` – `9` | Jump to position |
| `'` | Name-jump (type prefix) |
| `Enter` | Drill into env |
| `Tab` / `Shift-Tab` | Switch scope (Envs ↔ Apps) |
| `a` | Actions menu |
| `b` | Open env in AWS console |
| `D` | Describe overlay (raw env JSON) |
| `space` | Multi-select |
| `*` | Pin / unpin |
| `/` | Filter |
| `:` | Command bar |
| `^K` | Command palette |
| `s` / `S` | Cycle sort key / reverse |
| `^G` | Toggle group-by-application |
| `^E` | Toggle events panel |
| `^]` | Cycle focus (table ↔ events panel) |
| `^D` | Cycle view mode (default / compact / spacious) |
| `^X` | Toggle redact mode |
| `^Y` | Yank filtered table as TSV |
| `^W` | Yank equivalent `aws elasticbeanstalk describe-environments` |
| `y` / `Y` | Yank CNAME / name |
| `f` | Freeze / unfreeze auto-refresh |
| `r` / `p` | Switch region / profile |
| `^R` / `F5` | Force refresh |
| `?` | Help |
| `q` / `^C` | Quit |

### Detail view

Tabs cycle with `Tab` / `Shift-Tab` (or `l` / `h`). `^R` re-fetches the active tab; `R` toggles per-tab auto-refresh; `a` opens the env actions menu; `b` opens the env in the AWS console.

| Tab | Per-tab keys |
|---|---|
| Events | `/` filter, `n`/`N` next/prev match |
| Instances | `Enter` / `i` info overlay, `b` EC2 console, `s` embedded SSM shell, `y` yank id, `x` terminate (Y/N) |
| Metrics | `[` / `]` cycle range (15m → 24h), mouse hover for value-at-cursor |
| Queue | `j`/`k` Main ↔ DLQ, `Enter` opens viewer, `d` quick-open DLQ |
| Logs | `^R` one-shot snapshot, `s` open live CW Logs streaming overlay, `/` filter |
| Config | scrollable; tags + env vars + cost estimate read-only |

### DLQ viewer

`j`/`k` move, `Enter` view body, `r` resend (DLQ → main), `x` delete, `p` purge (typed-name confirm), `m` toggle Main ↔ DLQ, `^R` re-peek.

## Command reference

Type `:` to open the command bar. Tab-completion is not implemented, but `Ctrl-K` fuzzy-searches every command + env + view + plugin.

### Navigation / inspection

- `:region NAME` / `:region all` — switch region, or fan out across every configured region.
- `:profile NAME` — switch AWS profile.
- `:account NAME` — alias for `:profile`.
- `:sort KEY [desc]` — set sort (name/app/status/health/version/age).
- `:group on|off` — toggle group-by-application.
- `:redact on|off` — toggle redact mode.
- `:events on|off` — toggle events panel.
- `:filter NAME` / `:f NAME` — load a saved filter.
- `:save NAME` / `:drop NAME` / `:filters` — manage named filters.
- `:save-view NAME` / `:view NAME` / `:views` / `:view-drop NAME` — saved views (filter + sort + grouping + scope).
- `:cols list|hide NAME|show NAME|reset` — manage columns.
- `:pin` — pin / unpin selected env.
- `:alias NAME LABEL` / `:alias-drop NAME` — local env aliases.
- `:minimap on|off` — corner mini-map of env health.
- `:loglevel LEVEL` — live-reload the tracing filter.

### Per-env inspection

- `:diff NAME` — side-by-side env comparison.
- `:resources` / `:res` — `DescribeEnvironmentResources` dump.
- `:alarms` — CloudWatch alarms referencing the env.
- `:versions` — application versions (deployed marker, total count, deploy hint).
- `:saved-configs` / `:configs` — saved configuration templates (interactive: `a` apply, `i` inspect, `x` delete, `c` create).
- `:custom-platforms` / `:platforms` — custom EB platforms.
- `:plugins` — list user plugin commands.
- `:history` — recent status / error log.
- `:pending` / `:in-flight` — overlay of dispatched actions + outcomes.
- `:whatsnew` — embedded changelog.

### Write — env state

- `:rebuild` / `:restart` / `:terminate` — action menu shortcuts (Terminate requires typed-name confirm).
- `:deploy LABEL` — ship an existing application version to the selected env.
- `:deploy --from PATH [--label L] [--describe D] [--no-deploy]` — upload a local `.zip` (or `--from s3://bucket/key`), register a new version, optionally deploy.
- `:upgrade [ARN]` — list compatible platforms; with ARN, dispatch the migration.
- `:clone NEWNAME` — clone the selected env.
- `:scale N` / `:stop` / `:start` — set ASG min=max=N / 0 / 1.
- `:swap TARGET` — swap CNAMEs (Y/N confirm).
- `:abort` — abort an in-flight env update.

### Write — env config

- `:env list | set KEY VAL | unset KEY` — application env-var editor.
- `:tag KEY VALUE` / `:untag KEY` — env tag editor.
- `:set-option NAMESPACE OPTION VALUE` / `:unset-option NAMESPACE OPTION` — generic option-settings escape hatch.
- `:instance-type TYPE` — EC2 instance type (e.g. `t3.medium`).
- `:keypair NAME` / `:service-role ARN` / `:instance-profile NAME` — security tab.
- `:public-ip on|off` / `:elb-scheme public|internal` — network tab.
- `:deployment-policy AllAtOnce|Rolling|RollingWithAdditionalBatch|Immutable|TrafficSplitting` — deploy policy.
- `:rolling-update on|off` — ASG rolling-update policy.
- `:health-check-url /path` — HTTP health-check path.
- `:logs-stream on|off [--retention DAYS]` — toggle CW Logs streaming.
- `:logs-tail [LOG_GROUP]` — open a live streaming overlay for a CW Logs group (auto-picks `web.stdout.log`).
- `:notify EMAIL_OR_SNS_ARN | off` — notification endpoint.
- `:managed-window DAY HOUR | off` — managed-platform-updates window.

### Write — application versions / configs / alarms / platforms

- `:delete-version LABEL [--force]` — delete an application version (with optional source-bundle removal).
- `:config-save NAME` / `:config-apply NAME` / `:config-delete APP NAME` / `:config-inspect NAME` — saved-configuration templates.
- `:alarm-create NAME KIND THRESHOLD [OP]` — CloudWatch alarm (KIND: `health`, `4xx`, `5xx`, `latency`).
- `:alarm-delete NAME` — remove a CW alarm.
- `:custom-platform-delete ARN` — delete a custom EB platform.
- `:metric add LABEL NAMESPACE NAME [STAT] [DIM=VAL,...]` / `:metric remove LABEL` / `:metric list` — custom Metrics-tab charts.

### Multi-account / multi-region

- `:region all` — fan across `extra_regions` + current.
- `:find-env SUBSTRING` — scan every profile in `~/.aws/{config,credentials}`.
- `:org-health` — aggregate env / red counts per profile.

### Multi-env

- `space` — toggle multi-select.
- `:batch-rebuild` / `:batch-restart` — dispatch a non-destructive action across the selection.
- `:deselect` / `:select-clear` — clear selection.

### Output / yank

- `:export` — yank filtered view as TSV.
- `:json` — yank filtered view as JSON array.
- `:report` / `:markdown` — yank filtered view as a Markdown table.

### Read-only mode

- `:readonly on|off` — toggle. `--read-only` on the CLI also locks every write surface.

## Configuration

`~/.config/ebman/config.toml`:

```toml
# Refresh interval in seconds (default 15).
refresh_interval_secs = 15

# Extra regions to expose in the region picker, comma-separated.
extra_regions = ""

# Theme: "dark" (default), "light", or "high-contrast".
theme = "dark"

# Glyph set: "unicode" (default), "ascii" for low-feature terminals, or
# "powerline" (alias "nerd") for Powerline-patched / Nerd Fonts.
icons = "unicode"

# Start with these toggles on (state.toml takes precedence after first run).
redact_default = false
grouped_default = false

# Notification bell on increase in Red-env count.
notify_bell = false

# Tag policy — flag envs missing any of these tags in the Config tab.
required_tags = "Owner,Project"

# Webhook URL to POST to when an env transitions to Red.
webhook_url = ""
```

`~/.config/ebman/keys.toml` (optional) — custom keybindings:

```toml
# Aliases must be F1-F12 or an uppercase A-Z; map to a :command.
F1 = "refresh"
F2 = "region us-east-1"
Q = "history"
```

`~/.config/ebman/commands.toml` (optional) — user plugin commands. Each `:NAME` substitutes `{name}` / `{cname}` / `{application}` / `{tier}` / `{region}` / `{profile}` placeholders and yanks the rendered command to the clipboard.

```toml
[commands.tunnel]
template = "aws ssm start-session --target $(aws ec2 describe-instances --filters Name=tag:elasticbeanstalk:environment-name,Values={name} --query 'Reservations[].Instances[].InstanceId' --output text) --profile {profile}"
description = "Yank a tunnel command into clipboard"
```

`~/.config/ebman/state.toml` is managed by the app — filter / sort / cursor position / named filters / saved views / pinned envs / custom metrics live there.

## Headless interface (`--control-socket`)

Launch ebman with `--control-socket PATH` to expose a Unix-socket interface. A second binary, `ebman ctl <op>`, is the one-shot client (defaults to `~/.cache/ebman/control.sock`).

```bash
ebman ctl state                   # JSON: mode, profile, region, account, envs, selected, ...
ebman ctl screen                  # plain-text dump of the current frame
ebman ctl key Down                # synthesise a keypress
ebman ctl key Ctrl+R              # … or a combo
ebman ctl cmd ':region eu-west-2' # run a : command
```

Useful for integration tests, screenshot capture, scripted workflows.

## What's stored locally

- `~/.config/ebman/config.toml` — user configuration (see above).
- `~/.config/ebman/keys.toml` — optional custom keybindings.
- `~/.config/ebman/commands.toml` — optional plugin commands.
- `~/.config/ebman/state.toml` — persisted UI state: profile, region, filter, sort, grouping, redact, selected env, named filters, saved views, pinned envs, aliases, hidden columns, custom metrics. No credentials.
- `~/.cache/ebman/ebman.log` — application log; rotates as needed. Set `RUST_LOG=debug` for verbose output.
- `~/.cache/ebman/audit.log` — every dispatched action and outcome (account, profile, region, action, target). Rotates at 1 MiB to `audit.log.1`.
- `~/.cache/ebman/crash-*.log` — panic backtraces (10 most recent kept; 30-day TTL).
- Clipboard — `y` / `Y` / `^Y` / `^W` write via `arboard`.

## Safety model

- **Read-only mode** (`--read-only` or `:readonly on`) disables every write surface: action menu, DLQ resend / purge, all `:`-commands that mutate state. A green `READ-ONLY` pill in the header makes it visible.
- **Strict-typed confirm** for irreversible actions: typing the env name is required to Terminate; typing the literal string to Purge.
- **Pre-flight checks** in the confirm modal: `DescribeInstancesHealth` impact count, last 3 events, traffic warnings for env-in-deploy / recently-changed / currently-Red.
- **Audit log** records dispatch + outcome of every action.

## Distribution

- **Cargo**: `cargo install --path .` (publish to crates.io is maintainer-driven).
- **GitHub Releases**: tagging `v<X.Y.Z>` triggers `.github/workflows/release.yml`, which builds release binaries for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-apple-darwin` and attaches tarballs + SHA-256 checksums to a draft release.
- **Homebrew**: tap lives at [`tombaldwin/homebrew-tap`](https://github.com/tombaldwin/homebrew-tap). Per-release: bump the version + 3 platform SHAs in `Formula/ebman.rb` in both this repo (for `brew install --formula PATH`) and the tap.

## Development

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

See `BACKLOG.md` for in-flight and planned work. See `CLAUDE.md` for the AI-assisted-contributor rules (the project has been developed heavily with Claude Code).

## License

Dual-licensed under MIT or Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
