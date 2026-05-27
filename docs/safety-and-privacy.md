# Safety, privacy, and what's stored locally

## Safety model

- **Read-only mode** (`--read-only` or `:readonly on`) disables every write surface: action menu, DLQ resend / purge, all `:`-commands that mutate state. A green `READ-ONLY` pill in the header makes it visible.
- **Strict-typed confirm** for irreversible actions: typing the env name is required to Terminate; typing the literal string to Purge.
- **Pre-flight checks** in the confirm modal: `DescribeInstancesHealth` impact count, last 3 events, traffic warnings for env-in-deploy / recently-changed / currently-Red.
- **Audit log** records dispatch + outcome of every action.
- **Per-env / per-account safety pins** in `config.toml` (`safety.envs.NAME.read_only = true` / `safety.accounts.NAME.read_only = true`) refuse destructive actions even when the global `--read-only` toggle is off.

## What's stored locally

- `~/.config/ebman/config.toml` — user configuration (see [configuration](configuration.md)).
- `~/.config/ebman/commands.toml` — optional plugin commands.
- `<repo>/.ebman/ebman.toml` — optional project-local pinning (profile / region / filter / runbooks). Walked up from cwd.
- `~/.config/ebman/state.toml` — persisted UI state: profile, region, filter, sort, grouping, redact, selected env, saved views, pinned envs, aliases, hidden columns, custom metrics. No credentials.
- `~/.cache/ebman/ebman.log` — application log; rotates as needed. Set `RUST_LOG=debug` for verbose output.
- `~/.cache/ebman/audit.log` — every dispatched action and outcome (account, profile, region, action, target). Rotates at 1 MiB to `audit.log.1`.
- `~/.cache/ebman/crash-*.log` — panic backtraces (10 most recent kept; 30-day TTL).
- Clipboard — `y` / `Y` / `^Y` / `^W` write via `arboard`.

## Privacy / telemetry

Ebman does not phone home. There is no usage telemetry, no anonymous identifier, no crash auto-reporting, no third-party analytics endpoint. The only outbound HTTP from ebman itself is to AWS (the SDK calls you'd expect) and a single version-check ping to crates.io (`update_check.rs`), which crates.io logs as "client IP requested ebman version metadata" and nothing more.

**Bug reports** are operator-driven via `:report-bug`. Ebman builds a scrubbed payload locally — version / OS / icons / theme / refresh interval / last 30 log lines / last 10 on-screen messages / latest panic backtrace — and runs it through a redactor that strips account IDs (any 12-digit ASCII number), ARNs (`arn:aws:*`), every env name + application name + CNAME currently in the in-memory table, and the active profile name. The result lands in an overlay where you see the exact bytes before they leave the machine. Two delivery paths, both initiated by you:

- `y` copies the scrubbed payload to clipboard. Paste into a new GitHub issue.
- `b` opens https://github.com/tombaldwin/ebman/issues/new in your browser with the body pre-filled via URL params (truncated at ~7900 chars so the URL stays under GitHub's 8k limit).

Ebman never sends the payload itself. The redactor isn't bulletproof — a freeform error message could still embed a customer name in an unscrubbable shape — which is why you review the payload before pasting / opening the browser.

Crash logs are written locally to `~/.cache/ebman/crash-*.log` by the panic hook (10 most recent kept, 30-day TTL). They're plain-text files; do whatever you want with them.
