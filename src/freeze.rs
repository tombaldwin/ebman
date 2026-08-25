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
pub(crate) struct FreezeMarker {
    pub pid: u32,
    pub reason: String,
    /// True when set via `:incident START` (the refusal remedy is
    /// `:incident END`); false for a plain `:freeze-deploys`.
    pub incident: bool,
    pub at: String,
}

impl FreezeMarker {
    /// The operator-facing remedy string for a refusal message.
    pub(crate) fn remedy(&self) -> &'static str {
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

/// Write (or overwrite) the marker for this process. Returns `Err`
/// when persistence failed — the caller MUST surface that, because a
/// silently-absent marker fails OPEN (agent + CLI writes are NOT
/// blocked) while the operator believes the fleet is frozen.
pub(crate) fn write_marker(reason: &str, incident: bool) -> std::io::Result<()> {
    write_marker_at(&marker_path(), std::process::id(), reason, incident)
}

fn write_marker_at(path: &Path, pid: u32, reason: &str, incident: bool) -> std::io::Result<()> {
    let body = format!(
        "{{\"pid\":{pid},\"reason\":{},\"incident\":{incident},\"at\":{}}}\n",
        crate::util::json_string(reason),
        crate::util::json_string(&chrono::Utc::now().to_rfc3339()),
    );
    // Atomic: write a temp then rename, so a concurrent reader never
    // catches a half-written (fail-open) marker (M1). Same 0600.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    crate::util::write_secure(&tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(error = %e, "could not persist freeze marker — cross-process enforcement inactive");
        return Err(e);
    }
    Ok(())
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

/// The one refusal sentence for an active fleet freeze.
///
/// Shared because it was written twice — once in `cli::refuse_if_frozen`
/// and once in the MCP write gate — with the same body and different
/// framing, so the two surfaces disagreed in wording about the same
/// condition and either could drift on the next edit.
pub(crate) fn refusal_message(marker: &FreezeMarker) -> String {
    let reason = if marker.reason.is_empty() {
        "no reason given"
    } else {
        marker.reason.as_str()
    };
    format!(
        "fleet freeze active ({reason}) — lift with `{}` in the owning TUI (pid {})",
        marker.remedy(),
        marker.pid
    )
}

/// The active freeze, if any: marker exists AND its owning pid is
/// alive. A dead-pid marker is stale (crashed TUI) — ignored and
/// removed so it can't confuse later readers.
pub(crate) fn read_active() -> Option<FreezeMarker> {
    read_active_with(&marker_path(), pid_alive)
}

/// Testable core: liveness probe injected.
fn read_active_with(path: &Path, alive: impl Fn(u32) -> bool) -> Option<FreezeMarker> {
    let m = parse_file(path)?;
    if alive(m.pid) {
        return Some(m);
    }
    // Dead pid → stale. Re-read immediately before removing and only
    // delete if the on-disk pid is STILL the dead one (I1): between
    // our read and here, another session could have overwritten the
    // file with a live-pid freeze, and a blind remove would silently
    // drop a VALID cross-process freeze.
    if parse_file(path).map(|m2| m2.pid) == Some(m.pid) {
        let _ = std::fs::remove_file(path);
    }
    None
}

fn parse_file(path: &Path) -> Option<FreezeMarker> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker(&text)
}

/// Pure parse of the marker body. Tolerant of unknown keys; missing
/// required keys yield `None` (a corrupt marker never blocks writes —
/// it also never *enables* enforcement, which the writer's warn line
/// covers).
pub(crate) fn parse_marker(text: &str) -> Option<FreezeMarker> {
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
        write_marker_at(&p, 4242, "checkout 5xx", true).unwrap();
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
        write_marker_at(&p, 4242, "stale", false).unwrap();
        assert!(read_active_with(&p, |_| false).is_none());
        assert!(!p.exists(), "stale marker must be removed by the reader");
    }

    #[test]
    fn live_pid_marker_is_active() {
        let p = tmp("live");
        write_marker_at(&p, 4242, "deploy freeze", false).unwrap();
        let m = read_active_with(&p, |_| true).expect("active");
        assert_eq!(m.remedy(), ":thaw-deploys");
        assert!(p.exists());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn clear_only_removes_own_marker() {
        let p = tmp("own");
        write_marker_at(&p, 111, "someone else's", false).unwrap();
        clear_if_pid_at(&p, 222);
        assert!(p.exists(), "another session's marker survives");
        clear_if_pid_at(&p, 111);
        assert!(!p.exists());
    }

    #[test]
    fn reader_cleanup_does_not_delete_a_freshly_written_live_marker() {
        // I1 (0.28 pre-tag): a reader that decided a marker is stale
        // (dead pid) must NOT delete it if another session overwrote
        // the file with a LIVE-pid freeze in the meantime.
        let p = tmp("toctou");
        write_marker_at(&p, 4242, "dead session", false).unwrap();
        // Reader sees pid 4242 as dead, but between its read and the
        // delete the file now holds a live-pid marker. Model that by
        // overwriting inside the liveness closure.
        let overwritten = std::cell::Cell::new(false);
        let result = read_active_with(&p, |_pid| {
            if !overwritten.get() {
                // First call: rewrite the file with a "live" pid.
                write_marker_at(&p, 111, "new live freeze", true).unwrap();
                overwritten.set(true);
            }
            false // report the ORIGINAL pid as dead
        });
        assert!(
            result.is_none(),
            "original dead marker not returned as active"
        );
        assert!(p.exists(), "the freshly-written live marker must survive");
        let m = parse_file(&p).unwrap();
        assert_eq!(m.pid, 111, "live marker intact");
        let _ = std::fs::remove_file(&p);
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
        write_marker_at(&p, 1, "the \"big\" one\nline2", false).unwrap();
        let m = parse_file(&p).expect("parses");
        assert_eq!(m.reason, "the \"big\" one\nline2");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn one_freeze_refusal_sentence_for_every_surface() {
        // The CLI and the MCP write gate each rendered their own copy
        // with the same body and different framing, so the two
        // surfaces described one condition in two ways and either
        // could drift on the next edit.
        let m = FreezeMarker {
            pid: 4242,
            reason: "prod incident".into(),
            incident: false,
            at: "2026-08-22T10:00:00Z".into(),
        };
        let msg = refusal_message(&m);
        assert!(msg.contains("prod incident"), "{msg}");
        assert!(msg.contains("4242"), "names the owning pid: {msg}");
        assert!(msg.contains(m.remedy()), "names the remedy: {msg}");

        // An empty reason still reads as a sentence rather than
        // trailing off into "()".
        let m = FreezeMarker {
            reason: String::new(),
            ..m
        };
        let msg = refusal_message(&m);
        assert!(msg.contains("no reason given"), "{msg}");
        assert!(!msg.contains("()"), "{msg}");
    }

    /// `pid_alive` itself was untested — `cargo mutants` reported four
    /// survivors here, including "replace with false".
    ///
    /// The logic that consumes it is well covered, because
    /// `read_active_with` takes the probe as a parameter and the tests
    /// pass a fake. That is exactly the seam mutation testing exists to
    /// find: the pure logic is exercised against a stub while the real
    /// implementation is exercised by nothing.
    ///
    /// It matters because the freeze marker is a fleet-wide write lock.
    /// A probe stuck on `false` treats a live session's freeze as stale
    /// and lifts it out from under them; stuck on `true`, a marker left
    /// by a crashed process never clears and deploys stay blocked with
    /// no way to release them.
    #[cfg(unix)]
    #[test]
    fn pid_alive_says_yes_to_this_process_and_no_to_a_reaped_one() {
        assert!(
            super::pid_alive(std::process::id()),
            "this very process is alive; a probe that says otherwise would \
             lift a live session's freeze"
        );

        // A pid that definitely no longer exists: spawn a child, wait for
        // it, and let `status()` reap it. Deterministic, unlike guessing
        // at a high pid number that might have been recycled.
        let child = std::process::Command::new("true")
            .spawn()
            .expect("spawn a trivial child");
        let pid = child.id();
        let mut child = child;
        let _ = child.wait().expect("reap the child");
        assert!(
            !super::pid_alive(pid),
            "pid {pid} exited and was reaped; a probe that still says alive \
             would leave a crashed session's freeze in place forever"
        );
    }
}
