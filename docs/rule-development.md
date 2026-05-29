# Adding a lint rule

This guide walks through adding a new `EBL###` lint rule from start to ship. The rule engine is in `src/lint.rs` and is shared between three surfaces: the TUI `:lint` overlay, the CLI `ebman lint` subcommand, and the confirm-modal warning lines.

The reference page for shipped rules is [lint-rules.md](lint-rules.md).

---

## Anatomy of a rule

Every rule implements the `Rule` trait (`src/lint.rs`):

```rust
pub trait Rule: Send + Sync {
    fn id(&self) -> &'static str;
    fn severity(&self) -> Severity;
    fn applies(&self, ctx: &LintContext) -> Option<Issue>;
    fn fix(&self, _ctx: &LintContext) -> Option<FixAction> {
        None // default: no auto-fix, not even manual
    }
}
```

- **`id()`**: stable `EBL###` identifier. Use the next free number.
- **`severity()`**: `Error` / `Warn` / `Info`. See [lint-rules.md](lint-rules.md) for guidance.
- **`applies()`**: returns `Some(Issue)` when the rule fires, `None` otherwise. **Must short-circuit early on missing inputs** — if your rule needs `ctx.dlq_depth` and it's `None`, return `None`, don't fabricate a default.
- **`fix()`**: optional auto-remediation. Three variants:
  - `FixAction::SetOption { namespace, name, value, description }` — one option-setting flip. Most auto-fixable rules collapse to this.
  - `FixAction::Manual { instructions }` — rule knows there's an issue but the right answer depends on operator context (e.g. EBL002: "set a health-check URL" — we don't know the path).
  - Default (return `None`) — no fix, even manual. Use for state-based rules (EBL003: env Red >4h is a state, not a config issue).

## The `LintContext` builder

Rules read from a `LintContext` reference. The builder is in `src/lint.rs`:

```rust
let ctx = LintContext::for_env(&env, &options)
    .with_required_tags(&required_tags)
    .with_env_tag_keys(&env_tag_keys)
    .with_newer_stack_available(newer_stack)
    .with_dlq_depth(depth)
    .with_healthy_count(count);
```

When your rule needs a new external input (e.g., a new AWS fetch result), add a field to `LintContext` and a `with_*` builder method. This is a 3-site edit:

1. **`LintContext` struct definition** — add the field with a sensible empty default.
2. **`LintContext::for_env`** — initialise the field to its empty default.
3. **A new `with_*` method** — sets the field and returns `self`.

Then the four lint call sites need to be updated to populate the new field:
- `src/app.rs::spawn_confirm_lint` — fires at confirm-modal-open time.
- `src/app.rs::cmd_explain_issue` — fires at `:explain ISSUE_ID` time.
- `src/app/cmd_misc.rs::cmd_lint` — fires at `:lint` time.
- `src/cli/lint.rs::run` — fires at `ebman lint` time.

Each site fetches the input (typically via `tokio::join!` parallel with the existing fetches) and chains the new `with_*` method on the builder.

## Steps

### 1. Pick a rule ID

Find the next free `EBL###`. As of 0.20: 001-013 + 017 + 019 are in use, so 014-016, 018, 020+ are free.

### 2. Implement the rule struct

In `src/lint.rs`, alongside the other rules:

```rust
/// EBL### — Short title. One-paragraph why-it-matters.
///
/// Detection: the precise condition under which `applies()` returns
/// `Some`. Be explicit about which namespace / option / context field
/// drives the decision so future maintainers can verify correctness.
///
/// Auto-fix: SetOption / Manual / None — and why.
pub struct YourRuleName;

impl Rule for YourRuleName {
    fn id(&self) -> &'static str { "EBL###" }
    fn severity(&self) -> Severity { Severity::Warn }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        // Always call applies() first so fix() and applies() can't
        // disagree. The `?` short-circuits when applies() returned None.
        self.applies(ctx)?;
        Some(FixAction::SetOption {
            namespace: "ns".into(),
            name: "OptionName".into(),
            value: "expected-value".into(),
            description: "Human-readable summary of what this fix changes".into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Read your context fields. Use `?` to short-circuit on
        // missing optional inputs.
        let your_input = ctx.your_field?;
        if !your_condition(your_input) {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("key".into(), value.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("Short scannable line: {value}"),
            detail: "Multi-line explanation of why this matters and what \
                     the operator should do about it.".into(),
            suggestion: Some(":command-the-operator-should-run".into()),
            fields,
        })
    }
}
```

### 3. Register in `default_rules()`

In the same file:

```rust
pub fn default_rules(disabled: &[String]) -> Vec<Box<dyn Rule>> {
    let candidates: Vec<Box<dyn Rule>> = vec![
        // ... existing rules ...
        Box::new(YourRuleName),
    ];
    candidates
        .into_iter()
        .filter(|r| !disabled.iter().any(|d| d == r.id()))
        .collect()
}
```

### 4. Add tests

At minimum:
- **Fires on the expected input.** Build a synthetic `Environment` + options that triggers the rule and assert `applies()` returns `Some`.
- **Does not fire without the trigger.** Same shape but with the input condition unmet — assert `applies()` returns `None` and `fix()` returns `None`.
- **Fix shape (if auto-fixable).** Match on `FixAction::SetOption` and assert the namespace/name/value are correct.
- **At least one false-positive guard.** A common variant that LOOKS like the rule should fire but shouldn't (e.g., the option is set to an empty string, the env is the wrong tier, etc.).

Test fixtures `mk_env`, `mk_opt`, and `ctx` are already defined in the `tests` module.

```rust
#[test]
fn ebl###_fires_on_expected_input() {
    let env = mk_env("prod", "Web", "Green");
    let opts = vec![mk_opt("ns", "OptionName", "bad-value")];
    let ctx = LintContext::for_env(&env, &opts);
    let issue = YourRuleName.applies(&ctx).expect("should fire");
    assert_eq!(issue.rule_id, "EBL###");
}
```

### 5. Update the rule-registry size assertion

`rules_satisfy_trait_invariants` asserts the exact rule count. Bump it by one:

```rust
assert_eq!(rules.len(), N+1, "rule registry size changed");
```

### 6. Document the rule

Add an entry to [lint-rules.md](lint-rules.md) under the appropriate severity section, following the existing format:

```markdown
### EBL### — Short title

**Severity:** Warn · **Auto-fix:** SetOption / Manual / None

Detection: ...

Why it matters: ...

Fix: ...

Live: 0.NN+.
```

Also update the rule count in `docs/commands.md`'s `:lint` entry.

### 7. Plumbing (only if the rule needs new context inputs)

If your rule needs an input that isn't already in `LintContext` (new AWS fetch, new App state field), see the "LintContext builder" section above. The 3-site plumbing edit + the 4 lint-call-site updates land in the same PR as the rule itself.

---

## House conventions

- **Pure detection.** `applies()` and `fix()` should be pure functions of the context. No I/O, no global state, no time-dependent logic (use `ctx.now` if you really need a clock).
- **Short-circuit on missing inputs.** Use `?` on `Option` fields to bail early. Don't fabricate defaults that could let the rule fire on contexts it shouldn't see.
- **`fix()` must match `applies()`.** If `applies()` returns `None`, `fix()` must return `None` too. The `rules_satisfy_trait_invariants` test enforces this; the trait pattern `self.applies(ctx)?` at the top of `fix()` is the cheapest way to satisfy it.
- **Fields are operator-facing.** The `Issue.fields: BTreeMap<String, String>` map is the audit-log key for `--baseline` identity. Use stable field names (`policy`, `max_size`, `subnet_count`) so a future rule revision doesn't silently churn operators' baseline files.
- **Auto-fix only with an obvious correct answer.** EBL001's "AllAtOnce → Rolling" is obvious; EBL002's "set a health-check URL to X" requires knowing X (operator-context). When in doubt, ship `Manual` instructions and let the operator dispatch.
- **Document the why, not the what.** The code already says what — comments should explain why the threshold is N, why the namespace is this, what the historical AWS context is.

## Testing the new rule against a live env

Once shipped, you can verify against your own env:

```sh
# TUI overlay
ebman :lint                           # fires against selected env
ebman :explain EBL###                 # LLM walkthrough (if [explain] enabled)

# CLI
ebman lint --env NAME --rules EBL###  # filter to just this rule
ebman lint --env NAME --json | jq '.[] | select(.rule_id == "EBL###")'

# Auto-fix dry-run (if fix() returns SetOption)
ebman lint --env NAME --fix --dry-run --rules EBL###

# Confirm-modal lint warning (any rule with severity >= Warn fires at write time)
ebman :rebuild                        # rule fires inline in the confirm modal
```

If the detection over-fires (false-positive on legit envs), tighten `applies()` and add a regression test. If it under-fires (misses a real case), broaden and add a regression test. The cheapest review tool is `ebman lint --json | jq` against the fleet you already operate.
