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

/// Parsed `ebman versions` arguments. Separated from [`run`] so the
/// arg-parsing (the part with usage-error exit codes) is unit-testable
/// without driving the live AWS path or tripping `std::process::exit`.
#[derive(Debug, PartialEq, Eq)]
struct VersionsArgs {
    env_name: String,
    json: bool,
}

/// Pure arg parser for `ebman versions --env NAME [--json]`. `args` is
/// the full argv (`args[0]` is the subcommand name), matching the
/// `run(args)` convention. Returns `Err(usage_message)` for the two
/// usage-error (exit-2) cases: an unknown flag, or a missing `--env`.
fn parse_versions_args(args: &[String]) -> Result<VersionsArgs, String> {
    let mut env_name: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--json" => json = true,
            other => return Err(format!("ebman versions: unknown arg '{other}'")),
        }
    }
    let Some(env_name) = env_name else {
        return Err("usage: ebman versions --env NAME [--json]".into());
    };
    Ok(VersionsArgs { env_name, json })
}

pub async fn run(args: &[String]) -> Result<()> {
    let VersionsArgs { env_name, json } = match parse_versions_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_env_and_defaults_json_false() {
        let parsed = parse_versions_args(&argv(&["versions", "--env", "prod-api"])).unwrap();
        assert_eq!(parsed.env_name, "prod-api");
        assert!(!parsed.json);
    }

    #[test]
    fn json_flag_sets_json_true_regardless_of_order() {
        let a = parse_versions_args(&argv(&["versions", "--json", "--env", "prod"])).unwrap();
        let b = parse_versions_args(&argv(&["versions", "--env", "prod", "--json"])).unwrap();
        assert!(a.json && b.json);
        assert_eq!(a, b);
    }

    #[test]
    fn missing_env_is_usage_error() {
        let err = parse_versions_args(&argv(&["versions", "--json"])).unwrap_err();
        assert!(err.contains("usage:"), "got: {err}");
    }

    #[test]
    fn unknown_flag_is_usage_error_naming_the_flag() {
        let err = parse_versions_args(&argv(&["versions", "--env", "p", "--bogus"])).unwrap_err();
        assert!(
            err.contains("unknown arg") && err.contains("--bogus"),
            "got: {err}"
        );
    }

    #[test]
    fn env_as_trailing_token_consumes_nothing_and_is_usage_error() {
        // `--env` with no following value: iter.next() yields None, so
        // env_name stays unset → usage error (matches pre-refactor behaviour).
        let err = parse_versions_args(&argv(&["versions", "--env"])).unwrap_err();
        assert!(err.contains("usage:"), "got: {err}");
    }
}
