# ebman

A k9s-style terminal UI for AWS Elastic Beanstalk.

Browse environments, drill into events / instances / metrics / queue / config, search regex over event streams, and run safe-by-default actions (rebuild / restart / swap CNAMEs / terminate) — all without leaving the terminal.

## Features

- **Live environment table** — name, application, tier, status, health, trend sparkline, platform, version, CNAME, age.
- **Drill-down view** per env: Events (regex-searchable), Instances (with health causes), Metrics (CloudWatch line charts), Queue (for Worker tier — main + DLQ stats), Config.
- **DLQ viewer** with per-message resend and strict-typed purge.
- **Actions menu**: Rebuild, Restart, Swap CNAMEs, Terminate. Destructive actions require typing the env name.
- **Command bar (`:`)** — `:region X`, `:profile X`, `:sort KEY`, `:save NAME`, `:filter NAME`, `:redact on`, `:export`, `:refresh`, `:help`, `:q`.
- **Filter** (case-insensitive, multi-field) and **named filters** (`:save dev`, `:f dev`) persisted across runs.
- **Group by application** with coloured horizontal partitions and per-app palette colours.
- **Compact / default view modes** (`Ctrl-D`).
- **Redact mode** (`Ctrl-X`) blurs account ID, caller ARN, and CNAMEs — for screen-sharing / streaming.
- **Themes**: `dark` (default) and `light` presets in `config.toml`. ASCII icon fallback for low-feature terminals.
- **Mouse support**: scroll, click-to-select, hover highlight.
- **Persistent state**: filter, sort, grouping, redact, events panel, selected env, named filters — all restored across restarts.

## Install

```bash
cargo install --path .
```

Tested on Rust 1.82+. macOS and Linux. AWS SDK uses the standard credentials chain (`AWS_PROFILE` / `AWS_REGION` env, `~/.aws/credentials`, instance role, etc.).

## Usage

```bash
ebman            # launch the TUI
ebman --version  # print version
ebman --help     # print CLI help
```

Once running, press `?` for the in-app keymap.

### Most-used keys

| Key                 | Action                                  |
| ------------------- | --------------------------------------- |
| `j` / `k` / wheel   | Move selection                          |
| `g` / `G`           | Top / bottom                            |
| `Enter`             | Open drill-down for selected env        |
| `/`                 | Filter                                  |
| `:`                 | Command bar                             |
| `s` / `S`           | Cycle sort / reverse direction          |
| `Tab` / `Shift-Tab` | Switch scope (Envs ↔ Apps)              |
| `a`                 | Actions menu (rebuild / restart / …)    |
| `r` / `p`           | Switch region / profile                 |
| `Ctrl-R` / `F5`     | Force refresh                           |
| `Ctrl-G`            | Toggle group-by-application             |
| `Ctrl-E`            | Toggle events panel                     |
| `Ctrl-D`            | Cycle view mode (default / compact)     |
| `Ctrl-X`            | Toggle redact mode                      |
| `Ctrl-Y`            | Export filtered table as TSV            |
| `y` / `Y`           | Yank CNAME / name                       |
| `?`                 | Help                                    |
| `q` / `Ctrl-C`      | Quit                                    |

## Configuration

`~/.config/ebman/config.toml`:

```toml
# refresh interval, seconds (default 15)
refresh_interval_secs = 15

# extra regions to expose in the region picker, comma-separated
extra_regions = ""

# theme: "dark" or "light"
theme = "dark"

# "unicode" (default) or "ascii" for terminals without nerd/unicode support
icons = "unicode"

# start with these toggles on (state.toml takes precedence after first run)
redact_default = false
grouped_default = false
```

`~/.config/ebman/state.toml` is managed by the app — filter / sort / cursor position / named filters live there.

## What's stored locally

- `~/.config/ebman/state.toml` — selected profile/region, current filter, sort, grouping, redact state, last-selected env name, named filters. No credentials or secrets.
- `~/.cache/ebman/ebman.log` — application log. Set `RUST_LOG=debug` for verbose output.
- Clipboard — `y` / `Y` / `Ctrl-Y` write to the system clipboard via `arboard`.

## Development

```bash
cargo build
cargo test
cargo clippy -- -D warnings
```

See `BACKLOG.md` for in-flight and planned work, and `CLAUDE.md` for AI-assisted contributor rules.

## License

Dual-licensed under MIT or Apache-2.0.
