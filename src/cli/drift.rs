//! `ebman drift [--env NAME] [--regions r1,r2,r3] [--tfstate PATH]
//! [--tfdir PATH] [--json] [--quiet]` — terraform drift report for
//! CI gates / git hooks.
//!
//! Discovery walks up from cwd for `.terraform/terraform.tfstate` or
//! a local `terraform.tfstate`, or honors explicit `--tfstate PATH`
//! / `--tfdir PATH`. Compares tf-declared option_settings +
//! version_label against live EB state. Non-zero exit on drift so
//! CI scripts can gate `terraform plan` on a clean ebman state.

use color_eyre::eyre::Result;

use crate::{aws, terraform};

pub async fn run(args: &[String]) -> Result<()> {
    let mut env_name: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut tfstate_path: Option<std::path::PathBuf> = None;
    let mut tfdir: Option<std::path::PathBuf> = None;
    let mut json = false;
    let mut quiet = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--regions" => regions_csv = iter.next().cloned(),
            "--tfstate" => tfstate_path = iter.next().map(std::path::PathBuf::from),
            "--tfdir" => tfdir = iter.next().map(std::path::PathBuf::from),
            "--json" => json = true,
            "--quiet" => quiet = true,
            other => {
                eprintln!("ebman drift: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    let (tf_state, used_path) = if let Some(path) = tfstate_path.as_ref() {
        let Some(state) = terraform::load_from_path(path) else {
            eprintln!(
                "ebman drift: could not read or parse tfstate at {}",
                path.display()
            );
            std::process::exit(2);
        };
        (state, Some(path.clone()))
    } else {
        let start = tfdir
            .as_deref()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let abs = start.canonicalize().unwrap_or(start);
        let Some(found) = terraform::find_tfstate(&abs) else {
            if !quiet {
                if json {
                    println!("{{\"tfstate\":null,\"envs\":[]}}");
                } else {
                    eprintln!(
                        "ebman drift: no terraform.tfstate found under {}",
                        abs.display()
                    );
                }
            }
            return Ok(());
        };
        let Some(state) = terraform::load_from_path(&found) else {
            eprintln!(
                "ebman drift: could not parse tfstate at {}",
                found.display()
            );
            std::process::exit(2);
        };
        (state, Some(found))
    };

    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parsed.is_empty() {
                eprintln!("ebman drift: --regions list is empty");
                std::process::exit(2);
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    let multi_region = regions.len() > 1;
    let mut reports: Vec<(Option<String>, String, bool, Vec<terraform::DriftField>)> = Vec::new();
    let mut any_drift = false;
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
        let live_envs = match aws.list_environments().await {
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
            Some(name) => match live_envs.iter().find(|e| e.name == name) {
                Some(env) => vec![env],
                None => {
                    if multi_region && !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: env '{name}' not in region '{region_label}' — skipping"
                        );
                    } else if !multi_region {
                        eprintln!("ebman drift: env '{name}' not found in current context");
                        std::process::exit(2);
                    }
                    continue;
                }
            },
            None => live_envs
                .iter()
                .filter(|e| tf_state.env_by_name(&e.name).is_some())
                .collect(),
        };

        for env in targets {
            let tf_env = tf_state.env_by_name(&env.name);
            let tf_managed = tf_env.is_some();
            let drift = if let Some(tf) = tf_env {
                match aws
                    .fetch_env_option_settings(&env.application, &env.name)
                    .await
                {
                    Ok(opts) => terraform::compute_drift(tf, env, &opts),
                    Err(e) => {
                        if !quiet {
                            eprintln!(
                                "warning: skipping {} — fetch_env_option_settings: {e}",
                                env.name
                            );
                        }
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            if !drift.is_empty() {
                any_drift = true;
            }
            reports.push((region_opt.clone(), env.name.clone(), tf_managed, drift));
        }
    }

    if !quiet {
        if json {
            let shaped: Vec<(String, bool, Vec<terraform::DriftField>)> = reports
                .iter()
                .map(|(region, env, managed, drift)| {
                    let name = if multi_region {
                        if let Some(r) = region {
                            format!("{r}/{env}")
                        } else {
                            env.clone()
                        }
                    } else {
                        env.clone()
                    };
                    (name, *managed, drift.clone())
                })
                .collect();
            println!(
                "{}",
                terraform::render_drift_json(used_path.as_deref(), &shaped)
            );
        } else {
            for (region, env, managed, drift) in &reports {
                let prefix = if multi_region {
                    let r = region.as_deref().unwrap_or("default");
                    format!("{r}\t")
                } else {
                    String::new()
                };
                if drift.is_empty() {
                    if *managed {
                        println!("{prefix}{env}\t✓ no drift");
                    }
                    continue;
                }
                for d in drift {
                    let target = match (d.namespace.as_deref(), d.name.as_deref()) {
                        (Some(ns), Some(n)) => format!("{ns}/{n}"),
                        (_, Some(n)) => n.to_string(),
                        _ => d.kind.clone(),
                    };
                    println!(
                        "{prefix}{env}\t{}\t{target}\ttf={}\tlive={}",
                        d.kind, d.tf_value, d.live_value
                    );
                }
            }
        }
    }

    if any_drift {
        std::process::exit(3);
    }
    Ok(())
}
