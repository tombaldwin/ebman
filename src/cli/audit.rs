//! `ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID]
//! [--action NAME] [--json]` — surface `~/.cache/ebman/audit.log`
//! for scripting / Slack-bot routing / CI gating.
//!
//! Two phases: read existing file end-to-end (parse + filter +
//! render), then optionally `--tail` poll for new bytes every
//! second from EOF. Rotation detected by file shrink.

use color_eyre::eyre::Result;

use crate::{audit as audit_log, aws, util};

/// Parsed `ebman audit` flags. `--since` is resolved only as far as a
/// millisecond window here (the `Utc::now()` subtraction stays in
/// [`run`] so this struct is deterministic + testable). `since_ms` is
/// `None` when `--since` was absent.
#[derive(Debug, PartialEq, Eq)]
struct AuditArgs {
    tail: bool,
    since_ms: Option<i64>,
    env_filter: Option<String>,
    rule_filter: Option<String>,
    action_filter: Option<String>,
    json: bool,
}

/// Pure parser for `ebman audit`. Returns `Err(msg)` for the two
/// exit-2 usage paths: an unknown flag, or a `--since` value that
/// isn't a valid duration. Deliberately does NOT call `Utc::now()` —
/// returning the parsed window keeps the parser deterministic.
fn parse_audit_args(args: &[String]) -> Result<AuditArgs, String> {
    let mut tail = false;
    let mut since_str: Option<String> = None;
    let mut env_filter: Option<String> = None;
    let mut rule_filter: Option<String> = None;
    let mut action_filter: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--tail" => tail = true,
            "--since" => since_str = iter.next().cloned(),
            "--env" => env_filter = iter.next().cloned(),
            "--rule" => rule_filter = iter.next().cloned(),
            "--action" => action_filter = iter.next().cloned(),
            "--json" => json = true,
            other => return Err(format!("ebman audit: unknown flag '{other}'")),
        }
    }

    let since_ms: Option<i64> = match since_str.as_deref() {
        None => None,
        Some(s) => match aws::parse_window_ms(s) {
            Some(ms) => Some(ms),
            None => {
                return Err(
                    "ebman audit: --since expects a duration like `5m` / `30m` / `1h` / `2d`"
                        .into(),
                )
            }
        },
    };

    Ok(AuditArgs {
        tail,
        since_ms,
        env_filter,
        rule_filter,
        action_filter,
        json,
    })
}

pub async fn run(args: &[String]) -> Result<()> {
    let AuditArgs {
        tail,
        since_ms,
        env_filter,
        rule_filter,
        action_filter,
        json,
    } = match parse_audit_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let since_dt: Option<chrono::DateTime<chrono::Utc>> =
        since_ms.map(|ms| chrono::Utc::now() - chrono::Duration::milliseconds(ms));

    let filter = audit_log::AuditFilter {
        since: since_dt,
        env: env_filter.as_deref(),
        rule: rule_filter.as_deref(),
        action: action_filter.as_deref(),
    };

    let path = util::cache_dir().join("audit.log");
    if !path.exists() {
        if !json {
            println!("(no audit entries — log not yet created)");
        }
        return Ok(());
    }

    let bytes = std::fs::read(&path)
        .map_err(|e| color_eyre::eyre::eyre!("read {}: {e}", path.display()))?;
    let initial_offset = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let entries: Vec<audit_log::AuditEntry> = text
        .lines()
        .filter_map(audit_log::parse_audit_line)
        .filter(|e| filter.matches(e))
        .collect();
    if json {
        print!("{}", audit_log::render_audit_entries_json(&entries));
    } else {
        print!("{}", audit_log::render_audit_entries_text(&entries));
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();

    if tail {
        let mut offset = initial_offset;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let len = meta.len();
            if len < offset {
                offset = 0;
            }
            if len == offset {
                continue;
            }
            use std::io::{Read, Seek, SeekFrom};
            let mut f = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if f.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }
            let mut buf = Vec::with_capacity((len - offset) as usize);
            if f.read_to_end(&mut buf).is_err() {
                continue;
            }
            offset = len;
            let chunk = String::from_utf8_lossy(&buf);
            let new_entries: Vec<audit_log::AuditEntry> = chunk
                .lines()
                .filter_map(audit_log::parse_audit_line)
                .filter(|e| filter.matches(e))
                .collect();
            if new_entries.is_empty() {
                continue;
            }
            if json {
                print!("{}", audit_log::render_audit_entries_json(&new_entries));
            } else {
                for e in &new_entries {
                    let outcome = match (e.outcome.as_deref(), e.err.as_deref()) {
                        (_, Some(err)) => format!("err=\"{err}\""),
                        (Some("ok"), _) => "ok".into(),
                        (Some(s), _) => s.into(),
                        _ => "-".into(),
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        e.when,
                        e.region.as_deref().unwrap_or("-"),
                        e.stage.as_deref().unwrap_or("-"),
                        e.action.as_deref().unwrap_or("-"),
                        e.target.as_deref().unwrap_or("-"),
                        outcome,
                    );
                }
            }
            let _ = std::io::stdout().flush();
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
    fn collects_filters_and_flags() {
        let p = parse_audit_args(&argv(&[
            "audit", "--tail", "--env", "prod", "--rule", "EBL001", "--action", "Deploy", "--json",
        ]))
        .unwrap();
        assert!(p.tail && p.json);
        assert_eq!(p.env_filter.as_deref(), Some("prod"));
        assert_eq!(p.rule_filter.as_deref(), Some("EBL001"));
        assert_eq!(p.action_filter.as_deref(), Some("Deploy"));
        assert!(p.since_ms.is_none());
    }

    #[test]
    fn since_resolves_to_window_ms() {
        // 2d = 2 * 86_400_000 ms. Deterministic — no Utc::now() in parse.
        let p = parse_audit_args(&argv(&["audit", "--since", "2d"])).unwrap();
        assert_eq!(p.since_ms, Some(172_800_000));
    }

    #[test]
    fn bad_since_is_usage_error() {
        let err = parse_audit_args(&argv(&["audit", "--since", "yesterday"])).unwrap_err();
        assert!(err.contains("--since expects a duration"), "got: {err}");
    }

    #[test]
    fn unknown_flag_is_usage_error() {
        let err = parse_audit_args(&argv(&["audit", "--bogus"])).unwrap_err();
        assert!(
            err.contains("unknown flag") && err.contains("--bogus"),
            "got: {err}"
        );
    }
}
