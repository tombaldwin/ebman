# ebman

[![crates.io](https://img.shields.io/crates/v/ebman.svg)](https://crates.io/crates/ebman)
[![downloads](https://img.shields.io/crates/d/ebman.svg)](https://crates.io/crates/ebman)
[![CI](https://github.com/tombaldwin/ebman/actions/workflows/ci.yml/badge.svg)](https://github.com/tombaldwin/ebman/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![homebrew](https://img.shields.io/badge/homebrew-tombaldwin%2Ftap-orange.svg)](https://github.com/tombaldwin/homebrew-tap)

**A k9s-style TUI for AWS Elastic Beanstalk.** Triage red envs, stream logs, edit option settings, deploy new versions — all from the keyboard. If you've used k9s with Kubernetes, the muscle memory carries: `:` for commands, `/` to filter, `Enter` to drill in, `?` for context-aware help.

[Install](#install) · [Quickstart](#quickstart) · [Triage workflow](#triage-workflow) · [Highlights](#highlights) · [Why ebman?](#why-ebman) · [Docs](#documentation)

![ebman demo — filter → :why → drill into Detail/Instances → embedded SSM session](demo.gif)

> Captured from `ebman --demo`, the synthetic-fleet mode that ships with the binary. Regenerate the gif with `vhs demo.tape` after a code change.

## Install

**Homebrew (macOS / Linux):**

```bash
brew install tombaldwin/tap/ebman
```

**Cargo:**

```bash
cargo install ebman
```

**Pre-built binary:** download the tarball for your platform from the [GitHub Releases page](https://github.com/tombaldwin/ebman/releases), verify the `*.sha256` next to it, extract, and put `ebman` on your `PATH`.

Tested on Rust 1.91+. macOS (Apple Silicon + Intel) and Linux x86_64. AWS SDK uses the standard credentials chain (`AWS_PROFILE` / `AWS_REGION` env, `~/.aws/credentials`, instance role, etc.).

For the prettier glyph set (Powerline pill chain, Nerd Font tab icons), see [docs/fonts.md](docs/fonts.md). The default `icons = "unicode"` works fine in any terminal.

## Quickstart

```bash
ebman                                  # launch the TUI
ebman --read-only                      # disable all write surfaces (audit-friendly)
ebman --demo                           # synthetic fleet (no AWS calls) — for screenshots / VHS
ebman --version
ebman --help
```

Once running, press `?` for a per-context keymap (Detail, DLQ, Action menu, Saved-configs overlay all have scoped help).

## Triage workflow

Production env goes red at 3am. From your terminal:

1. `ebman` — launch; `prod-api` shows up tinted red in the table
2. `/prod-api` `↵` — jump to it
3. `!` — open `:why`: recent events / alarms / instance health / last deploys, all in one overlay
4. `:diff prod-api staging-api` — confirm staging is still on the previous version
5. `:rollback` — redeploy the last-known-good version label, with a 5-second undo window
6. Action + outcome land in `~/.cache/ebman/audit.log`

Five keystrokes to triage, one command to fix. The AWS-console alternative is a minimum of five page-loads, two tabs, and zero audit trail.

Built for operators who triage EB envs daily and don't want the AWS console round-trip — or the `eb deploy ; aws elasticbeanstalk describe-events --max-items 50 | jq ...` shell-pipeline every time something goes red.

## Highlights

- **Live env table** with sort / filter / group-by-app / health sparkline / severity tints / mouse support.
- **Per-env drill-down** — tabs for Health / Events / Instances / Metrics / Queue / Logs / Config.
- **Red-env triage** — `:why` opens a one-screen diagnostic: recent events + alarms + instance health + last deploys, with a DLQ peek for Worker envs.
- **Honest health** — envs with alarms in ALARM, DLQs with messages, or stale platforms surface on the row itself, not behind a tab.
- **Forensics** — `:diff env-A env-B` for option-setting deltas, `:lineage` for the deploy timeline, `:alarm-history NAME` for CW state transitions, `:config-diff-local` against a local EB CLI saved config.
- **Daily-driver writes** — env vars, tags, deploys (label / local zip / S3, with `--preview`), saved configs, CW alarms CRUD, ALB scheme, instance type, capacity, deployment policy, plus a generic `:set-option` escape hatch.
- **Worker / SQS** — DLQ viewer with resend, typed-name purge, bulk delete, peek-and-tail.
- **SSM** — `:ssh i-abc` opens an embedded session; `:ssm-run "<cmd>"` fans a shell command across the env's instances.
- **Multi-account / multi-region** — `:region all` parallel queries, `:account NAME` via `sts:AssumeRole`, `:find-env` / `:org-health` walk every `~/.aws` profile + configured account.
- **Bulk ops** — `space` multi-selects, then `:batch-*` fans out in parallel with audit + pending-pill rows.
- **Safety + audit** — `--read-only` flag, typed-name confirms, per-env / per-account read-only pins, full audit log at `~/.cache/ebman/audit.log`.
- **Headless / scriptable** — `--control-socket PATH`, `ebman envs --json`, `ebman action rebuild --env NAME`, `ebman ctl screen / state / cmd <:cmd>`.

## Why ebman?

You probably already have one of these:

| Tool | What it's good at | Where it falls short for daily EB triage |
| --- | --- | --- |
| **AWS Console** | Approachable, complete UI surface. | Page loads, eventually-consistent state, 5 tabs to triage one env. Fine for occasional ops, painful at 3am. |
| **`eb` CLI** | A single project's deploy flow (`eb deploy`, `eb logs`). | No multi-env view, no live drill-down, no diff between envs, no SQS DLQ workflow. |
| **`aws elasticbeanstalk`** | Raw API access, scriptable. | You build the workflow out of `--query` / `jq` pipelines yourself. No live updates, no triage view. |
| **k9s + EKS** | The pattern this tool is modelled on. | Doesn't exist for Elastic Beanstalk. |

ebman is k9s-for-EB: keyboard-driven, drill-down-first, focused on operators who triage red envs daily and want one screen for "what's wrong" and "what changed". The nearest peer in the broader Rust-TUI / k9s-style space is [`e1s`](https://github.com/keidarcy/e1s) (k9s-for-ECS); ebman is broader on the write surface and adds multi-account fan-out, an audit log, per-env safety pins, and an embedded SSM-session pane.

## Documentation

- [Keys](docs/keys.md) — normal mode, detail view tabs, DLQ viewer.
- [Command reference](docs/commands.md) — every `:command` grouped by job (navigation, per-env, write, multi-account, bulk).
- [Configuration](docs/configuration.md) — `~/.config/ebman/config.toml`, plugin commands, project-local pinning.
- [Fonts](docs/fonts.md) — installing a Nerd Font for the Powerline glyph set.
- [Headless interface](docs/headless.md) — `--control-socket` + `ebman ctl` for scripts / CI.
- [Safety, privacy, what's stored locally](docs/safety-and-privacy.md) — read-only mode, audit log, bug-report scrubbing.
- [Development](docs/development.md) — build / test / clippy + distribution notes.

## License

Dual-licensed under MIT or Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
