//! The store every surface holds: holder files under `freeze.d/`, the
//! legacy mirror, declare and thaw.

use super::*;

/// One file on disk that may hold a freeze, parsed once.
#[derive(Debug, Clone)]
pub(super) struct Holder {
    pub(super) path: PathBuf,
    pub(super) marker: FreezeMarker,
    /// The legacy `freeze.json` rather than a file in `freeze.d/`.
    pub(super) legacy: bool,
    /// A legacy file written as a mirror: never a holder of its own.
    pub(super) mirror: bool,
}

/// Where the freeze lives: `freeze.d/` holder files, the legacy
/// `freeze.json` mirror beside them, and the ended record. A value, so
/// every surface reaches the freeze one way, through the store it holds:
/// a TUI's `App`, an MCP `Server`, a CLI write's `WriteEnv`. In production
/// that is [`FreezeStore::real`]; under test each has
/// [`FreezeStore::isolated`].
#[derive(Clone)]
pub(crate) struct FreezeStore {
    /// The legacy marker, `freeze.json`. The holders' directory and the
    /// ended record are derived from it.
    pub(super) marker: PathBuf,
    pub(super) liveness: Liveness,
}

impl std::fmt::Debug for FreezeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FreezeStore")
            .field("marker", &self.marker)
            .finish()
    }
}

impl FreezeStore {
    /// The store every process shares, under the cache directory.
    pub(crate) fn real() -> Self {
        Self::at(crate::util::cache_dir().join("freeze.json"))
    }

    /// A store at `marker` (the legacy file's path), judged by the real
    /// probes.
    pub(crate) fn at(marker: PathBuf) -> Self {
        Self {
            marker,
            liveness: Liveness::REAL,
        }
    }

    /// A store nothing else shares, in this process or another: what a
    /// test `App`, MCP server or CLI write body reads, so no test sees a
    /// freeze another declared, and what a `--demo` App reads, so demo
    /// neither sees nor sets a real freeze. Named by pid as well as by a
    /// counter: the counter restarts in each process, and two `--demo`
    /// sessions shared `freeze-isolated-0.json`.
    ///
    /// In the OS temp dir, not the cache: a demo `:freeze-deploys` left
    /// its `.d/` directory in `~/.cache/ebman` on thaw, and every file
    /// after a crash, one name per session, which nothing ever reads
    /// again or removes.
    ///
    /// One directory per process, and the first isolated store a
    /// process makes removes those of processes no longer running: a
    /// `--demo` session's records were left in the temp dir for good.
    pub(crate) fn isolated() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static SWEPT: std::sync::Once = std::sync::Once::new();
        let tmp = std::env::temp_dir();
        let pid = std::process::id();
        SWEPT.call_once(|| sweep_isolated(&tmp, pid, pid_alive));
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = tmp.join(format!("{ISOLATED_PREFIX}{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        Self::at(dir.join(format!("{n}.json")))
    }

    /// [`at`](Self::at), judged by `liveness`.
    #[cfg(test)]
    pub(crate) fn with_liveness(marker: PathBuf, liveness: Liveness) -> Self {
        Self { marker, liveness }
    }

    /// The legacy marker's path.
    #[cfg(test)]
    pub(crate) fn marker(&self) -> &Path {
        &self.marker
    }

    pub(super) fn holders_dir(&self) -> PathBuf {
        self.marker.with_extension("d")
    }

    pub(super) fn persistent_file(&self) -> PathBuf {
        self.holders_dir().join("persistent.json")
    }

    pub(super) fn session_file(&self, pid: u32) -> PathBuf {
        self.holders_dir().join(format!("session-{pid}.json"))
    }

    pub(super) fn ended_file(&self) -> PathBuf {
        self.marker.with_extension("ended.json")
    }

    pub(super) fn thawed_file(&self) -> PathBuf {
        self.marker.with_extension("thawed.json")
    }

    /// Declare session `pid`'s freeze. Writes its own holder file only,
    /// so no other holder is replaced. `Err` when it could not be
    /// written — the caller MUST surface that, because an absent marker
    /// fails OPEN (agent + CLI writes are NOT blocked) while the
    /// operator believes the fleet is frozen.
    pub(crate) fn declare_session(
        &self,
        pid: u32,
        reason: &str,
        incident: bool,
    ) -> std::io::Result<()> {
        write_marker_full(
            &self.session_file(pid),
            pid,
            reason,
            incident,
            false,
            false,
            None,
        )?;
        self.sync_legacy(&self.holders());
        self.clear_ended();
        self.clear_thawed();
        Ok(())
    }

    /// Declare a persistent freeze (`ebman freeze`): no session owns it.
    /// Same failure contract as [`declare_session`](Self::declare_session).
    pub(crate) fn declare_persistent(&self, reason: &str, incident: bool) -> std::io::Result<()> {
        self.write_persistent(reason, incident, None)
    }

    /// [`declare_persistent`](Self::declare_persistent) for MCP
    /// `freeze_fleet`, recording which client declared it, and only if
    /// no persistent freeze stands: an agent's declare must never
    /// rewrite the operator's reason, incident flag and time. Created
    /// exclusively, so an `ebman freeze` landing between the caller's
    /// check and this write is kept, not overwritten: the answer is then
    /// [`AgentDeclare::Held`] with that freeze.
    pub(crate) fn declare_persistent_by_agent(
        &self,
        reason: &str,
        incident: bool,
        agent: &str,
    ) -> std::io::Result<AgentDeclare> {
        // The operator's thaw holds an agent off: enforced here, by the
        // store, not only by the tool asking first.
        if let Some(t) = self.recently_thawed() {
            return Ok(AgentDeclare::Thawed(t));
        }
        let file = self.persistent_file();
        sweep_stale_temps(&self.holders_dir(), std::time::SystemTime::now());
        // What stands at `file` when the exclusive link finds one: a
        // freeze (kept), nothing any more (a thaw lifted it in between:
        // an error to retry, never a non-exclusive write), or bytes that
        // are no marker.
        let standing = || -> std::io::Result<Result<FreezeMarker, Vec<u8>>> {
            match std::fs::read(&file) {
                Ok(bytes) => Ok(std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(parse_marker)
                    .ok_or(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(std::io::Error::other(
                    "a freeze was lifted while this one was declared; try again",
                )),
                Err(e) => Err(e),
            }
        };
        if !write_marker_exclusive(&file, reason, incident, agent)? {
            let corrupt = match standing()? {
                Ok(m) => return Ok(AgentDeclare::Held(m)),
                Err(bytes) => bytes,
            };
            // A corrupt marker blocks nothing, so it must not stop a
            // freeze either. Removed only if it is still the file just
            // read, then linked again, exclusively: replacing it in place
            // overwrote an `ebman freeze` landing in between.
            if std::fs::read(&file).ok().as_deref() == Some(corrupt.as_slice()) {
                let _ = std::fs::remove_file(&file);
            }
            if !write_marker_exclusive(&file, reason, incident, agent)? {
                return match standing()? {
                    Ok(m) => Ok(AgentDeclare::Held(m)),
                    Err(_) => Err(std::io::Error::other(
                        "the freeze marker is unreadable and could not be replaced",
                    )),
                };
            }
        }
        // A thaw that landed while this was being written: it lifted
        // what was there before, so the hold it records is about this
        // very moment. Take this one back rather than stand over it.
        if let Some(t) = self.recently_thawed() {
            if parse_file(&file)
                .is_some_and(|m| m.agent.as_deref() == Some(agent) && m.reason == reason)
            {
                let _ = std::fs::remove_file(&file);
            }
            self.sync_legacy(&self.holders());
            return Ok(AgentDeclare::Thawed(t));
        }
        self.sync_legacy(&self.holders());
        self.clear_ended();
        Ok(AgentDeclare::Declared)
    }

    fn write_persistent(
        &self,
        reason: &str,
        incident: bool,
        agent: Option<&str>,
    ) -> std::io::Result<()> {
        write_marker_full(
            &self.persistent_file(),
            PERSISTENT_PID,
            reason,
            incident,
            true,
            false,
            agent,
        )?;
        self.sync_legacy(&self.holders());
        self.clear_ended();
        // An operator's freeze is a new ruling, so a thaw no longer
        // holds; an agent's must not erase the one that binds it.
        if agent.is_none() {
            self.clear_thawed();
        }
        Ok(())
    }

    /// End session `pid`'s freeze, and only that: a session's thaw or
    /// exit must not clear another session's freeze or a persistent one.
    pub(crate) fn clear_session(&self, pid: u32) {
        let _ = std::fs::remove_file(self.session_file(pid));
        self.sync_legacy(&self.holders());
    }

    /// `ebman thaw` / `:thaw-deploys` on a persistent freeze: lift it.
    /// Never a live session's freeze, which only that session (or
    /// deleting its file) may end.
    pub(crate) fn thaw(&self) -> Thaw {
        let file = self.persistent_file();
        // The hold is recorded BEFORE the marker goes: removed first, there
        // was an instant with neither, and an agent freezing in it stood
        // over the operator's thaw. A thaw that then fails to remove the
        // marker leaves a record that binds nothing while a freeze stands.
        // Lifted only if this call removed it: a thaw racing another must
        // not report, and audit, a lift it did not make.
        let lifted = parse_file(&file).filter(|marker| {
            write_thawed_at(&self.thawed_file(), marker, chrono::Utc::now());
            std::fs::remove_file(&file).is_ok()
        });
        let still_held = self.all().into_iter().next();
        match lifted {
            Some(marker) => {
                self.clear_ended();
                Thaw::Lifted { marker, still_held }
            }
            None => match still_held {
                Some(m) => Thaw::HeldBySession(m),
                None => Thaw::Nothing,
            },
        }
    }

    /// Every freeze in force, the persistent one first, then sessions in
    /// the order they were declared. A session file whose process is
    /// gone is removed as it is read and recorded as an ended freeze,
    /// and the legacy mirror is put right whenever it does not name a
    /// live holder (a reap, an older TUI's clear, a mirror that names a
    /// dead pid).
    pub(crate) fn all(&self) -> Vec<FreezeMarker> {
        let mut live = Vec::new();
        for h in self.holders() {
            if h.mirror {
                continue;
            }
            if self.liveness.holds(&h.marker) {
                live.push(h);
            } else {
                self.reap(&h);
            }
        }
        self.sync_legacy(&live);
        live.into_iter().map(|h| h.marker).collect()
    }

    /// A freeze in force, if any: the persistent one first.
    pub(crate) fn active(&self) -> Option<FreezeMarker> {
        self.all().into_iter().next()
    }

    /// A session freeze that ended with its session in the last day.
    pub(crate) fn ended(&self) -> Option<EndedFreeze> {
        read_ended_at(&self.ended_file(), chrono::Utc::now())
    }

    /// A persistent freeze lifted in the last [`THAW_HOLDS_MINS`]: while
    /// it holds, `freeze_fleet` refuses, so an agent cannot reverse the
    /// operator's thaw by re-declaring what they just lifted.
    pub(crate) fn recently_thawed(&self) -> Option<ThawedFreeze> {
        read_thawed_at(&self.thawed_file(), chrono::Utc::now())
    }

    /// Forget a thaw: an operator's freeze since is a new ruling.
    fn clear_thawed(&self) {
        let _ = std::fs::remove_file(self.thawed_file());
    }

    /// End a thaw's hold now, as 30 minutes passing would.
    #[cfg(test)]
    pub(crate) fn expire_thaw_for_test(&self) {
        self.clear_thawed();
    }

    /// Record a thaw at `thawed_at`, as [`thaw`](Self::thaw) does now.
    #[cfg(test)]
    pub(crate) fn write_test_thawed(
        &self,
        m: &FreezeMarker,
        thawed_at: chrono::DateTime<chrono::Utc>,
    ) {
        write_thawed_at(&self.thawed_file(), m, thawed_at);
    }

    /// Forget an ended freeze: a freeze or a thaw since is the
    /// operator's answer to it.
    pub(crate) fn clear_ended(&self) {
        let _ = std::fs::remove_file(self.ended_file());
    }

    /// A holder with any owner, for a test standing in for another
    /// session (`pid`) or for `ebman freeze` (`persistent`).
    #[cfg(test)]
    pub(crate) fn write_test_marker(&self, pid: u32, reason: &str, persistent: bool) {
        let written = if persistent {
            self.declare_persistent(reason, false)
        } else {
            self.declare_session(pid, reason, false)
        };
        written.expect("test marker written");
    }

    /// Record an ended freeze, as a reader that found it stale does.
    #[cfg(test)]
    pub(crate) fn write_test_ended(&self, reason: &str) {
        let m = FreezeMarker {
            pid: 4242,
            reason: reason.into(),
            incident: true,
            at: chrono::Utc::now().to_rfc3339(),
            persistent: false,
            agent: None,
        };
        write_ended_at(&self.ended_file(), &m);
    }

    /// Every file that may hold a freeze, parsed once: the persistent
    /// holder, the sessions in declaration order, then the legacy file.
    pub(super) fn holders(&self) -> Vec<Holder> {
        let dir_holder = |path: PathBuf| {
            parse_file(&path).map(|marker| Holder {
                path,
                marker,
                legacy: false,
                mirror: false,
            })
        };
        let mut out: Vec<Holder> = dir_holder(self.persistent_file()).into_iter().collect();
        let mut sessions: Vec<Holder> = std::fs::read_dir(self.holders_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("session-") && n.ends_with(".json"))
            })
            .filter_map(dir_holder)
            .collect();
        sessions.sort_by(|a, b| a.marker.at.cmp(&b.marker.at).then(a.path.cmp(&b.path)));
        out.extend(sessions);
        if let Ok(text) = std::fs::read_to_string(&self.marker) {
            if let Some(marker) = parse_marker(&text) {
                out.push(Holder {
                    path: self.marker.clone(),
                    marker,
                    legacy: true,
                    mirror: is_mirror_text(&text),
                });
            }
        }
        out
    }

    /// Remove a holder found stale and record it as ended, but only if
    /// the file is still the one judged: between the read and here it
    /// could have been rewritten with a live freeze, and a blind remove
    /// would silently drop it.
    pub(super) fn reap(&self, h: &Holder) {
        if parse_file(&h.path).as_ref() == Some(&h.marker) {
            let _ = std::fs::remove_file(&h.path);
            // Said, not swallowed: a freeze that ended because its
            // session did is news to whoever looks next.
            write_ended_at(&self.ended_file(), &h.marker);
        }
    }

    /// Keep the legacy `freeze.json` naming a LIVE holder, for ebman
    /// older than 0.47, which reads only that file and removes it when
    /// its pid is dead: a mirror naming a dead session failed open for
    /// those readers while a live freeze held. Left alone when it is an
    /// older TUI's own marker (no mirror mark) and that TUI lives.
    /// Judged without reaping, so mirroring never ends a freeze.
    pub(super) fn sync_legacy(&self, holders: &[Holder]) {
        let live = |h: &&Holder| self.liveness.holds(&h.marker);
        if holders
            .iter()
            .filter(|h| h.legacy && !h.mirror)
            .any(|h| live(&h))
        {
            return;
        }
        let current = parse_file(&self.marker);
        // Read again at the moment of acting: an older TUI may have
        // written its own marker since `holders` was read. That file is
        // a real freeze with no `freeze.d/` twin to restore it, so the
        // mirror never removes or overwrites it while its holder lives.
        let an_older_tui_holds = || {
            parse_file(&self.marker)
                .is_some_and(|m| !is_mirror(&self.marker) && self.liveness.holds(&m))
        };
        match holders.iter().filter(|h| !h.legacy).find(live) {
            Some(h) => {
                let m = &h.marker;
                let up_to_date = current.as_ref().is_some_and(|c| {
                    c.pid == m.pid
                        && c.reason == m.reason
                        && c.persistent == m.persistent
                        && c.agent == m.agent
                }) && is_mirror(&self.marker);
                if !up_to_date && !an_older_tui_holds() {
                    let _ = write_marker_full(
                        &self.marker,
                        m.pid,
                        &m.reason,
                        m.incident,
                        m.persistent,
                        true,
                        m.agent.as_deref(),
                    );
                }
            }
            None => {
                if current.is_some() && !an_older_tui_holds() {
                    let _ = std::fs::remove_file(&self.marker);
                }
            }
        }
    }
}

/// What an explicit thaw found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Thaw {
    /// A persistent freeze, now lifted. `still_held` is a live session's
    /// freeze, if one remains: writes are still refused, and saying the
    /// fleet is thawed would be wrong.
    Lifted {
        marker: FreezeMarker,
        still_held: Option<FreezeMarker>,
    },
    /// No persistent freeze, but a live TUI session holds one: that
    /// session's to lift.
    HeldBySession(FreezeMarker),
    /// No freeze in force.
    Nothing,
}

/// What an agent's declare did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentDeclare {
    Declared,
    /// A persistent freeze already stood, and stands unchanged.
    Held(FreezeMarker),
    /// The operator lifted a freeze in the last [`THAW_HOLDS_MINS`]:
    /// nothing was declared.
    Thawed(ThawedFreeze),
}

/// What every isolated store's files are named under the temp dir,
/// followed by the pid of the process that made them.
pub(super) const ISOLATED_PREFIX: &str = "ebman-freeze-isolated-";

/// Remove the isolated stores under `tmp` whose process is gone: a
/// directory per process now, files named `…-PID-N.*` before. One whose
/// pid is `me` or `alive` is kept; a pid reused by another process keeps
/// its files until a later sweep, which costs a few bytes.
pub(super) fn sweep_isolated(tmp: &Path, me: u32, alive: fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(ISOLATED_PREFIX)) else {
            continue;
        };
        let digits = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        let Ok(pid) = digits.parse::<u32>() else {
            continue;
        };
        if pid == me || alive(pid) {
            continue;
        }
        let path = e.path();
        let _ = match e.file_type() {
            Ok(t) if t.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}
