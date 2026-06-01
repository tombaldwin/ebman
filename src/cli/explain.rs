//! `ebman explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]`
//! — LLM-backed explanation of a lint issue. Loads the configured
//! Provider (Anthropic API or Ollama), assembles the prompt via
//! [`crate::llm::build_prompt`], dispatches, prints the response.
//!
//! Requires explicit operator consent via `[explain] enabled = true`
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
            "--env" => env_name = iter.next().cloned(),
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
    let rules = lint::default_rules(&disabled);

    let aws_client = aws::AwsClient::with(None, None).await?;
    let envs = aws_client
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    let targets: Vec<&aws::Environment> = match env_name.as_deref() {
        Some(name) => match envs.iter().find(|e| e.name == name) {
            Some(env) => vec![env],
            None => {
                eprintln!("ebman explain: env '{name}' not found in current context");
                std::process::exit(2);
            }
        },
        None => envs.iter().collect(),
    };

    let mut matched: Vec<lint::Issue> = Vec::new();
    for env in targets {
        let opts = match aws_client
            .fetch_env_option_settings(&env.application, &env.name)
            .await
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "warning: skipping {} — fetch_env_option_settings: {e}",
                    env.name
                );
                continue;
            }
        };
        let ctx = lint::LintContext::for_env(env, &opts);
        let issues = lint::run_rules(&rules, &ctx);
        for i in issues {
            if i.rule_id == issue_id {
                matched.push(i);
            }
        }
    }

    if matched.is_empty() {
        eprintln!("ebman explain: no env in scope has issue '{issue_id}' — nothing to explain");
        std::process::exit(3);
    }

    let mut json_blocks: Vec<String> = Vec::new();
    for issue in &matched {
        let prompt = llm::build_prompt(issue);
        if dry_run {
            if json {
                json_blocks.push(format!(
                    "{{\"rule_id\":{},\"env\":{},\"dry_run\":true,\"prompt\":{}}}",
                    json_string(&issue.rule_id),
                    json_string(issue.env_name.as_deref().unwrap_or("")),
                    json_string(&prompt),
                ));
            } else {
                println!(
                    "── {} ({}) — DRY RUN ──",
                    issue.rule_id,
                    issue.env_name.as_deref().unwrap_or("-")
                );
                println!("{prompt}\n");
            }
            continue;
        }

        let cached = if no_cache {
            None
        } else {
            llm::read_cache(issue)
        };
        let response = match cached {
            Some(c) => c,
            None => match llm::dispatch(&settings, &prompt).await {
                Ok(r) => {
                    if !no_cache {
                        llm::write_cache(issue, &r);
                    }
                    r
                }
                Err(e) => {
                    eprintln!("ebman explain: {e}");
                    std::process::exit(1);
                }
            },
        };

        if json {
            json_blocks.push(format!(
                "{{\"rule_id\":{},\"env\":{},\"response\":{}}}",
                json_string(&issue.rule_id),
                json_string(issue.env_name.as_deref().unwrap_or("")),
                json_string(&response),
            ));
        } else {
            println!(
                "── {} ({}) ──",
                issue.rule_id,
                issue.env_name.as_deref().unwrap_or("-")
            );
            println!("{}\n", response.trim());
        }
    }

    if json {
        if json_blocks.len() == 1 {
            println!("{}", json_blocks[0]);
        } else {
            println!("[{}]", json_blocks.join(","));
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
