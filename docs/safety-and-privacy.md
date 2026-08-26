# Safety, privacy, and what's stored locally

## Safety model

- **Read-only mode** (`--read-only` or `:readonly on`) disables every write surface: action menu, DLQ resend / purge, all `:`-commands that mutate state. A green `READ-ONLY` pill in the header makes it visible.
- **Strict-typed confirm** for irreversible actions: typing the env name is required to Terminate; typing the literal string to Purge.
- **Pre-flight checks** in the confirm modal: `DescribeInstancesHealth` impact count, last 3 events, traffic warnings for env-in-deploy / recently-changed / currently-Red.
- **Audit log** records dispatch + outcome of every action, naming the region the write actually went to. Since 0.30 that is the *row's* region, not the session's: under a `:region all` fan-out the selected environment is routinely in another region, and every write now dispatches there. Both lines of the dispatch/outcome pair carry the same region, so `ebman audit` correlates them correctly.
- **Everything about one environment uses that environment's region** (0.30) — Detail's tabs, `:why`, the DLQ viewer, the lint and drift reports, and every write. Before 0.30 these used the session's region, which under a fan-out showed another region's data under the right environment's name, and could dispatch a destructive action against a same-named environment at home.
- **Per-env / per-account safety pins** in `config.toml` (`safety.envs.NAME.read_only = true` / `safety.accounts.NAME.read_only = true`) refuse destructive actions even when the global `--read-only` toggle is off. Enforced CLI-side too: `ebman action` (rebuild / restart / terminate / deploy / rollout), `ebman audit replay`, and `ebman lint --fix` all refuse pinned targets before dispatch (exit 3).
- **Session-scoped fleet freeze**: `:freeze-deploys [REASON]` (or `:incident START "headline"`, which sets the same lock plus a header banner and audit lines) makes every destructive op refuse until `:thaw-deploys` / `:incident END` or exit. Since 0.28 it also persists a **pid-scoped marker** at `~/.cache/ebman/freeze.json` (0600) so *other processes* honour it — the MCP write tools and the CLI write paths (`ebman action`, `audit replay`, `lint --fix`) refuse while a live TUI session holds a freeze. The pid keeps the semantics session-scoped: a crashed TUI's marker is ignored (and cleaned up) by the next reader, so it can't leave a phantom freeze. Deleting the file is the manual unfreeze of last resort. Not durable policy — it lives and dies with the TUI session that set it.

  A multi-region `ebman action rollout` re-reads the freeze **between
  regions**, not just at the start. Declaring a freeze part-way through
  halts the regions that haven't dispatched yet; they are reported as
  `skipped (rollout halted)` alongside the ones that ran. Under
  `--parallel` it stops un-started waves — regions already in flight
  cannot be cancelled server-side and run to completion. This matters
  because there is no cap on `--wait-for-green` or on region count, so a
  sequential rollout can dispatch its last region hours after it began.
  A rollout halted this way exits **3**, the same code a failed region
  produces — it did not do what was asked, and a CI gate must not read
  it as success.
- **`ebman audit replay`** re-dispatches a previously-audited action and is itself audit-logged (`replay_of=`-tagged dispatched/completed lines); destructive verbs still require `--yes`.
- **`ebman mcp serve` is reads-only** (v1): no tool can dispatch a write. Redaction-by-default covers every tool that can carry option values — `get_option_settings`, `drift` (tf + live values of drifted secrets), and `audit_log` (`value=` extras from `:set-option` / `lint --fix` lines) — so an MCP client sees config *keys*, not secrets, with `--no-redact` as the explicit opt-out.
- **Drift redaction everywhere** (0.27+): the same contract covers `ebman drift` (`--no-redact` opts out — piped CI logs shouldn't collect drifted secrets by default) and the TUI `:drift` overlay (always on; the deliberate paths for reading real values are the Config tab / `:env list`).

## What's stored locally

- `~/.config/ebman/config.toml` — user configuration (see [configuration](configuration.md)). Written 0600 (operator-only), like everything else ebman writes: it carries `notify_webhook` and `accounts.*.external_id`.
- `~/.config/ebman/commands.toml` — optional plugin commands.
- `<repo>/.ebman/ebman.toml` — optional project-local pinning (profile / region / filter / runbooks). Walked up from cwd.
- `~/.config/ebman/state.toml` — persisted UI state: profile, region, filter, sort, grouping, redact, selected env, saved views, pinned envs, aliases, hidden columns, custom metrics. No credentials.
- `~/.cache/ebman/ebman.log` — application log; rotates as needed. Set `RUST_LOG=debug` for verbose output.
- `~/.cache/ebman/audit.log` — every dispatched action and outcome (account, profile, region, action, target). Rotates at 1 MiB to `audit.log.1`.
- `~/.cache/ebman/crash-*.log` — panic backtraces (10 most recent kept; 30-day TTL).
- Clipboard — `y` / `Y` / `^Y` / `^W` write via `arboard`.

## Privacy / telemetry

Ebman does not phone home. There is no usage telemetry, no anonymous identifier, no crash auto-reporting, no third-party analytics endpoint. Outbound HTTP from ebman itself is limited to: AWS (the SDK calls you'd expect); a single version-check ping to crates.io (`update_check.rs`), which crates.io logs as "client IP requested ebman version metadata" and nothing more; and three **operator-configured** integrations that are off until you wire them — `notify_webhook` (POSTs every audit line, including `cmd="…"` strings from `:ssm-run`, to your URL), `ebman lint --watch --webhook URL` (lint findings to your URL), and `:explain` / `ebman explain` (lint-issue titles, details, and env names to Anthropic or your Ollama endpoint; consent-gated via `explain.enabled`).

**Bug reports** are operator-driven via `:report-bug`. Ebman builds a scrubbed payload locally — version / OS / icons / theme / refresh interval / last 30 log lines / last 10 on-screen messages / latest panic backtrace — and runs it through a redactor that strips account IDs (any 12-digit ASCII number), ARNs (`arn:aws:*`), every env name + application name + CNAME currently in the in-memory table, and the active profile name. The result lands in an overlay where you see the exact bytes before they leave the machine. Two delivery paths, both initiated by you:

- `y` copies the scrubbed payload to clipboard. Paste into a new GitHub issue.
- `b` opens https://github.com/tombaldwin/ebman/issues/new in your browser with the body pre-filled via URL params (truncated at ~7900 chars so the URL stays under GitHub's 8k limit).

Ebman never sends the payload itself. The redactor isn't bulletproof — a freeform error message could still embed a customer name in an unscrubbable shape — which is why you review the payload before pasting / opening the browser.

Crash logs are written locally to `~/.cache/ebman/crash-*.log` by the panic hook (10 most recent kept, 30-day TTL). They're plain-text files; do whatever you want with them.

## Verifying a release binary

Release tarballs carry a signed build provenance attestation linking
each artefact to the workflow run that produced it. The `*.sha256` files
prove integrity; the attestation proves origin, which matters more for a
prebuilt binary that holds AWS credentials:

```bash
gh attestation verify ebman-v0.31.1-aarch64-apple-darwin.tar.gz --repo tombaldwin/ebman
```
