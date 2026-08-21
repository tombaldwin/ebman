# Contributing

Issues and pull requests are welcome. `ebman` is a working tool before it's a
project, so the bar is "does this make it better to use, and can I read it in
six months".

## Getting set up

```bash
git clone https://github.com/tombaldwin/ebman
cd ebman
cargo build
cargo test
```

You don't need AWS credentials to develop or to run the tests — `cargo test`
stubs the SDK, and `cargo run -- --demo` starts the TUI against a synthetic
fleet. Real credentials are only needed to exercise a live account.

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) first. It's short, and it covers the
four rules the compiler doesn't enforce — breaking one of those is the most
likely way for an otherwise-good change to be wrong.

## Before you open a PR

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs all three, and clippy is `-D warnings`, so a warning is a failure.

Beyond green:

- **New pure logic gets a test.** Any new helper, parser or formatter needs at
  least one `#[cfg(test)]` test covering the happy path and the obvious
  failure. If the logic is tangled up in a UI or event handler, extract it —
  that's the usual reason something is hard to test.
- **New `:command`s need three edits**: a registry entry in
  `src/commands.rs`, a dispatch arm in `src/app/dispatch.rs`, and a line in
  `docs/commands.md`. A test pins the first two together; the third is on you.
- **New keybindings go in `docs/keys.md`**, and new config keys in
  `docs/configuration.md`.
- **No hardcoded colours or paths.** Use `app.theme.*` and
  `util::config_dir()` / `util::cache_dir()` / `util::config_file(...)`.
- **No `println!` / `eprintln!` in the running app.** The alternate screen
  swallows them and they corrupt the display. Use `tracing::*`.

## What makes a change easy to accept

A PR that does one thing, explains why in the description, and comes with the
test that would have caught the bug. If you're changing behaviour an operator
depends on — a keybinding, a default, what a command writes — say so
explicitly, because that's the part worth arguing about.

Large refactors are fine, but open an issue first so we agree on the seam
before you spend the evening on it.

## Anything that writes to AWS

Every mutating path goes through `App::deny_write` in `src/app/safety.rs` —
TUI, CLI and MCP alike. If you add a new write, route it through there and add
it to the audit log. A write that bypasses the safety gate is the one class of
change that will be rejected on sight, because operators rely on
`--deny-write` and `safety.envs.*.read_only` actually meaning it.

## AI-assisted contributions

This project has been developed heavily with Claude Code, and
[`CLAUDE.md`](CLAUDE.md) is the working agreement for that — build green,
self-review, act on what the review finds, keep `BACKLOG.md` honest. If you're
using an agent, point it at that file. The same standards apply either way:
the code has to be readable and the tests have to be real.

## License

By contributing you agree that your contributions are dual-licensed under MIT
or Apache-2.0, matching the project.
