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
    read_active_with(&marker_path(), pid_alive, process_start_epoch)
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

/// Testable core: liveness and start-time probes injected.
fn read_active_with(
    path: &Path,
    alive: impl Fn(u32) -> bool,
    start_epoch: impl Fn(u32) -> Option<i64>,
) -> Option<FreezeMarker> {
    let m = parse_file(path)?;
    // `at` is RFC3339 text on disk. An unparseable timestamp means we
    // cannot judge reuse, so fall back to "assume owner" — closed.
    let at_epoch = chrono::DateTime::parse_from_rfc3339(&m.at)
        .map(|t| t.timestamp())
        .ok();
    if marker_owner_is_live(alive(m.pid), start_epoch(m.pid), at_epoch) {
        return Some(m);
    }
    // Dead pid, or the pid was reused → stale. Re-read immediately
    // before removing and only
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
        assert!(read_active_with(&p, |_| false, |_| None).is_none());
        assert!(!p.exists(), "stale marker must be removed by the reader");
    }

    #[test]
    fn live_pid_marker_is_active() {
        let p = tmp("live");
        write_marker_at(&p, 4242, "deploy freeze", false).unwrap();
        let m = read_active_with(&p, |_| true, |_| None).expect("active");
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
        let result = read_active_with(
            &p,
            |_pid| {
                if !overwritten.get() {
                    // First call: rewrite the file with a "live" pid.
                    write_marker_at(&p, 111, "new live freeze", true).unwrap();
                    overwritten.set(true);
                }
                false // report the ORIGINAL pid as dead
            },
            |_| None,
        );
        assert!(
            result.is_none(),
            "original dead marker not returned as active"
        );
        assert!(p.exists(), "the freshly-written live marker must survive");
        let m = parse_file(&p).unwrap();
        assert_eq!(m.pid, 111, "live marker intact");
        let _ = std::fs::remove_file(&p);
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
    fn corrupt_marker_never_blocks() {
        let p = tmp("corrupt");
        let _ = crate::util::write_secure(&p, b"not json at all");
        assert!(read_active_with(&p, |_| true, |_| None).is_none());
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
        assert!(path.exists(), "nothing was written to {}", path.display());

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
}
