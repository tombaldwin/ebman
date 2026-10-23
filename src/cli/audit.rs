//! `ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID]
//! [--action NAME] [--json]` — surface `~/.cache/ebman/audit.log`
//! for scripting / Slack-bot routing / CI gating.
//!
//! Two phases: read existing file end-to-end (parse + filter +
//! render), then optionally `--tail` poll for new bytes every
//! second from EOF. Rotation detected by file shrink.
//!
//! `ebman audit replay` is dispatched from here but lives in
//! [`crate::cli::audit_replay`] (split 0.26 once replay outgrew the
//! log reader it started in).

use color_eyre::eyre::Result;

use crate::{audit as audit_log, aws, util};

/// Parsed `ebman audit` flags. `--since` is resolved only as far as a
/// millisecond window here (the subtraction from "now" happens in
/// [`audit_body`], which is handed the clock). `since_ms` is
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
            "--since" => {
                since_str = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman audit",
                    "--since",
                    "a window like 5m / 1h / 2d",
                )?)
            }
            "--env" => {
                env_filter = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman audit",
                    "--env",
                    "an env name",
                )?)
            }
            "--rule" => {
                rule_filter = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman audit",
                    "--rule",
                    "a rule id",
                )?)
            }
            "--action" => {
                action_filter = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman audit",
                    "--action",
                    "an action name",
                )?)
            }
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

/// `ebman audit` — read this machine's audit log, or `replay` a
/// recorded rollout.
///
/// Local only: the log records what THIS installation dispatched, so
/// it answers "what did we do" and never "what happened to the
/// fleet". Exit 2 on a usage error. The thin shell over [`audit_body`]
/// and, for `--tail`, [`follow`].
pub async fn run(args: &[String]) -> Result<()> {
    if args.get(1).map(String::as_str) == Some("replay") {
        return crate::cli::audit_replay::run_replay(args).await;
    }
    let path = util::cache_dir().join("audit.log");
    let (code, tail) = audit_body(
        args,
        &path,
        chrono::Utc::now(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    if code != 0 {
        std::process::exit(code);
    }
    if let Some(tail) = tail {
        follow(&path, tail).await;
    }
    Ok(())
}

/// Where `--tail` picks up after the first read: past the bytes already
/// shown, with the same filter and format.
#[derive(Debug)]
pub(crate) struct Tail {
    offset: u64,
    args: AuditArgs,
    since: Option<chrono::DateTime<chrono::Utc>>,
}

impl Tail {
    fn filter(&self) -> audit_log::AuditFilter<'_> {
        audit_log::AuditFilter {
            since: self.since,
            env: self.args.env_filter.as_deref(),
            rule: self.args.rule_filter.as_deref(),
            action: self.args.action_filter.as_deref(),
        }
    }
}

/// `ebman audit`'s whole one-shot behaviour against any log path and any
/// writers: parse, read, filter, render. Returns the exit code and, under
/// `--tail`, where to follow from.
pub(crate) fn audit_body(
    args: &[String],
    path: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> (i32, Option<Tail>) {
    let parsed = match parse_audit_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            let _ = writeln!(err, "{msg}");
            return (2, None);
        }
    };
    let since = parsed
        .since_ms
        .map(|ms| now - chrono::Duration::milliseconds(ms));
    let json = parsed.json;
    let mut tail = Tail {
        offset: 0,
        args: parsed,
        since,
    };
    if !path.exists() {
        if !json {
            let _ = writeln!(out, "(no audit entries — log not yet created)");
        }
        // Done, even under `--tail`: there is no log to follow, and a
        // script tailing a fresh machine's log must not hang.
        return (0, None);
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(err, "ebman audit: read {}: {e}", path.display());
            return (1, None);
        }
    };
    tail.offset = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let filter = tail.filter();
    let entries: Vec<audit_log::AuditEntry> = text
        .lines()
        .filter_map(audit_log::parse_audit_line)
        .filter(|e| filter.matches(e))
        .collect();
    let _ = write!(
        out,
        "{}",
        if json {
            audit_log::render_audit_entries_json(&entries)
        } else {
            audit_log::render_audit_entries_text(&entries)
        }
    );
    let _ = out.flush();
    (0, tail.args.tail.then_some(tail))
}

/// `--tail`: poll the log every second from `tail.offset` and print new
/// entries, until the process is stopped. A shrink is a rotation.
async fn follow(path: &std::path::Path, mut tail: Tail) {
    use std::io::Write;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Some(chunk) = read_new_lines(path, &mut tail.offset) else {
            continue;
        };
        let filter = tail.filter();
        let new_entries: Vec<audit_log::AuditEntry> = chunk
            .lines()
            .filter_map(audit_log::parse_audit_line)
            .filter(|e| filter.matches(e))
            .collect();
        if new_entries.is_empty() {
            continue;
        }
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{}", tail_render(&new_entries, tail.args.json));
        let _ = out.flush();
    }
}

/// The complete lines appended past `offset`, advancing it past them. A
/// torn trailing line (a writer mid-append) is left for the next read,
/// not consumed and dropped; a file shorter than `offset` was rotated.
pub(crate) fn read_new_lines(path: &std::path::Path, offset: &mut u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = std::fs::metadata(path).ok()?.len();
    if len < *offset {
        *offset = 0;
    }
    if len == *offset {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(*offset)).ok()?;
    let mut buf = Vec::with_capacity((len - *offset) as usize);
    f.read_to_end(&mut buf).ok()?;
    let complete_up_to = buf.iter().rposition(|&b| b == b'\n')? + 1;
    *offset += complete_up_to as u64;
    buf.truncate(complete_up_to);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// New entries under `--tail`: the JSON renderer, or one tab-separated
/// line each.
fn tail_render(entries: &[audit_log::AuditEntry], json: bool) -> String {
    if json {
        return audit_log::render_audit_entries_json(entries);
    }
    let mut s = String::new();
    for e in entries {
        let outcome = match (e.outcome.as_deref(), e.err.as_deref()) {
            (_, Some(err)) => format!("err=\"{err}\""),
            (Some("ok"), _) => "ok".into(),
            (Some(o), _) => o.into(),
            _ => "-".into(),
        };
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            e.when,
            e.region.as_deref().unwrap_or("-"),
            e.stage.as_deref().unwrap_or("-"),
            e.action.as_deref().unwrap_or("-"),
            e.target.as_deref().unwrap_or("-"),
            outcome,
        ));
    }
    s
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

    fn log_with(lines: &[&str]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ebman-audit-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn body(args: &[&str], path: &std::path::Path) -> (i32, String, String, bool) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-23T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let (code, tail) = audit_body(&argv(args), path, now, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            tail.is_some(),
        )
    }

    const OLD: &str = "2026-10-20T12:00:00+00:00\taccount=1\tprofile=-\tregion=us-east-1\tstage=dispatched action=Deploy target=api-prod";
    const NEW: &str = "2026-10-23T11:00:00+00:00\taccount=1\tprofile=-\tregion=eu-west-2\tstage=dispatched action=RestartAppServer target=worker";

    #[test]
    fn the_log_is_read_filtered_and_rendered() {
        let path = log_with(&[OLD, NEW]);
        let (code, out, _, tail) = body(&["audit", "--since", "1d"], &path);
        assert_eq!(code, 0);
        assert!(out.contains("worker") && !out.contains("api-prod"), "{out}");
        assert!(!tail, "no --tail, nothing to follow");
        let (_, out, _, _) = body(&["audit", "--env", "api-prod", "--json"], &path);
        // One JSON object per line.
        let objects: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).expect("a JSON line"))
            .collect();
        assert_eq!(objects.len(), 1, "{out}");
        assert_eq!(objects[0]["target"], "api-prod");
        let (_, _, _, tail) = body(&["audit", "--tail"], &path);
        assert!(tail, "--tail follows");
        // From the end of what was printed, or the follow repeats it.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (_, tail) = audit_body(
            &argv(&["audit", "--tail"]),
            &path,
            chrono::Utc::now(),
            &mut out,
            &mut err,
        );
        assert_eq!(
            tail.map(|t| t.offset),
            Some(std::fs::metadata(&path).unwrap().len())
        );
    }

    #[test]
    fn exit_codes_follow_the_table() {
        let path = log_with(&[OLD]);
        let (code, _, err, _) = body(&["audit", "--bogus"], &path);
        assert_eq!(code, 2, "{err}");
        // A log that cannot be read: a directory where the file should be.
        let dir = path.parent().unwrap().to_path_buf();
        let (code, _, err, _) = body(&["audit"], &dir);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("read"), "{err}");
        let missing = dir.join("nope.log");
        let (code, out, _, _) = body(&["audit"], &missing);
        assert_eq!(code, 0);
        assert!(out.contains("not yet created"), "{out}");
        let (_, out, _, _) = body(&["audit", "--json"], &missing);
        assert!(
            out.is_empty(),
            "nothing but JSON on stdout under --json: {out}"
        );
        let (code, _, _, tail) = body(&["audit", "--tail"], &missing);
        assert_eq!(code, 0);
        assert!(!tail, "no log to follow: done, not a hang");
    }

    /// `--tail` advances past complete lines only, and starts over on a
    /// rotation.
    #[test]
    fn following_reads_whole_lines_and_survives_rotation() {
        let path = log_with(&[OLD]);
        let mut offset = std::fs::metadata(&path).unwrap().len();
        assert_eq!(read_new_lines(&path, &mut offset), None, "nothing new");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write as _;
        write!(f, "{NEW}\npartial").unwrap();
        let chunk = read_new_lines(&path, &mut offset).expect("the new line");
        assert_eq!(chunk.lines().collect::<Vec<_>>(), vec![NEW]);
        assert_eq!(
            read_new_lines(&path, &mut offset),
            None,
            "the torn line waits"
        );
        std::fs::write(&path, format!("{NEW}\n")).unwrap();
        assert!(
            read_new_lines(&path, &mut offset).is_some(),
            "rotation reads from the top"
        );
        let e = crate::audit::parse_audit_line(NEW).unwrap();
        let line = tail_render(&[e], false);
        assert!(
            line.starts_with(
                "2026-10-23T11:00:00+00:00\teu-west-2\tdispatched\tRestartAppServer\tworker\t"
            ),
            "{line}"
        );
    }
}
