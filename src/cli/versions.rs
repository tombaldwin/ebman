//! `ebman versions --env NAME [--json]` — list application versions
//! for the env's application, newest-first. CLI mirror of the TUI
//! `:versions` overlay.
//!
//! Useful for "what versions are deployed in this app right now" CI
//! scripts that want to:
//! - Validate the candidate label exists before running
//!   `ebman action deploy` (avoids a confusing UpdateEnvironment
//!   error several seconds later).
//! - Surface the candidate's description / age in a Slack notify
//!   alongside the deploy.
//! - Drive a `--from-version` flag for downstream tools.
//!
//! Exit codes (per the 0.13 CLI charter):
//! - 0 ok (versions listed, even if empty)
//! - 1 AWS-layer error (list_environments / list_application_versions
//!   failed)
//! - 2 usage error (missing --env, env not found)

use color_eyre::eyre::Result;

use crate::aws;
use crate::cli::cli_esc;

pub async fn run(args: &[String]) -> Result<()> {
    let mut env_name: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--json" => json = true,
            other => {
                eprintln!("ebman versions: unknown arg '{other}'");
                std::process::exit(2);
            }
        }
    }
    let Some(env_name) = env_name else {
        eprintln!("usage: ebman versions --env NAME [--json]");
        std::process::exit(2);
    };

    let aws_client = aws::AwsClient::with(None, None).await?;
    let envs = aws_client
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    let Some(env) = envs.iter().find(|e| e.name == env_name) else {
        eprintln!("ebman versions: env '{env_name}' not found in current context");
        std::process::exit(2);
    };
    let app_name = env.application.clone();
    let deployed_label = env.version_label.clone();
    let versions = aws_client
        .list_application_versions(&app_name)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_application_versions: {e}"))?;

    if json {
        // Hand-rolled JSON to match the project's no-serde_json
        // convention. Schema: array of {label, deployed, created, description}.
        let entries: Vec<String> = versions
            .iter()
            .map(|v| {
                let created_iso = v
                    .created
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default();
                let deployed = v.label == deployed_label;
                format!(
                    "{{\"label\":\"{}\",\"deployed\":{},\"created\":\"{}\",\"description\":\"{}\"}}",
                    cli_esc(&v.label),
                    deployed,
                    cli_esc(&created_iso),
                    cli_esc(&v.description),
                )
            })
            .collect();
        println!("[{}]", entries.join(","));
    } else {
        println!("LABEL\tDEPLOYED\tCREATED\tDESCRIPTION");
        for v in &versions {
            let created_iso = v.created.map(|d| d.to_rfc3339()).unwrap_or_default();
            let marker = if v.label == deployed_label { "*" } else { "" };
            println!(
                "{}\t{}\t{}\t{}",
                v.label, marker, created_iso, v.description
            );
        }
    }
    Ok(())
}
