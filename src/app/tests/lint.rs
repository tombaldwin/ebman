//! The environment lint rules and the drift/baseline paths.
//!
//! Split out of the 9,515-line `app/tests.rs`. Bodies moved
//! unchanged apart from one rewrite: `super::` meant `crate::app` in
//! the flat file and would mean `crate::app::tests` here, so every
//! explicit `super::` path was re-anchored (rustfmt reflowed some
//! lines as a result, since the new path is longer).

#[allow(unused_imports)]
use super::super::*;
#[allow(unused_imports)]
use super::support::*;

#[tokio::test]
async fn cmd_drift_refresh_reloads_tf_state_and_pins_status() {
    // `:drift refresh` re-reads tfstate from cwd. We can't
    // easily test the cwd discovery in isolation, but we
    // can verify the command path completes + pins a status
    // (either "reloaded N envs" or "no tfstate found").
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.rebuild_view();
    app.execute_command("drift refresh");
    // Status message should mention tfstate either way.
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("tfstate"),
        "expected tfstate status, got: {msg}"
    );
}

#[tokio::test]
async fn cmd_drift_with_no_tfstate_loaded_hints_at_discovery() {
    // No tfstate cached → :drift surfaces a discovery hint
    // rather than firing an empty drift report. Sets the
    // operator on the right path (run from a tf project dir).
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.tf_state = None;
    app.execute_command("drift");
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("no terraform.tfstate found"),
        "expected discovery hint, got: {msg}"
    );
}

#[test]
fn render_lint_overlay_empty_shows_clean_stub() {
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &[]);
    assert!(body.contains("prod-api"));
    assert!(body.contains("✓ No issues found"));
    assert!(body.contains("esc / q to close"));
}

#[test]
fn render_lint_overlay_with_issues_renders_per_severity_glyph() {
    use crate::lint::{Issue, Severity};
    use std::collections::BTreeMap;
    let issues = vec![
        Issue {
            rule_id: "EBL001".into(),
            severity: Severity::Warn,
            env_name: Some("prod".into()),
            title: "AllAtOnce on 4-instance env".into(),
            detail: "Deployment policy AllAtOnce with MaxSize=4 means full unavailability.".into(),
            suggestion: Some(":deployment-policy Rolling".into()),
            fields: BTreeMap::new(),
        },
        Issue {
            rule_id: "EBL005".into(),
            severity: Severity::Info,
            env_name: Some("prod".into()),
            title: "Single-instance env".into(),
            detail: "MinSize=MaxSize=1.".into(),
            suggestion: None,
            fields: BTreeMap::new(),
        },
    ];
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &issues);
    // Warn gets ⚠, Info gets ·.
    assert!(body.contains("⚠ [EBL001]"));
    assert!(body.contains("· [EBL005]"));
    // Suggestion lines prefixed with →.
    assert!(body.contains("→ :deployment-policy Rolling"));
    // Detail wrapped under each issue with indent.
    assert!(body.contains("    Deployment policy AllAtOnce"));
    // Plural / singular handling.
    assert!(body.contains("2 issues found"));
}

#[test]
fn ebl010_tells_an_untagged_env_from_an_unloaded_one() {
    // `env_tag_keys` was a bare slice, so "the fetch failed" and "this
    // env has no tags" were the same value — a failed
    // `ListTagsForResource` silently disabled the rule, and an env
    // with no tags at all, the worst case the rule exists to catch,
    // looked identical to one whose tags hadn't loaded. Same
    // conflation as `describe_worker_queues` returning an empty list
    // for AccessDenied, fixed in 0.27.
    use crate::lint::LintContext;
    let env = mk_env("api-prod", "uflexi", "Web", "Green");
    let opts: Vec<(String, String, String)> = Vec::new();
    let required = vec!["Owner".to_string(), "CostCentre".to_string()];
    let rules = crate::lint::default_rules(&[]);

    // Not loaded: skip. Firing here would flag every env in the fleet
    // on a transient API error.
    let ctx = LintContext::for_env(&env, &opts).with_required_tags(&required);
    assert!(
        !crate::lint::run_rules(&rules, &ctx)
            .iter()
            .any(|i| i.rule_id == "EBL010"),
        "unloaded tags must not fire"
    );

    // Loaded and empty: fires for both keys. This is the env that has
    // no tags at all, which used to be invisible.
    let none_at_all: Vec<String> = Vec::new();
    let ctx = LintContext::for_env(&env, &opts)
        .with_required_tags(&required)
        .with_env_tag_keys(&none_at_all);
    let issue = crate::lint::run_rules(&rules, &ctx)
        .into_iter()
        .find(|i| i.rule_id == "EBL010")
        .expect("an env with no tags at all must fire");
    assert!(issue.detail.contains("Owner"), "{}", issue.detail);
    assert!(issue.detail.contains("CostCentre"), "{}", issue.detail);

    // Loaded and complete: silent.
    let all = vec!["Owner".to_string(), "CostCentre".to_string()];
    let ctx = LintContext::for_env(&env, &opts)
        .with_required_tags(&required)
        .with_env_tag_keys(&all);
    assert!(!crate::lint::run_rules(&rules, &ctx)
        .iter()
        .any(|i| i.rule_id == "EBL010"));
}

#[test]
fn no_lint_caller_flattens_a_failed_tag_fetch_into_an_empty_list() {
    // Making `env_tag_keys` an `Option` fixed the rule but INVERTED the
    // bug at the call sites: all three collapsed `None` (fetch failed,
    // or the env has no ARN) into an empty Vec before calling, so a
    // failed `ListTagsForResource` went from silently skipping the rule
    // to firing a false positive for every required key on every env.
    // Worse than what it replaced.
    //
    // Pinned structurally because the failure is a lost distinction,
    // not a wrong value: `unwrap_or_default()` on the tags option is
    // exactly the shape that throws it away.
    for (name, src) in [
        ("app/cmd_misc.rs", include_str!("../cmd_misc.rs")),
        ("app/spawn_deploy.rs", include_str!("../spawn_deploy.rs")),
        ("cli/lint.rs", include_str!("../../cli/lint.rs")),
    ] {
        let code: String = src
            .lines()
            .map(super::scan::strip_line_comment)
            .collect::<Vec<_>>()
            .join("\n");
        // Find each tag-keys binding and check the WHOLE expression,
        // not just its first line — the binding routinely wraps, and a
        // single-line check missed a two-line `Some(tags_opt
        // .unwrap_or_default() …)` when this guard was mutation-tested.
        let lines: Vec<&str> = code.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if !(line.contains("env_tag_keys") && line.contains('=')) {
                continue;
            }
            // Read to the end of the statement.
            let mut expr = String::new();
            for l in &lines[n..] {
                expr.push_str(l);
                if l.trim_end().ends_with(';') {
                    break;
                }
            }
            assert!(
                !expr.contains("unwrap_or_default"),
                "{name}:{} flattens the tag-fetch failure into an empty list, \
                 which makes EBL010 fire instead of skip: {}",
                n + 1,
                expr.trim()
            );
        }
    }
}
