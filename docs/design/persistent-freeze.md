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
- **One file per holder.** `freeze.d/persistent.json` for `ebman
  freeze`, `freeze.d/session-<pid>.json` for each TUI session. Readers
  refuse while any is live; a session's end removes only its own file,
  and a thaw only the persistent one. *Revised 2026-10-23 after review:*
  the first version kept one `freeze.json`, last writer wins, so a TUI's
  `:incident START` replaced an `ebman freeze` and its END then deleted
  it, and `ebman freeze` replaced a session's marker so a later thaw
  lifted the session's freeze too.
- **`freeze.json` stays, as a mirror for older binaries.** ebman before
  0.47 reads only that file, and removes it when its pid is dead. It
  names one LIVE holder (persistent first, then the earliest-declared
  session) and is marked `"mirror": true`, so a new reader does not
  count it twice. Every read puts it right when it is missing or names a
  holder that has gone (a reap, an older TUI's clear), so older readers
  are back under the freeze within one read by any 0.47+ process; one without the mark was written by an older TUI and is a
  holder of its own, never overwritten while its process lives. A
  persistent freeze's mirror carries `pid: 1`, which an older reader
  takes for a live owner that started before the marker, so it refuses
  writes. In a container whose pid 1 started after the marker, an older
  reader judges it reused and removes the mirror; the holder file is
  untouched and every 0.47+ reader still honours it.
- **Every TUI honours every live marker**, not only its own session's.
  A refusal names the marker's owner and its remedy. `:thaw-deploys`
  lifts this session's freeze or a persistent one, never another live
  session's.
- **"The freeze ended with its session" is said, not swallowed.** When a
  reader removes a stale session file, it writes `freeze.ended.json`
  (the marker, plus when it was found stale). With no freeze in force,
  the TUI shows it on startup, `ebman freeze --status` prints it, and
  `doctor` (MCP) reports it, for 24 hours or until the next freeze or
  thaw. It says how to re-declare one that should still hold. A session
  that ends cleanly (`:thaw-deploys`, `:incident END`, quit) removes its
  own file and records nothing: that was a decision.

## Known limits

- **A reader in another pid namespace** (an MCP server in a container
  sharing `~/.cache`, a home directory on NFS) cannot see a live TUI's
  pid, judges its session file stale, removes it and records an ended
  freeze that has not ended. The same was true of the single marker
  since 0.28; `ebman freeze`, which no pid owns, is the freeze to use
  across such boundaries.
- **A reaper racing a write** of the very file it judged stale removes
  the new write: it re-reads and compares before removing, but the
  window between the two is not locked. For a `freeze.d/` file that
  needs the same pid reused at that moment; for an older TUI writing
  `freeze.json`, the next 0.47+ read restores a mirror only if a 0.47+
  holder exists.
- **ebman before 0.47 records nothing** when it reaps a stale marker, so
  a freeze that ended with its session is said only when a 0.47+ reader
  found it.
- **A reaper racing a declare** can record an ended freeze just after a
  new freeze cleared the record. The TUI and `doctor` show the note only
  with no freeze in force, so it cannot tell an operator to declare a
  freeze that holds.

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
