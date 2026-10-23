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

use crate::{audit as audit_log, aws, util};

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
    /// The shared verb this replays.
    fn verb(self) -> crate::verb::Verb {
        use crate::verb::Verb;
        match self {
            ReplayVerb::Rebuild => Verb::Rebuild,
            ReplayVerb::Restart => Verb::RestartAppServer,
            ReplayVerb::Terminate => Verb::Terminate,
            ReplayVerb::Deploy => Verb::Deploy,
        }
    }

    fn label(self) -> &'static str {
        self.verb().audit_label()
    }

    /// Mirrors `ebman action`'s gate: terminate is the verb that needs
    /// `--yes`. A GATE, not a classification — it was named
    /// `destructive`, beside two other `destructive()`s that disagreed
    /// with it and with each other; rebuild is destructive
    /// (`Verb::destructive`) and still replays without `--yes`.
    fn requires_yes(self) -> bool {
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
    // Through the shared vocabulary, so every spelling a verb has ever
    // been written under replays — a log spans them (the CLI wrote
    // "Restart" until 0.30.3, MCP until 0.45). This parser used to keep
    // its own copy of the aliases.
    use crate::verb::Verb;
    let Some(label) = entry.action.as_deref() else {
        return Err("line carries no action= field — not a replayable action line".into());
    };
    let verb = match Verb::from_audit_label(label) {
        Some(Verb::Rebuild) => ReplayVerb::Rebuild,
        Some(Verb::RestartAppServer) => ReplayVerb::Restart,
        Some(Verb::Terminate) => ReplayVerb::Terminate,
        Some(Verb::Deploy) => ReplayVerb::Deploy,
        _ => {
            return Err(format!(
                "action '{label}' isn't replayable via the CLI (supported: Rebuild / RestartAppServer / Terminate / Deploy)"
            ))
        }
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
    let code = audit_replay_body(
        args,
        &util::cache_dir().join("audit.log"),
        // Safety pins gate every dispatch site, CLI included (house
        // rule), through the audited funnel, freeze included.
        |env: &str, profile: Option<&str>, region: Option<&str>, label: &str| {
            crate::cli::write_refusal(
                &crate::config::load(),
                env,
                &profile.map(str::to_string),
                crate::freeze::FreezeStore::real().active(),
                region,
                label,
            )
        },
        std::env::var("AWS_PROFILE").ok(),
        aws::AwsClient::with,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .await;
    // Every exit drains: a refusal's or a dispatch's audit line may
    // have a webhook POST in flight.
    if code != 0 {
        crate::cli::exit_after_drain(code).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
}

/// What decides whether a write may go: `(env, profile, region, verb)`
/// to the refusal, if any. [`crate::cli::write_refusal`] in production.
pub(crate) trait Refusal:
    Fn(&str, Option<&str>, Option<&str>, &str) -> Option<String>
{
}
impl<T: Fn(&str, Option<&str>, Option<&str>, &str) -> Option<String>> Refusal for T {}

/// `ebman audit replay`'s whole behaviour against any log, safety
/// decision, client factory (`profile, region`) and writers. Returns the
/// exit code. `ambient_profile` is `AWS_PROFILE`: the profile a line
/// written under default creds (`profile=-`) replays under.
pub(crate) async fn audit_replay_body<F, Fut>(
    args: &[String],
    log: &std::path::Path,
    refusal: impl Refusal,
    ambient_profile: Option<String>,
    client_for: F,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32
where
    F: FnOnce(Option<String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<aws::AwsClient>>,
{
    let ReplayArgs { id, yes } = match parse_replay_args(args) {
        Ok(parsed) => parsed,
        Err(ReplayArgError { msg, code }) => {
            let _ = writeln!(err, "{msg}");
            return code;
        }
    };

    if !log.exists() {
        let _ = writeln!(
            err,
            "ebman audit replay: no audit log at {} — nothing to replay",
            log.display()
        );
        return 2;
    }
    let bytes = match std::fs::read(log) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(err, "ebman audit replay: read {}: {e}", log.display());
            return 1;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let entries: Vec<audit_log::AuditEntry> = text
        .lines()
        .filter_map(audit_log::parse_audit_line)
        .collect();

    let matches = select_replay_matches(&entries, &id);
    let entry = match matches.as_slice() {
        [] => {
            let _ = writeln!(err, "ebman audit replay: no audit line matches '{id}'");
            return 2;
        }
        [one] => *one,
        many => {
            let _ = writeln!(
                err,
                "ebman audit replay: '{id}' is ambiguous — {} lines match; give a longer timestamp prefix:",
                many.len()
            );
            for e in many.iter().take(10) {
                let _ = writeln!(
                    err,
                    "  {}\t{}\t{}\t{}",
                    e.when,
                    e.stage.as_deref().unwrap_or("-"),
                    e.action.as_deref().unwrap_or("-"),
                    e.target.as_deref().unwrap_or("-"),
                );
            }
            if many.len() > 10 {
                let _ = writeln!(err, "  … and {} more", many.len() - 10);
            }
            return 2;
        }
    };

    let plan = match replay_plan(entry) {
        Ok(p) => p,
        Err(msg) => {
            let _ = writeln!(err, "ebman audit replay: {msg}");
            return 2;
        }
    };

    // Replaying a known failure is legitimate (retry after fixing the
    // cause) but shouldn't be silent about it.
    if entry.outcome.as_deref() == Some("err") {
        let _ = writeln!(
            err,
            "note: the original run ended outcome=err{} — replaying anyway",
            entry
                .err
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
    }

    // Checked against the profile the replay will run under: the one
    // on the audited line, or — when the line was written under default
    // creds (`profile=-`) — the ambient AWS_PROFILE, which is what
    // `AwsClient::with(None, …)` will resolve to. Same fallback
    // `lint --fix` uses.
    let pin_profile = plan.profile.clone().or(ambient_profile);
    if let Some(reason) = refusal(
        &plan.env,
        pin_profile.as_deref(),
        plan.region.as_deref(),
        plan.verb.label(),
    ) {
        let subject = format!("{} on {}", plan.verb.label(), plan.env);
        let _ = writeln!(
            err,
            "ebman audit replay: {}",
            crate::cli::refusal_naming(reason, &plan.env, &subject)
        );
        return 3;
    }
    if plan.verb.requires_yes() && !yes {
        let _ = writeln!(
            err,
            "ebman audit replay: '{}' is destructive; re-run with --yes to confirm",
            plan.verb.label()
        );
        return 3;
    }

    // Replay writes its own audit lines (unlike ad-hoc single-env
    // `ebman action` dispatches): replay exists for incident review,
    // so the trail must show what was re-run and from which line.
    // init wires the notify_webhook fan-out for those lines.
    audit_log::init_from_config_disk();

    let aws = match client_for(plan.profile.clone(), plan.region.clone()).await {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "ebman audit replay: {e}");
            return 1;
        }
    };
    let version_note = plan
        .version
        .as_deref()
        .map(|v| format!(" (version={v})"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
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

    let err_text = result.as_ref().err().map(|e| e.to_string());
    audit_log::append_action_completed(
        entry.account.as_deref(),
        plan.profile.as_deref(),
        region,
        plan.verb.label(),
        &plan.env,
        match err_text.as_deref() {
            None => Ok(()),
            Some(e) => Err(e),
        },
        &extras,
    );
    match err_text {
        None => {
            let _ = writeln!(
                out,
                "ok: {} on {} dispatched (replay of {})",
                plan.verb.label(),
                plan.env,
                entry.when
            );
            0
        }
        Some(e) => {
            let _ = writeln!(err, "err: {e}");
            1
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
        assert!(!plan.verb.requires_yes());
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
        assert!(plan.verb.requires_yes());
    }

    #[test]
    fn safety_pins_gate_replay_by_env_then_account() {
        // Goes through `write_refusal` — the gate replay actually
        // consults — rather than a config helper. The previous version
        // called `Config::pin_reason` directly, which pinned the
        // helper's behaviour and not this path's; when the decision
        // moved into `write_gate` the helper was deleted and the test
        // was the only thing still holding it up.
        //
        // Asserting on the rendered message keeps the CLI's wording
        // pinned here too, which is the half `write_gate`'s own tests
        // deliberately do not cover.
        use crate::cli::write_refusal;
        let mut cfg = crate::config::Config::default();
        cfg.safety_envs.insert("api-prod".into(), true);
        cfg.safety_accounts.insert("prod-admin".into(), true);

        assert_eq!(
            write_refusal(&cfg, "api-prod", &None, None, None, "Test").as_deref(),
            Some("refusing api-prod — pinned by safety.envs.api-prod.read_only")
        );
        assert_eq!(
            write_refusal(
                &cfg,
                "other-env",
                &Some("prod-admin".into()),
                None,
                None,
                "Test"
            )
            .as_deref(),
            Some("refusing other-env — pinned by safety.accounts.prod-admin.read_only")
        );
        assert_eq!(
            write_refusal(&cfg, "other-env", &Some("dev".into()), None, None, "Test"),
            None
        );
    }

    #[test]
    fn a_tui_written_restart_is_replayable() {
        let parse = |l: &str| crate::audit::parse_audit_line(l).expect("the fixture line parses");
        // The TUI audits every action under its Debug name, so it has
        // always written `RestartAppServer` — and replay accepted only
        // `Restart`, the spelling the CLI used. So the most common
        // restart in any log, the one dispatched from the TUI, came
        // back as "isn't replayable via the CLI". Found by a test
        // written for an unrelated refactor of the CLI's verb table.
        let tui = "2026-08-22T10:00:00Z\taccount=1\tprofile=p\tregion=us-east-1\t\
                   stage=dispatched action=RestartAppServer target=api-prod";
        let spec = super::replay_plan(&parse(tui)).expect("the TUI's spelling replays");
        assert_eq!(spec.verb.label(), "RestartAppServer");
        assert_eq!(spec.env, "api-prod");

        // And the historical CLI spelling still replays — logs written
        // before 0.30.3 carry it, and refusing them would trade one
        // broken half for the other.
        let cli = "2026-08-22T10:00:00Z\taccount=1\tprofile=p\tregion=us-east-1\t\
                   stage=dispatched action=Restart target=api-prod";
        let spec = super::replay_plan(&parse(cli)).expect("the old CLI spelling still replays");
        assert_eq!(
            spec.verb.label(),
            "RestartAppServer",
            "re-emitted canonically"
        );

        // A genuinely unsupported action is still refused, and the
        // message names the canonical spelling.
        let err = super::replay_plan(&parse(
            "2026-08-22T10:00:00Z\tregion=r\tstage=dispatched action=Clone target=api-prod",
        ))
        .expect_err("clone isn't replayable");
        assert!(err.contains("RestartAppServer"), "{err}");
    }

    /// A line the WRITER produced must be replayable by the READER.
    ///
    /// Every other test here feeds `replay_plan` a hand-typed line —
    /// `replay_plan`'s own doc says "unit-tested against synthetic
    /// lines". So the reader is pinned against a copy of the writer's
    /// format rather than the format itself, and the two can drift
    /// apart with the whole suite green. Renaming `stage=dispatched` in
    /// `audit.rs` is caught by nothing today; `ebman audit replay`
    /// would simply stop finding anything to replay.
    ///
    /// This closes the loop: write a real line, parse it with the real
    /// parser, and hand it to the real planner.
    #[test]
    fn a_line_the_writer_emitted_is_replayable() {
        let env = "replay-roundtrip-probe-env";
        let path = crate::util::cache_dir().join("audit.log");
        let before = std::fs::read_to_string(&path).unwrap_or_default();

        crate::audit::append_action_dispatched(
            Some("123456789012"),
            Some("staging"),
            "eu-west-1",
            // The label production actually writes — both the TUI
            // (`format!("{action:?}")`) and the CLI (`CliVerb::ALL`)
            // use the Debug name. Writing the legacy "Restart" here
            // would test a spelling nothing emits any more.
            "RestartAppServer",
            env,
            &[],
        );

        let after = std::fs::read_to_string(&path).unwrap_or_default();
        let delta = after
            .strip_prefix(&before)
            .expect("the audit log is append-only");
        let line = delta
            .lines()
            .find(|l| l.contains(env))
            .expect("the writer emitted a line");

        let entry = audit_log::parse_audit_line(line)
            .unwrap_or_else(|| panic!("the writer's own line must parse: {line}"));
        let plan = replay_plan(&entry).unwrap_or_else(|e| {
            panic!("the writer's own line must be replayable, got: {e}\n  line: {line}")
        });

        assert_eq!(plan.env, env, "the target must survive the round trip");
        assert_eq!(plan.region.as_deref(), Some("eu-west-1"));
        assert_eq!(plan.profile.as_deref(), Some("staging"));
        assert_eq!(plan.verb.label(), "RestartAppServer");
    }
}

#[cfg(test)]
mod body {
    use super::*;
    use std::sync::Mutex;

    const RESTART: &str = "2026-07-15T10:11:12+00:00\taccount=123\tprofile=-\tregion=eu-west-1\tstage=dispatched action=Restart target=api-prod";
    const TERMINATE: &str = "2026-07-15T11:00:00+00:00\taccount=123\tprofile=staging\tregion=eu-west-1\tstage=dispatched action=Terminate target=old-env";

    /// A log holding `lines`, in a directory of its own.
    fn log(lines: &[&str]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ebman-replay-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    /// A client whose `RestartAppServer` for `api-prod` succeeds, or is
    /// refused when `fails`; the rule, to count what reached AWS.
    fn client(fails: bool) -> (aws::AwsClient, aws_smithy_mocks::Rule) {
        let restart = if fails {
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::restart_app_server)
                .match_requests(|r| r.environment_name() == Some("api-prod"))
                .then_error(|| {
                    aws_sdk_elasticbeanstalk::operation::restart_app_server::RestartAppServerError::generic(
                        aws_smithy_types::error::ErrorMetadata::builder()
                            .code("AccessDeniedException")
                            .message("not authorized")
                            .build(),
                    )
                })
        } else {
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::restart_app_server)
                .match_requests(|r| r.environment_name() == Some("api-prod"))
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::restart_app_server::RestartAppServerOutput::builder().build()
                })
        };
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&restart]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("eu-west-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let c = aws::AwsClient::for_tests(
            eb,
            aws_sdk_sqs::Client::new(&cfg),
            aws_sdk_cloudwatch::Client::new(&cfg),
            aws_sdk_cloudwatchlogs::Client::new(&cfg),
            aws_sdk_s3::Client::new(&cfg),
            aws_sdk_ec2::Client::new(&cfg),
        );
        (c, restart)
    }

    struct Outcome {
        code: i32,
        out: String,
        err: String,
        /// The profile the safety decision was asked about, if it was.
        asked_profile: Option<Option<String>>,
        /// The `(profile, region)` a client was built for, if one was.
        built: Option<(Option<String>, Option<String>)>,
    }

    async fn replay(
        args: &[&str],
        lines: &[&str],
        refuse: Option<&str>,
        aws: Option<aws::AwsClient>,
    ) -> Outcome {
        let argv: Vec<String> = ["audit", "replay"]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect();
        let path = log(lines);
        let asked = Mutex::new(None);
        let built = Mutex::new(None);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = audit_replay_body(
            &argv,
            &path,
            |env: &str, profile: Option<&str>, _region: Option<&str>, _verb: &str| {
                *asked.lock().unwrap() = Some(profile.map(String::from));
                refuse.map(|why| format!("refusing {env} — {why}"))
            },
            Some("ambient".into()),
            |profile, region| {
                *built.lock().unwrap() = Some((profile, region));
                async move { aws.ok_or_else(|| color_eyre::eyre::eyre!("no credentials")) }
            },
            &mut out,
            &mut err,
        )
        .await;
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        Outcome {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            asked_profile: asked.into_inner().unwrap(),
            built: built.into_inner().unwrap(),
        }
    }

    #[tokio::test]
    async fn the_audited_action_is_dispatched_again_where_it_ran() {
        let (aws, restart) = client(false);
        let r = replay(
            &["2026-07-15T10:11"],
            &[RESTART, TERMINATE],
            None,
            Some(aws),
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(restart.num_calls(), 1);
        assert!(
            r.out
                .contains("ok: RestartAppServer on api-prod dispatched"),
            "{}",
            r.out
        );
        // `profile=-`: the client resolves default creds, and the pins
        // are checked against the ambient profile that will mean.
        assert_eq!(r.built, Some((None, Some("eu-west-1".into()))));
        assert_eq!(r.asked_profile, Some(Some("ambient".into())));
    }

    #[tokio::test]
    async fn a_refusal_or_a_missing_yes_reaches_no_client() {
        let r = replay(
            &["2026-07-15T10:11"],
            &[RESTART],
            Some("pinned read-only"),
            None,
        )
        .await;
        assert_eq!(r.code, 3);
        assert!(r.built.is_none());
        assert!(
            r.err
                .contains("refusing RestartAppServer on api-prod — pinned read-only"),
            "names the action: {}",
            r.err
        );

        let r = replay(&["2026-07-15T11"], &[TERMINATE], None, None).await;
        assert_eq!(r.code, 3, "{}", r.err);
        assert!(r.built.is_none());
        assert!(r.err.contains("--yes"), "{}", r.err);
        assert_eq!(
            r.asked_profile,
            Some(Some("staging".into())),
            "the line's own profile"
        );
    }

    #[tokio::test]
    async fn exit_codes_follow_the_table() {
        let (aws, restart) = client(true);
        let r = replay(&["2026-07-15T10:11"], &[RESTART], None, Some(aws)).await;
        assert_eq!((r.code, restart.num_calls()), (1, 1), "AWS refused");
        assert!(r.err.contains("err:"), "{}", r.err);

        let r = replay(&["2026-07-15T10:11"], &[RESTART], None, None).await;
        assert_eq!(r.code, 1, "no client: {}", r.err);

        let r = replay(&["2026-07-16"], &[RESTART], None, None).await;
        assert_eq!(r.code, 2, "matches nothing");
        let r = replay(&["2026-07-15"], &[RESTART, TERMINATE], None, None).await;
        assert_eq!(r.code, 2, "ambiguous");
        assert!(r.err.contains("ambiguous"), "{}", r.err);
        assert!(r.asked_profile.is_none());
        let r = replay(&[], &[RESTART], None, None).await;
        assert_eq!(r.code, 2, "usage");
    }
}
