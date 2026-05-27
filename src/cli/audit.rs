//! `ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID]
//! [--action NAME] [--json]` — surface `~/.cache/ebman/audit.log`
//! for scripting / Slack-bot routing / CI gating.
//!
//! Two phases: read existing file end-to-end (parse + filter +
//! render), then optionally `--tail` poll for new bytes every
//! second from EOF. Rotation detected by file shrink.

use color_eyre::eyre::Result;

use crate::{audit as audit_log, aws, util};

pub async fn run(args: &[String]) -> Result<()> {
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
            other => {
                eprintln!("ebman audit: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    let since_dt: Option<chrono::DateTime<chrono::Utc>> = match since_str.as_deref() {
        None => None,
        Some(s) => match aws::parse_window_ms(s) {
            Some(ms) => Some(chrono::Utc::now() - chrono::Duration::milliseconds(ms)),
            None => {
                eprintln!(
                    "ebman audit: --since expects a duration like `5m` / `30m` / `1h` / `2d`"
                );
                std::process::exit(2);
            }
        },
    };

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
