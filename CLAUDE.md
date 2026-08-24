# AI working rules for ebman

This file is read by Claude Code (and similar agents) on session start. Follow it.

## What this project is

`ebman` is a Rust + ratatui TUI for AWS Elastic Beanstalk, k9s-styled. Source under `src/`. Backlog in `BACKLOG.md`. Tests live alongside the code in `#[cfg(test)] mod tests` blocks.

## Mandatory loop for autonomous work

When the user asks for autonomous work (e.g. "run autonomously", "build all the above", "next", or any directive to ship multiple items without per-step approval), you **must**:

1. **Build green before claiming done.** `cargo build` must succeed with no new warnings. `cargo test` must pass. If either fails, fix it before moving on.

2. **Self-review every meaningful change.** After each substantive feature or pass, perform a code review against the changes you made and surface bugs, design issues, dead code, missed edge cases, and inconsistencies. The review goes in your message back to the user — not just internal thinking.

3. **Act on review findings — don't just list them.** Anything you identify in a self-review that is a bug, an inconsistency, dead code, or a borderline-design choice that could be tightened *must be fixed in the same turn*, unless the user has been asked and has explicitly deferred it. "I noticed X but left it" is not acceptable in autonomous mode.

4. **Add tests for new pure logic.** Any new helper / parser / pure function (sorting, filtering, formatting, parsing config, etc.) needs at least one `#[cfg(test)]` test covering happy path and obvious failure modes. Extract pure logic out of UI/event handlers when needed to make it testable.

5. **Update `BACKLOG.md`** when items move from pending → done, or when new items are discovered. Keep the "Done" and "Backlog" sections in sync with reality.

## Stop conditions — skip and continue, don't halt

If an autonomous-mode item hits any of these, **skip it and move to the next item in the run.** Don't halt the whole run; don't ask permission mid-stream. Record the skip in the final summary message so the user can pick it up later.

- A destructive AWS action that wasn't pre-authorised.
- A refactor that touches more than ~3 modules and isn't clearly required by the current task.
- A design trade-off with no obvious winner (more than one reasonable shape).
- Repeated failure on the same compile error after 2 attempts.
- Any other hard blocker (missing credentials, missing dep, third-party API change, etc.).

The final message must explicitly list **skipped items** alongside what shipped, what was reviewed-and-fixed, and what tests were added. Each skip needs a one-line reason so the user can decide whether to retry or drop it.

## House conventions (don't re-discover by breaking)

- **Match-arm order matters.** Guarded `KeyCode::Char(...) if Ctrl` arms must come before the unguarded `KeyCode::Char(...)` arm for the same character. Compiler does not warn on shadowing here.
- **State mutations that affect the view must call `App::rebuild_view()`.** `App::view` is a `ViewState` (`src/app/view_state.rs`): the derived slices are private, mutating `filter` / `grouped` through it marks them stale automatically, and reading a stale one trips a `debug_assert`. Sort is private too — `App::set_sort(key, desc)` is the only way in and it always re-sorts. The inputs `ViewState` does *not* own — `environments`, `aliases`, `latest_stacks`, `theme` — still need an explicit `self.view.invalidate()` before `rebuild_view()`. If you need to *ask* the filter buffer something without dirtying the cache, use `filter_handle_key` / `filter()`, not `filter_mut()`.
- **Per-env work uses the row's region.** `self.aws` is the HOME client (`context.region`). Under a multi-region fan-out the selected row is usually elsewhere, so anything about one env goes through `App::client_for_env` / `client_for_app` / `current_env_client` / `detail_client` / `why_red_client` / `dlq_client`, or `spawn_aws_in`. Audit lines take the same region, and a dispatch and its completion must agree. `every_spawn_declares_whether_it_is_per_env` pins the exceptions.
- **Async-result handlers check `generation`.** Every spawned task carries the generation it was launched at; if the App's `generation` has advanced (context switch) the result is dropped. New `AppMsg` variants must follow this pattern.
- **No hardcoded colours.** Use `app.theme.*`. Hardcoded `Color::Cyan` / `Color::Gray` is a regression.
- **No hardcoded paths.** Use `util::config_dir()` / `util::cache_dir()` / `util::config_file(...)`.
- **Wrapped string literals need a `\` continuation.** A literal split across lines without one embeds the newline *and* the next line's indentation, so the operator sees a 30-space hole mid-sentence — and the TUI's status/error bar is one line, so a narrow terminal pushes the rest off-screen. This has shipped twice. Assert on the *rendered* message in a test, not on the literal.
- **No `println!` / `eprintln!` in the running app** — the alternate screen swallows them and they corrupt the TUI. Use `tracing::*` macros; output goes to `~/.cache/ebman/ebman.log`.
- **The animation ticker is gated on `loading_since.is_some()`.** Don't move work into it that needs to run while idle — add a separate ticker.
- **`State` and `Config` parsing is in pure `parse(&str)` functions.** Keep the I/O wrappers thin so the parsers stay unit-testable.
- **Tests must not touch the developer's machine.** `util::config_dir()` / `cache_dir()` redirect under `cfg(test)` and `yank` is a no-op there. Three separate times a test wrote to the real `~/.cache/ebman`, the real `~/.config/ebman/state.toml`, or the real clipboard. Any new side channel to the host gets the same treatment plus a guard test.
- **A change that adds a distinction must be chased to every call site.** Three times in one week a correct fix was undone at its call sites: EBL010's `Option` was flattened by `unwrap_or_default()` at all three callers (turning "silently skip" into "false-positive on every env" — worse than the bug), a client cache was added that nothing read, and a region sweep missed the one place deliberately bent around the old behaviour. Types surface the call sites but don't stop each one "fixing" the error by destroying the distinction. So: after widening a type, grep for what destroys it — `unwrap_or_default`, `unwrap_or`, `.ok()`, empty-slice defaults — and pin the result with a guard test.
- **Test production code, not a copy of it.** An extracted helper whose call site kept its inline copy means the test pins the copy while production drifts; `-D warnings` catches it as dead code, which is one reason clippy is not optional here. Same rule for a test that exercises a data structure the production path doesn't reach — pin the wiring, not just the branch.
- **A mutation experiment must be proven to have applied.** `cargo fmt` reformats — a multi-line match arm becomes one line — so a `replace` written against the pre-fmt shape silently no-ops and the test "passes" the mutation. That happened here: a fix was reported as shipped while the code path still called the old function, and the test only proved a neighbouring property. Assert the string matched, or count occurrences before and after, before believing a mutation result.
- **Use `scripts/mutate.sh` for mutation experiments.** It encodes the four rules below that each cost a real mistake: back up with `cp` (never `git checkout`), prove the mutation applied (a `sed` written against the pre-`fmt` shape silently no-ops and the test "passes" the mutation), treat a run with no `test result:` line as inconclusive rather than a pass (a mutation that doesn't compile looks identical to one the tests ignored — this was misread twice in one session), and restore on every exit path.
- **Clean up after a mutation sweep.** Each experiment is a fresh compile and cargo never garbage-collects the dep artefacts left behind: one session accumulated **427 GiB** in `target/` and filled the disk mid-run. `scripts/mutate.sh` warns past 40G; `cargo clean` fixes it and a fresh `target/` is about 7G.
- **Never `git checkout <file>` while it carries uncommitted work** — it is a silent destructive revert of everything unstaged in that file, not an undo of the last edit. To back out a mutation experiment, `cp` the file first and restore from that copy; back up the file the mutation actually edits, not the one you meant to edit. Commit before experimenting.

## What "done" looks like for each landed item

- Code compiles, no new warnings.
- All tests pass.
- New pure logic has tests.
- `BACKLOG.md` reflects the change.
- Final message to the user explicitly lists: what shipped, what was reviewed-and-fixed in the same pass, what tests were added, **what was skipped (with one-line reasons)**, and any follow-ups deliberately deferred.

## When not in autonomous mode

When the user is driving step-by-step (asks "what do you think?", "next?", per-item approvals), prefer brief recommendations over large changes. Don't trigger the full mandatory loop above; instead, propose and await direction. Still keep `cargo build` and `cargo test` green at every commit point.

## Release procedure

When the user asks to cut a release (e.g. "tag 0.X", "ship the release", "prepare 0.X for release"), in addition to the version-bump / `CHANGELOG.md` / `Formula/ebman.rb` SHA-update mechanics:

1. **Audit `docs/` against the shipped code before tagging.** The `src/commands.rs` registry is the source of truth for command help — CI pins it to the dispatch arms, but it does *not* pin it to `docs/commands.md`. Diff the registry's command names against `docs/commands.md` and add any that shipped this cycle. Then walk:
   - `docs/keys.md` — every new keybinding added in the lineup is in the table (normal mode / Detail / DLQ section, whichever applies).
   - `docs/configuration.md` — every new `config.toml` / `.ebman/ebman.toml` key in the lineup is documented; TOML examples actually parse.
   - `docs/headless.md` — every new top-level `ebman <subcommand>` (from `src/main.rs`'s dispatch) is mentioned.
   - `docs/fonts.md` / `docs/safety-and-privacy.md` / `docs/development.md` — spot-check for stale references to commands, files, or behaviours that changed this cycle.
   - `README.md` — any feature it specifically calls out (e.g. the Triage workflow's `:rollback`) still works as described.

2. **Code-review the lineup against the previous tag before tagging.** Two parallel agents, sharp briefs, different focuses so the work doesn't overlap:
   - **Architecture + refactor agent.** Read the changed modules; assess whether `src/app.rs` / `src/main.rs` growth or new module placement is sustainable; identify refactor candidates with file:line refs + effort estimates. Don't propose new features.
   - **Bugs + correctness agent.** Read the actual files (don't infer from naming); scan new/changed code for race conditions (generation guards on every spawned `AppMsg`), broken invariants (`rebuild_view` after view-state mutation; match-arm order for guarded Ctrl chords), missed safety paths (`deny_write` / `safety_envs` / `safety_accounts` honoured by every dispatch site, CLI included), dead code, security issues, CLI exit-code matrix vs docs, HTTP error paths in `llm.rs`, audit-log writer/parser consistency. Report Critical / Important / Minor with file:line refs + suggested fixes.
   - **Act on findings — don't just list them.** Same rule as the autonomous-mode loop: Critical and Important findings get fixed *before* tagging unless the user has explicitly deferred. Bundle the fixes into the release commit (or a same-day patch tag like `0.X.1` if the release already shipped). Architecture refactors usually defer to the next release with the user's go-ahead.
   - **0.14.0 caught two real bugs this way** — `lint --fix` bypassing `safety.envs.*.read_only` and rollouts losing failed-region audit lines. Both went out same-day as 0.14.1. Worth the 5 minutes every cycle.

3. **Enumerate the breaking changes for the changelog.** The CI
   `cargo-semver-checks` job answers "is the declared bump big enough?"
   — it catches an API break shipped as a patch. It does **not** list
   what broke once the version is already at the breaking position: a
   declared major bump permits everything, so `0.30.2 -> 0.31.0` reports
   `0 checks: 0 pass, 254 skip / no semver update required` no matter
   what changed. To write an accurate **Breaking** section, run it
   against the last published version as if it were a patch:

   ```bash
   cargo semver-checks check-release --baseline-version <last-published> --release-type patch
   ```

   Every `--- failure ... ---` block is a breaking change that belongs in
   the changelog.

   **This does not catch a break inherited from a dependency, and 0.33.0
   found that out.** `cargo-semver-checks` compares the rustdoc
   `resolved_path` string. A public item typed `ratatui::Terminal` has
   that same path before and after ratatui 0.29 → 0.30, so the tool sees
   no change while the type identity has in fact moved and downstream
   code no longer compiles. Run against published 0.32.0 as a patch it
   reported `223 checks: 223 pass` — a false clean covering **38**
   publicly reachable items. It is a structural blind spot, not a lint
   anyone forgot to enable.

   So the enumeration has a second half, and it is a *lockfile*
   question, not a semver-checks run: **did any dependency that appears
   in the public API change major?** If yes, walk every `pub mod` from
   the crate root and list every reachable item whose signature, public
   field, enum variant, or type alias names a type from that crate — and
   remember that `pub mod app` re-exports far more than the module list
   suggests (`App` alone has ~91 public fields). This is a checklist step, not a gate — it fails by
   construction on any legitimate major bump, so it cannot be wired into
   CI. 0.31.0 shipped with `Form.banner` undocumented because this was
   only run after the tag.

4. **Surface findings in the release message.** What the docs audit + code review fixed lands in the release notes / final summary alongside what shipped, so the audit and review aren't invisible work.

5. **No silent edits — flag intentional gaps.** If a command shipped behind a feature flag or as a soft preview, say so in the audit summary rather than just documenting it as if it were generally available. If a code-review finding was deferred (not fixed in the release commit), say WHY and what version it's tracked against.
