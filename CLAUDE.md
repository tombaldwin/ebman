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
- **State mutations that affect the view (`filter`, `sort_key`, `sort_desc`, `grouped`, `environments`) must call `App::rebuild_view()`.** The cached `cached_filtered` / `cached_display` slices are stale otherwise.
- **Async-result handlers check `generation`.** Every spawned task carries the generation it was launched at; if the App's `generation` has advanced (context switch) the result is dropped. New `AppMsg` variants must follow this pattern.
- **No hardcoded colours.** Use `app.theme.*`. Hardcoded `Color::Cyan` / `Color::Gray` is a regression.
- **No hardcoded paths.** Use `util::config_dir()` / `util::cache_dir()` / `util::config_file(...)`.
- **No `println!` / `eprintln!` in the running app** — the alternate screen swallows them and they corrupt the TUI. Use `tracing::*` macros; output goes to `~/.cache/ebman/ebman.log`.
- **The animation ticker is gated on `loading_since.is_some()`.** Don't move work into it that needs to run while idle — add a separate ticker.
- **`State` and `Config` parsing is in pure `parse(&str)` functions.** Keep the I/O wrappers thin so the parsers stay unit-testable.

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

3. **Surface findings in the release message.** What the docs audit + code review fixed lands in the release notes / final summary alongside what shipped, so the audit and review aren't invisible work.

4. **No silent edits — flag intentional gaps.** If a command shipped behind a feature flag or as a soft preview, say so in the audit summary rather than just documenting it as if it were generally available. If a code-review finding was deferred (not fixed in the release commit), say WHY and what version it's tracked against.
