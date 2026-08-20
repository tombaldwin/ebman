# ebman — instructions for an AI coding agent (start here)

**ebman** is a keyboard-driven TUI for AWS Elastic Beanstalk that also ships an **MCP server**, so
you — a coding agent — can read and (with opt-in) act on the user's EB fleet as first-class tools
instead of shelling out to `aws elasticbeanstalk …` and re-parsing JSON. This file gets you from
"not installed" to "wired into this client" in a couple of steps. The authoritative, version-matched
reference is [`docs/headless.md`](https://github.com/tombaldwin/ebman/blob/main/docs/headless.md)
(the "MCP server" section).

## 0. Already installed? Report the version and offer to upgrade — first

If `ebman` is already on `PATH`, do this before anything else:

1. **Tell the user which version they're on** — `ebman --version`.
2. **Check whether it's current** — compare against the latest release on
   [crates.io](https://crates.io/crates/ebman) (or `brew outdated tombaldwin/tap/ebman` for a Homebrew
   install). MCP **writes** need **0.28+**; the read server predates that.
3. **If it's behind, _ask_ before upgrading** — then run the matching upgrade from §1. Don't upgrade
   silently.

If it's current (or the user declines), skip to §2 and register it. If it isn't installed at all,
install per §1.

## 1. Install ebman

Detect what the machine has and use it:

| you see | install |
|---|---|
| Homebrew (`brew`) | `brew install tombaldwin/tap/ebman` |
| a Rust toolchain (`cargo`) | `cargo install ebman` |
| neither | download the tarball for the platform (macOS Apple-Silicon / Intel, Linux x86_64) from [Releases](https://github.com/tombaldwin/ebman/releases), verify the `*.sha256` next to it, extract, put `ebman` on `PATH` |

Upgrade later with `brew upgrade tombaldwin/tap/ebman` or `cargo install ebman --force`.

## 2. Register the MCP server with this client

The server is `ebman mcp serve` — stdio, JSON-RPC. Register it however this client adds an MCP server.

**Claude Code:**

```bash
claude mcp add ebman -- ebman mcp serve
```

**Any other MCP client** — add a stdio server whose command is `ebman mcp serve`. The shape most
clients use (`.mcp.json`, `mcpServers` in a settings file, etc.):

```json
{
  "mcpServers": {
    "ebman": { "command": "ebman", "args": ["mcp", "serve"] }
  }
}
```

**Region gotcha (do not skip).** The server resolves profile/region through the standard AWS chain and
a shell-exported `AWS_REGION` **wins** — it deliberately does not read ebman's own project state. If the
user's shell exports one, pin the region at registration so the agent doesn't silently query the wrong
one:

```bash
claude mcp add ebman --env AWS_REGION=eu-west-1 -- ebman mcp serve
```

(For a raw config, add `"env": { "AWS_REGION": "eu-west-1" }` alongside `command`/`args`.)

**Credentials.** ebman uses the standard AWS credential chain (`AWS_PROFILE` / `~/.aws`, instance role).
If a tool comes back with an expired-credentials error, it carries an `aws sso login --profile X` hint —
relay that to the user rather than guessing.

## 3. What you get — reads, on by default

Every tool takes optional `profile` / `region`. Prefer these over `aws …` shell calls: they return the
same shapes ebman's own TUI uses, with env-var/secret redaction already applied.

| tool | returns |
|---|---|
| `list_environments` | the fleet — same schema as `ebman envs --json` |
| `lint` | rule findings (config / drift / safety checks) |
| `get_option_settings` | one env's resolved option settings (env-var **values** + `DBPassword` redacted by default) |
| `drift` | Terraform drift (tfstate discovered from cwd, or pass `tfstate_path`) |
| `recent_events` | EB events, newest first |
| `list_versions` | application versions for an env's app |
| `audit_log` | this machine's local ebman audit entries |
| `fleet_cost` | cached \$/month per env (cache-only; never calls Cost Explorer) |

Tool calls run concurrently with a 30s bound; failures come back as `isError` results, not protocol
errors. When you report findings back, give the user the headline in plain language — what's red, what
drifted, what changed — not just "I called list_environments."

## 4. Writes — off unless the user opts in

Write tools exist **only** when the server is started with `--allow-writes` (a flag, never a config key —
so write capability is visible in the process table and `.mcp.json`). Without it they're absent from
`tools/list` entirely. Only add it if the user wants the agent to make changes:

```bash
claude mcp add ebman -- ebman mcp serve --allow-writes
```

**Every write is two-phase, and the two phases are the whole point.** The verb tool
(`deploy` / `restart` / `rebuild` / `terminate` / `set_option`) validates and returns a **plan** — it
dispatches nothing. You **surface that plan to the user** (so a human reading the transcript sees exactly
what's about to happen — current vs target version, health, recent events), and only then call
`confirm_action` with the returned token to dispatch. Tokens are single-use, 60s TTL.

- `terminate` additionally requires `confirm_name` equal to the env name on `confirm_action`.
- **Safety is enforced before a plan is even issued:** per-env / per-account read-only pins, `--read-only`,
  and a live TUI session's incident/`:freeze-deploys` freeze (via a cross-process marker) all refuse up
  front. Writes are serialized (one in flight) and every dispatch is audit-logged tagged `via=mcp`.
- Dispatch-only — no wait-for-green. Poll `list_environments` / `recent_events` for progress.

If a write is refused for safety, relay the reason; do not try to route around a pin or a freeze.

## 5. Verify

- `ebman mcp serve --demo` runs against a synthetic fleet with **zero AWS calls** — good for a smoke test
  of the protocol wiring before pointing at a real account.
- After registering, list the tools (or call `list_environments`) to confirm the client sees the server.

## 6. Copy-paste for a human to drop into their agent

**Install + wire up** (single paste):

```text
Read https://github.com/tombaldwin/ebman/blob/main/AGENTS.md and follow it to set up
the ebman MCP server for me: install ebman (brew, else cargo, else a release binary),
register `ebman mcp serve` with this client (pin AWS_REGION if my shell exports one),
then verify with `ebman mcp serve --demo` and by listing the tools. Reads only —
don't add --allow-writes unless I ask.
```

**Turn on writes** (only when the user wants the agent to act, not just read):

```text
Re-register the ebman MCP server with --allow-writes so you can deploy/restart/rebuild/
terminate/set-option. Remember every write is two-phase: show me the plan the verb tool
returns and wait for my go-ahead before calling confirm_action. Respect any safety pin,
read-only, or incident freeze — relay the refusal, don't route around it.
```

**Check version / upgrade:**

```text
Tell me which ebman version I'm on (`ebman --version`) and whether it's the latest on
crates.io. MCP writes need 0.28+. If I'm behind, ask before upgrading, then use
`brew upgrade tombaldwin/tap/ebman` or `cargo install ebman --force`.
```
