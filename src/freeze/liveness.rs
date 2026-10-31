//! Is a holder's process still the one that declared it? The pid and
//! start-time probes, and the pure judgement over them.

use super::*;

/// How "is this holder's process still the one that declared it?" is
/// answered. The real probes in production; fakes in a test, so a
/// fixture's pid is not judged against whatever happens to run on the
/// machine (pid 777 was alive on a dev box and dead on CI).
#[derive(Clone, Copy)]
pub(crate) struct Liveness {
    pub(super) alive: fn(u32) -> bool,
    pub(super) start_epoch: fn(u32) -> Option<i64>,
}

impl Liveness {
    pub(crate) const REAL: Liveness = Liveness {
        alive: pid_alive,
        start_epoch: process_start_epoch,
    };

    /// Every pid alive, no start time known: holders are judged by what
    /// is on disk.
    #[cfg(test)]
    pub(crate) const ALL_ALIVE: Liveness = Liveness {
        alive: |_| true,
        start_epoch: |_| None,
    };

    /// No pid alive: every session holder is stale.
    #[cfg(test)]
    pub(crate) const NONE_ALIVE: Liveness = Liveness {
        alive: |_| false,
        start_epoch: |_| None,
    };

    /// Does `m`'s owner still hold it? Always, for a persistent freeze.
    pub(super) fn holds(&self, m: &FreezeMarker) -> bool {
        if m.persistent {
            return true;
        }
        // `at` is RFC3339 text on disk. An unparseable timestamp means we
        // cannot judge reuse, so fall back to "assume owner" — closed.
        let at_epoch = chrono::DateTime::parse_from_rfc3339(&m.at)
            .map(|t| t.timestamp())
            .ok();
        marker_owner_is_live((self.alive)(m.pid), (self.start_epoch)(m.pid), at_epoch)
    }
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
pub(super) const START_SLACK_SECS: i64 = 300;

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
pub(super) fn marker_owner_is_live(
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

#[cfg(unix)]
pub(super) fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0): signal 0 performs the permission/existence check
    // without delivering anything. EPERM still means "exists".
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(super) fn pid_alive(_pid: u32) -> bool {
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
pub(super) fn process_start_epoch(pid: u32) -> Option<i64> {
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
pub(super) fn process_start_epoch(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = start_ticks_from_stat(&stat)?;
    // SAFETY: `sysconf` takes a constant and returns a long.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let btime = btime_from_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)?;
    start_epoch_from(btime, ticks, hz)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn process_start_epoch(_pid: u32) -> Option<i64> {
    // No cheap probe: assume owner, which fails closed.
    None
}
