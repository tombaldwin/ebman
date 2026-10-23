# A freeze that outlives the TUI session

*Design note, 2026-10-23. BACKLOG: "The incident freeze dies with the TUI
session".*

## The problem

`:freeze-deploys` and `:incident START` write `~/.cache/ebman/freeze.json`
with the TUI's pid, and every reader treats the marker as active only
while that pid is alive. That was deliberate (a crashed TUI must not
phantom-freeze the fleet), and it has two consequences nobody chose:

1. **Quitting, or losing the SSH connection, lifts a fleet-wide freeze
   mid-incident**, silently. The next reader deletes the stale marker
   and says nothing.
2. **There is no way to declare a freeze without a TUI**, so the
   operator who is in a terminal, a CI job, or an agent session cannot
   stop writes at all.

And one found while reading the code:

3. **A second TUI ignores the first one's freeze.** The TUI's write gate
   reads only its own session's `deploy_freeze`; it never reads the
   marker. MCP and the CLI honour a TUI's freeze; another TUI does not.

## The shape

- **`ebman freeze [--incident] [REASON…]`** declares a *persistent*
  freeze: the marker carries `"persistent": true` and holds until an
  explicit thaw, whatever process is or is not running.
  `ebman freeze --status` says what is in force. **`ebman thaw`** lifts a
  persistent freeze; it refuses (exit 3) to lift a live TUI session's
  freeze, which belongs to that session, and names it.
- **The marker keeps a `pid`.** A persistent marker is written with
  `pid: 1`, which every ebman since 0.28 reads as a live owner that
  started before the marker. An older binary (an MCP server spawned
  before an upgrade) therefore refuses writes under a persistent freeze
  rather than ignoring a marker it cannot parse: it fails closed.
- **Every TUI honours every live marker**, not only its own session's.
  A refusal names the marker's owner and its remedy. `:thaw-deploys`
  lifts this session's freeze or a persistent one, never another live
  session's.
- **"The freeze ended with its session" is said, not swallowed.** When a
  reader removes a stale session marker, it writes
  `freeze-ended.json` (the marker, plus when it was found stale). The
  TUI shows it on startup, `ebman freeze --status` prints it, and
  `doctor` (MCP) reports it, for 24 hours or until the next freeze or
  thaw. It says how to re-declare one that should still hold.

## Not doing

- **An MCP tool to declare a freeze.** Declaring one only restricts, so
  it would be safe to grant, but it adds a tool to the registry surface
  and no one has asked for it. Recorded as a BACKLOG candidate.
- **Making the TUI's own freeze persistent.** Its session scope is what
  stops a crashed TUI phantom-freezing the fleet; an operator who wants
  a freeze to outlive the session now has `ebman freeze` for that.

## Exit codes

`ebman freeze`: 0 declared or status printed, 1 the marker could not be
written (the freeze is NOT in force; said loudly), 2 usage.
`ebman thaw`: 0 lifted, or nothing to lift; 3 refused, a live TUI session
holds the freeze; 2 usage.
