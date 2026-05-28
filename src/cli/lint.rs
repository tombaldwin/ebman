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

pub async fn run(args: &[String]) -> Result<()> {
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
                    eprintln!("ebman lint: --baseline expects a file path");
                    std::process::exit(2);
                };
                if p.starts_with("--") {
                    eprintln!("ebman lint: --baseline expects a file path, got flag '{p}'");
                    std::process::exit(2);
                }
                baseline_write = Some(p.clone());
            }
            "--against-baseline" => {
                let Some(p) = iter.next() else {
                    eprintln!("ebman lint: --against-baseline expects a file path");
                    std::process::exit(2);
                };
                if p.starts_with("--") {
                    eprintln!("ebman lint: --against-baseline expects a file path, got flag '{p}'");
                    std::process::exit(2);
                }
                baseline_against = Some(p.clone());
            }
            "--severity" => {
                let Some(v) = iter.next() else {
                    eprintln!("ebman lint: --severity expects a value (info / warn / error)");
                    std::process::exit(2);
                };
                let Some(sev) = lint::Severity::parse(v) else {
                    eprintln!("ebman lint: unknown severity '{v}' (info / warn / error)");
                    std::process::exit(2);
                };
                severity_filter = Some(sev);
            }
            "--rules" => {
                let Some(v) = iter.next() else {
                    eprintln!("ebman lint: --rules expects a comma-separated rule id list");
                    std::process::exit(2);
                };
                rule_filter = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            other => {
                eprintln!("ebman lint: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(project::load_lint_disables_from_cwd());
    let rules = lint::default_rules(&disabled);

    let mut fix_disabled: Vec<String> = config::load_lint_fix_disables();
    fix_disabled.extend(project::load_lint_fix_disables_from_cwd());

    let safety_cfg = config::load();
    let active_profile_for_safety = std::env::var("AWS_PROFILE").ok();

    if watch && fix {
        eprintln!("ebman lint: --watch and --fix are mutually exclusive (use one)");
        std::process::exit(2);
    }
    if baseline_write.is_some() && baseline_against.is_some() {
        eprintln!(
            "ebman lint: --baseline (write) and --against-baseline (compare) are mutually exclusive"
        );
        std::process::exit(2);
    }
    if (baseline_write.is_some() || baseline_against.is_some()) && (fix || watch) {
        eprintln!(
            "ebman lint: --baseline / --against-baseline are incompatible with --fix / --watch"
        );
        std::process::exit(2);
    }
    if fix && !yes && !dry_run {
        eprintln!("ebman lint --fix: requires --yes to dispatch writes (or --dry-run to preview)");
        std::process::exit(2);
    }
    if fix && yes && dry_run {
        eprintln!("ebman lint --fix: --yes and --dry-run are mutually exclusive");
        std::process::exit(2);
    }
    // Default interval = 60s. Parse the same way other deadlines
    // are parsed (`5m / 30m / 1h`); accept a bare integer as
    // seconds for monitoring-friendly shapes like `--interval 30`.
    let interval_secs: u64 = match interval_str.as_deref() {
        None => 60,
        Some(s) => {
            if let Ok(n) = s.parse::<u64>() {
                if n == 0 {
                    eprintln!("ebman lint: --interval must be > 0");
                    std::process::exit(2);
                }
                n
            } else if let Some(ms) = aws::parse_window_ms(s) {
                ((ms / 1000) as u64).max(1)
            } else {
                eprintln!(
                    "ebman lint: --interval expects seconds (`30`) or a duration (`5m`/`1h`)"
                );
                std::process::exit(2);
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
                eprintln!("ebman lint: --regions list is empty");
                std::process::exit(2);
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

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
                let opts = match aws
                    .fetch_env_option_settings(&env.application, &env.name)
                    .await
                {
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
                // Plumb `required_tags` from config so `LintContext` is
                // built the same way the TUI's `:lint` overlay builds it
                // (`spawn_confirm_lint` / `cmd_explain_issue` both pass
                // `App.required_tags`). This is the precondition for
                // EBL010 (missing required tags) firing from CLI. The
                // other precondition — `env_tag_keys`, from a
                // `DescribeTags` fetch — is deferred to 0.18 alongside
                // the TUI-side wiring.
                //
                // `latest_stacks` (for EBL008 newer-stack detection)
                // also stays deferred to 0.18 — it would require a
                // one-shot `ListAvailableSolutionStacks` fetch per
                // region. The gap is noted in the 0.17 CHANGELOG.
                let ctx = lint::LintContext::for_env(env, &opts)
                    .with_required_tags(&safety_cfg.required_tags);
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
