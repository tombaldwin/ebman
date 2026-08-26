# Architecture

A map of the codebase for anyone reading it for the first time — where things
live, how a keystroke becomes an AWS call, and the handful of rules that
aren't enforced by the compiler.

For build/test commands see [`docs/development.md`](docs/development.md); for
the AI-assisted-contributor rules see [`CLAUDE.md`](CLAUDE.md).

## Shape of the crate

`ebman` is lib + bin, but the library is not a general-purpose API. The
public surface is **212 items** as `cargo public-api` counts them with
no flags — 67 once auto-derived and blanket impls are omitted. In
hand-written terms: `App` plus its seven methods, the ten
`cli::*::run` entry points, `config::load`, and one function each from a
handful of other modules. Everything else is `pub(crate)`.

Count items, not modules. This section used to say "only the nine
modules `src/main.rs` needs are public", which was true and misleading —
a public module re-exports everything `pub` inside it, so those nine
modules carried 4565 items, the bulk of them from `pub mod app` alone.
Narrowing the modules had barely moved the number.

That is deliberate — a wide public surface made every internal refactor
a semver event, and `cargo-semver-checks` a tax on ordinary work rather
than a guard on the API anyone uses. `#![warn(unreachable_pub)]` in
`src/lib.rs` keeps it that way by catching the specific leak that is
invisible to an API listing: a `pub` item inside a `pub(crate)` module,
reachable through any public signature that names it.


`ebman` is lib + bin. `src/lib.rs` holds everything testable; `src/main.rs` is
a thin entry point that parses argv, sets up logging and the panic hook, and
either dispatches a headless subcommand or enters the TUI.

```
src/main.rs      argv, logging, panic hook, alt-screen lifecycle
src/lib.rs       module list + the `Tui` / `LogReloadHandle` aliases
├── app/         the TUI: state, event loop, everything it can do
├── ui/          rendering only — takes &App, returns nothing. One
│                module per surface (chrome / header / table / events /
│                footer / detail / overlays / action / dlq / shell /
│                help); `src/ui.rs` is the dispatcher and the map
├── aws/         every AWS SDK call, behind plain Rust types — one
│                module per service, so `aws/eb.rs` (the domain) is
│                separable from the twelve generic ones
├── cli/         headless subcommands (`ebman envs`, `action`, `ctl`, `mcp`)
├── lint/        the environment lint engine — `mod.rs` is the framework
│                (Rule / Issue / LintContext / run_rules / baseline),
│                `rules.rs` is one struct + impl per rule id
│                (see docs/lint-rules.md)
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
5. **[`src/ui.rs`](src/ui.rs)** — the render dispatcher. Its module doc
   maps each surface to the module that owns it; `draw` picks the
   layout for the current `Mode` and hands each region on.
6. **[`src/aws.rs`](src/aws.rs)** — the AWS boundary. Its module doc maps
   the per-service split; `aws/eb.rs` is the Elastic Beanstalk domain and
   the other twelve are generic AWS surface.

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

## The five rules

The compiler won't catch you breaking these, so each has something else
behind it. Rule 1 is enforced by the type system — the story of how that came
about is in `src/app/view_state.rs`. Rules 2, 3, 4 and 5 each have a test that
walks the tree (`every_spawn_declares_whether_it_is_per_env`,
`generation_guard.rs`, `key_arm_order.rs`, `no_tui_stdout.rs`). Four of the
five have bitten.

**1. Mutating view state means rebuilding the view.**
The table `ui` draws is a filtered, optionally grouped projection of
`environments`, plus two per-row lookup maps. It's cached, and
[`ViewState`](src/app/view_state.rs) is what keeps the cache honest: the
derived slices are private, changing `filter` or `grouped` marks them stale
automatically, and reading a stale one trips a `debug_assert` (and logs once
in release). Sort is private too: `App::set_sort` is the only way to change
it and it always re-sorts, so the header arrow can't disagree with the rows.
The inputs `ViewState` doesn't own — `environments`, `aliases`,
`latest_stacks`, the theme palette — still need an explicit
`view.invalidate()` before `rebuild_view()`.

One trap worth naming: `filter_mut()` marks the cache stale on the *borrow*,
not on an actual edit. If you only want to offer a key to the buffer, use
`filter_handle_key`, which marks it stale only when the key was consumed.

**2. Per-env work uses the row's region.**
`self.aws` is the *home* client — its region is `context.region`. Under a
multi-region fan-out the selected row is routinely somewhere else, so
anything about one environment goes through
[`App::client_for_env`](src/app.rs) (or `client_for_app` /
`current_env_client` / `detail_client` / `why_red_client` / `dlq_client`),
which resolves inside the spawned task. `spawn_aws_in` is the per-region
sibling of `spawn_aws`. Audit lines take the same region, so the journal
names where the write actually went — and a dispatch and its completion
have to agree. A test in `app/tests/refresh.rs` requires every remaining
`self.aws` spawn site to declare why account- or region-wide is right for
it.

**3. Async results check `generation`.**
Every spawned task captures the `generation` it launched at. If the operator
switches region, profile or account while it's in flight, `generation`
advances and the result is dropped rather than applied to the new context.

Structurally this is the best-defended of the five: there is one enforcement
point rather than a convention each handler follows. `AppMsg::generation()`
classifies every variant, `handle_msg` drops the message once before
dispatching, and the match is exhaustive — so the compiler *forces* a new
variant to be classified.

What the compiler can't force is classifying it *correctly*, and the cheapest
way to satisfy it is to append the variant to the `None` arm. That is a
one-line change with a plausible reason that quietly exempts a whole result
path. [`app/tests/generation_guard.rs`](src/app/tests/generation_guard.rs)
closes it from both ends: carrying a `gen: u64` field and being classified
`Some` have to agree, which needs no allowlist at all, and the three variants
carrying no `gen` each have a recorded reason that has to still be true. A
behavioural test covers the rest, since none of that says whether `handle_msg`
still acts on the classification.

**4. Guarded key arms come first.**
A `KeyCode::Char(c) if ctrl` arm must precede the unguarded `KeyCode::Char(c)`
arm for the same character. The compiler does not warn when the unguarded one
shadows it — both arms are reachable *patterns*, and it is only the guard that
makes one a subset of the other, so the chord silently does the unmodified
thing.

This one is checked by
[`app/tests/key_arm_order.rs`](src/app/tests/key_arm_order.rs), which parses
the tree with `syn` and compares arm positions *within each `match`*. It parses
rather than greps for a reason: judging order means knowing which `match` an
arm belongs to. A line-level scan can't tell, and the one written first
reported four violations in `input.rs`, every one of them false. Six
characters currently carry both forms (`r g y ] [ k`), so the rule has real
surface to police.

**5. Never print to stdout from the running app.**
The alternate screen swallows `println!`/`eprintln!` and they corrupt the
display. Use `tracing::*`; output goes to `~/.cache/ebman/ebman.log`. The same
reason is why a panic in the TUI is worse than a wrong frame — see the release
note in `ViewState::assert_fresh`.

Checked by [`app/tests/no_tui_stdout.rs`](src/app/tests/no_tui_stdout.rs) over
`src/app`, `src/ui` and `src/aws`. Not over `src/cli` or `src/main.rs`: the
headless subcommands print by design, and that same fact is what keeps the
guard honest — a companion test points the detector at the CLI and requires it
to find plenty, so "the TUI is clean" can't be confused with "the detector is
broken".

Print macros aren't the only way to reach the terminal, so direct writes to
`stdout()` / `stderr()` are checked too, against an allowlist of `(path, count,
why)`. There is one entry: the BEL byte `spawn_refresh.rs` writes to ring the
bell on a new red alert, which is a control character rather than display text
and so can't corrupt the screen. The count is part of the pin — file-level
granularity would let a second, unjustified write shelter behind the first
one's reason.

### Clusters that own their invariant

Two of `App`'s field groups are types rather than loose fields, and both
exist because the loose version shipped a bug.

[`ViewState`](src/app/view_state.rs) is rule 1 above: the derived slices
are private, so mutating an input marks them stale and reading a stale
one trips a `debug_assert`.

[`Costs`](src/app/costs.rs) is the same shape applied to Cost Explorer
data. `costs` / `costs_complete` / `costs_fetched_at` / `cost_enabled`
were four `pub(crate)` fields where three had to move together, and the
compiler could not say so — a truncated walk populated the map without
marking it partial, so the first failure's data survived the session
while every retry paid for twenty metered pages and discarded them. The
fields are private now and the three transitions are methods:
`set_complete` takes the timestamp, `set_partial` cannot take one
(a timestamp is what suppresses the retry), and `clear` is a settled
empty. The rule that used to live in a comment lives in the signature.

The other field clusters on `App` — tail, palette, throttle, status —
have no comparable invariant today. Group them when one appears, not
for tidiness.

## Writes and safety

Every mutating path funnels through one gate, but there are two of them
because the TUI and the CLI need different things from a refusal. In the
TUI it is `App::deny_write` / `deny_write_batch`
([`src/app/safety.rs`](src/app/safety.rs)), which sets a toast and
returns. In the CLI and the MCP server it is `cli::write_refusal`
([`src/cli/mod.rs`](src/cli/mod.rs)), which returns the reason so the
server can hand it to a client and `cli::refuse_write` can print it and
exit 3. Both resolve the same layers.
`--deny-write`, `safety.envs.NAME.read_only`, `safety.accounts.NAME.read_only`
and the freeze window are all resolved there, so there is exactly one place to
audit. Writes are journalled by [`src/audit.rs`](src/audit.rs).

Two of those confirms make the operator type the environment's name:
`Terminate` and the DLQ purge. Both are irreversible, and both are
checked by a test that refuses a near-miss rather than only accepting an
exact match — the 2026-08-26 mutation sweep found the purge's gate
completely untested, including the inversion that purges when the typed
name is *wrong*. `every_typed_confirmation_gate_names_its_test` in
`app/tests/safety.rs` keeps that from recurring: every typed-input
comparison in production is classified, a gate has to name the test that
covers it, and "not a gate" has to say why.

Destructive actions go through a confirm modal and then sit in an undo window
(`app/action_flow.rs`) before they're dispatched — `tick_pending_dispatch` is
what finally fires them.

## Testing

Tests live beside the code in `#[cfg(test)] mod tests` blocks; `app`'s are in
[`src/app/tests/`](src/app/tests/), one module per surface, with the shared
fixtures in [`support.rs`](src/app/tests/support.rs). AWS is stubbed via
`AwsClient::stub()`, and `App::for_tests` builds an `App` without touching the
network or the filesystem. Pure logic — parsers, formatters, the sorting and
diffing helpers — is deliberately extracted out of UI and event handlers so it
can be tested directly; `app/render.rs`, `app/text.rs`, `app/deploy_math.rs`
and `app/config_diff.rs` are all `&str`-in, `String`-out.

`src/demo_fixture.rs` builds a synthetic fleet, which is what `ebman --demo`
runs against and what the render tests draw.
