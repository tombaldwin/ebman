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

### `ctl key` spec vocabulary

Pieces are joined with `+` and are case-insensitive. Modifiers may appear
in any order and combine; the last key name in the spec wins.

| kind | accepted |
|---|---|
| modifiers | `ctrl` / `control` / `^`, `shift`, `alt` / `meta` / `option` |
| arrows | `up`, `down`, `left`, `right` |
| editing | `enter` / `return`, `esc` / `escape`, `tab`, `backtab`, `backspace`, `delete` / `del`, `space` |
| navigation | `home`, `end`, `pageup`, `pagedown` |
| function | `f1` … `f12` |
| character | any single character (case-sensitive: `J` ≠ `j`), or `Char(x)` to be explicit |

`shift+tab` is `backtab` — the two are the same key, and the TUI binds
BackTab to *reverse* field/tab/scope cycling, so they have to agree.

A spec naming no key (`ctrl` alone), an unknown name, or an out-of-range
function key (`f13`) is rejected rather than guessed at.

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
ebman completions <bash|zsh|fish>                                           # print a shell completion script to stdout (no AWS)
```

Exit-code convention (CI scripts can branch on these): `0` clean, `1` AWS-layer error, `2` usage error, `3` issues / drift found.

`3` also covers **refused and halted writes**, which is worth spelling
out because a script branching on "issues found" will otherwise
misread them:

- A write refused before it started — a `safety.envs.*` / `safety.accounts.*`
  pin, read-only mode, or an active `:freeze-deploys` / `:incident`
  marker from a live TUI session. Nothing was dispatched; the reason
  goes to stderr.
- A `rollout` **halted part-way** by a freeze declared while it was
  running. Regions already dispatched stay dispatched; the rest are not
  attempted. This exits `3` even though no region *failed*, because the
  rollout did not complete and reporting success would be a lie.

  The unattempted regions are listed on **stdout** alongside the ones
  that ran — `REGION\tskipped (rollout halted)` in text mode, or a row
  with `"ok":false,"err":"skipped (rollout halted)"` in `--json`. Only
  the halt *reason* goes to stderr. So a script can recover exactly
  which regions still need deploying from the normal output, without
  parsing stderr.

For a refused write, stderr names the cause, so a script that needs to
tell a pin from a freeze should read it rather than infer from the
code.

`ebman lint` exits `1` when a check could not run — for example an
`AccessDenied` on `iam:SimulatePrincipalPolicy`, which EBL020 needs.
The rule skips rather than firing a false positive, but the run is not
clean and does not claim to be: the reason goes to stderr. If a rule
isn't for you, `lint.disable` (or `--rules`) switches it off and its
probe stops running, so it can't affect the exit code.

### Shell completion (`ebman completions`)

`ebman completions <bash|zsh|fish>` prints a completion script to stdout — subcommands, global flags, and each subcommand's flags / verbs. It's static (no AWS round-trip), so it does **not** complete live environment names; that lives in the TUI command bar (`Tab` after `:diff` / `:config-diff` / `:rds-detach`). Install:

```bash
# zsh — drop it on $fpath, then restart the shell
ebman completions zsh  > "${fpath[1]}/_ebman"
# bash
ebman completions bash > ~/.local/share/bash-completion/completions/ebman
# fish
ebman completions fish > ~/.config/fish/completions/ebman.fish
```

### `ebman lint --json`, and knowing when a run was incomplete

The payload is `{"issues": [...], "degraded": bool, "degraded_reasons": [...]}`.

`degraded` is the field to check in CI. A probe that cannot run — an
`AccessDenied` on `iam:SimulatePrincipalPolicy`, a region that fails to
list — makes its rule **skip** rather than report a false positive,
which is the right call but means an incomplete run can otherwise look
like a clean one. `degraded_reasons` says which checks did not happen.

A degraded run exits non-zero, and the reasons are printed to stderr
**even under `--quiet`**. `--quiet` suppresses per-env chatter; it does
not suppress the reason a run failed, because a CI step that exits
non-zero with an empty log is the one outcome nobody can act on.

`--baseline` refuses to snapshot a degraded run outright: adopting one
would grandfather whatever the outage hid.

## MCP server (`ebman mcp serve`)

Every tool response is prefixed with a warning when the binary on disk
is no longer the one this server is running — what `brew upgrade` does
to a server already in flight. The handshake carries the version, but
the handshake has already happened, so `initialize` cannot tell a client
about an upgrade that came later. The notice names the running version
and says to reconnect (in Claude Code: `/mcp`, Reconnect), which
re-spawns the child from the current binary and re-runs the handshake.

The `initialize` response carries an `instructions` block naming the
capabilities ebman has that this surface does **not** expose — worker
queue depth and message peek, the `:why` correlation bundle, the live
log tail — and where they live in the TUI. An agent can only see the
tool list, so without that a TUI-only capability is indistinguishable
from one ebman lacks; on a real incident that sent the diagnosis out
into raw `aws sqs` calls while ebman had had a DLQ peek all along. A
test fails if a tool ships that makes the block stale.

The block also opens with the running build's version. That is **not**
redundant with `serverInfo.version`: a client consumes the handshake and
need not pass server identity on to the agent — Claude Code does not —
so authored content is the only version signal an agent is guaranteed to
see. The same distinction applies to anything added here: *the client
can see X* and *the agent can see X* are different claims, and the gap
is invisible from the server side. Tool annotations are the deliberate
exception, since they are aimed at the client.


A stdio MCP server exposing ebman's read surface as tools, so Claude Code (or any MCP client) can query fleet state first-class:

```bash
claude mcp add ebman -- ebman mcp serve          # register with Claude Code
ebman mcp serve --demo                            # synthetic fleet, zero AWS — try the protocol
ebman mcp serve --no-redact                       # disable get_option_settings env-var redaction
```

### Wiring it up (`ebman mcp setup`)

Not sure how to register it? Run `ebman mcp setup` — it prints the exact commands (the `claude mcp add` line, a `.mcp.json` snippet for other clients, and the `AWS_REGION` pin) from the installed binary. It's the secure way to hand setup to an agent: the instructions come from the signed binary you already installed, so there's no remote file to fetch, tamper with, or auto-execute. `--allow-writes` prints the write-enabled form; `--allow-writes=dlq_resend,dlq_delete` prints a narrow one. It's print-only — it never edits a client's config.

```bash
ebman mcp setup                    # reads-only registration instructions
ebman mcp setup --allow-writes     # the write-enabled form
ebman mcp setup --allow-writes=dlq_resend,dlq_delete   # only these two verbs
```

### Discovery (MCP Registry)

ebman is published to the official [MCP Registry](https://registry.modelcontextprotocol.io) as `io.github.tombaldwin/ebman`, so MCP clients and directories can discover it. The manifest is [`server.json`](../server.json) (a `cargo` package pointing at the crate); the `release.yml` `mcp_registry` job auto-publishes it on each release via GitHub OIDC, after the crate is live on crates.io (the registry verifies ownership via the `mcp-name:` marker in the crate README). Discovery is passive — nothing tells an agent to fetch and run anything.

The server resolves profile/region through the standard AWS chain (env vars beat
profile config) and deliberately does **not** read ebman's own `state.toml` — so a
shell-exported `AWS_REGION` pointing at another project's region silently wins. If
your shell exports one, pin the region at registration:

```bash
claude mcp add ebman --env AWS_REGION=us-west-1 -- ebman mcp serve
```


Every tool carries the standard MCP `annotations` — `readOnlyHint`,
`destructiveHint`, `idempotentHint`, `openWorldHint` — so a client can
tell a read from a destructive write and prompt accordingly without
parsing the description. `confirm_action` is annotated at its **worst
case** (destructive, non-idempotent): it dispatches whatever is pending,
which may be a terminate, and its token is single-use.

These are hints. They are there so a client asks the right question, not
as a control — the boundary is IAM. What actually gates the write
surface is the operator's answer to the confirmation dialog, or on a
client that cannot be asked, the `--allow-writes` flag.

Whether the write tools appear depends on your client: one that can put a question to you mid-request gets them by default, one that cannot needs `--allow-writes`. Either way no write dispatches without a confirmation — see [Writes](#writes) below. Tools (all take optional `profile` / `region`):

| Tool | Returns | Notes |
|---|---|---|
| `drift` | tf-vs-live comparison | resolves state as `tfstate_path` arg → `terraform.state_path` in config → discovery from cwd. For a remote backend (HCP, S3, Consul), `terraform state pull > state.json` and set the config key — ebman reads state files and does not talk to backends. The report carries a `state` block (`serial`, `lineage`, `pulled_at`) because a pulled file goes stale silently: a drift report against a six-day-old `state.json` looks exactly like one against current state. ebman cannot tell whether a serial is the latest, so it names which one it compared |
| `why` | everything bearing on one env's health in one call: events, alarms, instances, dead-letter queue + messages, recent versions | the TUI's `:why` overlay. Deliberately **not** a narrative — adjacent facts, conclusion left to the reader. A section that failed to fetch is `null` with its reason in `errors`, never an empty array: "could not look" and "nothing there" are opposite conclusions |
| `recent_logs` | the **newest** log lines for an env, with `complete`. **Not redacted** — log lines are free text and ebman's redaction is namespace-and-key based | `FilterLogEvents` returns matches oldest-first, so a truncated window hands back the OLDEST lines and answers "is this still running?" with evidence from hours ago. `complete: false` means exactly that — narrow `since_minutes` rather than trusting the result. It also goes false when an env has more than 8 log groups and the fan-out was capped, since a dropped group might hold the newest lines |
| `dlq_undo` | put back a message THIS server deleted, within 10 minutes | single-phase, no plan/confirm — it is the least destructive action here and is reached for under time pressure. Rides along with any write grant, like `confirm_action`, since it can only ever undo a delete that was already authorised. CAVEATS: held in memory by this server only, so a restart loses them and nothing deleted by another process is there; a purge is never recoverable; and the restore is a re-send, so the message id changes, `receive_count` resets to 0 and the enqueue time becomes now. Body and attributes return verbatim |
| `dlq_resend` / `dlq_delete` / `dlq_purge` | dead-letter management, two-phase, write surface only | resend and delete name messages by id from a `worker_queues` peek — `message_id` for one, or `message_ids` for up to 10 in a single plan and a single confirmation. Over 10 is refused rather than truncated: the cap is what keeps the list readable in the dialog, and dispatching a subset while reporting the whole is the worst outcome available. Each message is dispatched and audited separately, and the result reports per message — one that vanished between plan and confirm is a failed item among successes, not a failed batch. If *nothing* succeeded the call is an error, so an agent branching on `isError` cannot read a no-op as a delete. The plan carries the **id**, not a receipt handle — a handle expires with the peek's 5s visibility timeout while a confirm token lives 60s, so confirm re-reads the queue, finds that id, and refuses if it is gone rather than acting on whatever is at the head. `dlq_purge` is the bluntest: arguably more destructive than `terminate`, since an environment can be rebuilt from its configuration and a purged message cannot |
| `worker_queues` | main + dead-letter queue depth for one env; with `peek`, the dead-lettered messages and their `beanstalk.sqsd.*` task attributes | answers EB's "1 message in Dead Letter Queue", which names no task. `dead_letter_queue.origin` distinguishes a queue EB reported from one derived by the `<main>-dlq` convention. A peek is non-destructive but increments each returned message's `receive_count`, which counts every receive and is **not** a retry count |
| `list_environments` | env list | same schema as `ebman envs --json`: `name`, `application`, `tier` (Web/Worker), `status`, `health`, `platform`, `cname`, `version_label`, `updated` (EB's `DateUpdated`, RFC3339 or null), `region` (or null). **`updated` is the environment's last change, NOT a health-since** — an env that went Yellow on its own still reports the last config change, so do not read it as when the health moved. |
| `lint` | rule findings | EBL011 never fires here (no queue polling) and EBL016 doesn't run (no live HTTP probe) — stated in the tool description. The EBL020 X-Ray probe, the EBL018 WAF probe, and the EBL015 account-level pass all run (EBL015 only when not scoped to one env) |
| `get_option_settings` | one env's resolved options | env-var **values** + `DBPassword` redacted by default (keys visible) |
| `doctor` | what THIS connection can and cannot do | reports the ebman build, what your client declared at handshake (elicitation), the write surface in force, and the operator's standing restrictions. Call it before reporting a capability as missing: a feature ebman lacks, a feature your CLIENT lacks, and a thing the operator forbade all look identical from the agent's side. Touches neither AWS nor the filesystem, so it answers even when what it describes is broken |
| `audit_log` | local audit entries | this machine's log only; default 100, cap 500 |
| `recent_events` | EB events, newest first | default 50, cap 200 |
| `list_versions` | app versions for an env's app | default 50 |
| `fleet_cost` | cached $/month per env | cache-only; never calls Cost Explorer |

**Exit codes.** `serve` exits 0 when stdin closes, and 2 on any usage
error — an unknown flag, a `--allow-writes` value naming a verb that
does not exist, `--allow-writes=` with no verbs after it,
`--allow-writes` or `--read-only` given more than once, or
`--read-only` combined with `--allow-writes`. All are refused at startup
rather than at first use, so a bad registration fails when you make it
rather than the first time an agent tries to write. `setup` is the
same: 0, or 2 on a usage error.

Tool calls run concurrently with a 30s bound — except `confirm_action` on a client that can be asked, which gets five minutes, because the bound is waiting on a person rather than on AWS; expired-credential errors surface as the `aws sso login --profile X` hint so the agent can relay it. Failures come back as `isError` tool results, not protocol errors.

### Writes

#### If your client can ask you (0.42+)

If your client declared **elicitation** at handshake — it can put a
question to you in the middle of a request — the write tools are
available with no flag and no restart, and every `confirm_action` shows
you the action and waits for your answer.

That is the same bargain as the TUI. There, you press `r`, read the
confirmation, and press `y`. Here your agent proposes the action, you
read the same foreclosure line, and you accept or decline. The flag was
never what made a write safe; a person seeing it was, and once the
client can show you one there is nothing left for the flag to carry.

A decline is final. The plan is spent, and the instructions tell the
agent not to re-plan the same action — a tool that lets an agent retry
a refusal until you tire of reading it is worse than one that never
asked.

Run `doctor` to see which case you are in: it reports what your client
declared and what this connection may do.

**What it does not change.** Standing restrictions are untouched:
`safety.read_only`, pins, freeze and `deny_write` all still refuse, and
the ask never appears because there is nothing to approve. An operator
who has said no in config has said no, and no dialog overrides it.
Config may only say no — there is deliberately no config key that
grants.

**An explicit narrow grant is not widened.** `--allow-writes=dlq_delete`
is you saying "only this", so it stays that way even on a client that
can ask. Elicitation supplies the default where you set none; it does
not overrule one you set.

#### Several messages, one confirmation (0.42+)

Every write asks, so a ten-message clean-up would be ten dialogs — and
a person answering the same dialog ten times stops reading it, which
costs more safety than the asking bought. So `dlq_resend` and
`dlq_delete` take `message_ids` (an array, up to 10) and cover the set
with **one** plan and **one** confirmation:

```json
{"env": "poly-batch", "message_ids": ["d3b0…0001", "5d41…0002"]}
```

Every confirmation also names the identity the write would go out
under — `as arn:aws:sts::123456789012:assumed-role/Admin/sess` — and
the plan carries it as an `identity` object. If `sts:GetCallerIdentity`
is denied the plan is not refused: `identity` is null, `identity_error`
says why, and the dialog says the identity is UNKNOWN. A policy can
deny STS while Elastic Beanstalk works.

The confirmation **enumerates** them — task and id per line — rather
than saying "2 messages". A count is something to agree with; a list is
something to read.

Three rules worth knowing before you hit them:

- **Over 10 is refused, never truncated.** Dispatching a subset while
  reporting the whole is the worst available outcome. Name fewer, or
  use `dlq_purge` if the intent is to empty the queue — that is one
  deliberate action with one honest foreclosure line.
- **Ambiguous requests are refused, not guessed.** `message_id` and
  `message_ids` together, an empty array, or the same id twice all
  fail with a reason. A silently deduplicated list would show you a
  count that does not match what happens.
- **Failure is per message.** One id consumed or redriven between plan
  and confirm is reported as that item failing; the rest still go. The
  result carries `succeeded`, `failed`, and a line per message
  including the ones that did not work — an absent item would be
  indistinguishable from a truncated list. If nothing succeeded the
  whole call is an error.

Every message gets its own audit line naming its own id and task. For
a delete that log is the only place the answer still exists.

#### Turning it off: `--read-only` (0.42+)

If you want your agent read-only regardless of what its client can do:

```bash
claude mcp add ebman -- ebman mcp serve --read-only
```

The write tools are absent, exactly as they are on a client that
cannot be asked and has no flag. This is a **server** control, so it
does not touch your TUI — `safety.read_only` in `config.toml` is the
one that refuses writes everywhere, including your own hands.

`--read-only` together with `--allow-writes` is refused at startup.
Picking a winner silently would hand you a posture you did not choose,
whichever way it went.

#### If it cannot: `--allow-writes` (0.28+)

Clients without elicitation work exactly as before. Start the server
with `--allow-writes` (flag only — never a config key, so write
capability is visible in the process table and `.mcp.json`) and the
write tools plus `confirm_action` appear in `tools/list`. Without the
flag they're absent entirely. Nobody can be asked on such a connection,
so the flag is the only signal of intent available and it still carries
the whole grant.

```bash
claude mcp add ebman -- ebman mcp serve --allow-writes
```

#### Granting only some verbs (0.40+)

`--allow-writes=verb,verb` grants exactly those and nothing else:

```bash
claude mcp add ebman -- ebman mcp serve --allow-writes=dlq_resend,dlq_delete
```

An agent triaging a red environment can then clear the dead-lettered
message holding it red, without also holding `terminate` over every
environment the credentials reach. It composes with the pins: a narrow
grant plus `safety.envs.prod.read_only = true` is "delete DLQ messages
anywhere except prod".

**You make this edit, not your agent.** The grant is small enough to
look like a routine change you could hand off, and that is exactly the
trap: the party that benefits from a grant is not the party that makes
it. It is also enforced rather than merely discouraged — an agent in
Claude Code that tries to add this flag to its own MCP config has the
edit refused outright, classified as self-modification. Not a prompt it
can accept; a denial. (Reported first-hand from one client; treat the
principle as general and the specific behaviour as that client's.)

So the instruction is: **operator edits the config, operator restarts
the client.** Never "ask your agent to add `--allow-writes=…`" — in at
least one major client that describes something which cannot happen.

**Changing the FLAG needs a client restart; upgrading the BINARY does
not.** Two different questions, and conflating them sends people the
long way round. On a client that can be asked, this whole problem is
moot — there is no flag to change.

A stdio MCP server is a child process, so a reconnect terminates and
respawns it, which re-executes `ebman` and picks up whatever is now on
PATH. A version upgrade therefore needs only `brew upgrade` and a
reconnect.

**Measured, 2026-09-19**, in Claude Code: a session running ebman
0.38.0 upgraded to 0.39.0 on disk, the operator ran `/mcp` Reconnect,
and the `instructions` block afterwards read `ebman 0.39.0`. No client
restart at any point.

Whether that respawn uses NEW argv, or the spawn command cached at
session start, is the part nobody has established.
The failure mode if it caches matters: the server comes back without
the verb, `tools/list` omits it, and that reads as "the feature does
not work" rather than "the flag has not taken effect yet". A full
client restart is correct under either behaviour, so prefer it. If a
verb you granted is missing from `tools/list`, restart before
concluding anything.

The nameable verbs are `deploy`, `restart`, `rebuild`, `terminate`,
`set_option`, `dlq_resend`, `dlq_delete`, `dlq_purge`. `confirm_action`
is not one — it is the second phase of every write and rides along with
any grant.

An ungranted verb is **absent** from `tools/list` and **refused** at
dispatch, at both the plan and confirm phases. Absent alone would leave
it callable by a client holding a list cached from a wider grant.

Calling one anyway returns a refusal naming the flag that would grant
it — **not** `unknown tool`, which would tell the agent ebman lacks the
verb. The attempt is audited as `rule=not_granted`: a client reaching
an unadvertised verb is working from a stale list or probing, and that
is invisible any other way. A name that is not a verb at all is still a
`-32602` protocol error.

An unknown verb is a startup error naming the typo and listing what is
known — `--allow-writes=dlq_delte` fails rather than silently granting
nothing (which would look exactly like a working narrow grant until the
first write) or silently granting everything.

Bare `--allow-writes` still means every verb, so existing registrations
are unaffected. The `initialize` instructions block states the grant, so
an agent can tell "not granted" from "ebman can't do this" and ask you
to widen it rather than reporting a capability gap.

`ebman mcp setup --allow-writes=dlq_resend,dlq_delete` prints the
matching `.mcp.json`.

**Every write is two-phase.** The verb tool (`deploy` / `restart` / `rebuild` / `terminate` / `set_option` / `dlq_resend` / `dlq_delete` / `dlq_purge`) validates and returns a plan — it dispatches nothing:

```json
{"pending":true,"confirm_token":"…","expires_in_secs":60,
 "plan":{"action":"Deploy","env":"prod","current_version":"2026-31.0","target_version":"2026-32.0","health":"Green","recent_events":[…]},
 "next":"call confirm_action with the confirm_token to dispatch"}
```

The agent surfaces the plan (that's the point — a human reading the transcript sees what's about to happen), then calls `confirm_action` with the token to dispatch. Tokens are single-use with a 60s TTL; expired/reused/unknown → `isError` "re-plan required".

- **`terminate`** additionally requires `confirm_name` equal to the env name on `confirm_action` (the MCP strict-typed confirm; one retry per token).
- **`set_option`** caps at 10 settings, refuses namespaces not already in the env's config, and its plan shows old→new (old env-var values redacted).
- **Safety**: pins (`safety.envs.*` / `safety.accounts.*`), a live TUI session's `:freeze-deploys` / `:incident` (via the cross-process marker), read-only — all refuse before a plan is issued. Writes are serialized server-wide (one in flight). **Dispatch-only**: no wait-for-green; poll `list_environments` / `recent_events` for progress. Every dispatch writes audit lines tagged `via=mcp client=<name> can_ask=<bool>` (the last records whether the client declared MCP elicitation support) and fires the configured webhook. Every *refusal* writes a `stage=refused` line naming the rule and the remedy, at both the plan and confirm phases — so an agent repeatedly attempting a pinned environment is visible rather than silent.
- **Excluded by design**: rollout (compose from `deploy` per region + read polling — more inspectable in a transcript). Demo mode plans and "dispatches" synthetically — no AWS, no audit, no webhook.
