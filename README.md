# ebman

[![crates.io](https://img.shields.io/crates/v/ebman.svg)](https://crates.io/crates/ebman)
[![downloads](https://img.shields.io/crates/d/ebman.svg)](https://crates.io/crates/ebman)
[![CI](https://github.com/tombaldwin/ebman/actions/workflows/ci.yml/badge.svg)](https://github.com/tombaldwin/ebman/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![homebrew](https://img.shields.io/badge/homebrew-tombaldwin%2Ftap-orange.svg)](https://github.com/tombaldwin/homebrew-tap)

**A k9s-style TUI for AWS Elastic Beanstalk.** Triage red envs, stream logs, edit option settings, deploy new versions — all from the keyboard. If you've used k9s with Kubernetes, the muscle memory carries: `:` for commands, `/` to filter, `Enter` to drill in, `?` for context-aware help.

Built for operators who triage EB envs daily and don't want the AWS console round-trip — or the `eb deploy ; aws elasticbeanstalk describe-events --max-items 50 | jq ...` shell-pipeline every time something goes red.

<!--
  Hero asset to drop in here:
    * Preferred: 15–20s asciinema cast or VHS gif showing launch → drill into
      an env → `:why` overlay → `:diff staging prod`.
    * Acceptable: 3-up static screenshot row (main table / `:why` overlay /
      Detail/Health) + a single big `:why` shot under it.
  Capture with `asciinema rec demo.cast` then convert via `agg`, or with VHS
  (https://github.com/charmbracelet/vhs) for a deterministic gif.
-->

## Triage workflow

Production env goes red at 3am. From your terminal:

1. `ebman` — launch; `prod-api` shows up tinted red in the table
2. `/prod-api` `↵` — jump to it
3. `!` — open `:why`: recent events / alarms / instance health / last deploys, all in one overlay
4. `:diff prod-api staging-api` — confirm staging is still on the previous version
5. `:rollback` — redeploy the last-known-good version label, with a 5-second undo window
6. Action + outcome land in `~/.cache/ebman/audit.log`

Five keystrokes to triage, one command to fix. The AWS-console alternative is a minimum of five page-loads, two tabs, and zero audit trail.

## Highlights

- **Live env table** with sort / filter / group-by-app / health sparkline / severity tints / mouse support.
- **Per-env drill-down** — tabs for Health / Events / Instances / Metrics / Queue / Logs / Config.
- **Red-env triage** — `:why` opens a one-screen diagnostic: recent events + alarms + instance health + last deploys, with a DLQ peek for Worker envs.
- **Honest health** — envs with alarms in ALARM, DLQs with messages, or stale platforms surface on the row itself, not behind a tab.
- **Forensics** — `:diff env-A env-B` for option-setting deltas, `:lineage` for the deploy timeline, `:alarm-history NAME` for CW state transitions, `:config-diff-local` against a local EB CLI saved config.
- **Cost-aware** — opt-in `:cost on` adds a per-env $ column from Cost Explorer; same number surfaces in `:why` and Detail/Health.
- **Daily-driver writes** — env vars, tags, deploys (label / local zip / S3, with `--preview`), saved configs, CW alarms CRUD, ALB scheme, instance type, capacity, deployment policy, plus a generic `:set-option` escape hatch.
- **Worker / SQS** — DLQ viewer with resend, typed-name purge, bulk delete, peek-and-tail.
- **SSM** — `:ssh i-abc` opens an embedded session; `:ssm-run "<cmd>"` fans a shell command across the env's instances and aggregates per-instance status / exit code / stdout / stderr.
- **Multi-account / multi-region** — `:region all` parallel queries, `:account NAME` via `sts:AssumeRole`, `:find-env` / `:org-health` walk every `~/.aws` profile + configured account.
- **Bulk ops** — `space` multi-selects, then `:batch-*` fans out in parallel with audit + pending-pill rows.
- **Safety** — `--read-only` flag, typed-name confirms for destructive actions, pre-flight dry-runs, per-env / per-account read-only pins via `safety.envs.NAME` in `config.toml`.
- **Audit log** at `~/.cache/ebman/audit.log` — every action + outcome, rotated at 1 MiB.
- **Power-user** — `Ctrl-K` palette, named filters with `]` / `[` cycle, plugin commands (`~/.config/ebman/commands.toml`).
- **Headless / scriptable** — `--control-socket PATH` exposes a Unix socket; `ebman envs --json`, `ebman action rebuild --env NAME`, `ebman ctl screen / state / cmd <:cmd>` for scripts and CI.

## Why ebman?

You probably already have one of these:

| Tool | What it's good at | Where it falls short for daily EB triage |
| --- | --- | --- |
| **AWS Console** | Approachable, complete UI surface. | Page loads, eventually-consistent state, 5 tabs to triage one env. Fine for occasional ops, painful at 3am. |
| **`eb` CLI** | A single project's deploy flow (`eb deploy`, `eb logs`). | No multi-env view, no live drill-down, no diff between envs, no SQS DLQ workflow. |
| **`aws elasticbeanstalk`** | Raw API access, scriptable. | You build the workflow out of `--query` / `jq` pipelines yourself. No live updates, no triage view. |
| **k9s + EKS** | The pattern this tool is modelled on. | Doesn't exist for Elastic Beanstalk. |

ebman is k9s-for-EB: keyboard-driven, drill-down-first, focused on operators who triage red envs daily and want one screen for "what's wrong" and "what changed". The nearest peer in the broader Rust-TUI / k9s-style space is [`e1s`](https://github.com/keidarcy/e1s) (k9s-for-ECS); ebman is broader on the write surface and adds multi-account fan-out, an audit log, per-env safety pins, and an embedded SSM-session pane.

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
cargo install ebman
```

Tested on Rust 1.91+. macOS (Apple Silicon + Intel) and Linux x86_64. AWS SDK uses the standard credentials chain (`AWS_PROFILE` / `AWS_REGION` env, `~/.aws/credentials`, instance role, etc.).

### Fonts (optional, for the prettier glyph set)

Ebman runs fine in any terminal with the default `icons = "unicode"` config. For the Powerline-style pill chain, tab ribbon, and per-tab MDI icons (`icons = "powerline"` or `icons = "auto"`), your terminal needs a Nerd Font installed — vanilla Powerline fonts give you the triangles but tofu/boxes where the tab icons should be.

**1. Install a Nerd Font:**

```bash
brew install font-meslo-lg-nerd-font           # Powerlevel10k crowd; safe default
brew install font-jetbrains-mono-nerd-font     # modern monospace, no ligature surprises
```

**2. Set your terminal's font** to one of the `Mono` variants — they're sized for fixed-width TUIs (e.g. `MesloLGS Nerd Font Mono`, `JetBrainsMono Nerd Font Mono`):

- iTerm2: Preferences → Profiles → Text → Font
- Terminal.app: Preferences → Profiles → Font → Change
- Ghostty / Alacritty / WezTerm: `font-family` in the relevant config file
- VS Code / Cursor terminal: `terminal.integrated.fontFamily` in settings

**3. Tell ebman to use the new glyphs** — either run `:settings` in ebman and pick `auto` (or `powerline`) from the Icons field, or add this to `~/.config/ebman/config.toml`:

```toml
icons = "auto"   # probes the terminal at startup; falls back to "unicode"
```

Restart ebman (or use `ebman ctl reload` if you're driving via the control socket) so the startup probe runs against your new font. `icons = "powerline"` skips the probe and forces the Nerd glyph set unconditionally.

Without a Nerd Font, stick to `icons = "unicode"` (the default) — everything still works, you just don't get the per-tab MDI icons.

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
| `!` | `:why` overlay (Red-env diagnostic) |
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
| Health (default) | `j`/`k` walk items, `Enter` drill into source tab (Events / Instances / Queue) |
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
- `:account NAME` — switch to a configured AssumeRole account (`accounts.NAME` in `config.toml`). Falls back to `:profile NAME` aliasing when no `accounts.` entry exists.
- `:accounts` — list child accounts in the active AWS organization; rows matching a configured `accounts.NAME` get a `:account NAME` switch hint.
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
- `:loglevel LEVEL` — live-reload the tracing filter.

### Per-env inspection

- `:why` — Red-env diagnostic overlay (recent events / alarms / instance health / recent deploys; main + DLQ peek for Worker envs). Bound to `!` on the env table.
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
- `:about` / `:credits` — version, license, attributions.
- `:update` — show (and yank to clipboard) the upgrade command for whichever install channel (Homebrew / cargo-bin / tarball) ebman was installed from.
- `:settings` — interactive form to edit `~/.config/ebman/config.toml`; writes back on submit and live-applies theme / icons / refresh interval.

### Write — env state

- `:rebuild` / `:restart` / `:terminate` — action menu shortcuts (Terminate requires typed-name confirm).
- `:deploy LABEL` — ship an existing application version to the selected env.
- `:deploy LABEL --preview` — open a side-by-side overlay of the currently-deployed version vs the candidate (label, description, S3 source, timestamp + rollback / traffic warnings) without dispatching.
- `:deploy --from PATH [--label L] [--describe D] [--no-deploy]` — upload a local `.zip` (or `--from s3://bucket/key`), register a new version, optionally deploy.
- `:upgrade [ARN]` — list compatible platforms; with ARN, dispatch the migration.
- `:clone NEWNAME` — clone the selected env.
- `:scale N` / `:stop` / `:start` — set ASG min=max=N / 0 / 1.
- `:capacity` — modal form to edit Min / Max / Instance type / Cooldown in one shot (pre-filled from `DescribeConfigurationSettings`).
- `:swap TARGET` — swap CNAMEs (Y/N confirm).
- `:abort` — abort an in-flight env update.

### Write — env config

- `:env list | set KEY VAL | unset KEY` — application env-var editor.
- `:tag KEY VALUE` / `:untag KEY` — env tag editor.
- `:set-option NAMESPACE OPTION VALUE` / `:unset-option NAMESPACE OPTION` — generic option-settings escape hatch.
- `:instance-type TYPE` — EC2 instance type (e.g. `t3.medium`).
- `:keypair NAME` / `:service-role ARN` / `:instance-profile NAME` — security tab.
- `:public-ip on|off` / `:elb-scheme public|internal` — network tab.
- `:subnets` / `:elb-subnets` / `:security-groups` — MultiSelect picker forms pre-filled with the env's current selection (lists available subnets / SGs from the VPC).
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
- `:account NAME` — switch to a configured AssumeRole account (see Configuration).
- `:accounts` — list AWS-Organizations child accounts; switch hints rendered for those with a configured `accounts.NAME`.
- `:find-env SUBSTRING` — scan every profile in `~/.aws/{config,credentials}` **and** every configured AssumeRole account in REGION.
- `:org-health` — aggregate env / red counts per profile + per configured AssumeRole account.

### Multi-env

- `space` — toggle multi-select.
- `:batch-rebuild` / `:batch-restart` — dispatch a non-destructive action across the selection.
- `:batch-deploy LABEL` — deploy the same version to every selected env in parallel.
- `:batch-tag KEY VALUE` / `:batch-untag KEY` — fan a tag write across the selection.
- `:batch-set-option NAMESPACE OPTION VALUE` — fan an option-settings write across the selection.
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

# Glyph set: "unicode" (default), "ascii" for low-feature terminals,
# "powerline" (alias "nerd") for Powerline-patched / Nerd Fonts, or
# "auto" to probe the terminal at startup and pick powerline if its
# support is detected (one-cell U+E0B0 advance), unicode otherwise.
icons = "unicode"

# Per-profile theme override — pin a theme per AWS profile so the screen
# itself says "you're in prod" without reading the breadcrumb. Format:
# "PROFILE:THEME,PROFILE:THEME". Theme names match the `theme = ...` key.
profile_themes = "prod:high-contrast,staging:dark"

# Start with these toggles on (state.toml takes precedence after first run).
redact_default = false
grouped_default = false

# Notification bell on increase in Red-env count.
notify_bell = false

# Tag policy — flag envs missing any of these tags in the Config tab.
required_tags = "Owner,Project"

# Red-transition notifications — ebman emits a `tracing::warn!` and writes
# a `stage=event kind=red_transition env=…` line to the audit log at
# `~/.cache/ebman/audit.log` for every env that transitions into Red.
# Wire your own notifier (Slack, PagerDuty, …) by tailing that file. The
# previous built-in `webhook_url` POST was trimmed — single-URL POST was
# too rigid for real ops workflows, and the audit log already carried the
# same data with timestamps.

# AssumeRole targets reachable via `:account NAME`. One stanza per
# account. `source_profile` carries the base creds for the
# sts:AssumeRole call. `external_id` and `region` are optional.
# The temporary credentials build a fresh SdkConfig carrying only the
# assumed-role identity — source-profile creds never leak into request
# signing once the switch lands.
accounts.prod.role_arn = "arn:aws:iam::111122223333:role/EbmanReadOnly"
accounts.prod.source_profile = "default"
accounts.prod.region = "eu-west-2"
# accounts.prod.external_id = "..."

# Per-env / per-account read-only locks. Borrowed from pgman's safety
# system. When pinned here, destructive actions against the env (or
# anything under the named account) are refused even when the global
# `--read-only` toggle is off. The global toggle is still the master
# switch; these add granular pins on top.
safety.envs.uflexi-prod.read_only = true
safety.accounts.prod.read_only = true
```

`~/.config/ebman/commands.toml` (optional) — user plugin commands. Each `:NAME` substitutes `{name}` / `{cname}` / `{application}` / `{tier}` / `{region}` / `{profile}` placeholders and yanks the rendered command to the clipboard.

```toml
[commands.tunnel]
template = "aws ssm start-session --target $(aws ec2 describe-instances --filters Name=tag:elasticbeanstalk:environment-name,Values={name} --query 'Reservations[].Instances[].InstanceId' --output text) --profile {profile}"
description = "Yank a tunnel command into clipboard"
```

`~/.config/ebman/state.toml` is managed by the app — filter / sort / cursor position / named filters / saved views / pinned envs / custom metrics live there.

`<repo>/.ebman/ebman.toml` (optional) — project-local pinning. Commit
this to git so a team launches ebman from the repo with the right
profile / region / filter pre-applied. Walks up from cwd to find the
`.ebman/` directory, so running ebman from any subdirectory of the
project works. Profile / region win over `~/.config/ebman/state.toml`
when both are set. Per-env runbook URLs merge with the user-level
`runbooks.ENV = …` map — project entries win on collision.

```toml
# <repo>/.ebman/ebman.toml — commit this. Credentials still come from
# ~/.aws/credentials, never this file.
profile = "prod"          # AWS profile to use
region  = "us-west-1"     # AWS region
application = "uflexi"    # filter envs to this app on launch
filter  = "prod-"         # pre-fill the search filter

[runbooks]
runbooks.uflexi-prod = "https://wiki/runbooks/uflexi-prod"
```

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
- `~/.config/ebman/commands.toml` — optional plugin commands.
- `<repo>/.ebman/ebman.toml` — optional project-local pinning (profile / region / filter / runbooks). Walked up from cwd.
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

## Privacy / telemetry

Ebman does not phone home. There is no usage telemetry, no anonymous identifier, no crash auto-reporting, no third-party analytics endpoint. The only outbound HTTP from ebman itself is to AWS (the SDK calls you'd expect) and a single version-check ping to crates.io (`update_check.rs`), which crates.io logs as "client IP requested ebman version metadata" and nothing more.

**Bug reports** are operator-driven via `:report-bug`. Ebman builds a scrubbed payload locally — version / OS / icons / theme / refresh interval / last 30 log lines / last 10 on-screen messages / latest panic backtrace — and runs it through a redactor that strips account IDs (any 12-digit ASCII number), ARNs (`arn:aws:*`), every env name + application name + CNAME currently in the in-memory table, and the active profile name. The result lands in an overlay where you see the exact bytes before they leave the machine. Two delivery paths, both initiated by you:

- `y` copies the scrubbed payload to clipboard. Paste into a new GitHub issue.
- `b` opens https://github.com/tombaldwin/ebman/issues/new in your browser with the body pre-filled via URL params (truncated at ~7900 chars so the URL stays under GitHub's 8k limit).

Ebman never sends the payload itself. The redactor isn't bulletproof — a freeform error message could still embed a customer name in an unscrubbable shape — which is why you review the payload before pasting / opening the browser.

Crash logs are written locally to `~/.cache/ebman/crash-*.log` by the panic hook (10 most recent kept, 30-day TTL). They're plain-text files; do whatever you want with them.

## Distribution

- **Cargo**: `cargo install ebman` from crates.io; `cargo install --path .` from a checkout.
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
