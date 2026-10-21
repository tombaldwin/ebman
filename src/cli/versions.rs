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
            "--env" => {
                env_name = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman versions",
                    "--env",
                    "an env name",
                )?)
            }
            "--json" => json = true,
            other => return Err(format!("ebman versions: unknown arg '{other}'")),
        }
    }
    let Some(env_name) = env_name else {
        return Err("usage: ebman versions --env NAME [--json]".into());
    };
    Ok(VersionsArgs { env_name, json })
}

/// `ebman versions` — list an application's versions.
///
/// Exit 2 on a usage error. The thin shell over [`versions_with`].
pub async fn run(args: &[String]) -> Result<()> {
    let args = match parse_versions_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let aws_client = aws::AwsClient::with(None, None).await?;
    let code = versions_with(
        &aws_client,
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .await?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// The body of `ebman versions`, against any client and writers.
/// Returns the exit status: 2 when the env is not in the current
/// context (a usage error, per the module's charter); an AWS failure
/// is the `Err`, which `main` exits 1 on.
async fn versions_with(
    aws_client: &aws::AwsClient,
    args: &VersionsArgs,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<i32> {
    let VersionsArgs { env_name, json } = args;
    let envs = aws_client
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    let Some(env) = envs.iter().find(|e| &e.name == env_name) else {
        writeln!(
            err,
            "ebman versions: env '{env_name}' not found in current context"
        )?;
        return Ok(2);
    };
    let app_name = env.application.clone();
    let deployed_label = env.version_label.clone();
    let versions = aws_client
        .list_application_versions(&app_name)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_application_versions: {e}"))?;

    if *json {
        // Hand-rolled JSON to match the project's no-serde_json
        // convention. Schema: array of {label, deployed, created, description}.
        let entries: Vec<String> = versions
            .iter()
            .map(|v| {
                // `created` is null when EB reported no date — an
                // empty string confused date-parsing consumers.
                let created_json = v
                    .created
                    .map(|d| format!("\"{}\"", cli_esc(&d.to_rfc3339())))
                    .unwrap_or_else(|| "null".into());
                let deployed = v.label == deployed_label;
                format!(
                    "{{\"label\":\"{}\",\"deployed\":{},\"created\":{},\"description\":\"{}\"}}",
                    cli_esc(&v.label),
                    deployed,
                    created_json,
                    cli_esc(&v.description),
                )
            })
            .collect();
        writeln!(out, "[{}]", entries.join(","))?;
    } else {
        writeln!(out, "LABEL\tDEPLOYED\tCREATED\tDESCRIPTION")?;
        for v in &versions {
            let created_iso = v.created.map(|d| d.to_rfc3339()).unwrap_or_default();
            let marker = if v.label == deployed_label { "*" } else { "" };
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                v.label, marker, created_iso, v.description
            )?;
        }
    }
    Ok(0)
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
    fn env_value_flag_guards() {
        // 0.27 take_value class: a trailing `--env` (or one eating a
        // following flag) is a specific usage error, not the generic
        // usage line the pre-refactor behaviour produced.
        let err = parse_versions_args(&argv(&["versions", "--env"])).unwrap_err();
        assert!(err.contains("--env expects"), "got: {err}");
        let err = parse_versions_args(&argv(&["versions", "--env", "--json"])).unwrap_err();
        assert!(err.contains("got flag"), "got: {err}");
    }
}

/// `ebman versions` driven against a mocked SDK: the deployed marker,
/// and the exit-2 path for an env that is not there.
#[cfg(test)]
mod run_tests {
    use super::*;

    fn client() -> aws::AwsClient {
        use aws_sdk_elasticbeanstalk::operation::describe_application_versions::DescribeApplicationVersionsOutput;
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::types::{
            ApplicationVersionDescription, EnvironmentDescription,
        };
        let listing =
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_environments)
                .then_output(|| {
                    DescribeEnvironmentsOutput::builder()
                        .environments(
                            EnvironmentDescription::builder()
                                .environment_name("shop-prod")
                                .application_name("shop")
                                .version_label("build-2")
                                .build(),
                        )
                        .build()
                });
        let versions = aws_smithy_mocks::mock!(
            aws_sdk_elasticbeanstalk::Client::describe_application_versions
        )
        .then_output(|| {
            let v = |label: &str| {
                ApplicationVersionDescription::builder()
                    .version_label(label)
                    .build()
            };
            DescribeApplicationVersionsOutput::builder()
                .application_versions(v("build-2"))
                .application_versions(v("build-1"))
                .build()
        });
        let sdk = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        aws::AwsClient::for_tests(
            aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&listing, &versions]
            ),
            aws_sdk_sqs::Client::new(&sdk),
            aws_sdk_cloudwatch::Client::new(&sdk),
            aws_sdk_cloudwatchlogs::Client::new(&sdk),
            aws_sdk_s3::Client::new(&sdk),
            aws_sdk_ec2::Client::new(&sdk),
        )
    }

    async fn run(env: &str) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = VersionsArgs {
            env_name: env.into(),
            json: false,
        };
        let code = versions_with(&client(), &args, &mut out, &mut err)
            .await
            .expect("no AWS failure");
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn the_deployed_version_is_marked() {
        let (code, out, _) = run("shop-prod").await;
        assert_eq!(code, 0);
        let marked: Vec<&str> = out
            .lines()
            .skip(1)
            .filter(|l| l.split('\t').nth(1) == Some("*"))
            .collect();
        assert_eq!(marked.len(), 1, "{out}");
        assert!(marked[0].starts_with("build-2\t"), "{out}");
    }

    #[tokio::test]
    async fn an_env_not_in_the_context_exits_2_and_says_so() {
        let (code, out, err) = run("nope").await;
        assert_eq!(code, 2);
        assert!(out.is_empty(), "{out}");
        assert!(err.contains("env 'nope' not found"), "{err}");
    }
}
