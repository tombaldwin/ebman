//! The two sidecar records: a session freeze that ended with its
//! session, and a persistent one the operator thawed.

use super::*;

/// How long an ended freeze is reported.
pub(super) const ENDED_REPORT_HOURS: i64 = 24;

/// A session freeze that ended because its session did, as the reader
/// that found it stale recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndedFreeze {
    pub marker: FreezeMarker,
    pub ended_at: chrono::DateTime<chrono::Utc>,
}

impl EndedFreeze {
    /// One line for the TUI, `ebman freeze --status` and `doctor`.
    pub(crate) fn message(&self) -> String {
        let reason = self.marker.reason_or_default();
        format!(
            "a fleet freeze ({reason}) ended when its TUI session (pid {}) exited, found at {} \
             — if it should still hold, declare it with `ebman freeze`",
            self.marker.pid,
            self.ended_at.format("%Y-%m-%d %H:%M UTC")
        )
    }
}

pub(super) fn write_ended_at(path: &Path, m: &FreezeMarker) {
    if let Err(e) = write_record(path, m, "ended_at", chrono::Utc::now()) {
        tracing::warn!(error = %e, "could not record that a freeze ended with its session");
    }
}

pub(super) fn read_ended_at(
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<EndedFreeze> {
    let (marker, ended_at) = read_record(path, "ended_at")?;
    within(now, ended_at, ENDED_REPORT_HOURS * 3600).then_some(EndedFreeze { marker, ended_at })
}

/// How long an operator's thaw binds `freeze_fleet`. A constant, not a
/// config key: a looping agent re-declares in seconds, a rollback takes
/// minutes, and a refusal in the window costs one message to an
/// operator who has just been at the keyboard.
pub(crate) const THAW_HOLDS_MINS: i64 = 30;

/// A persistent freeze the operator lifted, while the thaw holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThawedFreeze {
    pub marker: FreezeMarker,
    pub thawed_at: chrono::DateTime<chrono::Utc>,
}

impl ThawedFreeze {
    /// When `freeze_fleet` may declare again.
    pub(crate) fn holds_until(&self) -> chrono::DateTime<chrono::Utc> {
        self.thawed_at + chrono::Duration::minutes(THAW_HOLDS_MINS)
    }

    /// One line for `ebman thaw`, `ebman freeze --status` and `doctor`.
    pub(crate) fn message(&self) -> String {
        format!(
            "a fleet freeze ({}) was lifted at {}; an agent's `freeze_fleet` is refused \
             until {}",
            self.marker.reason_or_default(),
            self.thawed_at.format("%Y-%m-%d %H:%M UTC"),
            self.holds_until().format("%H:%M UTC")
        )
    }
}

pub(super) fn write_thawed_at(
    path: &Path,
    m: &FreezeMarker,
    thawed_at: chrono::DateTime<chrono::Utc>,
) {
    if let Err(e) = write_record(path, m, "thawed_at", thawed_at) {
        // Said, not swallowed: without the record an agent can freeze
        // again at once, which is the loop this exists to stop.
        tracing::warn!(error = %e, "could not record a thaw; freeze_fleet is not held off");
    }
}

/// `None` for a missing or unreadable record, as for a corrupt marker:
/// no hold, which only gives an agent back the tool it had.
pub(super) fn read_thawed_at(
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ThawedFreeze> {
    let (marker, thawed_at) = read_record(path, "thawed_at")?;
    within(now, thawed_at, THAW_HOLDS_MINS * 60).then_some(ThawedFreeze { marker, thawed_at })
}

/// Write `m` to `path` with `key` saying when, as both records are: the
/// marker's own fields, so it parses as one, and a timestamp.
///
/// Temp then rename, as the markers are: written in place, a reader
/// between the truncate and the write saw an empty record (for a thaw,
/// no hold). The ended record was written in place, and hand-formatted
/// apart from the thaw's `json!`.
fn write_record(
    path: &Path,
    m: &FreezeMarker,
    key: &str,
    when: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<()> {
    let mut v = serde_json::json!({
        "pid": m.pid,
        "reason": m.reason,
        "incident": m.incident,
        "at": m.at,
        "persistent": m.persistent,
    });
    v[key] = serde_json::Value::String(when.to_rfc3339());
    if let Some(agent) = &m.agent {
        v["agent"] = serde_json::Value::String(agent.clone());
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = crate::util::write_secure(&tmp, format!("{v}\n").as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The marker and the `key` timestamp a record holds; `None` when either
/// is missing or unreadable.
fn read_record(path: &Path, key: &str) -> Option<(FreezeMarker, chrono::DateTime<chrono::Utc>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let marker = parse_marker(&text)?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let when = chrono::DateTime::parse_from_rfc3339(v.get(key)?.as_str()?)
        .ok()?
        .with_timezone(&chrono::Utc);
    Some((marker, when))
}

/// Is `when` less than `secs` ago? Bounded below too: a record from the
/// future (a clock stepped back, a record copied from another host)
/// held until the clock passed it, however far that was.
fn within(
    now: chrono::DateTime<chrono::Utc>,
    when: chrono::DateTime<chrono::Utc>,
    secs: i64,
) -> bool {
    (-START_SLACK_SECS..secs).contains(&now.signed_duration_since(when).num_seconds())
}
