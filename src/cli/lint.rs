//! `ebman lint [--env NAME] [--regions r1,r2,r3] [--json] [--severity LVL]
//! [--rules ID1,ID2] [--quiet] [--fix (--yes | --dry-run)]` —
//! rule-engine diagnostics for git hooks / CI gates / monitoring,
//! with opt-in auto-remediation via `--fix`.
//!
//! Exit codes (per the 0.13 CLI charter):
//! - 0 clean / fix applied successfully
//! - 1 AWS-layer error (or `--fix` dispatch failure)
//! - 2 usage error
//! - 3 issues found (NOT used in `--fix` mode — operator's intent
//!   is "see issues then fix them"; a clean apply stays exit 0)
//!
//! `--fix` dispatches each rule's auto-remediation through the same
//! `update_env_option_settings` path the TUI uses. Respects
//! `safety.envs.NAME.read_only` + `safety.accounts.NAME.read_only`
//! pins (matched against `AWS_PROFILE`) so a TUI-locked env can't
//! be written from the CLI. Per-rule opt-out via `lint.fix_disable`.

use color_eyre::eyre::Result;

use crate::{audit, aws, config, lint, project};

/// Tracks whether any `--fix` dispatch failed during the run. Single
/// process-wide flag — CLI exits after `run` returns, so cross-run
/// state isn't a concern. Lives next to its sole reader/writer
/// (`run`).
static FIX_DISPATCH_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Print `--against-baseline --json` diff body. Hand-rolled to
/// avoid pulling serde_json; uses `crate::util::json_string` for
/// the value escapes. Shape:
///
/// ```json
/// {
///   "new": [{ "rule_id": "...", "env": "...", "title": "..." }, ...],
///   "cleared": [{ "rule_id": "...", "env": "...", "title": "..." }, ...]
/// }
/// ```
fn print_baseline_diff_json(new: &[&lint::Issue], cleared: &[&lint::BaselineIssue]) {
    let mut out = String::from("{\"new\":[");
    for (i, issue) in new.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"rule_id\":{},\"env\":{},\"title\":{}}}",
            crate::util::json_string(&issue.rule_id),
            crate::util::json_string(issue.env_name.as_deref().unwrap_or("")),
            crate::util::json_string(&issue.title),
        ));
    }
    out.push_str("],\"cleared\":[");
    for (i, b) in cleared.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"rule_id\":{},\"env\":{},\"title\":{}}}",
            crate::util::json_string(&b.rule_id),
            crate::util::json_string(b.env_name.as_deref().unwrap_or("")),
            crate::util::json_string(&b.title),
        ));
    }
    out.push_str("]}");
    println!("{out}");
}

/// Fully-resolved `ebman lint` arguments: flags parsed, interval and
/// region-CSV resolved, and all cross-flag validation already passed.
/// Separated from [`run`] so the whole parse+validate surface (every
/// exit-2 usage path) is unit-testable without `std::process::exit` or
/// the live config/AWS I/O that follows it.
#[derive(Debug, PartialEq, Eq)]
struct LintArgs {
    env_name: Option<String>,
    regions: Vec<Option<String>>,
    json: bool,
    quiet: bool,
    severity_filter: Option<lint::Severity>,
    rule_filter: Vec<String>,
    fix: bool,
    dry_run: bool,
    yes: bool,
    watch: bool,
    interval_secs: u64,
    baseline_write: Option<String>,
    baseline_against: Option<String>,
}

/// Pure parser + validator for `ebman lint`. Returns `Err(msg)` for
/// every usage error (all exit-2 here, so the code is left implicit).
/// Ordering note: validation runs here, before [`run`] loads config —
/// a usage error now exits before the (silent) config read rather than
/// after. No observable change; strictly less wasted work.
fn parse_lint_args(args: &[String]) -> Result<LintArgs, String> {
    let mut env_name: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut json = false;
    let mut quiet = false;
    let mut severity_filter: Option<lint::Severity> = None;
    let mut rule_filter: Vec<String> = Vec::new();
    let mut fix = false;
    let mut dry_run = false;
    let mut yes = false;
    let mut watch = false;
    let mut interval_str: Option<String> = None;
    let mut baseline_write: Option<String> = None;
    let mut baseline_against: Option<String> = None;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--regions" => regions_csv = iter.next().cloned(),
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--fix" => fix = true,
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
            "--watch" => watch = true,
            "--interval" => interval_str = iter.next().cloned(),
            "--baseline" => {
                let Some(p) = iter.next() else {
                    return Err("ebman lint: --baseline expects a file path".into());
                };
                if p.starts_with("--") {
                    return Err(format!(
                        "ebman lint: --baseline expects a file path, got flag '{p}'"
                    ));
                }
                baseline_write = Some(p.clone());
            }
            "--against-baseline" => {
                let Some(p) = iter.next() else {
                    return Err("ebman lint: --against-baseline expects a file path".into());
                };
                if p.starts_with("--") {
                    return Err(format!(
                        "ebman lint: --against-baseline expects a file path, got flag '{p}'"
                    ));
                }
                baseline_against = Some(p.clone());
            }
            "--severity" => {
                let Some(v) = iter.next() else {
                    return Err(
                        "ebman lint: --severity expects a value (info / warn / error)".into(),
                    );
                };
                let Some(sev) = lint::Severity::parse(v) else {
                    return Err(format!(
                        "ebman lint: unknown severity '{v}' (info / warn / error)"
                    ));
                };
                severity_filter = Some(sev);
            }
            "--rules" => {
                let Some(v) = iter.next() else {
                    return Err("ebman lint: --rules expects a comma-separated rule id list".into());
                };
                rule_filter = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            other => {
                return Err(format!("ebman lint: unknown flag '{other}'"));
            }
        }
    }

    if watch && fix {
        return Err("ebman lint: --watch and --fix are mutually exclusive (use one)".into());
    }
    if baseline_write.is_some() && baseline_against.is_some() {
        return Err(
            "ebman lint: --baseline (write) and --against-baseline (compare) are mutually exclusive"
                .into(),
        );
    }
    if (baseline_write.is_some() || baseline_against.is_some()) && (fix || watch) {
        return Err(
            "ebman lint: --baseline / --against-baseline are incompatible with --fix / --watch"
                .into(),
        );
    }
    if fix && !yes && !dry_run {
        return Err(
            "ebman lint --fix: requires --yes to dispatch writes (or --dry-run to preview)".into(),
        );
    }
    if fix && yes && dry_run {
        return Err("ebman lint --fix: --yes and --dry-run are mutually exclusive".into());
    }
    // Default interval = 60s. Parse the same way other deadlines
    // are parsed (`5m / 30m / 1h`); accept a bare integer as
    // seconds for monitoring-friendly shapes like `--interval 30`.
    let interval_secs: u64 = match interval_str.as_deref() {
        None => 60,
        Some(s) => {
            if let Ok(n) = s.parse::<u64>() {
                if n == 0 {
                    return Err("ebman lint: --interval must be > 0".into());
                }
                n
            } else if let Some(ms) = aws::parse_window_ms(s) {
                ((ms / 1000) as u64).max(1)
            } else {
                return Err(
                    "ebman lint: --interval expects seconds (`30`) or a duration (`5m`/`1h`)"
                        .into(),
                );
            }
        }
    };

    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parsed.is_empty() {
                return Err("ebman lint: --regions list is empty".into());
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    Ok(LintArgs {
        env_name,
        regions,
        json,
        quiet,
        severity_filter,
        rule_filter,
        fix,
        dry_run,
        yes,
        watch,
        interval_secs,
        baseline_write,
        baseline_against,
    })
}

pub async fn run(args: &[String]) -> Result<()> {
    let LintArgs {
        env_name,
        regions,
        json,
        quiet,
        severity_filter,
        rule_filter,
        fix,
        // `dry_run` is consumed entirely by the parser's validation
        // (--fix needs --yes XOR --dry-run); the apply path below keys
        // on `yes` alone, so it isn't bound here.
        dry_run: _,
        yes,
        watch,
        interval_secs,
        baseline_write,
        baseline_against,
    } = match parse_lint_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(project::load_lint_disables_from_cwd());
    let rules = lint::default_rules(&disabled);

    let mut fix_disabled: Vec<String> = config::load_lint_fix_disables();
    fix_disabled.extend(project::load_lint_fix_disables_from_cwd());

    let safety_cfg = config::load();
    let active_profile_for_safety = std::env::var("AWS_PROFILE").ok();

    let multi_region = regions.len() > 1;
    // `--watch` wraps the existing one-shot body in a polling loop
    // that emits each cycle's issues and sleeps `interval_secs`.
    // Ctrl-C breaks; the exit code reflects the LAST cycle's state
    // so a clean shutdown after a clean cycle exits 0, after a
    // dirty cycle exits 3.
    // Tracks the most-recent cycle's "no issues found" state.
    // Initialised here so the post-loop exit-code branch can read
    // it even if the loop somehow exits without running a full
    // cycle (currently impossible — the unconditional first
    // iteration always sets it — but the initial value keeps the
    // borrow checker honest and documents the invariant).
    let mut last_cycle_clean;
    loop {
        let cycle_started = chrono::Utc::now();
        if watch && !quiet && !json {
            println!("--- {} ---", cycle_started.to_rfc3339());
        }
        let mut all_issues: Vec<lint::Issue> = Vec::new();
        for region_opt in &regions {
            let aws = match aws::AwsClient::with(None, region_opt.clone()).await {
                Ok(c) => c,
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: skipping region '{region_label}' — AwsClient::with: {e}"
                        );
                    }
                    continue;
                }
            };
            let envs = match aws.list_environments().await {
                Ok(envs) => envs,
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: skipping region '{region_label}' — list_environments: {e}"
                        );
                    }
                    continue;
                }
            };
            // Per-region one-shot fetch for EBL008 (stale platform):
            // `ListAvailableSolutionStacks` is region-scoped + cheap
            // (single call, no pagination). On failure we just skip
            // EBL008 for the region rather than aborting lint — same
            // tolerance pattern the per-env opts/tags/health fetches
            // use below. Added in 0.18 to close the TUI/CLI parity
            // gap noted in the 0.17.1 CHANGELOG.
            let latest_stacks = match aws.list_solution_stacks().await {
                Ok(s) => aws::latest_stack_versions(&s),
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: region '{region_label}' — list_solution_stacks failed: {e} (EBL008 skipped)"
                        );
                    }
                    std::collections::HashMap::new()
                }
            };

            let targets: Vec<&aws::Environment> = match env_name.as_deref() {
                Some(name) => match envs.iter().find(|e| e.name == name) {
                    Some(env) => vec![env],
                    None => {
                        if multi_region && !quiet {
                            let region_label = region_opt.as_deref().unwrap_or("default");
                            eprintln!(
                                "warning: env '{name}' not in region '{region_label}' — skipping"
                            );
                        } else if !multi_region {
                            eprintln!("ebman lint: env '{name}' not found in current context");
                            std::process::exit(2);
                        }
                        continue;
                    }
                },
                None => envs.iter().collect(),
            };

            for env in targets {
                // Parallel per-env fetch: option settings (existing) +
                // tags (EBL010) + instance counts (EBL012). Matches the
                // TUI plumbing in `spawn_confirm_lint`. Tags + health
                // tolerated independently — missing input means the
                // corresponding rule just doesn't fire for that env.
                let opts_fut = aws.fetch_env_option_settings(&env.application, &env.name);
                let tags_fut = async {
                    match env.arn.as_deref() {
                        Some(arn) => aws.list_tags(arn).await.ok(),
                        None => None,
                    }
                };
                let health_fut = aws.fetch_env_instance_counts(&env.name);
                let (opts_res, tags_opt, health_res) = tokio::join!(opts_fut, tags_fut, health_fut);
                let opts = match opts_res {
                    Ok(opts) => opts,
                    Err(e) => {
                        if !quiet {
                            eprintln!(
                                "warning: skipping {} — fetch_env_option_settings: {e}",
                                env.name
                            );
                        }
                        continue;
                    }
                };
                let env_tag_keys: Vec<String> = tags_opt
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                let healthy_count = health_res.ok().map(|c| c.healthy as i64);
                let dlq_depth = None::<i64>; // CLI doesn't poll worker
                                             // queues; leave EBL011 unwired
                                             // for now (TUI-only signal).
                let newer_stack = aws::newer_stack_version(&env.solution_stack, &latest_stacks);
                let mut ctx = lint::LintContext::for_env(env, &opts)
                    .with_required_tags(&safety_cfg.required_tags)
                    .with_env_tag_keys(&env_tag_keys);
                if let Some(newer) = newer_stack.as_deref() {
                    ctx = ctx.with_newer_stack_available(newer);
                }
                if let Some(depth) = dlq_depth {
                    ctx = ctx.with_dlq_depth(depth);
                }
                if let Some(count) = healthy_count {
                    ctx = ctx.with_healthy_count(count);
                }
                let mut issues = lint::run_rules(&rules, &ctx);
                if let Some(min) = severity_filter {
                    issues.retain(|i| i.severity >= min);
                }
                if !rule_filter.is_empty() {
                    issues.retain(|i| rule_filter.contains(&i.rule_id));
                }
                if let Some(region) = region_opt {
                    for issue in &mut issues {
                        issue.fields.insert("region".into(), region.clone());
                    }
                }

                if fix && !issues.is_empty() {
                    let env_pinned = safety_cfg
                        .safety_envs
                        .get(&env.name)
                        .copied()
                        .unwrap_or(false);
                    let account_pinned = active_profile_for_safety
                        .as_deref()
                        .and_then(|p| safety_cfg.safety_accounts.get(p).copied())
                        .unwrap_or(false);
                    if env_pinned || account_pinned {
                        let reason = if env_pinned {
                            format!("safety.envs.{}.read_only", env.name)
                        } else {
                            format!(
                                "safety.accounts.{}.read_only",
                                active_profile_for_safety.as_deref().unwrap_or("?")
                            )
                        };
                        if !quiet {
                            eprintln!(
                                "ebman lint --fix: refusing {} — pinned by {reason}",
                                env.name
                            );
                        }
                        FIX_DISPATCH_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                        all_issues.extend(issues);
                        continue;
                    }
                    let region_label = region_opt.as_deref().unwrap_or("default").to_string();
                    let mut to_set: Vec<(String, String, String)> = Vec::new();
                    let mut planned: Vec<(String, lint::FixAction)> = Vec::new();
                    let mut planned_set_indices: Vec<usize> = Vec::new();
                    for issue in &issues {
                        if fix_disabled.contains(&issue.rule_id) {
                            if !quiet {
                                println!(
                                    "skip {} ({}): in lint.fix_disable",
                                    issue.rule_id, env.name
                                );
                            }
                            continue;
                        }
                        let Some(rule) = rules.iter().find(|r| r.id() == issue.rule_id) else {
                            continue;
                        };
                        let Some(action) = rule.fix(&ctx) else {
                            if !quiet {
                                println!(
                                    "no-fix {} ({}): rule has no auto-remediation",
                                    issue.rule_id, env.name
                                );
                            }
                            continue;
                        };
                        if let lint::FixAction::SetOption {
                            namespace,
                            name,
                            value,
                            ..
                        } = &action
                        {
                            planned_set_indices.push(planned.len());
                            to_set.push((namespace.clone(), name.clone(), value.clone()));
                        }
                        planned.push((issue.rule_id.clone(), action));
                    }
                    for (rule_id, action) in &planned {
                        match action {
                            lint::FixAction::SetOption { description, .. } => {
                                println!("fix {rule_id} ({}): {description}", env.name);
                            }
                            lint::FixAction::Manual { instructions } => {
                                println!(
                                "fix {rule_id} ({}) MANUAL — operator action required:\n  {instructions}",
                                env.name
                            );
                            }
                        }
                    }
                    if !to_set.is_empty() && yes {
                        match aws
                            .update_env_option_settings(&env.name, &to_set, &[])
                            .await
                        {
                            Ok(()) => {
                                for &idx in &planned_set_indices {
                                    let (rule_id, action) = &planned[idx];
                                    if let lint::FixAction::SetOption {
                                        namespace,
                                        name,
                                        value,
                                        ..
                                    } = action
                                    {
                                        audit::append_lint_fix(
                                            &region_label,
                                            &env.name,
                                            rule_id,
                                            namespace,
                                            name,
                                            value,
                                            None,
                                        );
                                    }
                                }
                                if !quiet {
                                    println!(
                                        "ok ({}): applied {} fix(es)",
                                        env.name,
                                        planned_set_indices.len()
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                "ebman lint --fix: dispatch failed for {} in {region_label}: {e}",
                                env.name
                            );
                                let err_str = e.to_string();
                                for &idx in &planned_set_indices {
                                    let (rule_id, action) = &planned[idx];
                                    if let lint::FixAction::SetOption {
                                        namespace,
                                        name,
                                        value,
                                        ..
                                    } = action
                                    {
                                        audit::append_lint_fix(
                                            &region_label,
                                            &env.name,
                                            rule_id,
                                            namespace,
                                            name,
                                            value,
                                            Some(&err_str),
                                        );
                                    }
                                }
                                FIX_DISPATCH_FAILED
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }

                all_issues.extend(issues);
            }
        }

        // Baseline modes (write / diff) handle their own output
        // shape; skip the standard text/json render in those paths.
        let baseline_mode = baseline_write.is_some() || baseline_against.is_some();
        if !quiet && !baseline_mode {
            if json {
                println!("{}", lint::render_issues_json(&all_issues));
            } else if all_issues.is_empty() {
                println!("✓ No issues found");
            } else {
                for issue in &all_issues {
                    let sev = issue.severity.as_str();
                    let env_str = issue.env_name.as_deref().unwrap_or("-");
                    if multi_region {
                        let region = issue
                            .fields
                            .get("region")
                            .map(String::as_str)
                            .unwrap_or("-");
                        println!(
                            "{region}\t{sev}\t{}\t{env_str}\t{}",
                            issue.rule_id, issue.title
                        );
                    } else {
                        println!("{sev}\t{}\t{env_str}\t{}", issue.rule_id, issue.title);
                    }
                    if let Some(s) = &issue.suggestion {
                        println!("\t→ {s}");
                    }
                }
            }
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        // --baseline FILE: snapshot current issues to disk, exit 0.
        // Operators use this once when adopting `ebman lint` on a
        // fleet with existing warnings — grandfathers them so
        // subsequent runs only flag NEW issues.
        if let Some(path) = baseline_write.as_deref() {
            let body = lint::render_issues_json(&all_issues);
            if let Err(e) = std::fs::write(path, &body) {
                eprintln!("ebman lint --baseline: write {path}: {e}");
                std::process::exit(1);
            }
            if !quiet {
                eprintln!(
                    "ebman lint --baseline: wrote {} issue(s) to {path}",
                    all_issues.len()
                );
            }
            last_cycle_clean = true; // snapshot ALWAYS exits 0
        } else if let Some(path) = baseline_against.as_deref() {
            // --against-baseline FILE: diff current issues against
            // the snapshot. NEW issues exit 3; CLEARED issues are
            // informational. Composes with --json (emits a single
            // {new:[...],cleared:[...]} blob).
            let baseline_text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("ebman lint --against-baseline: read {path}: {e}");
                    std::process::exit(1);
                }
            };
            let baseline_issues = match lint::parse_baseline(&baseline_text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("ebman lint --against-baseline: {e}");
                    std::process::exit(1);
                }
            };
            let baseline_set: std::collections::HashSet<&str> = baseline_issues
                .iter()
                .map(|b| b.identity.as_str())
                .collect();
            let current_identities: Vec<String> =
                all_issues.iter().map(lint::issue_identity).collect();
            let current_set: std::collections::HashSet<&str> =
                current_identities.iter().map(String::as_str).collect();

            let new_issues: Vec<&lint::Issue> = all_issues
                .iter()
                .zip(current_identities.iter())
                .filter(|(_, id)| !baseline_set.contains(id.as_str()))
                .map(|(i, _)| i)
                .collect();
            let cleared: Vec<&lint::BaselineIssue> = baseline_issues
                .iter()
                .filter(|b| !current_set.contains(b.identity.as_str()))
                .collect();

            if !quiet {
                if json {
                    print_baseline_diff_json(&new_issues, &cleared);
                } else {
                    if new_issues.is_empty() && cleared.is_empty() {
                        println!(
                            "✓ No drift vs baseline ({} issues stable)",
                            baseline_set.len()
                        );
                    }
                    for issue in &new_issues {
                        let sev = issue.severity.as_str();
                        let env_str = issue.env_name.as_deref().unwrap_or("-");
                        println!(
                            "+ NEW\t{sev}\t{}\t{env_str}\t{}",
                            issue.rule_id, issue.title
                        );
                    }
                    for b in &cleared {
                        let env_str = b.env_name.as_deref().unwrap_or("-");
                        println!("✓ CLEARED\t{}\t{env_str}\t{}", b.rule_id, b.title);
                    }
                }
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }

            last_cycle_clean = new_issues.is_empty();
        } else {
            last_cycle_clean = all_issues.is_empty();
        }

        if !watch {
            break;
        }
        // Sleep `interval_secs` or break on Ctrl-C — whichever
        // fires first. `tokio::signal::ctrl_c` panics if called
        // outside a Tokio runtime, but `run` is `#[tokio::main]`-
        // driven so we're always inside one here.
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if !quiet && !json {
                    eprintln!("(watch interrupted)");
                }
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => {}
        }
    }

    if fix {
        if FIX_DISPATCH_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            std::process::exit(1);
        }
        Ok(())
    } else if last_cycle_clean {
        Ok(())
    } else {
        std::process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_lint_has_sane_defaults() {
        let p = parse_lint_args(&argv(&["lint"])).unwrap();
        assert_eq!(p.regions, vec![None]);
        assert_eq!(p.interval_secs, 60);
        assert!(!p.json && !p.quiet && !p.fix && !p.watch);
        assert!(p.severity_filter.is_none() && p.rule_filter.is_empty());
        assert!(p.baseline_write.is_none() && p.baseline_against.is_none());
    }

    #[test]
    fn collects_filters_and_flags() {
        let p = parse_lint_args(&argv(&[
            "lint",
            "--env",
            "prod",
            "--json",
            "--quiet",
            "--severity",
            "warn",
            "--rules",
            "EBL001, EBL004 ,EBL019",
        ]))
        .unwrap();
        assert_eq!(p.env_name.as_deref(), Some("prod"));
        assert!(p.json && p.quiet);
        assert_eq!(p.severity_filter, Some(lint::Severity::Warn));
        // CSV split + trimmed.
        assert_eq!(p.rule_filter, vec!["EBL001", "EBL004", "EBL019"]);
    }

    #[test]
    fn unknown_flag_and_severity_are_usage_errors() {
        assert!(parse_lint_args(&argv(&["lint", "--bogus"]))
            .unwrap_err()
            .contains("unknown flag"));
        assert!(parse_lint_args(&argv(&["lint", "--severity", "loud"]))
            .unwrap_err()
            .contains("unknown severity"));
    }

    #[test]
    fn baseline_flag_requires_a_path_not_another_flag() {
        // value-flag that swallows the next token must reject a flag
        // sitting where the path should be (the `--baseline --json` trap).
        let err = parse_lint_args(&argv(&["lint", "--baseline", "--json"])).unwrap_err();
        assert!(err.contains("--baseline expects a file path"), "got: {err}");
        // ...and a totally missing value is the same class of error.
        let err2 = parse_lint_args(&argv(&["lint", "--baseline"])).unwrap_err();
        assert!(
            err2.contains("--baseline expects a file path"),
            "got: {err2}"
        );
    }

    #[test]
    fn interval_accepts_bare_seconds_and_durations_rejects_zero_and_garbage() {
        assert_eq!(
            parse_lint_args(&argv(&["lint", "--interval", "30"]))
                .unwrap()
                .interval_secs,
            30
        );
        assert_eq!(
            parse_lint_args(&argv(&["lint", "--interval", "5m"]))
                .unwrap()
                .interval_secs,
            300
        );
        assert!(parse_lint_args(&argv(&["lint", "--interval", "0"]))
            .unwrap_err()
            .contains("must be > 0"));
        assert!(parse_lint_args(&argv(&["lint", "--interval", "soon"]))
            .unwrap_err()
            .contains("expects seconds"));
    }

    #[test]
    fn fix_requires_yes_or_dry_run() {
        // --fix alone is a usage error: it must pick apply (--yes) or
        // preview (--dry-run) explicitly.
        assert!(parse_lint_args(&argv(&["lint", "--fix"]))
            .unwrap_err()
            .contains("requires --yes"));
        // --fix --yes and --fix --dry-run both parse.
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--yes"]))
                .unwrap()
                .fix
        );
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--dry-run"]))
                .unwrap()
                .dry_run
        );
        // ...but not both at once.
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--yes", "--dry-run"]))
                .unwrap_err()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn mutually_exclusive_mode_combinations_are_rejected() {
        assert!(
            parse_lint_args(&argv(&["lint", "--watch", "--fix", "--yes"]))
                .unwrap_err()
                .contains("--watch and --fix")
        );
        assert!(parse_lint_args(&argv(&[
            "lint",
            "--baseline",
            "b.json",
            "--against-baseline",
            "a.json"
        ]))
        .unwrap_err()
        .contains("mutually exclusive"));
        assert!(
            parse_lint_args(&argv(&["lint", "--baseline", "b.json", "--fix", "--yes"]))
                .unwrap_err()
                .contains("incompatible with --fix")
        );
    }

    #[test]
    fn empty_regions_csv_is_usage_error() {
        let err = parse_lint_args(&argv(&["lint", "--regions", " , "])).unwrap_err();
        assert!(err.contains("--regions list is empty"), "got: {err}");
    }
}
