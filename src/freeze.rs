//! Cross-process freeze marker (0.28, folded from the MCP v3 notes):
//! the TUI persists `:freeze-deploys` / `:incident START` to
//! `~/.cache/ebman/freeze.json` so processes that can't see its
//! session state — the MCP write tools and the CLI write paths
//! (`ebman action`, `audit replay`, `lint --fix`) — refuse to
//! dispatch while a freeze is active.
//!
//! Session-scoped semantics are preserved via the pid: readers treat
//! the marker as active only while the owning pid is alive, so a
//! crashed TUI can't phantom-freeze the fleet — a dead-pid marker is
//! ignored AND cleaned up by the next reader. Demo-mode TUIs never
//! persist (demo freeze is play-acting). Single file, last-writer-wins
//! across concurrent TUI sessions — documented, not defended against.
//! Deleting the file is the manual unfreeze of last resort.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreezeMarker {
    pub pid: u32,
    pub reason: String,
    /// True when set via `:incident START` (the refusal remedy is
    /// `:incident END`); false for a plain `:freeze-deploys`.
    pub incident: bool,
    pub at: String,
}

impl FreezeMarker {
    /// The operator-facing remedy string for a refusal message.
    pub fn remedy(&self) -> &'static str {
        if self.incident {
            ":incident END"
        } else {
            ":thaw-deploys"
        }
    }
}

fn marker_path() -> PathBuf {
    crate::util::cache_dir().join("freeze.json")
}

/// Write (or overwrite) the marker for this process. 0600 like every
/// other cache artifact.
pub fn write_marker(reason: &str, incident: bool) {
    write_marker_at(&marker_path(), std::process::id(), reason, incident);
}

fn write_marker_at(path: &Path, pid: u32, reason: &str, incident: bool) {
    let body = format!(
        "{{\"pid\":{pid},\"reason\":{},\"incident\":{incident},\"at\":{}}}\n",
        crate::util::json_string(reason),
        crate::util::json_string(&chrono::Utc::now().to_rfc3339()),
    );
    if let Err(e) = crate::util::write_secure(path, body.as_bytes()) {
        tracing::warn!(error = %e, "could not persist freeze marker — cross-process enforcement inactive");
    }
}

/// Remove the marker, but only if THIS process owns it — a thaw in
/// one TUI session must not silently clear another session's freeze.
pub fn clear_marker_if_own() {
    clear_if_pid_at(&marker_path(), std::process::id());
}

fn clear_if_pid_at(path: &Path, own_pid: u32) {
    if let Some(m) = parse_file(path) {
        if m.pid == own_pid {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The active freeze, if any: marker exists AND its owning pid is
/// alive. A dead-pid marker is stale (crashed TUI) — ignored and
/// removed so it can't confuse later readers.
pub fn read_active() -> Option<FreezeMarker> {
    read_active_with(&marker_path(), pid_alive)
}

/// Testable core: liveness probe injected.
fn read_active_with(path: &Path, alive: impl Fn(u32) -> bool) -> Option<FreezeMarker> {
    let m = parse_file(path)?;
    if alive(m.pid) {
        Some(m)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

fn parse_file(path: &Path) -> Option<FreezeMarker> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker(&text)
}

/// Pure parse of the marker body. Tolerant of unknown keys; missing
/// required keys yield `None` (a corrupt marker never blocks writes —
/// it also never *enables* enforcement, which the writer's warn line
/// covers).
pub fn parse_marker(text: &str) -> Option<FreezeMarker> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(FreezeMarker {
        pid: v.get("pid")?.as_u64()? as u32,
        reason: v.get("reason")?.as_str()?.to_string(),
        incident: v.get("incident").and_then(|b| b.as_bool()).unwrap_or(false),
        at: v
            .get("at")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0): signal 0 performs the permission/existence check
    // without delivering anything. EPERM still means "exists".
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // No cheap portable probe — fail active (enforce) rather than
    // silently dropping a freeze.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ebman-freeze-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn marker_round_trips() {
        let p = tmp("rt");
        write_marker_at(&p, 4242, "checkout 5xx", true);
        let m = parse_file(&p).expect("parses");
        assert_eq!(m.pid, 4242);
        assert_eq!(m.reason, "checkout 5xx");
        assert!(m.incident);
        assert_eq!(m.remedy(), ":incident END");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dead_pid_marker_is_ignored_and_cleaned() {
        let p = tmp("dead");
        write_marker_at(&p, 4242, "stale", false);
        assert!(read_active_with(&p, |_| false).is_none());
        assert!(!p.exists(), "stale marker must be removed by the reader");
    }

    #[test]
    fn live_pid_marker_is_active() {
        let p = tmp("live");
        write_marker_at(&p, 4242, "deploy freeze", false);
        let m = read_active_with(&p, |_| true).expect("active");
        assert_eq!(m.remedy(), ":thaw-deploys");
        assert!(p.exists());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn clear_only_removes_own_marker() {
        let p = tmp("own");
        write_marker_at(&p, 111, "someone else's", false);
        clear_if_pid_at(&p, 222);
        assert!(p.exists(), "another session's marker survives");
        clear_if_pid_at(&p, 111);
        assert!(!p.exists());
    }

    #[test]
    fn corrupt_marker_never_blocks() {
        let p = tmp("corrupt");
        let _ = crate::util::write_secure(&p, b"not json at all");
        assert!(read_active_with(&p, |_| true).is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reason_with_quotes_survives() {
        let p = tmp("quotes");
        write_marker_at(&p, 1, "the \"big\" one\nline2", false);
        let m = parse_file(&p).expect("parses");
        assert_eq!(m.reason, "the \"big\" one\nline2");
        let _ = std::fs::remove_file(&p);
    }
}
