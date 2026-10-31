//! The freeze marker: its model, its parse, and how one is written
//! (atomically, or exclusively for an agent's declare).

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreezeMarker {
    pub pid: u32,
    pub reason: String,
    /// True when set via `:incident START` (the refusal remedy is
    /// `:incident END`); false for a plain `:freeze-deploys`.
    pub incident: bool,
    pub at: String,
    /// Declared with `ebman freeze`: no session owns it, and it holds
    /// until an explicit thaw.
    pub persistent: bool,
    /// The MCP client that declared it with `freeze_fleet`, sanitised:
    /// the operator lifting a freeze needs to know an agent put it
    /// there, not that they ran `ebman freeze` and forgot. `None` for
    /// one an operator declared.
    pub agent: Option<String>,
}

impl FreezeMarker {
    /// The operator-facing remedy string for a refusal message.
    pub(crate) fn remedy(&self) -> &'static str {
        if self.persistent {
            "ebman thaw"
        } else if self.incident {
            ":incident END"
        } else {
            ":thaw-deploys"
        }
    }

    /// Who holds it, for a refusal or a status line.
    pub(crate) fn holder(&self) -> String {
        if let (true, Some(agent)) = (self.persistent, &self.agent) {
            format!("declared by an agent ({agent}) with `freeze_fleet`")
        } else if self.persistent {
            "declared with `ebman freeze`".to_string()
        } else {
            format!("held by a TUI session (pid {})", self.pid)
        }
    }

    /// Who holds it in a few words, for the header chip.
    pub(crate) fn short_holder(&self) -> String {
        if self.persistent && self.agent.is_some() {
            "agent freeze".to_string()
        } else if self.persistent {
            "ebman freeze".to_string()
        } else {
            format!("pid {}", self.pid)
        }
    }

    /// The reason, or the words every surface shows for none.
    pub(crate) fn reason_or_default(&self) -> &str {
        if self.reason.is_empty() {
            "no reason given"
        } else {
            &self.reason
        }
    }
}

/// The pid a persistent freeze's legacy mirror carries. ebman before
/// 0.47 reads only `freeze.json`, as active while its pid is alive and
/// started before the marker; pid 1 always is, on a host. So an older
/// binary (an MCP server spawned before an upgrade) still refuses writes
/// under a persistent freeze. In a container whose pid 1 started after
/// the marker, an older binary judges it reused and removes the mirror;
/// the holder file in `freeze.d/` is untouched and every 0.47+ reader
/// still honours it, and repairs the mirror when it next reads.
pub(super) const PERSISTENT_PID: u32 = 1;

#[cfg(test)]
thread_local! {
    /// Under test, make exclusive links fail with this error: as a
    /// full or read-only disk would, or as a marker a thaw then lifted
    /// would (`AlreadyExists` with nothing there). Nothing else can
    /// reach those paths, since the temp file is written into the same
    /// directory first.
    pub(crate) static LINK_FAILS: std::cell::Cell<Option<std::io::ErrorKind>> =
        const { std::cell::Cell::new(None) };
    /// Under test, run this right after a successful exclusive link,
    /// with the marker's path: what another process does in that
    /// instant (a thaw landing mid-declare).
    pub(crate) static AFTER_LINK: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
}

/// Temp files a write left behind when its process died between
/// writing and renaming or linking them (`*.tmp.PID.N`, `*.excl.PID.N`).
/// Nothing reads them, so they are harmless to enforcement, but nothing
/// else removes them either. Only ones over a minute old: a younger one
/// may be another process's write in flight.
pub(super) fn sweep_stale_temps(dir: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let stale = std::time::Duration::from_secs(60);
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let temp =
            name.starts_with("persistent.") && (name.contains(".tmp.") || name.contains(".excl."));
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > stale);
        if temp && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub(super) fn link_into_place(tmp: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(kind) = LINK_FAILS.with(|f| f.get()) {
        return Err(std::io::Error::from(kind));
    }
    std::fs::hard_link(tmp, path)?;
    #[cfg(test)]
    if let Some(after) = AFTER_LINK.with(|f| f.get()) {
        after(path);
    }
    Ok(())
}

/// A persistent agent marker at `path`, only if none is there: the
/// body goes to a temp file and is hard-linked into place, which is
/// atomic and fails if `path` exists, so a reader never sees a partial
/// marker and an existing one is never replaced. `Ok(false)` when one
/// was there.
pub(super) fn write_marker_exclusive(
    path: &Path,
    reason: &str,
    incident: bool,
    agent: &str,
) -> std::io::Result<bool> {
    // Unique per write, as `write_marker_full`'s temp is.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "excl.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_marker_full(
        &tmp,
        PERSISTENT_PID,
        reason,
        incident,
        true,
        false,
        Some(agent),
    )?;
    let linked = link_into_place(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => {
            tracing::warn!(error = %e, "could not persist freeze marker — cross-process enforcement inactive");
            Err(e)
        }
    }
}

/// Write one marker body to `path`, atomically. `mirror` marks the
/// legacy copy for older readers.
pub(super) fn write_marker_full(
    path: &Path,
    pid: u32,
    reason: &str,
    incident: bool,
    persistent: bool,
    mirror: bool,
    agent: Option<&str>,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let agent = agent
        .map(|a| format!(",\"agent\":{}", crate::util::json_string(a)))
        .unwrap_or_default();
    let body = format!(
        "{{\"pid\":{pid},\"reason\":{},\"incident\":{incident},\"at\":{},\"persistent\":{persistent},\"mirror\":{mirror}{agent}}}\n",
        crate::util::json_string(reason),
        crate::util::json_string(&chrono::Utc::now().to_rfc3339()),
    );
    // Atomic: write a temp then rename, so a concurrent reader never
    // catches a half-written (fail-open) marker (M1). Same 0600.
    // Unique per WRITE, not per process. Two concurrent writers in one
    // process shared a temp path, so one's `rename` moved the file out
    // from under the other's and the loser got ENOENT — a spurious `Err`
    // from a function whose doc says the caller MUST surface failure,
    // because a silently-absent marker fails open. Found as a ~30% flake
    // in the freeze tests (12 failures in 40 runs); the flake was the
    // symptom, this is the defect.
    static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    crate::util::write_secure(&tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(error = %e, "could not persist freeze marker — cross-process enforcement inactive");
        return Err(e);
    }
    Ok(())
}

pub(super) fn is_mirror_text(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("mirror").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

pub(super) fn is_mirror(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| is_mirror_text(&t))
}

pub(super) fn parse_file(path: &Path) -> Option<FreezeMarker> {
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
        persistent: v
            .get("persistent")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        agent: v.get("agent").and_then(|a| a.as_str()).map(str::to_string),
    })
}
