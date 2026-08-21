# Architecture

A map of the codebase for anyone reading it for the first time — where things
live, how a keystroke becomes an AWS call, and the handful of rules that
aren't enforced by the compiler.

For build/test commands see [`docs/development.md`](docs/development.md); for
the AI-assisted-contributor rules see [`CLAUDE.md`](CLAUDE.md).

## Shape of the crate

`ebman` is lib + bin. `src/lib.rs` holds everything testable; `src/main.rs` is
a thin entry point that parses argv, sets up logging and the panic hook, and
either dispatches a headless subcommand or enters the TUI.

```
src/main.rs      argv, logging, panic hook, alt-screen lifecycle
src/lib.rs       module list + the `Tui` / `LogReloadHandle` aliases
├── app/         the TUI: state, event loop, everything it can do
├── ui/          rendering only — takes &App, returns nothing
├── aws.rs       every AWS SDK call, behind plain Rust types
├── cli/         headless subcommands (`ebman envs`, `action`, `ctl`, `mcp`)
├── lint.rs      the environment lint rules (see docs/lint-rules.md)
└── ...          config, state, audit, themes, plugins, LLM explain, ...
```

The dependency direction is one-way: `ui` reads `app`, `app` calls `aws`,
`aws` knows about neither. `cli` reuses `aws` and the pure helpers in `app`
without constructing an `App`.

## Where to start reading

1. **[`src/app.rs`](src/app.rs)** — the `App` struct and the event loop. Its
   module doc lists the invariants and maps every submodule.
2. **[`src/app/input.rs`](src/app/input.rs)** — the keymap. Follow any key you
   care about from here.
3. **[`src/app/dispatch.rs`](src/app/dispatch.rs)** — the `:command` router.
   Pure one-liner routing; the bodies live in `app/cmd_*.rs`.
4. **[`src/commands.rs`](src/commands.rs)** — the command registry. Adding a
   `:command` means one entry here plus one arm in `dispatch.rs`; a test pins
   the two together so help, palette and dispatch can't drift.
5. **[`src/aws.rs`](src/aws.rs)** — the AWS boundary.

## How a keystroke becomes an AWS call

```
crossterm event
  └─ App::handle_event            app/input.rs
      └─ App::handle_key          per-mode keymap
          └─ App::execute_command app/dispatch.rs   (`:` commands)
              └─ App::deny_write  app/safety.rs     ← every mutation passes here
                  └─ App::spawn_* app/spawn_*.rs    tokio task, captures `generation`
                      └─ aws::…   src/aws.rs
                          └─ AppMsg → App::handle_msg   app/msg.rs
                              └─ App::rebuild_view      app/view.rs
                                  └─ ui::draw           src/ui.rs
```

The loop itself is `App::run`: it selects over terminal input, the `AppMsg`
channel, and timers, mutates `App`, and redraws. AWS work never blocks it —
every call is a spawned task that reports back as an `AppMsg`.

## The four rules

None of these are enforced by the compiler. Three of them have bitten.

**1. Mutating view state means rebuilding the view.**
The table `ui` draws is a filtered, optionally grouped projection of
`environments`, plus two per-row lookup maps. It's cached, and
[`ViewState`](src/app/view_state.rs) is what keeps the cache honest: the
derived slices are private, changing `filter` or `grouped` marks them stale
automatically, and reading a stale one trips a `debug_assert`. The inputs
`ViewState` doesn't own — `environments`, `aliases`, `latest_stacks`, the
theme palette — still need an explicit `view.invalidate()` before
`rebuild_view()`.

**2. Async results check `generation`.**
Every spawned task captures the `generation` it launched at. If the operator
switches region, profile or account while it's in flight, `generation`
advances and the handler drops the result rather than applying data from the
old context to the new one. Every new `AppMsg` variant must do this.

**3. Guarded key arms come first.**
A `KeyCode::Char(c) if ctrl` arm must precede the unguarded `KeyCode::Char(c)`
arm for the same character. The compiler does not warn when the unguarded one
shadows it.

**4. Never print to stdout from the running app.**
The alternate screen swallows `println!`/`eprintln!` and they corrupt the
display. Use `tracing::*`; output goes to `~/.cache/ebman/ebman.log`. The same
reason is why a panic in the TUI is worse than a wrong frame — see the release
note in `ViewState::assert_fresh`.

## Writes and safety

Every mutating path — TUI, CLI and the MCP server alike — funnels through
`App::deny_write` / `deny_write_batch` in [`src/app/safety.rs`](src/app/safety.rs).
`--deny-write`, `safety.envs.NAME.read_only`, `safety.accounts.NAME.read_only`
and the freeze window are all resolved there, so there is exactly one place to
audit. Writes are journalled by [`src/audit.rs`](src/audit.rs).

Destructive actions go through a confirm modal and then sit in an undo window
(`app/action_flow.rs`) before they're dispatched — `tick_pending_dispatch` is
what finally fires them.

## Testing

Tests live beside the code in `#[cfg(test)] mod tests` blocks; `app`'s are in
[`src/app/tests.rs`](src/app/tests.rs). AWS is stubbed via
`AwsClient::stub()`, and `App::for_tests` builds an `App` without touching the
network or the filesystem. Pure logic — parsers, formatters, the sorting and
diffing helpers — is deliberately extracted out of UI and event handlers so it
can be tested directly; `app/render.rs`, `app/text.rs`, `app/deploy_math.rs`
and `app/config_diff.rs` are all `&str`-in, `String`-out.

`src/demo_fixture.rs` builds a synthetic fleet, which is what `ebman --demo`
runs against and what the render tests draw.
