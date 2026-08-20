# Headless interface

ebman ships two scriptable surfaces: a **control socket** for driving a running TUI, and **non-interactive subcommands** that don't need a running instance.

## `ebman ctl` — drive a running TUI

Launch ebman with `--control-socket PATH` to expose a Unix-socket interface. The `ebman ctl <op>` subcommand is the one-shot client (defaults to `~/.cache/ebman/control.sock`; override with `--socket PATH`).

```bash
ebman ctl state                   # JSON: mode, profile, region, account, envs, selected, ...
ebman ctl screen                  # plain-text dump of the current frame
ebman ctl key Down                # synthesise a keypress
ebman ctl key Ctrl+R              # … or a combo
ebman ctl cmd ':region eu-west-2' # run a : command (leading : optional)
```

Useful for integration tests, screenshot capture, scripted workflows.

## Non-interactive subcommands

These don't need a running TUI — they connect to AWS, do their thing, and exit. CI-friendly.

```bash
ebman envs --json                                                          # print env list as JSON
ebman action rebuild --env myenv --yes                                     # dispatch a rebuild
ebman action rollout --version LABEL --env NAME --regions r1,r2,r3 --yes   # sequential cross-region deploy
ebman action rollout ... --parallel [--max-concurrency N]                  # fan-out variant; implies --continue-on-fail
ebman action rollout ... --continue-on-fail                                # sequential but attempt every region
ebman action rollout ... --staggered 5m --wait-for-green 10m               # canary: wait Nm between regions
ebman lint   [--env NAME] [--regions r1,r2,r3] [--json]                    # rule-engine diagnostics; exit 3 on issues
ebman lint   --fix (--yes | --dry-run) [--rules ID1,ID2] [--env NAME]      # opt-in auto-remediation (EBL001/004/006 ship with fixes)
ebman lint   --watch [--interval 60s] [--json] [--severity warn]           # cron-friendly monitoring loop; Ctrl-C to exit
ebman lint   --watch --webhook URL                                          # POST findings to a webhook when the issue set changes
ebman lint   --probe-live                                                   # enable EBL016: live HTTP probe of each env's health-check URL
ebman lint   --baseline FILE                                                # snapshot today's issues for CI grandfathering
ebman lint   --against-baseline FILE [--json]                              # diff vs snapshot; exit 3 only on NEW issues
ebman drift  [--env NAME] [--regions r1,r2,r3] [--tfstate PATH] [--json]   # terraform drift report; exit 3 on drift
ebman drift  --no-redact                                                    # show drifted env-var values verbatim (redacted by default, 0.27+)
ebman audit  [--tail] [--since DUR] [--env NAME] [--action NAME] [--json]  # surface ~/.cache/ebman/audit.log for scripts
ebman audit replay LINE_ID [--yes]                                          # re-dispatch an audited action (timestamp-prefix ID)
ebman explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]        # LLM-backed explanation of a lint issue (opt-in)
ebman versions --env NAME [--json]                                          # application versions for env's app, newest-first
```

Exit-code convention (CI scripts can branch on these): `0` clean, `1` AWS-layer error, `2` usage error, `3` issues / drift found.

## MCP server (`ebman mcp serve`)

A stdio MCP server exposing ebman's read surface as tools, so Claude Code (or any MCP client) can query fleet state first-class:

```bash
claude mcp add ebman -- ebman mcp serve          # register with Claude Code
ebman mcp serve --demo                            # synthetic fleet, zero AWS — try the protocol
ebman mcp serve --no-redact                       # disable get_option_settings env-var redaction
```

The server resolves profile/region through the standard AWS chain (env vars beat
profile config) and deliberately does **not** read ebman's own `state.toml` — so a
shell-exported `AWS_REGION` pointing at another project's region silently wins. If
your shell exports one, pin the region at registration:

```bash
claude mcp add ebman --env AWS_REGION=us-west-1 -- ebman mcp serve
```

v1 is **reads-only** by default — no tool dispatches a write unless the server is started with `--allow-writes` (see Writes below). Tools (all take optional `profile` / `region`):

| Tool | Returns | Notes |
|---|---|---|
| `list_environments` | env list | same schema as `ebman envs --json` |
| `lint` | rule findings | EBL011 never fires here (no queue polling) and EBL016 doesn't run (no live HTTP probe) — stated in the tool description. The EBL020 X-Ray probe, the EBL018 WAF probe, and the EBL015 account-level pass all run (EBL015 only when not scoped to one env) |
| `get_option_settings` | one env's resolved options | env-var **values** + `DBPassword` redacted by default (keys visible) |
| `drift` | terraform drift report | tfstate discovered from the server's cwd; pass `tfstate_path` otherwise |
| `audit_log` | local audit entries | this machine's log only; default 100, cap 500 |
| `recent_events` | EB events, newest first | default 50, cap 200 |
| `list_versions` | app versions for an env's app | default 50 |
| `fleet_cost` | cached $/month per env | cache-only; never calls Cost Explorer |

Tool calls run concurrently with a 30s bound; expired-credential errors surface as the `aws sso login --profile X` hint so the agent can relay it. Failures come back as `isError` tool results, not protocol errors.

### Writes (`--allow-writes`, 0.28+)

Start the server with `--allow-writes` (flag only — never a config key, so write capability is visible in the process table and `.mcp.json`) and five write tools plus `confirm_action` appear in `tools/list`. Without the flag they're absent entirely.

```bash
claude mcp add ebman -- ebman mcp serve --allow-writes
```

**Every write is two-phase.** The verb tool (`deploy` / `restart` / `rebuild` / `terminate` / `set_option`) validates and returns a plan — it dispatches nothing:

```json
{"pending":true,"confirm_token":"…","expires_in_secs":60,
 "plan":{"action":"Deploy","env":"prod","current_version":"2026-31.0","target_version":"2026-32.0","health":"Green","recent_events":[…]},
 "next":"call confirm_action with the confirm_token to dispatch"}
```

The agent surfaces the plan (that's the point — a human reading the transcript sees what's about to happen), then calls `confirm_action` with the token to dispatch. Tokens are single-use with a 60s TTL; expired/reused/unknown → `isError` "re-plan required".

- **`terminate`** additionally requires `confirm_name` equal to the env name on `confirm_action` (the MCP strict-typed confirm; one retry per token).
- **`set_option`** caps at 10 settings, refuses namespaces not already in the env's config, and its plan shows old→new (old env-var values redacted).
- **Safety**: pins (`safety.envs.*` / `safety.accounts.*`), a live TUI session's `:freeze-deploys` / `:incident` (via the cross-process marker), read-only — all refuse before a plan is issued. Writes are serialized server-wide (one in flight). **Dispatch-only**: no wait-for-green; poll `list_environments` / `recent_events` for progress. Every dispatch writes audit lines tagged `via=mcp client=<name>` and fires the configured webhook.
- **Excluded by design**: rollout (compose from `deploy` per region + read polling — more inspectable in a transcript). Demo mode plans and "dispatches" synthetically — no AWS, no audit, no webhook.
