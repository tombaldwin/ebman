# Coverage gaps as a type

**Status: drafted 2026-11-02, not yet built.** PLAN window item, from
the 0.45 release review. It changes no output an operator or an agent
reads; what changes is that a gap is matched by its rule, not by the
first word of a sentence.

## What is wrong

A lint check that could not run is reported, never silently skipped,
which is the property ARCHITECTURE.md rule 6 asks for. But "could not
run" is a `String` in every place it travels, and is written in three
formats:

| producer | format | travels as |
|---|---|---|
| `ProbeOutcome::coverage_warning` (`lint/inputs.rs`) | `EBL020 could not be evaluated for ENV: WHY — this is NOT a clean result, the check did not run` | `EnvLintInputs.coverage_warnings: Vec<String>` |
| `fetch_stale_platform_issues` (`lint/inputs.rs`) | `EBL015 skipped for 'BRANCH' — DescribePlatformVersion: E` | the `Vec<String>` beside its issues |
| `rule_skipped` (`cli/mcp/tools.rs`) | `EBL008 skipped — WHY` plus a credential hint | MCP `skipped_envs` |
| the deploy pre-flight (`app/spawn_deploy.rs`) | `lint could not run: E` (no rule) | `PreflightAnswer::Lint.not_run` |

`explain_verdict` decides whether `:explain EBL010` says "not
evaluated" by taking the first whitespace-separated word of each
warning and comparing it to the rule id. That holds today because every
warning it sees comes from `coverage_warning`, which leads with the id.
It stops holding the day a fourth formatter writes "Could not check
EBL010", and nothing would say so: `:explain` would answer "does not
fire" for a check that never ran, the clean bill of health rule 6 is
there to prevent.

The deploy modal's `not_run` mixes the two kinds in one list: "the
whole lint could not run" and "this rule could not run here".

## Decision (proposed)

```rust
/// A lint check that could not run, and why. Never a clean result.
pub(crate) struct CoverageGap {
    /// The rule that did not run; `None` when the whole lint did not.
    pub rule: Option<String>,
    /// What it did not run against.
    pub scope: GapScope,
    pub why: String,
}

pub(crate) enum GapScope { Env(String), Account, Branch(String) }
```

- **One `Display`**, which renders today's `coverage_warning` sentence
  for `Env`, today's EBL015 sentence for `Branch`, and `lint could not
  run: WHY` for `rule: None`. Output stays byte-identical, so no doc,
  no snapshot and no agent-facing format changes in this step.
- **`explain_verdict` matches `gap.rule == Some(rule_id)`.** The
  first-word parse goes.
- **`coverage_warnings: Vec<CoverageGap>`** on `EnvLintInputs` and
  `TuiLintRun`, and `not_run: Vec<CoverageGap>` on the pre-flight answer.
  The renderers (`cli/lint.rs`'s `report.degrade`, the TUI overlay, the
  deploy modal, MCP's `skipped_envs`) call `to_string()` at the edge.
- **MCP's `rule_skipped` keeps its own wording** (`EBL008 skipped —
  WHY` and the credential hint), built from a `CoverageGap` with
  `GapScope::Account`. Its strings are a published result field; an
  agent may match on them, so changing them is a separate, announced
  change, not a side effect of this one.

`rule` is a `String`, as `lint::Issue.rule_id` is; this note does not
introduce a rule-id newtype. MCP's per-branch EBL015 lines are
`fetch_stale_platform_issues`'s gaps with the credential hint added, so
they move with that producer.

## Call sites

`lint/inputs.rs` (producers, `explain_verdict`), `app/tui_lint.rs`,
`app/cmd_misc.rs` (overlay), `app/cmd_inspect.rs`, `app/spawn_deploy.rs`
and `mode_action.rs` / `app.rs` / `ui/action.rs` (the pre-flight
`not_run`), `cli/lint.rs`, `cli/explain.rs`, `cli/mcp/tools.rs`. Nine
modules, most of them a type change at the field and a `to_string()` at
the renderer.

## Tests

- `explain_verdict` finds a gap by rule whatever its sentence says (a
  gap whose `Display` does not lead with the id still reads as not
  evaluated): the case the first-word parse fails.
- `Display` pins each of today's three sentences, so the move is proven
  byte-identical.
- The deploy modal still renders a whole-lint failure and a per-rule gap,
  each in its line.

## Not decided here

- Rendering the modal's whole-lint failure apart from per-rule gaps (a
  heading of its own). The type makes it possible; whether the operator
  wants it is a UI question.
- Changing MCP's `skipped_envs` to structured objects. A wire change.
