# One environment for CLI and MCP writes

**Status: decided 2026-10-29, built with this note.** PLAN item 4 (1)
and (2), from the 0.47.0 release architecture review. Behaviour-neutral
for an operator; the change is in what a test can stand in.

## What was wrong

The CLI write bodies took what they read from the operator's machine
three ways:

- `action`, `rollout` and `audit replay` took a gate and the ambient
  profile as separate parameters, in different orders, and `rollout` a
  third closure for the freeze it re-reads between regions.
- `lint` took a `LintSetup` (config, disables, the profile) and a freeze
  closure.
- `freeze` and `thaw` took nothing and called `FreezeStore::real()`
  inline.

So the guard that every shell passes the real values
(`write_shells_pass_the_real_gate_and_the_ambient_profile`) had one
pattern per body, and the freeze was reached two ways: through the
store the TUI injects, and through `FreezeStore::real()` everywhere
else, including inside the production gate.

The second has a cost beyond style. Every test that reads the real
store shares one marker with every test that writes it. The writers
serialise on `freeze::MARKER_LOCK`; the readers do not, and every MCP
write test is a reader (`Server` read `FreezeStore::real()` in its write
path). A round-trip test holding a freeze could refuse an unrelated
test's write as frozen.

## Decision

**A `WriteEnv` value carries the three things a CLI write takes from
the machine,** and `WriteEnv::real()` is the one place that builds them
from it:

- `ambient_profile`: `AWS_PROFILE`, the account each account pin is
  checked against.
- `freeze`: the `FreezeStore`, read before each write, between rollout
  regions and before each lint fix, and declared and lifted by `freeze`
  and `thaw`.
- `gate`: decides one write **given the freeze as the env's store
  reads it**. Production's is `real_refusal`, which no longer reads
  `FreezeStore::real()` itself. A test gate that records the query or
  refuses at will keeps working, and a test that wants a freeze writes
  one to its own store.

Every write shell passes `WriteEnv::real()`, so the guard is one
pattern. `lint` takes the profile and the freeze from it and keeps
deciding with `write_refusal` over the config its setup loaded: that
config is injected already, and lint's precedence tests drive it.

**`Server` holds a `FreezeStore`,** as `App` does: the real one in
production, and one of its own per test server, so no MCP test reads a
marker another test wrote.

## Rejected

- **A data-only `WriteEnv` (config, freeze, profile) with the gate as a
  method.** It removes the gate seam, so every action, rollout and replay
  test that asserts *what the body asked the gate* would have to infer it
  from audit lines instead.
- **Keeping `rollout`'s freeze closure.** It is the freeze seen from one
  call site. With a store per test, a test declares a freeze in the store
  when the region it wants has dispatched, which is what happens in
  production.
