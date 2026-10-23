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

/// Parsed `ebman drift` flags. Region CSV is resolved to a list of
/// `Option<String>` targets here (a single `None` means "the default
/// region"), so the empty-CSV usage error is decided at parse time.
#[derive(Debug, PartialEq, Eq)]
struct DriftArgs {
    env_name: Option<String>,
    regions: Vec<Option<String>>,
    tfstate_path: Option<std::path::PathBuf>,
    tfdir: Option<std::path::PathBuf>,
    json: bool,
    quiet: bool,
    no_redact: bool,
}

/// Pure arg parser for `ebman drift`. Separated from [`run`] so the
/// flag matrix + the three usage-error (exit-2) cases — unknown flag,
/// `--regions` absent value, `--regions` CSV that trims to empty — are
/// unit-testable without the live AWS / tfstate I/O or `process::exit`.
/// Returns `Err(usage_message)` for those cases.
fn parse_drift_args(args: &[String]) -> Result<DriftArgs, String> {
    let mut env_name: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut tfstate_path: Option<std::path::PathBuf> = None;
    let mut tfdir: Option<std::path::PathBuf> = None;
    let mut json = false;
    let mut quiet = false;
    let mut no_redact = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => {
                env_name = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman drift",
                    "--env",
                    "an env name",
                )?)
            }
            "--regions" => {
                regions_csv = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman drift",
                    "--regions",
                    "a region list",
                )?)
            }
            "--tfstate" => {
                tfstate_path = Some(std::path::PathBuf::from(crate::cli::take_value(
                    &mut iter,
                    "ebman drift",
                    "--tfstate",
                    "a file path",
                )?))
            }
            "--tfdir" => {
                tfdir = Some(std::path::PathBuf::from(crate::cli::take_value(
                    &mut iter,
                    "ebman drift",
                    "--tfdir",
                    "a directory path",
                )?))
            }
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--no-redact" => no_redact = true,
            other => return Err(format!("ebman drift: unknown flag '{other}'")),
        }
    }

    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = crate::util::split_csv(&csv);
            if parsed.is_empty() {
                return Err("ebman drift: --regions list is empty".into());
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    Ok(DriftArgs {
        env_name,
        regions,
        tfstate_path,
        tfdir,
        json,
        quiet,
        no_redact,
    })
}

/// What the no-tfstate path emits.
///
/// Extracted as a value because the harness that found the gap cannot
/// see the alternative. `cargo mutants` runs `-- --lib`, so a
/// behavioural test in `tests/cli.rs` does not compile there: the
/// `delete !` mutant on `if !quiet` was reported MISSED, covered by an
/// integration test, and reported MISSED again. A decision that returns
/// a value is reachable from a lib test and therefore from the gate.
///
/// `quiet` wins over `json`: an operator who asked for silence gets it,
/// and a CI script that passes both is asking for nothing rather than
/// for JSON.
#[derive(Debug, PartialEq, Eq)]
enum NoState {
    Silent,
    Json(String),
    Hint(String),
}

fn no_state_output(quiet: bool, json: bool) -> NoState {
    if quiet {
        return NoState::Silent;
    }
    if json {
        // Through the renderer, not a hand-written literal: the literal
        // predated the `state` block and so omitted it, while every
        // other drift response carries it. A consumer parsing the
        // no-state case found a missing key where the rest of the
        // surface gives an explicit null.
        return NoState::Json(terraform::render_drift_json(None, None, &[]));
    }
    NoState::Hint(terraform::no_state_hint("--tfstate PATH"))
}

/// `ebman drift` — compare Terraform state against the live fleet.
///
/// Three outcomes, deliberately distinct: **3** drift found (the
/// actionable one, and it wins over a degraded run), **1** no drift
/// but coverage was incomplete so clean is unproven, **0** clean and
/// complete. Exit 2 on a usage error.
///
/// Reads state FILES and never talks to a backend — for a remote
/// backend, `terraform state pull` first.
pub async fn run(args: &[String]) -> Result<()> {
    let DriftArgs {
        env_name,
        regions,
        tfstate_path,
        tfdir,
        json,
        quiet,
        no_redact,
    } = match parse_drift_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    // Explicit flag, then `terraform.state_path`, then discovery. The
    // config rung is what makes this usable on a fleet whose state is
    // in a remote backend and therefore never discoverable from cwd.
    let configured = crate::config::load().terraform_state_path;
    // `--tfdir` suppresses the config rung. An explicit flag must never
    // lose to a config default: with `terraform.state_path` set
    // globally, `ebman drift --tfdir ~/git/fleetB` silently ignored
    // fleetB's local state and compared fleetB's LIVE environments
    // against fleetA's intent. Where both accounts have an `api-prod`
    // — an ordinary naming convention — that is a confident drift
    // report about the wrong fleet, which is the failure `lineage`
    // exists to expose after the fact and this prevents up front.
    let configured = if tfdir.is_some() { None } else { configured };
    // A `--tfdir` that does not resolve is an error, not a fall-back to
    // cwd. It silently became `"."`, so `drift --tfdir /no/such/dir`
    // with a tfstate in the current directory reported confidently on
    // whatever fleet THAT state describes — the wrong-fleet shape this
    // file's own comments warn about, reached by a typo. An operator
    // who named a directory must hear that the name was wrong.
    let start = match tfdir.as_deref() {
        Some(dir) => match dir.canonicalize() {
            Ok(abs) => abs,
            Err(e) => {
                eprintln!("ebman drift: --tfdir {}: {e}", dir.display());
                std::process::exit(2);
            }
        },
        // No `--tfdir`: discovery starts from the absolute cwd.
        // `Path::new(".").ancestors()` yields only `"."` and `""`, so a
        // relative start cannot walk up at all — the same defect just
        // fixed in the MCP drift path.
        None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    };
    let tfstate_path =
        terraform::resolve_state_path(tfstate_path.as_deref(), configured.as_deref(), &start);
    // One resolution, one load. `resolve_state_path` above already
    // applied the full precedence INCLUDING discovery over the same
    // start, so the old `else` branch re-ran `find_tfstate` and its
    // load arm was unreachable — only its "nothing found" message ever
    // executed. Collapsing removes a second copy of the load-and-parse
    // error path that could drift from this one.
    let Some(path) = tfstate_path else {
        match no_state_output(quiet, json) {
            NoState::Silent => {}
            NoState::Json(body) => println!("{body}"),
            NoState::Hint(msg) => eprintln!("ebman drift: {msg}"),
        }
        return Ok(());
    };
    let Some(tf_state) = terraform::load_from_path(&path) else {
        eprintln!(
            "ebman drift: could not read or parse tfstate at {}",
            path.display()
        );
        std::process::exit(2);
    };
    let code = drift_body(
        &DriftRun {
            env_name,
            regions,
            json,
            quiet,
            no_redact,
        },
        &tf_state,
        Some(&path),
        |region| async move { aws::AwsClient::with(None, region).await },
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .await;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// The flags the comparison itself reads.
pub(crate) struct DriftRun {
    pub env_name: Option<String>,
    pub regions: Vec<Option<String>>,
    pub json: bool,
    pub quiet: bool,
    pub no_redact: bool,
}

/// `ebman drift`'s comparison against any client factory and writers:
/// each region's live envs against `tf_state`, the report, and the exit
/// code. `client_for` builds the client for a region (`None`: the
/// default), as `lint`'s cycle does, so a test drives it with mocks.
pub(crate) async fn drift_body<F, Fut>(
    opts: &DriftRun,
    tf_state: &terraform::TfState,
    used_path: Option<&std::path::Path>,
    client_for: F,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32
where
    F: Fn(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<aws::AwsClient>>,
{
    let DriftRun {
        env_name,
        regions,
        json,
        quiet,
        no_redact,
    } = opts;
    let (json, quiet, no_redact) = (*json, *quiet, *no_redact);
    let multi_region = regions.len() > 1;
    let mut reports: Vec<(Option<String>, String, bool, Vec<terraform::DriftField>)> = Vec::new();
    let mut any_drift = false;
    // Any skipped region/env means the report is incomplete — the run
    // must exit 1 (the documented AWS-error code), not report a clean
    // 0 built from whatever survived the outage.
    let mut degraded = false;
    // `--env NAME` across regions: seen anywhere, and did every region
    // answer? See `lint::env_not_found_anywhere`, shared with `lint`.
    let mut env_found = false;
    let mut every_region_answered = true;
    for region_opt in regions {
        let region_label = region_opt.as_deref().unwrap_or("default");
        let aws = match client_for(region_opt.clone()).await {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    let _ = writeln!(
                        err,
                        "warning: skipping region '{region_label}' — AwsClient::with: {e}"
                    );
                }
                degraded = true;
                every_region_answered = false;
                continue;
            }
        };
        let live_envs = match aws.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                if !quiet {
                    let _ = writeln!(
                        err,
                        "warning: skipping region '{region_label}' — list_environments: {e}"
                    );
                }
                degraded = true;
                every_region_answered = false;
                continue;
            }
        };

        let targets: Vec<&aws::Environment> = match env_name.as_deref() {
            Some(name) => match live_envs.iter().find(|e| e.name == name) {
                Some(env) => {
                    env_found = true;
                    vec![env]
                }
                None => {
                    if multi_region && !quiet {
                        let _ = writeln!(
                            err,
                            "warning: env '{name}' not in region '{region_label}' — skipping"
                        );
                    } else if !multi_region {
                        let _ = writeln!(
                            err,
                            "ebman drift: env '{name}' not found in current context"
                        );
                        return 2;
                    }
                    continue;
                }
            },
            None => live_envs
                .iter()
                .filter(|e| tf_state.env_for(&e.name, &aws.context.region).is_some())
                .collect(),
        };

        for env in targets {
            let tf_env = tf_state.env_for(&env.name, &aws.context.region);
            let tf_managed = tf_env.is_some();
            let drift = if let Some(tf) = tf_env {
                match aws
                    .fetch_env_option_settings(&env.application, &env.name)
                    .await
                {
                    Ok(opts) => {
                        let mut fields = terraform::compute_drift(tf, env, &opts);
                        // Same redaction contract as get_option_settings
                        // / the MCP drift tool — a drifted env-var secret
                        // otherwise lands in CI logs verbatim.
                        if !no_redact {
                            terraform::redact_drift_fields(&mut fields);
                        }
                        fields
                    }
                    Err(e) => {
                        if !quiet {
                            let _ = writeln!(
                                err,
                                "warning: skipping {} — fetch_env_option_settings: {e}",
                                env.name
                            );
                        }
                        degraded = true;
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
    // Printed regardless of `--quiet`: it is the reason for the exit.
    if let Some(msg) = crate::cli::lint::env_not_found_anywhere(
        env_name.as_deref(),
        regions.len(),
        env_found,
        every_region_answered,
    ) {
        let _ = writeln!(err, "ebman drift: {msg}");
        return 2;
    }

    if !quiet {
        if json {
            let shaped: Vec<(String, bool, Vec<terraform::DriftField>)> = reports
                .iter()
                .map(|(region, env, managed, drift)| {
                    let name = match region {
                        Some(r) if multi_region => format!("{r}/{env}"),
                        _ => env.clone(),
                    };
                    (name, *managed, drift.clone())
                })
                .collect();
            let _ = writeln!(
                out,
                "{}",
                terraform::render_drift_json(
                    used_path,
                    Some(&terraform::StateProvenance::of(tf_state, used_path)),
                    &shaped,
                )
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
                        let _ = writeln!(out, "{prefix}{env}\t✓ no drift");
                    }
                    continue;
                }
                for d in drift {
                    let target = match (d.namespace.as_deref(), d.name.as_deref()) {
                        (Some(ns), Some(n)) => format!("{ns}/{n}"),
                        (_, Some(n)) => n.to_string(),
                        _ => d.kind.clone(),
                    };
                    let _ = writeln!(
                        out,
                        "{prefix}{env}\t{}\t{target}\ttf={}\tlive={}",
                        d.kind, d.tf_value, d.live_value
                    );
                }
            }
        }
    }

    if any_drift {
        // Drift found wins over degraded — exit 3 is actionable.
        return 3;
    }
    if degraded {
        // "No drift" but incomplete coverage: clean is unproven.
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_drift_defaults_to_single_default_region() {
        let p = parse_drift_args(&argv(&["drift"])).unwrap();
        assert_eq!(p.regions, vec![None]);
        assert!(p.env_name.is_none() && !p.json && !p.quiet);
        assert!(p.tfstate_path.is_none() && p.tfdir.is_none());
    }

    #[test]
    fn collects_all_flags() {
        let p = parse_drift_args(&argv(&[
            "drift",
            "--env",
            "prod-api",
            "--tfstate",
            "/tmp/terraform.tfstate",
            "--tfdir",
            "/repo/infra",
            "--json",
            "--quiet",
        ]))
        .unwrap();
        assert_eq!(p.env_name.as_deref(), Some("prod-api"));
        assert_eq!(
            p.tfstate_path,
            Some(std::path::PathBuf::from("/tmp/terraform.tfstate"))
        );
        assert_eq!(p.tfdir, Some(std::path::PathBuf::from("/repo/infra")));
        assert!(p.json && p.quiet);
    }

    #[test]
    fn regions_csv_is_split_trimmed_and_wrapped() {
        let p =
            parse_drift_args(&argv(&["drift", "--regions", " us-east-1 , eu-west-2 "])).unwrap();
        assert_eq!(
            p.regions,
            vec![Some("us-east-1".to_string()), Some("eu-west-2".to_string())]
        );
    }

    #[test]
    fn empty_regions_csv_is_usage_error() {
        // A CSV that trims to nothing (e.g. " , , ") must not silently
        // become a zero-region run — it's an exit-2 usage error.
        let err = parse_drift_args(&argv(&["drift", "--regions", " , , "])).unwrap_err();
        assert!(err.contains("--regions list is empty"), "got: {err}");
    }

    #[test]
    fn unknown_flag_is_usage_error_naming_the_flag() {
        let err = parse_drift_args(&argv(&["drift", "--bogus"])).unwrap_err();
        assert!(
            err.contains("unknown flag") && err.contains("--bogus"),
            "got: {err}"
        );
    }

    #[test]
    fn value_flags_reject_missing_or_flag_values() {
        // The 0.27 tightening the old test said would be "a deliberate
        // change" — this is it. A trailing `--regions` used to silently
        // fall back to the default region (scope change, not error).
        assert!(parse_drift_args(&argv(&["drift", "--regions"]))
            .unwrap_err()
            .contains("--regions expects"));
        assert!(parse_drift_args(&argv(&["drift", "--env", "--json"]))
            .unwrap_err()
            .contains("got flag"));
        assert!(parse_drift_args(&argv(&["drift", "--tfstate"]))
            .unwrap_err()
            .contains("--tfstate expects"));
    }

    /// Both drift surfaces must consult `terraform.state_path`.
    ///
    /// `resolve_state_path` is tested directly, but passing `None` for
    /// the config rung at either call site restores the bug it exists
    /// to fix — a fleet on a remote backend gets no drift — and the
    /// resolver's own tests would still pass. Neither site is reachable
    /// from a test: the CLI one reads config from disk and exits, the
    /// MCP one needs AWS.
    #[test]
    fn both_drift_paths_consult_the_configured_state_path() {
        for (file, needle) in [
            ("src/cli/drift.rs", "configured.as_deref()"),
            (
                "src/cli/mcp/tools.rs",
                "self.safety_cfg.terraform_state_path.as_deref()",
            ),
        ] {
            // `production_source`, not a hand-rolled split. The split
            // this replaced had already been fixed once: an inline
            // `#[cfg(test)]` statement — the injected-client seam in
            // `tools.rs` is one — truncated the slice and this guard
            // cried wolf over wiring that was there. The narrowed
            // version then missed the `#[path]` and `pub(crate) mod`
            // spellings, and being a PREFIX split it still dropped
            // everything after a mid-file test module. Third time
            // through, it goes to the shared helper.
            let prod = crate::app::tests::scan::production_source(file);
            assert!(
                prod.contains("resolve_state_path("),
                "{file} must resolve through the shared precedence"
            );
            assert!(
                prod.contains(needle),
                "{file} must pass the CONFIGURED path, not None — otherwise \
                 a fleet whose state is remote has no drift at all"
            );
        }
    }

    /// An explicit `--tfdir` must suppress the config rung.
    ///
    /// `resolve_state_path` puts config above discovery, and `--tfdir`
    /// feeds discovery — so with `terraform.state_path` set globally,
    /// `ebman drift --tfdir ~/git/fleetB` silently ignored fleetB's
    /// local state and compared fleetB's LIVE environments against
    /// fleetA's intent. Where both accounts have an `api-prod` — an
    /// ordinary naming convention — that is a confident drift report
    /// about the wrong fleet.
    ///
    /// Source-scanned because the call reads config from disk and
    /// exits; the same shape as its sibling guard below.
    #[test]
    fn an_explicit_tfdir_outranks_the_configured_state_path() {
        let prod = crate::app::tests::scan::production_source("cli/drift.rs");
        assert!(
            prod.contains("if tfdir.is_some() { None } else { configured }"),
            "an explicit --tfdir must suppress the config default, or a \
             global terraform.state_path silently wins over the directory \
             the operator named"
        );
        // Canary: the slice must be finding real code.
        assert!(
            prod.contains("resolve_state_path("),
            "the production slice is not finding the resolution"
        );
    }

    /// The no-tfstate JSON must have the same SHAPE as every other
    /// drift response.
    ///
    /// It was a hand-written literal that predated the `state` block,
    /// so it omitted it — a consumer parsing the degenerate case found
    /// a missing key where the rest of the surface gives an explicit
    /// null. `render_drift_json`'s own test insists those read
    /// differently.
    #[test]
    fn the_no_tfstate_json_carries_every_key() {
        let rendered = terraform::render_drift_json(None, None, &[]);
        let v: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        for key in ["tfstate", "state", "envs"] {
            assert!(
                v.get(key).is_some(),
                "the no-state response must carry `{key}` — a missing key \
                 and a null one read differently: {rendered}"
            );
        }
        assert!(v["tfstate"].is_null() && v["state"].is_null());
        assert!(v["envs"].as_array().is_some_and(|e| e.is_empty()));

        // And the CLI must emit exactly that, not a literal of its own.
        let prod = crate::app::tests::scan::production_source("cli/drift.rs");
        assert!(
            !prod.contains(r#""tfstate\":null,\"envs\":[]"#),
            "the hand-written literal is back and will drift from the \
             renderer again"
        );
    }

    /// `--quiet` must suppress, and its absence must not.
    ///
    /// This lives in the LIB, deliberately. The behavioural version in
    /// `tests/cli.rs` is the better test and does not close the gap:
    /// every `cargo mutants` invocation here passes `-- --lib`, so
    /// integration tests are never compiled and the `delete !` mutant
    /// on `if !quiet` was reported MISSED, covered by an integration
    /// test, and reported MISSED again. A decision that returns a value
    /// is reachable from where the gate actually looks.
    #[test]
    fn the_no_state_output_respects_quiet_and_json() {
        assert_eq!(
            no_state_output(true, false),
            NoState::Silent,
            "--quiet must suppress the hint"
        );
        assert_eq!(
            no_state_output(true, true),
            NoState::Silent,
            "--quiet wins over --json: an operator who asked for silence \
             gets it, and a script passing both is asking for nothing"
        );

        match no_state_output(false, false) {
            NoState::Hint(h) => assert!(
                h.contains("terraform state pull"),
                "the hint must name the remote-backend workflow — \
                 \"pass --tfstate\" alone is useless if your state is in \
                 HCP: {h}"
            ),
            other => panic!("a normal run must print the hint, got {other:?}"),
        }

        match no_state_output(false, true) {
            NoState::Json(body) => {
                let v: serde_json::Value =
                    serde_json::from_str(&body).expect("the no-state JSON must parse");
                for key in ["tfstate", "state", "envs"] {
                    assert!(
                        v.get(key).is_some(),
                        "a missing key and a null one read differently to a \
                         consumer: {body}"
                    );
                }
            }
            other => panic!("--json must emit JSON, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod body {
    use super::*;

    /// Which call a region's client refuses.
    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        None,
        Settings,
        Listing,
        /// Not a fault: the live settings hold `DB_PASSWORD=live-secret`.
        Secret,
    }
    use Fault::{Listing, Settings};
    const OK: Fault = Fault::None;

    /// A client for `region` listing `envs` as `(name, version_label)`,
    /// refusing the call `fault` names.
    fn client(region: &str, envs: &[(&str, &str)], fault: Fault) -> aws::AwsClient {
        let settings_denied = fault == Settings;
        let listing_denied = fault == Listing;
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
        let envs: Vec<(String, String)> = envs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        let region_s = region.to_string();
        let refused = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_environments)
            .match_requests(move |_| listing_denied)
            .then_error(|| {
                aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError::generic(
                    aws_smithy_types::error::ErrorMetadata::builder()
                        .code("AccessDeniedException")
                        .message("not authorized")
                        .build(),
                )
            });
        let listing = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_environments)
            .match_requests(move |_| !listing_denied)
            .then_output(move || {
                let mut b = DescribeEnvironmentsOutput::builder();
                for (name, version) in &envs {
                    b = b.environments(
                        EnvironmentDescription::builder()
                            .environment_name(name)
                            .environment_arn(format!(
                                "arn:aws:elasticbeanstalk:{region_s}:123456789012:environment/shop/{name}"
                            ))
                            .application_name("shop")
                            .version_label(version)
                            .status("Ready".into())
                            .health("Green".into())
                            .build(),
                    );
                }
                b.build()
            });
        let settings = if settings_denied {
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_configuration_settings)
                .then_error(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsError::generic(
                        aws_smithy_types::error::ErrorMetadata::builder()
                            .code("AccessDeniedException")
                            .message("not authorized")
                            .build(),
                    )
                })
        } else {
            let secret = fault == Fault::Secret;
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_configuration_settings)
                .then_output(move || {
                    use aws_sdk_elasticbeanstalk::types::{
                        ConfigurationOptionSetting, ConfigurationSettingsDescription,
                    };
                    let mut out = aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder();
                    if secret {
                        out = out.configuration_settings(
                            ConfigurationSettingsDescription::builder()
                                .option_settings(
                                    ConfigurationOptionSetting::builder()
                                        .namespace(SECRET_NS)
                                        .option_name("DB_PASSWORD")
                                        .value("live-secret")
                                        .build(),
                                )
                                .build(),
                        );
                    }
                    out.build()
                })
        };
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&refused, &listing, &settings]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new(region.to_string()))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let mut c = aws::AwsClient::for_tests(
            eb,
            aws_sdk_sqs::Client::new(&cfg),
            aws_sdk_cloudwatch::Client::new(&cfg),
            aws_sdk_cloudwatchlogs::Client::new(&cfg),
            aws_sdk_s3::Client::new(&cfg),
            aws_sdk_ec2::Client::new(&cfg),
        );
        c.context.region = region.to_string();
        c
    }

    /// Terraform says `api` runs `build-2`, in us-east-1 when `arn`.
    const SECRET_NS: &str = "aws:elasticbeanstalk:application:environment";

    /// [`state`], with Terraform pinning `DB_PASSWORD=tf-secret`.
    fn state_with_secret() -> terraform::TfState {
        let mut tf = state(false);
        tf.envs[0].options = vec![(SECRET_NS.into(), "DB_PASSWORD".into(), "tf-secret".into())];
        tf
    }

    fn state(arn: bool) -> terraform::TfState {
        terraform::TfState {
            serial: Some(7),
            lineage: None,
            envs: vec![terraform::TfEnv {
                name: "api".into(),
                application: "shop".into(),
                version_label: "build-2".into(),
                region: arn.then(|| "us-east-1".to_string()),
                ..Default::default()
            }],
        }
    }

    fn opts(regions: &[&str], env: Option<&str>, json: bool) -> DriftRun {
        DriftRun {
            env_name: env.map(Into::into),
            regions: if regions.is_empty() {
                vec![None]
            } else {
                regions.iter().map(|r| Some(r.to_string())).collect()
            },
            json,
            quiet: false,
            no_redact: false,
        }
    }

    /// One region of a test fleet: its name, its envs as
    /// `(name, version_label)`, and the call its client refuses.
    type Region = (&'static str, &'static [(&'static str, &'static str)], Fault);

    /// Run against clients per region, a region missing from the list
    /// failing to build a client.
    async fn run(
        o: &DriftRun,
        tf: &terraform::TfState,
        fleet: &'static [Region],
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = drift_body(
            o,
            tf,
            None,
            |region: Option<String>| async move {
                let r = region.unwrap_or_else(|| "us-east-1".into());
                match fleet.iter().find(|(name, _, _)| *name == r) {
                    Some((name, envs, fault)) => Ok(client(name, envs, *fault)),
                    None => Err(color_eyre::eyre::eyre!("no credentials for {r}")),
                }
            },
            &mut out,
            &mut err,
        )
        .await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn clean_drifted_and_incomplete_exit_0_3_and_1() {
        let (code, out, _) = run(
            &opts(&[], None, false),
            &state(false),
            &[("us-east-1", &[("api", "build-2")], OK)],
        )
        .await;
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("api\t✓ no drift"), "{out}");

        let (code, out, _) = run(
            &opts(&[], None, false),
            &state(false),
            &[("us-east-1", &[("api", "build-1")], OK)],
        )
        .await;
        assert_eq!(code, 3);
        assert!(
            out.contains("api\tversion_label\tversion_label\ttf=build-2\tlive=build-1"),
            "{out}"
        );

        let (code, _, err) = run(
            &opts(&[], None, false),
            &state(false),
            &[("us-east-1", &[("api", "build-2")], Settings)],
        )
        .await;
        assert_eq!(code, 1, "settings unreadable: clean is unproven: {err}");

        // Drift wins over an incomplete region.
        let (code, _, err) = run(
            &opts(&["us-east-1", "eu-west-2"], None, false),
            &state(false),
            &[("us-east-1", &[("api", "build-1")], OK)],
        )
        .await;
        assert_eq!(code, 3, "{err}");
        assert!(err.contains("skipping region 'eu-west-2'"), "{err}");
    }

    #[tokio::test]
    async fn a_named_env_nowhere_is_a_usage_error() {
        let (code, _, err) = run(
            &opts(&[], Some("nope"), false),
            &state(false),
            &[("us-east-1", &[("api", "build-2")], OK)],
        )
        .await;
        assert_eq!(code, 2, "{err}");
        let (code, _, err) = run(
            &opts(&["us-east-1", "eu-west-2"], Some("nope"), false),
            &state(false),
            &[
                ("us-east-1", &[("api", "build-2")], OK),
                ("eu-west-2", &[], OK),
            ],
        )
        .await;
        assert_eq!(code, 2, "in no region of two: {err}");
    }

    /// The resource's arn names us-east-1, so the eu-west-2 env of the
    /// same name is not compared with it.
    #[tokio::test]
    async fn the_twin_in_another_region_is_not_compared() {
        let (code, out, _) = run(
            &opts(&["us-east-1", "eu-west-2"], None, true),
            &state(true),
            &[
                ("us-east-1", &[("api", "build-2")], OK),
                ("eu-west-2", &[("api", "build-1")], OK),
            ],
        )
        .await;
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("us-east-1/api") && !out.contains("eu-west-2/api"),
            "{out}"
        );
    }

    /// A region that cannot be reached, or will not list its envs, makes
    /// a clean result unproven: exit 1, each on its own.
    #[tokio::test]
    async fn a_region_that_does_not_answer_makes_clean_unproven() {
        let two = opts(&["us-east-1", "eu-west-2"], None, false);
        let (code, _, err) = run(
            &two,
            &state(false),
            &[("us-east-1", &[("api", "build-2")], OK)],
        )
        .await;
        assert_eq!(code, 1, "eu-west-2 had no client: {err}");
        let (code, _, err) = run(
            &two,
            &state(false),
            &[
                ("us-east-1", &[("api", "build-2")], OK),
                ("eu-west-2", &[], Listing),
            ],
        )
        .await;
        assert_eq!(code, 1, "eu-west-2 would not list: {err}");
        assert!(err.contains("list_environments"), "{err}");
    }

    /// A drifted secret is redacted unless `--no-redact`: drift output
    /// lands in CI logs.
    #[tokio::test]
    async fn a_drifted_secret_is_redacted_unless_asked() {
        let fleet: &'static [Region] = &[("us-east-1", &[("api", "build-2")], Fault::Secret)];
        let (code, out, err) = run(&opts(&[], None, false), &state_with_secret(), fleet).await;
        assert_eq!(code, 3, "{err}");
        assert!(out.contains("DB_PASSWORD"), "the drift is reported: {out}");
        assert!(
            !out.contains("live-secret") && !out.contains("tf-secret"),
            "{out}"
        );

        let mut bare = opts(&[], None, false);
        bare.no_redact = true;
        let (_, out, _) = run(&bare, &state_with_secret(), fleet).await;
        assert!(
            out.contains("tf=tf-secret") && out.contains("live=live-secret"),
            "{out}"
        );
    }

    /// `--env NAME` found in one of two regions is a run, not a miss.
    #[tokio::test]
    async fn a_named_env_found_in_one_region_of_two_is_compared() {
        let (code, out, err) = run(
            &opts(&["us-east-1", "eu-west-2"], Some("api"), false),
            &state(false),
            &[
                ("us-east-1", &[("api", "build-2")], OK),
                ("eu-west-2", &[], OK),
            ],
        )
        .await;
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("us-east-1\tapi\t✓ no drift"), "{out}");
    }
}
