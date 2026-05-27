# Keys

Press `?` in-app for a per-context keymap — Detail, DLQ, Action menu, and the Saved-configs overlay all have scoped help.

## Normal mode (env table)

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
| `]` / `[` | Cycle saved views (filter + sort + grouping + scope) |
| `/` | Filter |
| `:` | Command bar |
| `^K` | Command palette |
| `s` / `S` | Cycle sort key / reverse |
| `^G` | Toggle group-by-application |
| `^E` | Toggle events panel |
| `^]` | Cycle focus (table ↔ events panel) |
| `^D` | Cycle view mode (default / compact / spacious) |
| `^X` | Toggle redact mode |
| `T` | Cycle event timestamp format (UTC / local / age) |
| `^Y` | Yank filtered table as TSV |
| `^W` | Yank equivalent `aws elasticbeanstalk describe-environments` |
| `y` / `Y` | Yank CNAME / name |
| `f` | Freeze / unfreeze auto-refresh |
| `r` / `p` | Switch region / profile |
| `^R` / `F5` | Force refresh |
| `?` | Help |
| `q` / `^C` | Quit |

## Detail view

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

## DLQ viewer

`j`/`k` move, `Enter` view body, `r` resend (DLQ → main), `x` delete, `p` purge (typed-name confirm), `m` toggle Main ↔ DLQ, `^R` re-peek.
