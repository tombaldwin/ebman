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

v1 is **reads-only** — no tool can dispatch a write. Tools (all take optional `profile` / `region`):

| Tool | Returns | Notes |
|---|---|---|
| `list_environments` | env list | same schema as `ebman envs --json` |
| `lint` | rule findings | EBL011 never fires here (no queue polling); EBL016/EBL020 are probe-gated and don't run — stated in the tool description |
| `get_option_settings` | one env's resolved options | env-var **values** + `DBPassword` redacted by default (keys visible) |
| `drift` | terraform drift report | tfstate discovered from the server's cwd; pass `tfstate_path` otherwise |
| `audit_log` | local audit entries | this machine's log only; default 100, cap 500 |
| `recent_events` | EB events, newest first | default 50, cap 200 |
| `list_versions` | app versions for an env's app | default 50 |
| `fleet_cost` | cached $/month per env | cache-only; never calls Cost Explorer |

Tool calls run concurrently with a 30s bound; expired-credential errors surface as the `aws sso login --profile X` hint so the agent can relay it. Failures come back as `isError` tool results, not protocol errors.
