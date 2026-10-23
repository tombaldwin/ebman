//! `ebman explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]`
//! — LLM-backed explanation of a lint issue. Loads the configured
//! Provider (Anthropic API or Ollama), assembles the prompt via
//! `llm::build_prompt` (crate-private), dispatches, prints the response.
//!
//! Requires explicit operator consent via `explain.enabled = true`
//! in `config.toml`. The presence of `ANTHROPIC_API_KEY` is not
//! implicit consent — the surface refuses with a clear message
//! pointing at the config edit needed.
//!
//! Exit codes (per the 0.13 CLI charter, extended for 0.14):
//! - 0 ok
//! - 1 provider error (HTTP, parse, auth)
//! - 2 usage error (missing flag, bad rule_id, env not found)
//! - 3 issue not found (rule didn't fire on any env in scope)

use color_eyre::eyre::Result;

use crate::cli::json_string;
use crate::{aws, config, lint, llm, project};

/// Parsed `ebman explain` invocation: the validated `EBL###` issue id
/// plus flags. Separated from [`run`] so the positional-vs-flag
/// disambiguation and the `EBL` prefix check are unit-testable without
/// `std::process::exit` or the live AWS path.
#[derive(Debug, PartialEq, Eq)]
struct ExplainArgs {
    issue_id: String,
    env_name: Option<String>,
    json: bool,
    dry_run: bool,
    no_cache: bool,
}

/// Pure parser for `ebman explain`. Returns `Err(msg)` for the three
/// exit-2 usage paths: an unknown `--flag`, a missing positional
/// ISSUE_ID, or an ISSUE_ID that isn't an `EBL###` id. A bare
/// (non-dash) positional is the ISSUE_ID; last positional wins.
fn parse_explain_args(args: &[String]) -> Result<ExplainArgs, String> {
    let mut issue_id: Option<String> = None;
    let mut env_name: Option<String> = None;
    let mut json = false;
    let mut dry_run = false;
    let mut no_cache = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => {
                env_name = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman explain",
                    "--env",
                    "an env name",
                )?)
            }
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--no-cache" => no_cache = true,
            other if other.starts_with("--") => {
                return Err(format!("ebman explain: unknown flag '{other}'"));
            }
            other => {
                // Positional ISSUE_ID; last positional wins (operator
                // re-typed the same arg).
                issue_id = Some(other.to_string());
            }
        }
    }
    let Some(issue_id) = issue_id else {
        return Err(
            "usage: ebman explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]".into(),
        );
    };
    if !issue_id.starts_with("EBL") {
        return Err("ebman explain: ISSUE_ID must be an EBL### rule id (e.g. EBL001)".into());
    }
    Ok(ExplainArgs {
        issue_id,
        env_name,
        json,
        dry_run,
        no_cache,
    })
}

/// `ebman explain <rule-id>` — an LLM explanation of one lint finding.
///
/// Exit 3 when no environment in scope has that issue — nothing to
/// explain is not an error in the rule engine, and must not read as
/// one. Exit 2 on a usage error, 1 on an LLM or network failure.
pub async fn run(args: &[String]) -> Result<()> {
    let ExplainArgs {
        issue_id,
        env_name,
        json,
        dry_run,
        no_cache,
    } = match parse_explain_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let cfg = config::load();
    let settings = llm::Settings::from_config(&cfg);
    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(project::load_lint_disables_from_cwd());
    let aws_client = aws::AwsClient::with(None, None).await?;
    let settings = &settings;
    let code = explain_body(
        &ExplainRun {
            issue_id,
            env_name,
            json,
            dry_run,
        },
        &aws_client,
        &disabled,
        &cfg.required_tags,
        // The cache, then the provider: the one part of this that leaves
        // the machine, so a test passes its own.
        |issue: lint::Issue, prompt: String| async move {
            if !no_cache {
                if let Some(cached) = llm::read_cache(&issue) {
                    return Ok(cached);
                }
            }
            let response = llm::dispatch(settings, &prompt)
                .await
                .map_err(|e| e.to_string())?;
            if !no_cache {
                llm::write_cache(&issue, &response);
            }
            Ok(response)
        },
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .await;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// The flags the explanation itself reads.
pub(crate) struct ExplainRun {
    pub issue_id: String,
    pub env_name: Option<String>,
    pub json: bool,
    pub dry_run: bool,
}

/// `ebman explain`'s whole behaviour against any client, any `ask`
/// (the cache and the LLM: `issue, prompt -> response`) and any
/// writers. Returns the exit code.
pub(crate) async fn explain_body<A, Fut>(
    o: &ExplainRun,
    aws_client: &aws::AwsClient,
    disabled: &[String],
    required_tags: &[String],
    ask: A,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32
where
    A: Fn(lint::Issue, String) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<String, String>>,
{
    let issue_id = &o.issue_id;
    let rules = lint::default_rules(disabled);
    let envs = match aws_client.list_environments().await {
        Ok(envs) => envs,
        Err(e) => {
            let _ = writeln!(err, "ebman explain: list_environments: {e}");
            return 1;
        }
    };
    let targets: Vec<&aws::Environment> = match o.env_name.as_deref() {
        Some(name) => match envs.iter().find(|e| e.name == name) {
            Some(env) => vec![env],
            None => {
                let _ = writeln!(
                    err,
                    "ebman explain: env '{name}' not found in current context"
                );
                return 2;
            }
        },
        None => envs.iter().collect(),
    };

    // The shared assembly — `lint::inputs`, as `ebman lint`, the MCP
    // tool and the TUI's `:lint` / `:explain` use. This fetched option
    // settings alone and built its own `LintContext`, so EBL010 / EBL012
    // / EBL018 / EBL020 could never fire here and a failed fetch was
    // invisible: "no env in scope has issue", exit 3, for a check that
    // never ran. The TUI's `:explain` had the same defect and was fixed
    // first; this sibling was missed by a guard that looked only inside
    // the functions it named.
    // A failed listing is EBL008's missing input: the assembly reports
    // it per env, so `explain EBL008` exits 1, not the clean 3. It used
    // to warn here and carry on with an empty map — "no env has it".
    let platforms =
        lint::inputs::Platforms::from_listing(aws_client.list_solution_stacks().await, |e| {
            e.to_string()
        });
    let mut matched: Vec<lint::Issue> = Vec::new();
    // Envs where the rule could not be evaluated, with why.
    let mut not_evaluated: Vec<String> = Vec::new();
    for env in targets {
        let inputs = match lint::inputs::fetch_env_lint_inputs(
            aws_client,
            env,
            &platforms,
            false,
            disabled,
            required_tags,
        )
        .await
        {
            Ok(inputs) => inputs,
            Err(e) => {
                not_evaluated.push(format!("{}: {e}", env.name));
                continue;
            }
        };
        let issues = lint::inputs::run_rules_for_env(&rules, env, &inputs, required_tags);
        match lint::inputs::explain_verdict(issue_id, &issues, &inputs.coverage_warnings) {
            lint::inputs::ExplainVerdict::Fires(issue) => matched.push(issue.clone()),
            lint::inputs::ExplainVerdict::NotEvaluated(why) => not_evaluated.push(why.to_string()),
            lint::inputs::ExplainVerdict::DoesNotFire => {}
        }
    }
    for why in &not_evaluated {
        let _ = writeln!(err, "warning: {why}");
    }

    if matched.is_empty() {
        if !not_evaluated.is_empty() {
            // Not "no env has it": some could not be checked. Exit 1,
            // the code `lint` uses for a run that did not see
            // everything — never the clean-result 3.
            let _ = writeln!(
                err,
                "ebman explain: '{issue_id}' could not be evaluated for {} env(s) — this is not a clean result",
                not_evaluated.len()
            );
            return 1;
        }
        let _ = writeln!(
            err,
            "ebman explain: no env in scope has issue '{issue_id}' — nothing to explain"
        );
        return 3;
    }

    let mut json_blocks: Vec<String> = Vec::new();
    for issue in &matched {
        let prompt = llm::build_prompt(issue);
        let env_label = issue.env_name.as_deref().unwrap_or("-");
        if o.dry_run {
            if o.json {
                json_blocks.push(format!(
                    "{{\"rule_id\":{},\"env\":{},\"dry_run\":true,\"prompt\":{}}}",
                    json_string(&issue.rule_id),
                    json_string(issue.env_name.as_deref().unwrap_or("")),
                    json_string(&prompt),
                ));
            } else {
                let _ = writeln!(out, "── {} ({env_label}) — DRY RUN ──", issue.rule_id);
                let _ = writeln!(out, "{prompt}\n");
            }
            continue;
        }
        let response = match ask(issue.clone(), prompt).await {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(err, "ebman explain: {e}");
                return 1;
            }
        };
        if o.json {
            json_blocks.push(format!(
                "{{\"rule_id\":{},\"env\":{},\"response\":{}}}",
                json_string(&issue.rule_id),
                json_string(issue.env_name.as_deref().unwrap_or("")),
                json_string(&response),
            ));
        } else {
            let _ = writeln!(out, "── {} ({env_label}) ──", issue.rule_id);
            let _ = writeln!(out, "{}\n", response.trim());
        }
    }

    if o.json {
        if json_blocks.len() == 1 {
            let _ = writeln!(out, "{}", json_blocks[0]);
        } else {
            let _ = writeln!(out, "[{}]", json_blocks.join(","));
        }
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
    fn positional_issue_id_and_flags_parse() {
        let p = parse_explain_args(&argv(&[
            "explain",
            "EBL001",
            "--env",
            "prod",
            "--json",
            "--dry-run",
            "--no-cache",
        ]))
        .unwrap();
        assert_eq!(p.issue_id, "EBL001");
        assert_eq!(p.env_name.as_deref(), Some("prod"));
        assert!(p.json && p.dry_run && p.no_cache);
    }

    #[test]
    fn positional_order_independent_and_last_wins() {
        // ISSUE_ID can come after flags, and a re-typed id wins.
        let p = parse_explain_args(&argv(&["explain", "--json", "EBL001", "EBL019"])).unwrap();
        assert_eq!(p.issue_id, "EBL019");
        assert!(p.json);
    }

    #[test]
    fn missing_issue_id_is_usage_error() {
        let err = parse_explain_args(&argv(&["explain", "--json"])).unwrap_err();
        assert!(err.contains("usage:"), "got: {err}");
    }

    #[test]
    fn non_ebl_issue_id_is_usage_error() {
        let err = parse_explain_args(&argv(&["explain", "clippy::foo"])).unwrap_err();
        assert!(err.contains("must be an EBL### rule id"), "got: {err}");
    }

    #[test]
    fn unknown_flag_is_usage_error_but_bare_word_is_the_id() {
        // A `--`-prefixed unknown is an error...
        assert!(parse_explain_args(&argv(&["explain", "EBL001", "--bogus"]))
            .unwrap_err()
            .contains("unknown flag"));
        // ...but a bare non-dash word is treated as the positional id,
        // not an unknown-flag error (here it then fails the EBL check).
        let err = parse_explain_args(&argv(&["explain", "notaflag"])).unwrap_err();
        assert!(err.contains("must be an EBL### rule id"), "got: {err}");
    }
}

#[cfg(test)]
mod body {
    use super::*;
    use crate::cli::lint::cycle_wiring::{mock_client_inner, MockFaults};

    fn opts(issue: &str, env: Option<&str>, json: bool, dry_run: bool) -> ExplainRun {
        ExplainRun {
            issue_id: issue.into(),
            env_name: env.map(Into::into),
            json,
            dry_run,
        }
    }

    /// Run against the mock fleet `api` (EBL001 fires on it under
    /// `update_rejected`, which also serves its `AllAtOnce` settings),
    /// answering every prompt with `answer`. Returns the code, both
    /// streams, and the prompts that were asked.
    async fn run(
        o: &ExplainRun,
        faults: MockFaults,
        answer: Result<&'static str, &'static str>,
    ) -> (i32, String, String, Vec<String>) {
        let client = mock_client_inner(vec!["api".into()], faults);
        let asked = std::sync::Mutex::new(Vec::new());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = explain_body(
            o,
            &client,
            &[],
            &[],
            |_issue, prompt| {
                asked.lock().unwrap().push(prompt);
                async move { answer.map(String::from).map_err(String::from) }
            },
            &mut out,
            &mut err,
        )
        .await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            asked.into_inner().unwrap(),
        )
    }

    fn fires() -> MockFaults {
        MockFaults {
            update_rejected: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_firing_issue_is_asked_about_and_answered() {
        let (code, out, err, asked) = run(
            &opts("EBL001", None, false, false),
            fires(),
            Ok("Use Rolling."),
        )
        .await;
        assert_eq!(code, 0, "{err}");
        assert_eq!(asked.len(), 1);
        assert!(
            asked[0].contains("EBL001"),
            "the prompt names the rule: {}",
            asked[0]
        );
        assert!(
            out.contains("── EBL001 (api) ──") && out.contains("Use Rolling."),
            "{out}"
        );

        let (_, out, _, _) = run(
            &opts("EBL001", None, true, false),
            fires(),
            Ok("Use Rolling."),
        )
        .await;
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["env"], "api");
        assert_eq!(v["response"], "Use Rolling.");
    }

    #[tokio::test]
    async fn a_named_env_is_the_one_explained() {
        let (code, out, err, asked) = run(
            &opts("EBL001", Some("api"), false, false),
            fires(),
            Ok("Use Rolling."),
        )
        .await;
        assert_eq!((code, asked.len()), (0, 1), "{err}");
        assert!(out.contains("── EBL001 (api) ──"), "{out}");
    }

    #[tokio::test]
    async fn a_dry_run_prints_the_prompt_and_asks_nothing() {
        let (code, out, _, asked) = run(
            &opts("EBL001", None, false, true),
            fires(),
            Err("must not be asked"),
        )
        .await;
        assert_eq!(code, 0);
        assert!(asked.is_empty());
        assert!(out.contains("DRY RUN"), "{out}");
    }

    #[tokio::test]
    async fn exit_codes_follow_the_table() {
        let (code, _, err, _) = run(
            &opts("EBL001", None, false, false),
            fires(),
            Err("HTTP 529"),
        )
        .await;
        assert_eq!(code, 1, "provider failure");
        assert!(err.contains("HTTP 529"), "{err}");

        let (code, _, _, asked) = run(
            &opts("EBL001", None, false, false),
            MockFaults::default(),
            Ok("x"),
        )
        .await;
        assert_eq!(code, 3, "fires nowhere");
        assert!(asked.is_empty());

        let (code, _, err, _) = run(
            &opts("EBL012", None, false, false),
            MockFaults {
                health_rejected: true,
                ..Default::default()
            },
            Ok("x"),
        )
        .await;
        assert_eq!(code, 1, "could not be evaluated is not clean: {err}");

        let (code, _, _, _) = run(
            &opts("EBL001", None, false, false),
            MockFaults {
                listing_rejected: true,
                ..Default::default()
            },
            Ok("x"),
        )
        .await;
        assert_eq!(code, 1, "the fleet would not list");

        let (code, _, _, _) = run(
            &opts("EBL001", Some("nope"), false, false),
            fires(),
            Ok("x"),
        )
        .await;
        assert_eq!(code, 2, "no such env");
    }
}
