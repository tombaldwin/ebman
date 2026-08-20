//! `ebman audit replay <line-id> [--yes]` — re-dispatch a
//! previously-audited action. Split out of `cli/audit.rs` (0.26,
//! per the 0.25 architecture review) once replay outgrew the log
//! reader it started in; `cli::audit::run` still owns the dispatch.
//!
//! `<line-id>` is a prefix of the target line's RFC3339 timestamp
//! (the first column of `ebman audit` output); ambiguous prefixes
//! are refused with the candidate lines listed. Safety pins are
//! enforced via the shared `Config::pin_reason`; Terminate needs
//! `--yes` (exit 3).

use color_eyre::eyre::Result;

use crate::{audit as audit_log, aws, config, util};

/// Parsed `ebman audit replay` invocation. `id` is a prefix of the
/// target line's RFC3339 timestamp — the first column of `ebman
/// audit` output — so operators can paste as much of it as needed
/// to disambiguate. `--yes` confirms destructive verbs, mirroring
/// `ebman action`.
#[derive(Debug, PartialEq, Eq)]
struct ReplayArgs {
    id: String,
    yes: bool,
}

/// A usage/gate failure carrying the exit code the CLI charter
/// assigns it (2 usage, 3 destructive-gate) — same pattern as
/// `cli::action`'s `ActionArgError`, so the gate logic stays
/// unit-testable without `std::process::exit`.
#[derive(Debug, PartialEq, Eq)]
struct ReplayArgError {
    msg: String,
    code: i32,
}

const REPLAY_USAGE: &str = "usage: ebman audit replay <line-id> [--yes]   (line-id = RFC3339 timestamp prefix from `ebman audit` output)";

fn parse_replay_args(args: &[String]) -> Result<ReplayArgs, ReplayArgError> {
    // args[0] = "audit", args[1] = "replay".
    let mut id: Option<String> = None;
    let mut yes = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--yes" => yes = true,
            flag if flag.starts_with('-') => {
                return Err(ReplayArgError {
                    msg: format!("ebman audit replay: unknown flag '{flag}'"),
                    code: 2,
                })
            }
            positional => {
                if id.is_some() {
                    return Err(ReplayArgError {
                        msg: REPLAY_USAGE.into(),
                        code: 2,
                    });
                }
                id = Some(positional.to_string());
            }
        }
    }
    let Some(id) = id else {
        return Err(ReplayArgError {
            msg: REPLAY_USAGE.into(),
            code: 2,
        });
    };
    Ok(ReplayArgs { id, yes })
}

/// The four audit `action=` labels replay can reconstruct into an
/// AWS call. Everything else on the log (Swap's `a ↔ b` target,
/// SetOption's namespace payload, SSM/DLQ ops, freezes) either
/// lacks the parameters on the line or has no CLI dispatch path —
/// those refuse with a pointer at the right tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayVerb {
    Rebuild,
    Restart,
    Terminate,
    Deploy,
}

impl ReplayVerb {
    fn label(self) -> &'static str {
        match self {
            ReplayVerb::Rebuild => "Rebuild",
            ReplayVerb::Restart => "Restart",
            ReplayVerb::Terminate => "Terminate",
            ReplayVerb::Deploy => "Deploy",
        }
    }

    /// Mirrors `ebman action`'s gate: terminate is the destructive
    /// verb requiring `--yes`.
    fn destructive(self) -> bool {
        matches!(self, ReplayVerb::Terminate)
    }
}

/// Everything needed to re-dispatch one audited action. `profile` /
/// `region` come off the line so the replay targets the original
/// account/region even if the operator's current defaults moved.
#[derive(Debug, PartialEq, Eq)]
struct ReplayPlan {
    verb: ReplayVerb,
    env: String,
    version: Option<String>,
    profile: Option<String>,
    region: Option<String>,
}

/// Timestamp-prefix match over the parsed log. Multiple hits are the
/// caller's ambiguity-refusal case.
fn select_replay_matches<'a>(
    entries: &'a [audit_log::AuditEntry],
    id: &str,
) -> Vec<&'a audit_log::AuditEntry> {
    entries.iter().filter(|e| e.when.starts_with(id)).collect()
}

/// Reconstruct a dispatchable plan from one audit line, or explain
/// why the line can't be replayed. Pure — unit-tested against
/// synthetic lines run through the real `parse_audit_line`.
fn replay_plan(entry: &audit_log::AuditEntry) -> Result<ReplayPlan, String> {
    if entry.rollout_id.is_some() {
        return Err(
            "rollout lines aren't replayable — re-run `ebman action rollout` with the original flags"
                .into(),
        );
    }
    match entry.stage.as_deref() {
        Some("dispatched") | Some("completed") => {}
        Some(other) => {
            return Err(format!(
                "stage={other} lines aren't replayable — pick the operation's stage=dispatched line"
            ))
        }
        None => return Err("line carries no stage= field — not a replayable action line".into()),
    }
    let verb = match entry.action.as_deref() {
        Some("Rebuild") => ReplayVerb::Rebuild,
        Some("Restart") => ReplayVerb::Restart,
        Some("Terminate") => ReplayVerb::Terminate,
        Some("Deploy") => ReplayVerb::Deploy,
        Some(other) => {
            return Err(format!(
                "action '{other}' isn't replayable via the CLI (supported: Rebuild / Restart / Terminate / Deploy)"
            ))
        }
        None => return Err("line carries no action= field — not a replayable action line".into()),
    };
    let Some(env) = entry.target.as_deref().filter(|t| !t.is_empty()) else {
        return Err("line carries no target= env — not a replayable action line".into());
    };
    let version = entry
        .version
        .clone()
        .or_else(|| entry.extras.get("version").cloned());
    if verb == ReplayVerb::Deploy && version.is_none() {
        return Err(
            "deploy line carries no version= — cannot reconstruct; use `ebman action deploy --env … --version …`"
                .into(),
        );
    }
    Ok(ReplayPlan {
        verb,
        env: env.to_string(),
        version,
        profile: entry.profile.clone(),
        region: entry.region.clone(),
    })
}

pub(crate) async fn run_replay(args: &[String]) -> Result<()> {
    let ReplayArgs { id, yes } = match parse_replay_args(args) {
        Ok(parsed) => parsed,
        Err(ReplayArgError { msg, code }) => {
            eprintln!("{msg}");
            std::process::exit(code);
        }
    };

    let path = util::cache_dir().join("audit.log");
    if !path.exists() {
        eprintln!(
            "ebman audit replay: no audit log at {} — nothing to replay",
            path.display()
        );
        std::process::exit(2);
    }
    let bytes = std::fs::read(&path)
        .map_err(|e| color_eyre::eyre::eyre!("read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let entries: Vec<audit_log::AuditEntry> = text
        .lines()
        .filter_map(audit_log::parse_audit_line)
        .collect();

    let matches = select_replay_matches(&entries, &id);
    let entry = match matches.as_slice() {
        [] => {
            eprintln!("ebman audit replay: no audit line matches '{id}'");
            std::process::exit(2);
        }
        [one] => *one,
        many => {
            eprintln!(
                "ebman audit replay: '{id}' is ambiguous — {} lines match; give a longer timestamp prefix:",
                many.len()
            );
            for e in many.iter().take(10) {
                eprintln!(
                    "  {}\t{}\t{}\t{}",
                    e.when,
                    e.stage.as_deref().unwrap_or("-"),
                    e.action.as_deref().unwrap_or("-"),
                    e.target.as_deref().unwrap_or("-"),
                );
            }
            if many.len() > 10 {
                eprintln!("  … and {} more", many.len() - 10);
            }
            std::process::exit(2);
        }
    };

    let plan = match replay_plan(entry) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("ebman audit replay: {msg}");
            std::process::exit(2);
        }
    };

    // Replaying a known failure is legitimate (retry after fixing the
    // cause) but shouldn't be silent about it.
    if entry.outcome.as_deref() == Some("err") {
        eprintln!(
            "note: the original run ended outcome=err{} — replaying anyway",
            entry
                .err
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
    }

    // Safety pins gate every dispatch site, CLI included (house
    // rule). Checked against the profile the replay will run under:
    // the one on the audited line, or — when the line was written
    // under default creds (`profile=-`) — the ambient AWS_PROFILE,
    // which is what `AwsClient::with(None, …)` will resolve to.
    // Same fallback `lint --fix` uses.
    let cfg = config::load();
    let pin_profile = plan
        .profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok());
    if let Some(pin) = cfg.pin_reason(&plan.env, pin_profile.as_deref()) {
        eprintln!(
            "ebman audit replay: refusing {} on {} — pinned by {pin}",
            plan.verb.label(),
            plan.env
        );
        std::process::exit(3);
    }
    if plan.verb.destructive() && !yes {
        eprintln!(
            "ebman audit replay: '{}' is destructive; re-run with --yes to confirm",
            plan.verb.label()
        );
        std::process::exit(3);
    }

    // Replay writes its own audit lines (unlike ad-hoc single-env
    // `ebman action` dispatches): replay exists for incident review,
    // so the trail must show what was re-run and from which line.
    // init wires the notify_webhook fan-out for those lines.
    audit_log::init_from_config_disk();

    let aws = aws::AwsClient::with(plan.profile.clone(), plan.region.clone()).await?;
    let version_note = plan
        .version
        .as_deref()
        .map(|v| format!(" (version={v})"))
        .unwrap_or_default();
    println!(
        "replaying {}: {} on {}{version_note}",
        entry.when,
        plan.verb.label(),
        plan.env
    );

    let region = plan.region.as_deref().unwrap_or("-");
    let mut extras: Vec<(&str, &str)> = vec![("replay_of", entry.when.as_str())];
    if let Some(v) = plan.version.as_deref() {
        extras.push(("version", v));
    }
    audit_log::append_action_dispatched(
        entry.account.as_deref(),
        plan.profile.as_deref(),
        region,
        plan.verb.label(),
        &plan.env,
        &extras,
    );

    let result = match plan.verb {
        ReplayVerb::Rebuild => aws.rebuild_env(&plan.env).await,
        ReplayVerb::Restart => aws.restart_app_server(&plan.env).await,
        ReplayVerb::Terminate => aws.terminate_env(&plan.env).await,
        ReplayVerb::Deploy => {
            aws.deploy_version(&plan.env, plan.version.as_deref().unwrap_or_default())
                .await
        }
    };

    match result {
        Ok(()) => {
            audit_log::append_action_completed(
                entry.account.as_deref(),
                plan.profile.as_deref(),
                region,
                plan.verb.label(),
                &plan.env,
                Ok(()),
                &extras,
            );
            println!(
                "ok: {} on {} dispatched (replay of {})",
                plan.verb.label(),
                plan.env,
                entry.when
            );
            crate::cli::drain_before_return().await;
            Ok(())
        }
        Err(e) => {
            let err_text = e.to_string();
            audit_log::append_action_completed(
                entry.account.as_deref(),
                plan.profile.as_deref(),
                region,
                plan.verb.label(),
                &plan.env,
                Err(&err_text),
                &extras,
            );
            eprintln!("err: {err_text}");
            crate::cli::exit_after_drain(1).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// Build an AuditEntry through the real line parser so plan
    /// reconstruction is tested against the actual wire format.
    fn entry(line: &str) -> audit_log::AuditEntry {
        audit_log::parse_audit_line(line).expect("test line should parse")
    }

    #[test]
    fn replay_args_take_id_and_yes() {
        let p =
            parse_replay_args(&argv(&["audit", "replay", "2026-07-15T10:11", "--yes"])).unwrap();
        assert_eq!(p.id, "2026-07-15T10:11");
        assert!(p.yes);
        let p = parse_replay_args(&argv(&["audit", "replay", "2026-07-15"])).unwrap();
        assert!(!p.yes);
    }

    #[test]
    fn replay_args_missing_id_is_usage_error() {
        let err = parse_replay_args(&argv(&["audit", "replay"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.msg.contains("usage:"), "got: {}", err.msg);
    }

    #[test]
    fn replay_args_reject_unknown_flag_and_second_positional() {
        let err = parse_replay_args(&argv(&["audit", "replay", "x", "--force"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.msg.contains("--force"), "got: {}", err.msg);
        let err = parse_replay_args(&argv(&["audit", "replay", "x", "y"])).unwrap_err();
        assert_eq!(err.code, 2);
    }

    #[test]
    fn select_matches_by_timestamp_prefix() {
        let entries = vec![
            entry("2026-07-15T10:11:12.111+00:00\taccount=1\tprofile=p\tregion=r\tstage=dispatched action=Restart target=env-a"),
            entry("2026-07-15T10:11:12.222+00:00\taccount=1\tprofile=p\tregion=r\tstage=completed action=Restart target=env-a outcome=ok"),
            entry("2026-07-15T11:00:00+00:00\taccount=1\tprofile=p\tregion=r\tstage=dispatched action=Rebuild target=env-b"),
        ];
        assert_eq!(select_replay_matches(&entries, "2026-07-15T10:11").len(), 2);
        assert_eq!(select_replay_matches(&entries, "2026-07-15T11").len(), 1);
        assert_eq!(select_replay_matches(&entries, "2026-07-16").len(), 0);
    }

    #[test]
    fn replay_plan_reconstructs_restart() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=123\tprofile=staging\tregion=eu-west-1\tstage=dispatched action=Restart target=api-prod",
        );
        let plan = replay_plan(&e).unwrap();
        assert_eq!(plan.verb, ReplayVerb::Restart);
        assert_eq!(plan.env, "api-prod");
        assert_eq!(plan.profile.as_deref(), Some("staging"));
        assert_eq!(plan.region.as_deref(), Some("eu-west-1"));
        assert!(plan.version.is_none());
        assert!(!plan.verb.destructive());
    }

    #[test]
    fn replay_plan_deploy_takes_version_from_extras() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=123\tprofile=p\tregion=r\tstage=dispatched action=Deploy target=api-prod version=build-900",
        );
        let plan = replay_plan(&e).unwrap();
        assert_eq!(plan.verb, ReplayVerb::Deploy);
        assert_eq!(plan.version.as_deref(), Some("build-900"));
    }

    #[test]
    fn replay_plan_deploy_without_version_refuses() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=123\tprofile=p\tregion=r\tstage=dispatched action=Deploy target=api-prod",
        );
        let err = replay_plan(&e).unwrap_err();
        assert!(err.contains("no version="), "got: {err}");
    }

    #[test]
    fn replay_plan_refuses_unsupported_action_and_stages() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=1\tprofile=p\tregion=r\tstage=dispatched action=FreezeDeploys target=",
        );
        assert!(replay_plan(&e).unwrap_err().contains("isn't replayable"));
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=1\tprofile=p\tregion=r\tstage=skipped action=Restart target=env-a reason=\"pinned\"",
        );
        assert!(replay_plan(&e).unwrap_err().contains("stage=skipped"));
    }

    #[test]
    fn replay_plan_refuses_rollout_lines() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\trollout_id=abc123\tregion=eu-west-1\tstage=dispatched action=Rollout target=api version=build-900",
        );
        assert!(replay_plan(&e).unwrap_err().contains("rollout"));
    }

    #[test]
    fn replay_plan_terminate_is_destructive() {
        let e = entry(
            "2026-07-15T10:11:12+00:00\taccount=1\tprofile=p\tregion=r\tstage=dispatched action=Terminate target=old-env",
        );
        let plan = replay_plan(&e).unwrap();
        assert!(plan.verb.destructive());
    }

    #[test]
    fn safety_pins_gate_replay_by_env_then_account() {
        // The shared `Config::pin_reason` (0.26 extraction) is what
        // replay consults; keep the replay-shaped assertions here so
        // this file still pins its own gate behaviour.
        let mut cfg = config::Config::default();
        cfg.safety_envs.insert("api-prod".into(), true);
        cfg.safety_accounts.insert("prod-admin".into(), true);
        assert_eq!(
            cfg.pin_reason("api-prod", None).as_deref(),
            Some("safety.envs.api-prod.read_only")
        );
        assert_eq!(
            cfg.pin_reason("other-env", Some("prod-admin")).as_deref(),
            Some("safety.accounts.prod-admin.read_only")
        );
        assert_eq!(cfg.pin_reason("other-env", Some("dev")), None);
        assert_eq!(cfg.pin_reason("other-env", None), None);
    }
}
