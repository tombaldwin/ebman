//! Cross-process fleet freeze (0.28, folded from the MCP v3 notes).
//! A TUI's `:freeze-deploys` / `:incident START`, and `ebman freeze`,
//! persist a marker so every process refuses to dispatch while one
//! holds: the TUI, the MCP write tools, and the CLI write paths
//! (`ebman action`, `audit replay`, `lint --fix`).
//!
//! One file per holder, under `freeze.d/` beside the marker path:
//! `persistent.json` for `ebman freeze`, which holds until an explicit
//! thaw, and `session-<pid>.json` for each TUI session, which holds
//! while that process lives. A single shared file was last-writer-wins:
//! a TUI's `:incident START` replaced an `ebman freeze`, and its
//! `:incident END` or exit then deleted it, lifting a freeze nobody had
//! thawed; and `ebman freeze` replaced a live session's marker, so a
//! later `ebman thaw` lifted the session's freeze too.
//!
//! Session scope is kept by the pid: a session file whose process is
//! gone (or whose pid was reused) is stale, and the next reader removes
//! it and records that in the ended-freeze file, so the next person to
//! look learns a freeze ended with its session rather than by decision.
//! Demo-mode TUIs never persist.
//!
//! The legacy `freeze.json` is kept as a mirror of one live holder for
//! ebman older than 0.47, which reads only that file. A mirror is
//! marked so; one without the mark was written by an older TUI and is a
//! holder in its own right. Deleting `freeze.d/` and `freeze.json` is
//! the manual unfreeze of last resort. See
//! `docs/design/persistent-freeze.md`.

use std::path::{Path, PathBuf};

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
        if self.persistent {
            "declared with `ebman freeze`".to_string()
        } else {
            format!("held by a TUI session (pid {})", self.pid)
        }
    }

    /// Who holds it in a few words, for the header chip.
    pub(crate) fn short_holder(&self) -> String {
        if self.persistent {
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

/// The pid a persistent marker's legacy mirror carries. ebman before
/// 0.47 reads only `freeze.json`, as active while its pid is alive and
/// started before the marker; pid 1 always is, on a host. So an older
/// binary (an MCP server spawned before an upgrade) still refuses writes
/// under a persistent freeze. In a container whose pid 1 started after
/// the marker, an older binary judges it reused and removes the mirror;
/// the holder file in `freeze.d/` is untouched and every 0.47+ reader
/// still honours it.
const PERSISTENT_PID: u32 = 1;

/// The legacy marker path. The holders live in [`holders_dir`] beside it.
pub(crate) fn marker_path() -> PathBuf {
    crate::util::cache_dir().join("freeze.json")
}

/// Where the holder files for `marker` live: `freeze.json` → `freeze.d/`.
fn holders_dir(marker: &Path) -> PathBuf {
    marker.with_extension("d")
}

fn persistent_file(marker: &Path) -> PathBuf {
    holders_dir(marker).join("persistent.json")
}

fn session_file(marker: &Path, pid: u32) -> PathBuf {
    holders_dir(marker).join(format!("session-{pid}.json"))
}

/// The ended-freeze record for `marker`: `freeze.json` →
/// `freeze.ended.json`, so a test App's marker has its own.
fn ended_file(marker: &Path) -> PathBuf {
    marker.with_extension("ended.json")
}

/// Declare this TUI session's freeze. Returns `Err` when persistence
/// failed — the caller MUST surface that, because a silently-absent
/// marker fails OPEN (agent + CLI writes are NOT blocked) while the
/// operator believes the fleet is frozen.
#[cfg(test)]
pub(crate) fn write_marker(reason: &str, incident: bool) -> std::io::Result<()> {
    declare_session_at(&marker_path(), std::process::id(), reason, incident)
}

/// [`write_marker`] for `marker`, as session `pid`. Writes its own
/// holder file only, so no other holder is replaced.
pub(crate) fn declare_session_at(
    marker: &Path,
    pid: u32,
    reason: &str,
    incident: bool,
) -> std::io::Result<()> {
    write_marker_full(
        &session_file(marker, pid),
        pid,
        reason,
        incident,
        false,
        false,
    )?;
    sync_legacy(marker);
    clear_ended_beside(marker);
    Ok(())
}

/// Declare a persistent freeze (`ebman freeze`): no session owns it.
/// Same failure contract as [`write_marker`].
pub(crate) fn write_persistent_marker(reason: &str, incident: bool) -> std::io::Result<()> {
    declare_persistent_at(&marker_path(), reason, incident)
}

/// [`write_persistent_marker`] for `marker`.
pub(crate) fn declare_persistent_at(
    marker: &Path,
    reason: &str,
    incident: bool,
) -> std::io::Result<()> {
    write_marker_full(
        &persistent_file(marker),
        PERSISTENT_PID,
        reason,
        incident,
        true,
        false,
    )?;
    sync_legacy(marker);
    clear_ended_beside(marker);
    Ok(())
}

/// A holder for `marker` with any owner, for a test standing in for
/// another session (`pid`) or for `ebman freeze` (`persistent`).
#[cfg(test)]
pub(crate) fn write_test_marker(marker: &Path, pid: u32, reason: &str, persistent: bool) {
    let written = if persistent {
        declare_persistent_at(marker, reason, false)
    } else {
        declare_session_at(marker, pid, reason, false)
    };
    written.expect("test marker written");
}

/// An ended-freeze record for `marker`, as a reader that found a
/// session's marker stale would leave it.
#[cfg(test)]
pub(crate) fn write_test_ended(marker: &Path, reason: &str) {
    let m = FreezeMarker {
        pid: 4242,
        reason: reason.into(),
        incident: true,
        at: chrono::Utc::now().to_rfc3339(),
        persistent: false,
    };
    write_ended_at(&ended_file(marker), &m);
}

/// Write one marker body to `path`, atomically. `mirror` marks the
/// legacy copy for older readers.
fn write_marker_full(
    path: &Path,
    pid: u32,
    reason: &str,
    incident: bool,
    persistent: bool,
    mirror: bool,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = format!(
        "{{\"pid\":{pid},\"reason\":{},\"incident\":{incident},\"at\":{},\"persistent\":{persistent},\"mirror\":{mirror}}}\n",
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

/// Keep the legacy `freeze.json` showing one live holder, for ebman
/// older than 0.47, which reads only that file. Left alone when an
/// older TUI wrote it (no mirror mark) and is still alive: it is that
/// session's own marker.
fn sync_legacy(marker: &Path) {
    sync_legacy_with(marker, pid_alive, process_start_epoch);
}

fn sync_legacy_with(
    marker: &Path,
    alive: impl Fn(u32) -> bool + Copy,
    start_epoch: impl Fn(u32) -> Option<i64> + Copy,
) {
    if old_tui_marker_live(marker, alive, start_epoch) {
        return;
    }
    // As written, not judged: an older reader judges the mirror's pid
    // itself, and reaping here would end a freeze as a side effect of
    // declaring another.
    match dir_holder_files(marker).first() {
        Some(h) => {
            let _ = write_marker_full(marker, h.pid, &h.reason, h.incident, h.persistent, true);
        }
        None => {
            let _ = std::fs::remove_file(marker);
        }
    }
}

/// Is `freeze.json` an older TUI's own marker (no mirror mark) whose
/// process lives? Judged without reaping.
fn old_tui_marker_live(
    marker: &Path,
    alive: impl Fn(u32) -> bool,
    start_epoch: impl Fn(u32) -> Option<i64>,
) -> bool {
    let Some(m) = parse_file(marker) else {
        return false;
    };
    !is_mirror(marker) && owner_live(&m, alive, start_epoch)
}

fn is_mirror(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("mirror").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Does `m`'s owner still hold it? Always, for a persistent marker.
fn owner_live(
    m: &FreezeMarker,
    alive: impl Fn(u32) -> bool,
    start_epoch: impl Fn(u32) -> Option<i64>,
) -> bool {
    if m.persistent {
        return true;
    }
    // `at` is RFC3339 text on disk. An unparseable timestamp means we
    // cannot judge reuse, so fall back to "assume owner" — closed.
    let at_epoch = chrono::DateTime::parse_from_rfc3339(&m.at)
        .map(|t| t.timestamp())
        .ok();
    marker_owner_is_live(alive(m.pid), start_epoch(m.pid), at_epoch)
}

/// The holder files in `freeze.d/` as written, persistent first, then
/// sessions in name order.
fn dir_holder_files(marker: &Path) -> Vec<FreezeMarker> {
    dir_holder_paths(marker)
        .iter()
        .filter_map(|p| parse_file(p))
        .collect()
}

fn dir_holder_paths(marker: &Path) -> Vec<PathBuf> {
    let mut out = vec![persistent_file(marker)];
    if let Ok(entries) = std::fs::read_dir(holders_dir(marker)) {
        let mut sessions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("session-") && n.ends_with(".json"))
            })
            .collect();
        sessions.sort();
        out.extend(sessions);
    }
    out
}

/// End this process's session freeze, and only that: a thaw in one TUI
/// session must not clear another session's freeze or a persistent one.
pub fn clear_marker_if_own() {
    clear_session_at(&marker_path(), std::process::id());
}

/// [`clear_marker_if_own`] for `marker`, as session `pid`.
pub(crate) fn clear_session_at(marker: &Path, pid: u32) {
    let _ = std::fs::remove_file(session_file(marker, pid));
    sync_legacy(marker);
}

/// What an explicit thaw found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Thaw {
    /// A persistent freeze, now lifted.
    Lifted(FreezeMarker),
    /// No persistent freeze, but a live TUI session holds one: that
    /// session's to lift.
    HeldBySession(FreezeMarker),
    /// No freeze in force.
    Nothing,
}

/// `ebman thaw` / `:thaw-deploys` on a persistent freeze: lift it.
/// Never a live session's freeze, which only that session (or deleting
/// its file) may end.
pub(crate) fn thaw_persistent() -> Thaw {
    thaw_persistent_at(&marker_path())
}

/// [`thaw_persistent`] for `marker`.
pub(crate) fn thaw_persistent_at(marker: &Path) -> Thaw {
    let thaw = thaw_persistent_with(marker, pid_alive, process_start_epoch);
    if matches!(thaw, Thaw::Lifted(_)) {
        clear_ended_beside(marker);
    }
    thaw
}

fn thaw_persistent_with(
    marker: &Path,
    alive: impl Fn(u32) -> bool + Copy,
    start_epoch: impl Fn(u32) -> Option<i64> + Copy,
) -> Thaw {
    let file = persistent_file(marker);
    // Lifted only if this call removed it: a thaw racing another must
    // not report, and audit, a lift it did not make.
    let lifted = parse_file(&file).filter(|_| std::fs::remove_file(&file).is_ok());
    sync_legacy_with(marker, alive, start_epoch);
    match lifted {
        Some(m) => Thaw::Lifted(m),
        None => match read_all_with(marker, alive, start_epoch).into_iter().next() {
            Some(m) => Thaw::HeldBySession(m),
            None => Thaw::Nothing,
        },
    }
}

/// The one refusal sentence for an active fleet freeze.
///
/// Shared because it was written twice — once in `cli::refuse_if_frozen`
/// and once in the MCP write gate — with the same body and different
/// framing, so the two surfaces disagreed in wording about the same
/// condition and either could drift on the next edit.
pub(crate) fn refusal_message(marker: &FreezeMarker) -> String {
    let reason = marker.reason_or_default();
    if marker.persistent {
        return format!(
            "fleet freeze active ({reason}) — {}; lift with `{}`",
            marker.holder(),
            marker.remedy()
        );
    }
    format!(
        "fleet freeze active ({reason}) — lift with `{}` in the owning TUI (pid {})",
        marker.remedy(),
        marker.pid
    )
}

/// A freeze in force, if any: the persistent one first, else a live
/// session's. Reaps stale session files as it reads (see
/// [`read_all_at`]).
pub(crate) fn read_active() -> Option<FreezeMarker> {
    read_active_at(&marker_path())
}

/// [`read_active`] for `marker`: the TUI reads through its own, so a
/// test App does not see another test's freeze.
pub(crate) fn read_active_at(marker: &Path) -> Option<FreezeMarker> {
    read_all_at(marker).into_iter().next()
}

/// Every freeze in force for `marker`, the persistent one first. A
/// session file whose process is gone is removed as it is read, and
/// recorded as an ended freeze; so is a legacy marker an older TUI left.
pub(crate) fn read_all_at(marker: &Path) -> Vec<FreezeMarker> {
    read_all_with(marker, pid_alive, process_start_epoch)
}

fn read_all_with(
    marker: &Path,
    alive: impl Fn(u32) -> bool + Copy,
    start_epoch: impl Fn(u32) -> Option<i64> + Copy,
) -> Vec<FreezeMarker> {
    let mut all = dir_holders_with(marker, alive, start_epoch);
    if let Some(old) = legacy_holder_with(marker, alive, start_epoch) {
        all.push(old);
    }
    all
}

/// The live holders in `freeze.d/`, persistent first. Stale session
/// files are removed and recorded.
fn dir_holders_with(
    marker: &Path,
    alive: impl Fn(u32) -> bool + Copy,
    start_epoch: impl Fn(u32) -> Option<i64> + Copy,
) -> Vec<FreezeMarker> {
    dir_holder_paths(marker)
        .iter()
        .filter_map(|p| live_or_reap(marker, p, alive, start_epoch))
        .collect()
}

/// The legacy marker as a holder: only one an older TUI wrote (no mirror
/// mark) and whose process lives; reaped when it does not. A mirror is
/// never a holder of its own; the file it mirrors is.
fn legacy_holder_with(
    marker: &Path,
    alive: impl Fn(u32) -> bool + Copy,
    start_epoch: impl Fn(u32) -> Option<i64> + Copy,
) -> Option<FreezeMarker> {
    if is_mirror(marker) {
        return None;
    }
    live_or_reap(marker, marker, alive, start_epoch)
}

/// The session marker at `path` if its process lives; otherwise remove
/// it, record it as ended, and return `None`.
fn live_or_reap(
    marker: &Path,
    path: &Path,
    alive: impl Fn(u32) -> bool,
    start_epoch: impl Fn(u32) -> Option<i64>,
) -> Option<FreezeMarker> {
    let m = parse_file(path)?;
    if owner_live(&m, &alive, &start_epoch) {
        return Some(m);
    }
    // Dead pid, or the pid was reused → stale. Re-read immediately
    // before removing, and only delete if the file is STILL the dead
    // one: between our read and here it could have been rewritten with a
    // live freeze, and a blind remove would silently drop it.
    if parse_file(path).as_ref() == Some(&m) {
        let _ = std::fs::remove_file(path);
        // Said, not swallowed: a freeze that ended because its session
        // did is news to whoever looks next.
        write_ended_at(&ended_file(marker), &m);
    }
    None
}

/// How far after the marker a process must have started before we call
/// the pid reused.
///
/// The true owner always starts BEFORE it writes its own marker, so any
/// positive difference is suspicious in principle. The slack is for
/// clock movement, not for reuse: an NTP step backwards would otherwise
/// make a live session's marker look like a reused pid and **lift the
/// freeze**, which is the one direction this must never fail in. Reuse
/// in practice happens hours or days later, so five minutes costs
/// nothing and buys immunity to ordinary clock wobble.
const START_SLACK_SECS: i64 = 300;

/// Does the process now holding `pid` look like the one that wrote the
/// marker?
///
/// Pure, so the interesting case — a reused pid — is testable without
/// waiting for the kernel to wrap its pid counter.
///
/// Unknown start time means "assume it is the owner": failing closed
/// keeps a phantom freeze refusing writes, which is recoverable by
/// deleting the file. Failing open would let writes through during a
/// declared incident.
///
/// `marker_at_epoch` is an `Option` rather than a sentinel. It was
/// `i64::MAX` for "unparseable timestamp", which read as the closed
/// direction and was the open one: `i64::MAX + START_SLACK_SECS`
/// overflows, wrapping to a large negative, so every real start time
/// compared greater and the marker was judged reused — deleted, freeze
/// silently lifted. Debug builds panicked instead. Encoding "unknown"
/// as an extreme value puts it in the arithmetic's range; `None` keeps
/// it out.
fn marker_owner_is_live(
    alive: bool,
    start_epoch: Option<i64>,
    marker_at_epoch: Option<i64>,
) -> bool {
    if !alive {
        return false;
    }
    let Some(marker_at) = marker_at_epoch else {
        // Cannot judge reuse without a marker timestamp — assume owner.
        return true;
    };
    // Reads as "not reused". Written this way round because the
    // interesting case is the exclusion: `None` and an early start both
    // mean "assume owner", which is the closed direction.
    !matches!(start_epoch, Some(start) if start > marker_at.saturating_add(START_SLACK_SECS))
}

/// How long an ended freeze is reported.
const ENDED_REPORT_HOURS: i64 = 24;

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

fn write_ended_at(path: &Path, m: &FreezeMarker) {
    let body = format!(
        "{{\"pid\":{},\"reason\":{},\"incident\":{},\"at\":{},\"ended_at\":{}}}\n",
        m.pid,
        crate::util::json_string(&m.reason),
        m.incident,
        crate::util::json_string(&m.at),
        crate::util::json_string(&chrono::Utc::now().to_rfc3339()),
    );
    if let Err(e) = crate::util::write_secure(path, body.as_bytes()) {
        tracing::warn!(error = %e, "could not record that a freeze ended with its session");
    }
}

/// A session freeze that ended with its session in the last day, if any.
pub(crate) fn read_ended() -> Option<EndedFreeze> {
    read_ended_beside(&marker_path())
}

/// [`read_ended`] for `marker`.
pub(crate) fn read_ended_beside(marker: &Path) -> Option<EndedFreeze> {
    read_ended_at(&ended_file(marker), chrono::Utc::now())
}

fn read_ended_at(path: &Path, now: chrono::DateTime<chrono::Utc>) -> Option<EndedFreeze> {
    let text = std::fs::read_to_string(path).ok()?;
    let marker = parse_marker(&text)?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let ended_at = chrono::DateTime::parse_from_rfc3339(v.get("ended_at")?.as_str()?)
        .ok()?
        .with_timezone(&chrono::Utc);
    (now.signed_duration_since(ended_at).num_hours() < ENDED_REPORT_HOURS)
        .then_some(EndedFreeze { marker, ended_at })
}

/// Forget an ended freeze: a freeze or a thaw since is the operator's
/// answer to it.
#[cfg(test)]
pub(crate) fn clear_ended() {
    clear_ended_beside(&marker_path());
}

/// [`clear_ended`] for `marker`.
pub(crate) fn clear_ended_beside(marker: &Path) {
    let _ = std::fs::remove_file(ended_file(marker));
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
        persistent: v
            .get("persistent")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
    })
}

/// Serialises tests that touch the REAL freeze-marker path.
///
/// `util::cache_dir()` is per-process under `cfg(test)`, not per-test,
/// and `clear_marker_if_own` matches on `pid == process::id()` — true
/// for every test in the binary. So any test driving `:freeze-deploys`,
/// `:thaw-deploys` or `:incident` shares one marker file with all the
/// others, running on parallel threads: one clears while another is
/// mid-round-trip.
///
/// They have always raced; the round-trip test added in 0.34.2 just made
/// it visible (12 failures in 40 runs of `cargo test freeze`). Every
/// test that writes or clears the real marker takes this.
#[cfg(test)]
pub(crate) static MARKER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Wall-clock epoch second at which the process holding `pid` started.
///
/// `kill(pid, 0)` proves a pid is *in use*; it says nothing about which
/// process is using it. Pids are allocated sequentially and wrap at
/// ~99999, so a marker left by a crashed session is reused within days
/// on a busy machine — and without this it reads as a live fleet freeze
/// refusing every write until someone deletes the file by hand. One was
/// found five days stale during a 2026-08-26 review.
///
/// `None` when the start time cannot be read, which the caller treats as
/// "assume owner" — see `marker_owner_is_live`.
#[cfg(target_os = "macos")]
fn process_start_epoch(pid: u32) -> Option<i64> {
    // SAFETY: `proc_pidinfo` fills a `proc_bsdinfo` we own and sized;
    // it reports how many bytes it wrote, which we check.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let want = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            want,
        );
        (got == want).then_some(info.pbi_start_tvsec as i64)
    }
}

/// Start time in clock ticks since boot, from a `/proc/<pid>/stat` body.
///
/// Field 22, counted from after the LAST `)`. The comm field is the
/// process name in parentheses and can itself contain spaces and
/// parentheses — `((sd-pam))` is a real one on any systemd box — so a
/// naive `split_whitespace().nth(21)` reads the wrong field for exactly
/// the processes whose names are unusual.
///
/// Gated on `linux` OR `test`, deliberately. Only Linux calls it, so on
/// a Mac it would be dead code in a release build — but a parser that
/// compiles only on the platform it parses for is a parser nobody can
/// test from a Mac, and this decides whether a fleet freeze is honoured.
/// The `test` arm buys the coverage without shipping unused code.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn start_ticks_from_stat(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Boot time in seconds since the epoch, from a `/proc/stat` body.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn btime_from_proc_stat(body: &str) -> Option<i64> {
    body.lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// Turn a boot time and a tick count into a wall-clock start time.
///
/// `hz` comes from `sysconf(_SC_CLK_TCK)`, which returns a `long` and is
/// documented to return -1 on error. Dividing by that would panic in
/// debug and produce nonsense in release, and the nonsense is the
/// dangerous half: a wrong start time makes `marker_owner_is_live`
/// misjudge a freeze.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn start_epoch_from(btime: i64, ticks: u64, hz: i64) -> Option<i64> {
    if hz <= 0 {
        return None;
    }
    Some(btime + (ticks / hz as u64) as i64)
}

#[cfg(target_os = "linux")]
fn process_start_epoch(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = start_ticks_from_stat(&stat)?;
    // SAFETY: `sysconf` takes a constant and returns a long.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let btime = btime_from_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)?;
    start_epoch_from(btime, ticks, hz)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_epoch(_pid: u32) -> Option<i64> {
    // No cheap probe: assume owner, which fails closed.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── /proc parsing (Linux start-time probe) ────────────────────────
    //
    // The whole of `process_start_epoch` showed as MISSED in the
    // 2026-08-27 sweep — fourteen mutants, including every arithmetic
    // operator. It ran only on Linux and only against the real `/proc`,
    // so nothing could reach it. These cover the parts that are pure.

    #[test]
    fn start_ticks_reads_field_22_after_the_last_paren() {
        // A normal line: `pid (comm) state ppid ...`, start time is the
        // 22nd field overall, i.e. the 20th after the comm.
        let f: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        let stat = format!("42 (bash) S {}", f.join(" "));
        // After `)` the fields are: S, then 1..30. nth(19) counts from
        // the field after `)`, so index 19 is "19".
        assert_eq!(start_ticks_from_stat(&stat), Some(19));
    }

    #[test]
    fn a_comm_containing_spaces_and_parens_does_not_shift_the_fields() {
        // `((sd-pam))` is a real process name on any systemd box, and
        // `Web Content` on a desktop. Counting from the LAST `)` is the
        // whole reason this is not a plain `split_whitespace`.
        let f: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        let plain = format!("42 (bash) S {}", f.join(" "));
        for comm in ["((sd-pam))", "(Web Content)", "(a b) c)", "(x (y) z)"] {
            let odd = format!("42 {comm} S {}", f.join(" "));
            assert_eq!(
                start_ticks_from_stat(&odd),
                start_ticks_from_stat(&plain),
                "comm {comm:?} shifted the field count"
            );
        }
    }

    #[test]
    fn malformed_stat_lines_yield_no_start_time() {
        // No paren, too few fields, a non-numeric tick count. Each must
        // be `None` — "assume owner" — rather than a wrong number, which
        // would make `marker_owner_is_live` misjudge the freeze.
        for bad in [
            "",
            "42 bash S 1 2 3",
            "42 (bash) S 1 2 3",
            "42 (bash) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 x",
        ] {
            assert_eq!(start_ticks_from_stat(bad), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn btime_is_read_from_its_own_line() {
        let body = "cpu  1 2 3\nintr 99\nbtime 1756000000\nprocesses 5\n";
        assert_eq!(btime_from_proc_stat(body), Some(1_756_000_000));
        // A line that merely CONTAINS btime is not the btime line.
        assert_eq!(btime_from_proc_stat("cpu 1\nnot_btime 5\n"), None);
        assert_eq!(btime_from_proc_stat(""), None);
        assert_eq!(btime_from_proc_stat("btime notanumber\n"), None);
    }

    #[test]
    fn start_epoch_converts_ticks_to_seconds_and_adds_boot_time() {
        // 100 Hz is the usual _SC_CLK_TCK.
        assert_eq!(start_epoch_from(1_000_000, 0, 100), Some(1_000_000));
        assert_eq!(start_epoch_from(1_000_000, 100, 100), Some(1_000_001));
        assert_eq!(
            start_epoch_from(1_000_000, 250, 100),
            Some(1_000_002),
            "truncates"
        );
    }

    #[test]
    fn a_bad_clock_tick_yields_no_start_time_rather_than_dividing_by_it() {
        // `sysconf` is documented to return -1 on error. Dividing by
        // that panics in debug and produces nonsense in release, and the
        // nonsense is the dangerous half.
        assert_eq!(start_epoch_from(1_000_000, 100, 0), None);
        assert_eq!(start_epoch_from(1_000_000, 100, -1), None);
    }

    /// A fresh marker path in a directory of its own: `freeze.json`,
    /// with `freeze.d/` and the ended record beside it.
    fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ebman-freeze-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("freeze.json")
    }

    fn done(marker: &Path) {
        let _ = std::fs::remove_dir_all(marker.parent().unwrap());
    }

    /// Every pid alive: the holders are judged by what is on disk.
    fn all_alive(marker: &Path) -> Vec<FreezeMarker> {
        read_all_with(marker, |_| true, |_| None)
    }

    #[test]
    fn a_session_marker_round_trips() {
        let m = fresh("rt");
        declare_session_at(&m, 4242, "the \"big\" one\nline2", true).unwrap();
        let got = parse_file(&session_file(&m, 4242)).expect("its own file");
        assert_eq!(got.pid, 4242);
        assert_eq!(got.reason, "the \"big\" one\nline2");
        assert!(got.incident && !got.persistent);
        assert_eq!(got.remedy(), ":incident END");
        done(&m);
    }

    #[test]
    fn a_dead_sessions_freeze_is_reaped_and_recorded() {
        let m = fresh("dead");
        declare_session_at(&m, 4242, "incident: db failover", true).unwrap();
        assert!(read_all_with(&m, |_| false, |_| None).is_empty());
        assert!(!session_file(&m, 4242).exists(), "removed by the reader");
        let ended = read_ended_at(&ended_file(&m), chrono::Utc::now()).expect("recorded");
        assert_eq!(ended.marker.reason, "incident: db failover");
        assert!(
            ended.message().contains("ebman freeze"),
            "{}",
            ended.message()
        );
        let later = chrono::Utc::now() + chrono::Duration::hours(ENDED_REPORT_HOURS);
        assert!(
            read_ended_at(&ended_file(&m), later).is_none(),
            "reported for a day"
        );
        done(&m);
    }

    #[test]
    fn a_live_sessions_freeze_holds_and_is_not_reaped() {
        let m = fresh("live");
        declare_session_at(&m, 4242, "deploy freeze", false).unwrap();
        assert_eq!(all_alive(&m).len(), 1);
        assert!(session_file(&m, 4242).exists());
        assert!(read_ended_at(&ended_file(&m), chrono::Utc::now()).is_none());
        done(&m);
    }

    /// The reviewers' case: `ebman freeze`, then a TUI's `:incident
    /// START`, then its END (or exit). One shared file let the session
    /// replace the persistent marker and then delete it.
    #[test]
    fn a_sessions_end_never_lifts_a_persistent_freeze() {
        let m = fresh("c1");
        declare_persistent_at(&m, "change freeze", false).unwrap();
        declare_session_at(&m, 4242, "incident: db", true).unwrap();
        clear_session_at(&m, 4242);
        let held = all_alive(&m);
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].persistent && held[0].reason == "change freeze");
        done(&m);
    }

    /// And the other way round: `ebman freeze` over a live session, then
    /// `ebman thaw`, must leave the session's freeze in force.
    #[test]
    fn a_thaw_never_lifts_a_sessions_freeze() {
        let m = fresh("i1");
        declare_session_at(&m, 4242, "incident: db", true).unwrap();
        declare_persistent_at(&m, "change freeze", false).unwrap();
        assert!(matches!(
            thaw_persistent_with(&m, |_| true, |_| None),
            Thaw::Lifted(_)
        ));
        let held = all_alive(&m);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].pid, 4242);
        assert!(matches!(
            thaw_persistent_with(&m, |_| true, |_| None),
            Thaw::HeldBySession(s) if s.pid == 4242
        ));
        assert!(session_file(&m, 4242).exists(), "that session's to lift");
        done(&m);
    }

    #[test]
    fn one_session_ending_leaves_anothers_freeze() {
        let m = fresh("two");
        declare_session_at(&m, 111, "a", false).unwrap();
        declare_session_at(&m, 222, "b", false).unwrap();
        clear_session_at(&m, 222);
        let held = all_alive(&m);
        assert_eq!(held.iter().map(|h| h.pid).collect::<Vec<_>>(), vec![111]);
        done(&m);
    }

    #[test]
    fn thaw_reports_a_lift_only_when_it_made_one() {
        let m = fresh("thaw");
        assert_eq!(thaw_persistent_with(&m, |_| true, |_| None), Thaw::Nothing);
        declare_persistent_at(&m, "x", false).unwrap();
        assert!(matches!(
            thaw_persistent_with(&m, |_| true, |_| None),
            Thaw::Lifted(_)
        ));
        assert!(!persistent_file(&m).exists());
        assert_eq!(thaw_persistent_with(&m, |_| true, |_| None), Thaw::Nothing);
        done(&m);
    }

    /// ebman before 0.47 reads only `freeze.json`. It mirrors a live
    /// holder, parseable by the old reader, and goes when none is left.
    #[test]
    fn the_legacy_file_mirrors_a_live_holder_for_older_readers() {
        let m = fresh("mirror");
        declare_session_at(&m, std::process::id(), "s", false).unwrap();
        let legacy = parse_file(&m).expect("mirrored");
        assert_eq!(legacy.pid, std::process::id());
        declare_persistent_at(&m, "p", false).unwrap();
        let legacy = parse_file(&m).expect("still mirrored");
        assert!(
            legacy.persistent && legacy.pid == PERSISTENT_PID,
            "persistent first"
        );
        assert_eq!(
            all_alive(&m).len(),
            2,
            "a mirror is not a holder of its own"
        );
        thaw_persistent_with(&m, |_| true, |_| None);
        assert_eq!(parse_file(&m).map(|l| l.pid), Some(std::process::id()));
        clear_session_at(&m, std::process::id());
        assert!(!m.exists(), "no holder, no mirror");
        done(&m);
    }

    /// A `freeze.json` an older TUI wrote (no mirror mark) is that
    /// session's freeze: honoured while it lives, never overwritten by a
    /// mirror, reaped when it dies.
    #[test]
    fn an_older_tuis_marker_is_a_holder_of_its_own() {
        let m = fresh("old");
        crate::util::write_secure(
            &m,
            br#"{"pid":777,"reason":"old tui","incident":false,"at":"2026-10-23T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            all_alive(&m).iter().map(|h| h.pid).collect::<Vec<_>>(),
            vec![777]
        );
        // Written directly and synced with a fake probe: declaring would
        // judge pid 777 with the real one, which is alive on some
        // machines and not on others.
        write_marker_full(&session_file(&m, 4242), 4242, "new", false, false, false).unwrap();
        sync_legacy_with(&m, |_| true, |_| None);
        assert_eq!(parse_file(&m).map(|l| l.pid), Some(777), "not clobbered");
        assert_eq!(all_alive(&m).len(), 2);
        assert!(read_all_with(&m, |p| p != 777, |_| None)
            .iter()
            .all(|h| h.pid != 777));
        assert!(
            !m.exists() || parse_file(&m).is_some_and(|l| l.pid != 777),
            "reaped"
        );
        done(&m);
    }

    /// A reader that judged a session stale must not delete it if the
    /// file was rewritten with a live freeze in the meantime.
    #[test]
    fn reaping_does_not_delete_a_freshly_rewritten_marker() {
        let m = fresh("toctou");
        declare_session_at(&m, 4242, "dead session", false).unwrap();
        let file = session_file(&m, 4242);
        let rewritten = std::cell::Cell::new(false);
        let held = read_all_with(
            &m,
            |_pid| {
                if !rewritten.get() {
                    write_marker_full(&file, 4242, "rewritten live", true, false, false).unwrap();
                    rewritten.set(true);
                }
                false
            },
            |_| None,
        );
        assert!(held.is_empty(), "the stale read is not returned");
        assert_eq!(
            parse_file(&file).map(|f| f.reason),
            Some("rewritten live".into())
        );
        done(&m);
    }

    #[test]
    fn a_corrupt_holder_never_blocks() {
        let m = fresh("corrupt");
        std::fs::create_dir_all(holders_dir(&m)).unwrap();
        crate::util::write_secure(&persistent_file(&m), b"not json").unwrap();
        crate::util::write_secure(&session_file(&m, 1), b"{").unwrap();
        assert!(all_alive(&m).is_empty());
        done(&m);
    }

    #[test]
    fn a_persistent_freeze_holds_whatever_any_pid_says() {
        let m = fresh("persistent");
        declare_persistent_at(&m, "change freeze", false).unwrap();
        let held = read_all_with(&m, |_| false, |_| None);
        assert!(held.len() == 1 && held[0].persistent, "{held:?}");
        assert_eq!(held[0].remedy(), "ebman thaw");
        let msg = refusal_message(&held[0]);
        assert!(
            msg.contains("ebman thaw") && msg.contains("change freeze"),
            "{msg}"
        );
        assert!(!msg.contains("pid"), "no session to name: {msg}");
        done(&m);
    }

    /// A reused pid must not read as a live freeze.
    ///
    /// `kill(pid, 0)` proves a pid is in use, not *who* is using it.
    /// Pids wrap at ~99999 and are handed out sequentially, so a marker
    /// from a crashed session gets adopted by an unrelated process
    /// within days — and before this, that read as a fleet freeze
    /// refusing every write across TUI, CLI and MCP until the file was
    /// deleted by hand. One was found five days stale on a real machine.
    #[test]
    fn a_reused_pid_does_not_hold_the_freeze() {
        // Marker written at T. The process now holding the pid started
        // well after T, so it cannot be the one that wrote it.
        let at = 1_700_000_000;
        assert!(
            !marker_owner_is_live(true, Some(at + 86_400), Some(at)),
            "a process that started a day after the marker cannot have \
             written it"
        );

        // The real owner always starts BEFORE it writes its own marker.
        assert!(marker_owner_is_live(true, Some(at - 60), Some(at)));
        assert!(
            marker_owner_is_live(true, Some(at), Some(at)),
            "same second"
        );

        // Dead pid is stale regardless of start time.
        assert!(!marker_owner_is_live(false, Some(at - 60), Some(at)));
        assert!(!marker_owner_is_live(false, None, Some(at)));
    }

    /// Every uncertain case assumes the marker is still owned, because
    /// the failure directions are not symmetric: a phantom freeze
    /// refuses writes and is fixed by deleting a file, while a lifted
    /// freeze lets writes through during a declared incident.
    #[test]
    fn an_unreadable_start_time_fails_closed() {
        let at = 1_700_000_000;
        assert!(
            marker_owner_is_live(true, None, Some(at)),
            "unknown start time must not lift the freeze"
        );

        // Clock wobble must not lift it either. An NTP step backwards
        // makes a live session look like it started after its own
        // marker; the slack absorbs that.
        assert!(
            marker_owner_is_live(true, Some(at + START_SLACK_SECS - 1), Some(at)),
            "a start time inside the slack window is still the owner"
        );
        assert!(
            !marker_owner_is_live(true, Some(at + START_SLACK_SECS + 1), Some(at)),
            "past the slack it is reuse"
        );
    }

    #[test]
    fn an_unreadable_marker_timestamp_keeps_the_freeze_rather_than_lifting_it() {
        // A truncated or corrupt `at` field means reuse cannot be
        // judged. The safe answer is "assume owner": a phantom freeze
        // refuses writes and the operator deletes the file, whereas
        // lifting it lets deploys through during a declared incident.
        //
        // This was inverted. `at` fell back to the sentinel `i64::MAX`,
        // and `i64::MAX + START_SLACK_SECS` overflows — wrapping to a
        // large negative that every real start time compares greater
        // than, so the marker was judged reused and DELETED. Release
        // builds lifted the freeze silently; debug builds panicked.
        let live_process_start = 1_756_000_000_i64;
        assert!(
            marker_owner_is_live(true, Some(live_process_start), None),
            "an unjudgeable marker must keep the freeze, not lift it"
        );
        // A dead pid still lifts it — "cannot judge the timestamp" must
        // not resurrect a freeze whose owner is gone.
        assert!(!marker_owner_is_live(false, Some(live_process_start), None));
    }

    /// The probe reports something sane for this very process — without
    /// which every case above is reasoning about a function that always
    /// returns None.
    #[test]
    fn the_start_time_probe_works_on_this_process() {
        let start = super::process_start_epoch(std::process::id());
        let start = start.expect(
            "this platform must report a process start time, or the \
             reuse check silently degrades to the old behaviour",
        );
        let now = chrono::Utc::now().timestamp();
        assert!(
            start <= now && start > now - 86_400 * 365,
            "start {start} is not a plausible epoch second near {now}"
        );
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
            persistent: false,
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

    /// The three thin wrappers — `marker_path`, `write_marker`,
    /// `read_active` — round-trip a real marker.
    ///
    /// `cargo mutants` reported all three as survivors, for the same
    /// reason `pid_alive` was one: the parameterised cores
    /// (`write_marker_at`, `read_active_with`, `clear_if_pid_at`) are
    /// well tested against explicit paths and fake probes, while the
    /// wrappers that supply the REAL path and the REAL probe are
    /// exercised by nothing.
    ///
    /// `write_marker`'s own doc comment names the stakes: a silently
    /// absent marker "fails OPEN (agent + CLI writes are NOT blocked)
    /// while the operator believes the fleet is frozen". Each survivor
    /// produces exactly that — `write_marker -> Ok(())` reports success
    /// having written nothing, and `read_active -> None` never sees a
    /// marker that is there.
    ///
    /// Safe against the shared test cache dir: `cache_dir()` is
    /// per-process under `cfg(test)`, `:freeze-deploys` tests already
    /// write real markers, and this clears up after itself.
    #[test]
    fn a_written_marker_is_readable_through_the_real_path() {
        // Exclusive access to the shared marker path; see MARKER_LOCK.
        let _guard = super::MARKER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::clear_marker_if_own();

        super::write_marker("incident #4321", true).expect("marker must be written");

        let path = super::marker_path();
        assert!(
            path.ends_with("freeze.json"),
            "the marker must land at the real path, not a default: {}",
            path.display()
        );
        assert!(
            session_file(&path, std::process::id()).exists(),
            "nothing was written beside {}",
            path.display()
        );

        let found = super::read_active().expect(
            "a marker written by THIS live process must read back as active — \
             otherwise a freeze silently fails open while the operator \
             believes the fleet is frozen",
        );
        assert_eq!(found.reason, "incident #4321");
        assert!(
            found.incident,
            "the incident flag must survive the round trip"
        );
        assert_eq!(found.pid, std::process::id());

        super::clear_marker_if_own();
        assert!(
            super::read_active().is_none(),
            "clearing our own marker must lift the freeze"
        );
    }

    /// The compat half of the design: a reader that does not know
    /// `persistent` judges the marker by its pid alone, and pid 1 is
    /// alive and started before any marker, so it still refuses writes.
    #[test]
    fn a_reader_blind_to_persistent_still_holds_the_freeze() {
        let marker_at = chrono::Utc::now().timestamp();
        assert!(marker_owner_is_live(
            pid_alive(PERSISTENT_PID),
            process_start_epoch(PERSISTENT_PID),
            Some(marker_at)
        ));
    }

    proptest::proptest! {
        /// Every write path reads the marker; a corrupt one must read as
        /// no freeze or a freeze, never a panic in a write gate.
        #[test]
        fn parse_marker_never_panics(s in "(?s).{0,200}") {
            let _ = parse_marker(&s);
        }
    }
}
