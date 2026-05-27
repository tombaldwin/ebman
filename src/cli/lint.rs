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

use crate::cli::FIX_DISPATCH_FAILED;
use crate::{audit, aws, config, lint, project};

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

    if fix && !yes && !dry_run {
        eprintln!("ebman lint --fix: requires --yes to dispatch writes (or --dry-run to preview)");
        std::process::exit(2);
    }
    if fix && yes && dry_run {
        eprintln!("ebman lint --fix: --yes and --dry-run are mutually exclusive");
        std::process::exit(2);
    }

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
    let mut all_issues: Vec<lint::Issue> = Vec::new();
    for region_opt in &regions {
        let aws = match aws::AwsClient::with(None, region_opt.clone()).await {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — AwsClient::with: {e}");
                }
                continue;
            }
        };
        let envs = match aws.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — list_environments: {e}");
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
            let ctx = lint::LintContext {
                env,
                options: &opts,
                events: &[],
                cost_usd_per_month: None,
                latest_stack_version: None,
            };
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
                            println!("skip {} ({}): in lint.fix_disable", issue.rule_id, env.name);
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
                            FIX_DISPATCH_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }

            all_issues.extend(issues);
        }
    }

    if !quiet {
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
    }

    if fix {
        if FIX_DISPATCH_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            std::process::exit(1);
        }
        Ok(())
    } else if all_issues.is_empty() {
        Ok(())
    } else {
        std::process::exit(3);
    }
}
