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
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &[], &[]);
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
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &issues, &[]);
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
    let env = mk_env("api-prod", "poly", "Web", "Green");
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
    //
    // Every production file, not a list. The list named three callers
    // and missed the fourth — `:explain`'s copy in `cmd_inspect.rs`,
    // which flattened exactly this way and fired EBL010 on every env
    // whose tags were never read. Then moving the shared assembly out
    // of a listed file blinded it again, silently. A list goes stale
    // on every move; the tree does not.
    let mut bindings_seen = 0usize;
    for (path, full) in super::scan::source_files() {
        if super::scan::is_test_path(&path) {
            continue;
        }
        let src = super::scan::production_half(&full);
        let name = path.as_str();
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
            bindings_seen += 1;
            assert!(
                !expr.contains("unwrap_or_default"),
                "{name}:{} flattens the tag-fetch failure into an empty list, \
                 which makes EBL010 fire instead of skip: {}",
                n + 1,
                expr.trim()
            );
        }
    }
    // The shared assembly binds `env_tag_keys`; seeing none means the
    // scan went blind, not that the tree is clean.
    assert!(
        bindings_seen > 0,
        "no `env_tag_keys` binding found anywhere — the scan is looking at nothing"
    );
}

/// Lost coverage is never rendered as a clean result.
///
/// The TUI's own copy of the assembly dropped tag and health failures
/// in silence, so `:lint` showed "✓ No issues found" over checks that
/// never ran — the same wrong answer the CLI and MCP gave before they
/// started degrading on it.
#[test]
fn lint_overlay_never_shows_a_clean_result_over_checks_that_did_not_run() {
    let warnings = vec![
        "EBL012 could not be evaluated for prod-api: DescribeEnvironmentHealth failed: AccessDenied"
            .to_string(),
    ];
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &[], &warnings);
    assert!(
        !body.contains('✓'),
        "no check-mark over a check that did not run: {body}"
    );
    assert!(body.contains("could NOT run"), "{body}");
    assert!(body.contains("EBL012"), "names the check: {body}");
}

/// ...and alongside real findings, the lost coverage is still listed.
#[test]
fn lint_overlay_lists_lost_coverage_beside_findings() {
    let issue = crate::lint::Issue {
        rule_id: "EBL001".into(),
        severity: crate::lint::Severity::Warn,
        env_name: Some("prod-api".into()),
        title: "EBL001 fired".into(),
        detail: String::new(),
        suggestion: None,
        fields: Default::default(),
    };
    let warnings = vec!["EBL010 could not be evaluated for prod-api: throttled".to_string()];
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &[issue], &warnings);
    assert!(body.contains("EBL001 fired"), "{body}");
    assert!(body.contains("EBL010 could not be evaluated"), "{body}");
}

/// The TUI's `:lint` and `:explain` assemble their inputs through the
/// shared `lint::inputs` path, and build no `LintContext` of their own.
///
/// Each kept a private copy of the assembly, and the copies drifted
/// from `ebman lint` and the MCP tool: silent tag/health failures, no
/// EBL020 / EBL018 probes, and in `:explain` an EBL010 false positive
/// on every env whose tags were never read. A copy reappearing is the
/// regression; this is the shape it would take.
#[test]
fn the_tui_lint_paths_use_the_shared_assembly() {
    for (file, func) in [
        ("app/cmd_misc.rs", "fn cmd_lint("),
        ("app/cmd_inspect.rs", "fn cmd_explain_issue("),
    ] {
        let prod = super::scan::production_source(file);
        let body = prod
            .split(func)
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{file}: `{func}` not found"));
        assert!(
            body.contains("lint::inputs::fetch_env_lint_inputs("),
            "{file} `{func}` must fetch through the shared assembly"
        );
        assert!(
            !body.contains("LintContext::for_env("),
            "{file} `{func}` builds its own LintContext — a private copy of the \
             assembly again"
        );
        // The inputs taken from App caches rather than fetched: their
        // gaps must be reported too, from the real cache state.
        assert!(
            body.contains("cached_input_gaps(")
                && body.contains("!self.latest_stacks.is_empty()")
                && body.contains("self.worker_dlq_absent.contains("),
            "{file} `{func}` must report the gaps in its cached inputs"
        );
    }
}

#[test]
fn a_cached_input_that_is_missing_is_reported_not_read_as_clean() {
    use crate::lint::inputs::{cached_input_gaps, explain_verdict, ExplainVerdict};
    let web = mk_env("api", "poly", "WebServer", "Green");
    let worker = mk_env("jobs", "poly", "Worker", "Green");

    // Platform list not loaded → EBL008 could not run, on any env.
    let gaps = cached_input_gaps(&web, &[], false, None, false);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with("EBL008 could not be evaluated for api"),
        "{gaps:?}"
    );
    // And `:explain EBL008` says so, rather than "doesn't fire".
    assert!(matches!(
        explain_verdict("EBL008", &[], &gaps),
        ExplainVerdict::NotEvaluated(_)
    ));
    assert!(cached_input_gaps(&web, &[], true, None, false).is_empty());

    // A worker with no depth read and no known absence → EBL011.
    let gaps = cached_input_gaps(&worker, &[], true, None, false);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with("EBL011 could not be evaluated for jobs"),
        "{gaps:?}"
    );
    // Known to have no DLQ, or a depth in hand → nothing missing.
    assert!(cached_input_gaps(&worker, &[], true, None, true).is_empty());
    assert!(cached_input_gaps(&worker, &[], true, Some(0), false).is_empty());
    // Disabled rules are silent.
    let off = vec!["EBL008".to_string(), "EBL011".to_string()];
    assert!(cached_input_gaps(&worker, &off, false, None, false).is_empty());
}

/// The pre-deploy lint reports a failed run as a reason, never as an
/// empty list. `Err(_) => Vec::new()` is the shape that made "could
/// not check" render exactly like "checked, clean" in the confirm
/// modal; the handler and the render are pinned elsewhere, and this
/// pins the sender, which runs in a spawned task no unit test reaches.
#[test]
fn the_pre_deploy_lint_reports_a_failed_run() {
    let prod = super::scan::production_source("app/spawn_deploy.rs");
    let body = prod
        .split("fn spawn_confirm_lint(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap_or_else(|| panic!("spawn_confirm_lint not found"));
    let code: String = body
        .lines()
        .map(super::scan::strip_line_comment)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("Err(_) => Vec::new()"),
        "a failed lint must not become an empty — i.e. clean — issue list"
    );
    assert!(
        code.matches("lint could not run").count() >= 2,
        "both whole-lint failure paths (client, option fetch) must carry a reason"
    );
    // And the per-rule inputs: a denied tag or health fetch was dropped
    // by `.ok()`, so the pane stayed empty — "clean" — over a check that
    // never ran (0.45 release review). Each fetch must keep its error.
    for call in [".list_tags(", ".fetch_env_instance_counts("] {
        let at = code
            .find(call)
            .unwrap_or_else(|| panic!("spawn_confirm_lint no longer calls {call}"));
        // The whole expression, not a fixed window: rustfmt splits the
        // chain one call per line, and a 160-char window ended before the
        // fourth line of the tag fetch — so `.ok()` there passed.
        let tail = code[at..].split(';').next().unwrap_or_default();
        assert!(
            !tail.contains(".ok()"),
            "{call} drops its error with `.ok()` — a failed fetch must be reported: {tail}"
        );
    }
    // And the kept errors must reach the reporting helper: a call that
    // passed `None` for either would compile and list nothing.
    let at = code
        .find("pre_deploy_coverage_gaps(")
        .unwrap_or_else(|| panic!("spawn_confirm_lint no longer reports coverage gaps"));
    let args = code[at..].split(';').next().unwrap_or_default();
    for arg in ["tags_res.as_ref()", "&health_res"] {
        assert!(
            args.contains(arg),
            "pre_deploy_coverage_gaps is not given {arg}: {args}"
        );
    }
}

/// Only `src/lint/` builds a `LintContext`: every surface gets its lint
/// inputs from the one assembly, `lint::inputs`.
///
/// The per-function pin above could only see the functions it named,
/// and the release review found a fifth private copy it could not: the
/// CLI's `ebman explain`, which fetched option settings alone — so four
/// rules could never fire there and a failed fetch read as a clean
/// "no env has this issue". Asked the other way round, a new copy
/// anywhere fails.
///
/// One exception, by COUNT so it cannot grow: the pre-deploy lint in
/// `spawn_deploy.rs`, recorded in BACKLOG.md ("The pre-deploy lint is
/// still its own copy of the assembly") pending two rulings.
#[test]
fn only_the_shared_assembly_builds_a_lint_context() {
    const ALLOWED: &[(&str, usize)] = &[("src/app/spawn_deploy.rs", 1)];
    let mut found: Vec<(String, usize)> = Vec::new();
    let mut scanned = 0usize;
    for (path, full) in super::scan::source_files() {
        if super::scan::is_test_path(&path) || path.contains("src/lint/") {
            continue;
        }
        scanned += 1;
        let prod = super::scan::production_half(&full);
        let n = prod
            .lines()
            .map(super::scan::strip_line_comment)
            .filter(|l| l.contains("LintContext::for_env("))
            .count();
        if n > 0 {
            found.push((path, n));
        }
    }
    assert!(scanned > 50, "scanned only {scanned} files");
    for (path, n) in &found {
        let allowed = ALLOWED
            .iter()
            .find(|(p, _)| path.ends_with(p))
            .map_or(0, |(_, c)| *c);
        assert!(
            *n <= allowed,
            "{path} builds its own LintContext ({n}×) — a private copy of the lint \
             assembly. Use `lint::inputs::fetch_env_lint_inputs` + `run_rules_for_env`."
        );
    }
}

#[test]
fn a_failed_tag_or_health_fetch_is_listed_as_not_run_before_a_deploy() {
    use super::super::spawn_deploy::pre_deploy_coverage_gaps;
    let env = fake_env("api-prod", "Ready", "Green", "v1");
    let tags = vec!["Owner".to_string()];
    let enhanced: Vec<(String, String, String)> = Vec::new();
    let gaps = pre_deploy_coverage_gaps(
        &env,
        &[],
        &tags,
        Some(&Err("AccessDenied".to_string())),
        &Err("Throttling".to_string()),
        Some(&enhanced),
    );
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert!(gaps[0].starts_with("EBL010 could not be evaluated for api-prod"));
    assert!(gaps[0].contains("ListTagsForResource: AccessDenied"));
    assert!(gaps[1].starts_with("EBL012 could not be evaluated for api-prod"));
    assert!(gaps[1].contains("DescribeEnvironmentHealth: Throttling"));
}

#[test]
fn a_failed_fetch_is_not_listed_when_its_rule_could_not_have_fired() {
    use super::super::spawn_deploy::pre_deploy_coverage_gaps;
    let env = fake_env("api-prod", "Ready", "Green", "v1");
    let basic = vec![(
        "aws:elasticbeanstalk:healthreporting:system".to_string(),
        "SystemType".to_string(),
        "basic".to_string(),
    )];
    // No required tags → EBL010 cannot fire; basic health → EBL012 cannot.
    let quiet = pre_deploy_coverage_gaps(
        &env,
        &[],
        &[],
        Some(&Err("AccessDenied".to_string())),
        &Err("Throttling".to_string()),
        Some(&basic),
    );
    assert!(quiet.is_empty(), "{quiet:?}");
    // Disabled rules are silent too.
    let disabled = vec!["EBL010".to_string(), "EBL012".to_string()];
    let off = pre_deploy_coverage_gaps(
        &env,
        &disabled,
        &["Owner".to_string()],
        Some(&Err("AccessDenied".to_string())),
        &Err("Throttling".to_string()),
        Some(&[]),
    );
    assert!(off.is_empty(), "{off:?}");
    // Fetches that succeeded report nothing.
    let clean = pre_deploy_coverage_gaps(
        &env,
        &[],
        &["Owner".to_string()],
        Some(&Ok(vec!["Owner".to_string()])),
        &Ok(2),
        Some(&[]),
    );
    assert!(clean.is_empty(), "{clean:?}");
}
