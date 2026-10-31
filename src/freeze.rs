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
//! ebman older than 0.47, which reads only that file, and every read
//! puts it right. A mirror is marked so; one without the mark was
//! written by an older TUI and is a holder in its own right. Everything
//! goes through a [`FreezeStore`] the surface holds: a TUI's `App`, an
//! MCP `Server`, a CLI write's `WriteEnv`. Deleting `freeze.d/` and `freeze.json` is
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
const PERSISTENT_PID: u32 = 1;

/// How "is this holder's process still the one that declared it?" is
/// answered. The real probes in production; fakes in a test, so a
/// fixture's pid is not judged against whatever happens to run on the
/// machine (pid 777 was alive on a dev box and dead on CI).
#[derive(Clone, Copy)]
pub(crate) struct Liveness {
    alive: fn(u32) -> bool,
    start_epoch: fn(u32) -> Option<i64>,
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
    fn holds(&self, m: &FreezeMarker) -> bool {
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

/// One file on disk that may hold a freeze, parsed once.
#[derive(Debug, Clone)]
struct Holder {
    path: PathBuf,
    marker: FreezeMarker,
    /// The legacy `freeze.json` rather than a file in `freeze.d/`.
    legacy: bool,
    /// A legacy file written as a mirror: never a holder of its own.
    mirror: bool,
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
    marker: PathBuf,
    liveness: Liveness,
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
    pub(crate) fn isolated() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        Self::at(std::env::temp_dir().join(format!("ebman-freeze-isolated-{pid}-{n}.json")))
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

    fn holders_dir(&self) -> PathBuf {
        self.marker.with_extension("d")
    }

    fn persistent_file(&self) -> PathBuf {
        self.holders_dir().join("persistent.json")
    }

    fn session_file(&self, pid: u32) -> PathBuf {
        self.holders_dir().join(format!("session-{pid}.json"))
    }

    fn ended_file(&self) -> PathBuf {
        self.marker.with_extension("ended.json")
    }

    fn thawed_file(&self) -> PathBuf {
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
    fn holders(&self) -> Vec<Holder> {
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
    fn reap(&self, h: &Holder) {
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
    fn sync_legacy(&self, holders: &[Holder]) {
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

/// Write one marker body to `path`, atomically. `mirror` marks the
/// legacy copy for older readers.
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

fn link_into_place(tmp: &Path, path: &Path) -> std::io::Result<()> {
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
fn write_marker_exclusive(
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

fn write_marker_full(
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

fn is_mirror_text(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("mirror").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

fn is_mirror(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| is_mirror_text(&t))
}

/// The one refusal sentence for an active fleet freeze.
///
/// Shared because it was written twice — once in the CLI's freeze
/// refusal (now the CLI gate, `cli::write_refusal_unaudited`) and once
/// in the MCP write gate — with the same body and different
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

/// Longest reason a freeze takes. It is shown wherever the freeze
/// refuses a write, a one-line status bar among them, and whoever lifts
/// it needs to read it at a glance.
pub(crate) const REASON_MAX: usize = 200;

/// Why a freeze reason was refused. Every declare checks it: `ebman
/// freeze`, `:freeze-deploys`, `:incident START` and `freeze_fleet`.
/// Only the last did, and the reason the rule exists (one line in a
/// status bar, read by whoever lifts it) is the same for all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BadReason {
    /// A control character, or a line break or direction override
    /// Unicode spells outside them: each can make the one line show
    /// something other than what was declared.
    BreaksTheLine,
    TooLong,
}

impl BadReason {
    /// What is wrong, after "the reason".
    pub(crate) fn what(self) -> String {
        match self {
            Self::BreaksTheLine => {
                "must be one line, with no control characters — it is shown in a one-line \
                 status bar"
                    .to_string()
            }
            Self::TooLong => format!("is over {REASON_MAX} characters"),
        }
    }
}

pub(crate) fn check_reason(reason: &str) -> Result<(), BadReason> {
    if reason.chars().any(crate::util::rewrites_the_line) {
        return Err(BadReason::BreaksTheLine);
    }
    if reason.chars().count() > REASON_MAX {
        return Err(BadReason::TooLong);
    }
    Ok(())
}

/// The audit label for declaring a freeze.
pub(crate) fn declare_label(incident: bool) -> &'static str {
    if incident {
        "IncidentStart"
    } else {
        "FreezeDeploys"
    }
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

fn write_thawed_at(path: &Path, m: &FreezeMarker, thawed_at: chrono::DateTime<chrono::Utc>) {
    let mut v = serde_json::json!({
        "pid": m.pid,
        "reason": m.reason,
        "incident": m.incident,
        "at": m.at,
        "persistent": m.persistent,
        "thawed_at": thawed_at.to_rfc3339(),
    });
    if let Some(agent) = &m.agent {
        v["agent"] = serde_json::Value::String(agent.clone());
    }
    // Temp then rename, as the markers are: written in place, a reader
    // between the truncate and the write saw an empty record, so no hold.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = crate::util::write_secure(&tmp, format!("{v}\n").as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        // Said, not swallowed: without the record an agent can freeze
        // again at once, which is the loop this exists to stop.
        tracing::warn!(error = %e, "could not record a thaw; freeze_fleet is not held off");
    }
}

/// `None` for a missing or unreadable record, as for a corrupt marker:
/// no hold, which only gives an agent back the tool it had.
fn read_thawed_at(path: &Path, now: chrono::DateTime<chrono::Utc>) -> Option<ThawedFreeze> {
    let text = std::fs::read_to_string(path).ok()?;
    let marker = parse_marker(&text)?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let thawed_at = chrono::DateTime::parse_from_rfc3339(v.get("thawed_at")?.as_str()?)
        .ok()?
        .with_timezone(&chrono::Utc);
    // Bounded below too: a record from the future (a clock stepped
    // back, a record copied from another host) held until the clock
    // passed it, however far that was.
    let age = now.signed_duration_since(thawed_at).num_seconds();
    (-START_SLACK_SECS..THAW_HOLDS_MINS * 60)
        .contains(&age)
        .then_some(ThawedFreeze { marker, thawed_at })
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
        agent: v.get("agent").and_then(|a| a.as_str()).map(str::to_string),
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

    /// A store in a directory of its own, judged by `liveness`.
    fn store(name: &str, liveness: Liveness) -> FreezeStore {
        let dir = std::env::temp_dir().join(format!("ebman-freeze-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        FreezeStore::with_liveness(dir.join("freeze.json"), liveness)
    }

    fn done(s: &FreezeStore) {
        let _ = std::fs::remove_dir_all(s.marker().parent().unwrap());
    }

    fn pids(s: &FreezeStore) -> Vec<u32> {
        s.all().iter().map(|m| m.pid).collect()
    }

    #[test]
    fn a_session_marker_round_trips() {
        let s = store("rt", Liveness::ALL_ALIVE);
        s.declare_session(4242, "the \"big\" one\nline2", true)
            .unwrap();
        let got = parse_file(&s.session_file(4242)).expect("its own file");
        assert_eq!(got.pid, 4242);
        assert_eq!(got.reason, "the \"big\" one\nline2");
        assert!(got.incident && !got.persistent);
        assert_eq!(got.remedy(), ":incident END");
        done(&s);
    }

    #[test]
    fn a_dead_sessions_freeze_is_reaped_and_recorded() {
        let s = store("dead", Liveness::NONE_ALIVE);
        s.declare_session(4242, "incident: db failover", true)
            .unwrap();
        assert!(s.all().is_empty());
        assert!(!s.session_file(4242).exists(), "removed by the reader");
        let ended = s.ended().expect("recorded");
        assert_eq!(ended.marker.reason, "incident: db failover");
        assert!(
            ended.message().contains("ebman freeze"),
            "{}",
            ended.message()
        );
        let later = chrono::Utc::now() + chrono::Duration::hours(ENDED_REPORT_HOURS);
        assert!(
            read_ended_at(&s.ended_file(), later).is_none(),
            "reported for a day"
        );
        done(&s);
    }

    #[test]
    fn a_live_sessions_freeze_holds_and_is_not_reaped() {
        let s = store("live", Liveness::ALL_ALIVE);
        s.declare_session(4242, "deploy freeze", false).unwrap();
        assert_eq!(pids(&s), vec![4242]);
        assert!(s.session_file(4242).exists());
        assert!(s.ended().is_none());
        done(&s);
    }

    /// The reviewers' case: `ebman freeze`, then a TUI's `:incident
    /// START`, then its END (or exit). One shared file let the session
    /// replace the persistent marker and then delete it.
    #[test]
    fn a_sessions_end_never_lifts_a_persistent_freeze() {
        let s = store("c1", Liveness::ALL_ALIVE);
        s.declare_persistent("change freeze", false).unwrap();
        s.declare_session(4242, "incident: db", true).unwrap();
        s.clear_session(4242);
        let held = s.all();
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].persistent && held[0].reason == "change freeze");
        done(&s);
    }

    /// And the other way round: `ebman freeze` over a live session, then
    /// `ebman thaw`, must leave the session's freeze in force, and say so.
    #[test]
    fn a_thaw_never_lifts_a_sessions_freeze_and_says_it_holds() {
        let s = store("i1", Liveness::ALL_ALIVE);
        s.declare_session(4242, "incident: db", true).unwrap();
        s.declare_persistent("change freeze", false).unwrap();
        assert!(matches!(
            s.thaw(),
            Thaw::Lifted { still_held: Some(h), .. } if h.pid == 4242
        ));
        assert_eq!(pids(&s), vec![4242]);
        assert!(matches!(s.thaw(), Thaw::HeldBySession(h) if h.pid == 4242));
        assert!(s.session_file(4242).exists(), "that session's to lift");
        done(&s);
    }

    #[test]
    fn one_session_ending_leaves_anothers_freeze() {
        let s = store("two", Liveness::ALL_ALIVE);
        s.declare_session(111, "a", false).unwrap();
        s.declare_session(222, "b", false).unwrap();
        s.clear_session(222);
        assert_eq!(pids(&s), vec![111]);
        done(&s);
    }

    #[test]
    fn thaw_reports_a_lift_only_when_it_made_one() {
        let s = store("thaw", Liveness::ALL_ALIVE);
        assert_eq!(s.thaw(), Thaw::Nothing);
        s.declare_persistent("x", false).unwrap();
        assert!(matches!(
            s.thaw(),
            Thaw::Lifted {
                still_held: None,
                ..
            }
        ));
        assert!(!s.persistent_file().exists());
        assert_eq!(s.thaw(), Thaw::Nothing);
        done(&s);
    }

    /// ebman before 0.47 reads only `freeze.json`. It mirrors a live
    /// holder, parseable by the old reader, and goes when none is left.
    #[test]
    fn the_legacy_file_mirrors_a_live_holder_for_older_readers() {
        let s = store("mirror", Liveness::ALL_ALIVE);
        s.declare_session(4242, "s", false).unwrap();
        assert_eq!(parse_file(s.marker()).map(|l| l.pid), Some(4242));
        s.declare_persistent("p", false).unwrap();
        let legacy = parse_file(s.marker()).expect("still mirrored");
        assert!(
            legacy.persistent && legacy.pid == PERSISTENT_PID,
            "persistent first"
        );
        assert_eq!(s.all().len(), 2, "a mirror is not a holder of its own");
        let _ = s.thaw();
        assert_eq!(parse_file(s.marker()).map(|l| l.pid), Some(4242));
        s.clear_session(4242);
        assert!(!s.marker().exists(), "no holder, no mirror");
        done(&s);
    }

    /// A stale holder sorting first must not be what the mirror names:
    /// an older reader removes a mirror whose pid is dead, and writes go
    /// through while a live session holds.
    #[test]
    fn the_mirror_names_a_live_holder_not_the_first_file() {
        let dir = store("mirror-live", Liveness::ALL_ALIVE);
        let marker = dir.marker().to_path_buf();
        // Pid 1 alive, everything else dead; the dead one declared first.
        let s = FreezeStore::with_liveness(
            marker,
            Liveness {
                alive: |p| p == 1,
                start_epoch: |_| None,
            },
        );
        write_marker_full(
            &s.session_file(9999),
            9999,
            "dead",
            false,
            false,
            false,
            None,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.declare_session(1, "live", false).unwrap();
        assert_eq!(
            parse_file(s.marker()).map(|l| l.pid),
            Some(1),
            "names the live one"
        );
        done(&s);
    }

    /// When the mirrored holder dies, the next read reaps it and points
    /// the mirror at a holder that lives.
    #[test]
    fn a_reap_repairs_the_mirror() {
        let s = store("repair", Liveness::ALL_ALIVE);
        s.declare_session(4242, "first", false).unwrap();
        s.declare_session(1, "second", false).unwrap();
        assert_eq!(parse_file(s.marker()).map(|l| l.pid), Some(4242));
        let judged = FreezeStore::with_liveness(
            s.marker().to_path_buf(),
            Liveness {
                alive: |p| p == 1,
                start_epoch: |_| None,
            },
        );
        assert_eq!(pids(&judged), vec![1]);
        assert_eq!(parse_file(s.marker()).map(|l| l.pid), Some(1));
        done(&s);
    }

    /// A 0.46 TUI's clear removes `freeze.json` though a persistent freeze
    /// holds; the next read puts the mirror back.
    #[test]
    fn a_missing_mirror_is_restored_on_read() {
        let s = store("restore", Liveness::ALL_ALIVE);
        s.declare_persistent("p", false).unwrap();
        std::fs::remove_file(s.marker()).unwrap();
        assert_eq!(s.all().len(), 1);
        assert!(parse_file(s.marker()).is_some_and(|l| l.persistent));
        done(&s);
    }

    /// An older TUI that writes its own marker after the holders were
    /// read is not removed (nothing else holds) or overwritten (a newer
    /// holder does) by the mirror: that TUI has no `freeze.d/` file, so
    /// nothing would restore it, and every older reader would fail open.
    #[test]
    fn the_mirror_never_destroys_an_older_tuis_marker_written_mid_sync() {
        const OLD: &[u8] =
            br#"{"pid":777,"reason":"old tui","incident":false,"at":"2026-10-23T00:00:00Z"}"#;
        let s = store("race-none", Liveness::ALL_ALIVE);
        let holders = s.holders();
        crate::util::write_secure(s.marker(), OLD).unwrap();
        s.sync_legacy(&holders);
        assert_eq!(
            parse_file(s.marker()).map(|l| l.pid),
            Some(777),
            "not removed"
        );
        done(&s);

        let s = store("race-some", Liveness::ALL_ALIVE);
        s.declare_session(4242, "new", false).unwrap();
        let holders = s.holders();
        crate::util::write_secure(s.marker(), OLD).unwrap();
        s.sync_legacy(&holders);
        assert_eq!(
            parse_file(s.marker()).map(|l| l.pid),
            Some(777),
            "not overwritten"
        );
        done(&s);
    }

    /// A `freeze.json` an older TUI wrote (no mirror mark) is that
    /// session's freeze: honoured while it lives, never overwritten by a
    /// mirror, reaped when it dies.
    #[test]
    fn an_older_tuis_marker_is_a_holder_of_its_own() {
        let s = store("old", Liveness::ALL_ALIVE);
        crate::util::write_secure(
            s.marker(),
            br#"{"pid":777,"reason":"old tui","incident":false,"at":"2026-10-23T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(pids(&s), vec![777]);
        s.declare_session(4242, "new", false).unwrap();
        assert_eq!(
            parse_file(s.marker()).map(|l| l.pid),
            Some(777),
            "not clobbered"
        );
        assert_eq!(s.all().len(), 2);
        let judged = FreezeStore::with_liveness(
            s.marker().to_path_buf(),
            Liveness {
                alive: |p| p != 777,
                start_epoch: |_| None,
            },
        );
        assert_eq!(pids(&judged), vec![4242], "reaped");
        assert_eq!(
            parse_file(s.marker()).map(|l| l.pid),
            Some(4242),
            "and mirrored"
        );
        done(&s);
    }

    /// A reaper must not delete a holder rewritten with a live freeze
    /// between its read and its remove.
    #[test]
    fn reaping_does_not_delete_a_freshly_rewritten_marker() {
        let s = store("toctou", Liveness::NONE_ALIVE);
        s.declare_session(4242, "dead session", false).unwrap();
        let read = s
            .holders()
            .into_iter()
            .find(|h| !h.legacy)
            .expect("the session file");
        write_marker_full(&read.path, 4242, "rewritten live", true, false, false, None).unwrap();
        s.reap(&read);
        assert_eq!(
            parse_file(&read.path).map(|f| f.reason),
            Some("rewritten live".into())
        );
        done(&s);
    }

    #[test]
    fn a_corrupt_holder_never_blocks() {
        let s = store("corrupt", Liveness::ALL_ALIVE);
        std::fs::create_dir_all(s.holders_dir()).unwrap();
        crate::util::write_secure(&s.persistent_file(), b"not json").unwrap();
        crate::util::write_secure(&s.session_file(1), b"{").unwrap();
        assert!(s.all().is_empty());
        done(&s);
    }

    #[test]
    fn a_persistent_freeze_holds_whatever_any_pid_says() {
        let s = store("persistent", Liveness::NONE_ALIVE);
        s.declare_persistent("change freeze", false).unwrap();
        let held = s.all();
        assert!(held.len() == 1 && held[0].persistent, "{held:?}");
        assert_eq!(held[0].remedy(), "ebman thaw");
        let msg = refusal_message(&held[0]);
        assert!(
            msg.contains("ebman thaw") && msg.contains("change freeze"),
            "{msg}"
        );
        assert!(!msg.contains("pid"), "no session to name: {msg}");
        done(&s);
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

    /// A freeze an agent declared says so wherever it is shown, and in
    /// the legacy mirror; one the operator declared does not.
    #[test]
    fn an_agent_s_freeze_names_the_agent() {
        let s = FreezeStore::isolated();
        s.declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap();
        let m = s.active().expect("frozen");
        assert_eq!(m.agent.as_deref(), Some("claude-code"));
        assert_eq!(
            m.holder(),
            "declared by an agent (claude-code) with `freeze_fleet`"
        );
        assert_eq!(m.short_holder(), "agent freeze");
        assert!(refusal_message(&m).contains("agent (claude-code)"));
        assert_eq!(
            parse_file(&s.marker).and_then(|m| m.agent).as_deref(),
            Some("claude-code"),
            "the mirror carries it"
        );

        // The operator's own, declared over it, drops the name: the
        // mirror is rewritten, not left naming the agent. The same
        // reason, so the name is all that differs.
        s.declare_persistent("bad deploy", false).unwrap();
        let m = s.active().expect("frozen");
        assert_eq!(m.agent, None);
        assert_eq!(m.holder(), "declared with `ebman freeze`");
        assert_eq!(m.short_holder(), "ebman freeze");
        assert_eq!(parse_file(&s.marker).and_then(|m| m.agent), None);
    }

    /// The store holds an agent off after a thaw, not only the tool:
    /// nothing is written and the hold is the answer.
    #[test]
    fn the_store_refuses_an_agent_s_declare_while_a_thaw_holds() {
        let s = store("agent-thawed", Liveness::ALL_ALIVE);
        s.declare_persistent("release window", false).unwrap();
        assert!(matches!(s.thaw(), Thaw::Lifted { .. }));
        match s
            .declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap()
        {
            AgentDeclare::Thawed(t) => assert_eq!(t.marker.reason, "release window"),
            other => panic!("{other:?}"),
        }
        assert!(s.active().is_none(), "nothing declared");
        done(&s);
    }

    /// A thaw that lands while an agent declares: the declare takes its
    /// marker back and answers with the hold, rather than stand over the
    /// operator's thaw.
    #[test]
    fn a_thaw_landing_mid_declare_takes_the_agent_s_freeze_back() {
        let s = store("thaw-mid", Liveness::ALL_ALIVE);
        // `freeze.d/persistent.json` -> `freeze.thawed.json`, as a thaw
        // in another process writes it.
        fn thaw_now(marker: &Path) {
            let record = marker.parent().unwrap().with_extension("thawed.json");
            let lifted = parse_marker(r#"{"pid":1,"reason":"earlier","persistent":true}"#).unwrap();
            write_thawed_at(&record, &lifted, chrono::Utc::now());
        }
        AFTER_LINK.with(|f| f.set(Some(thaw_now)));
        let r = s.declare_persistent_by_agent("bad deploy", false, "claude-code");
        AFTER_LINK.with(|f| f.set(None));
        assert!(matches!(r, Ok(AgentDeclare::Thawed(_))), "{r:?}");
        assert!(s.active().is_none(), "the agent's marker was taken back");
        done(&s);
    }

    /// A thaw records its hold before it removes the marker, so there is
    /// no instant with neither for an agent to freeze in. Seen through a
    /// thaw whose remove fails: the record is already there.
    #[cfg(unix)]
    #[test]
    fn a_thaw_records_its_hold_before_the_marker_goes() {
        use std::os::unix::fs::PermissionsExt;
        let s = store("thaw-order", Liveness::ALL_ALIVE);
        s.declare_persistent("release window", false).unwrap();
        let dir = s.holders_dir();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let thaw = s.thaw();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            !matches!(thaw, Thaw::Lifted { .. }),
            "the remove failed: {thaw:?}"
        );
        assert!(
            s.recently_thawed().is_some(),
            "the hold was recorded before the remove was tried"
        );
        done(&s);
    }

    /// it answers with that freeze, which is left byte for byte. One it
    /// cannot read is replaced (a corrupt marker blocks nothing, so it
    /// must not stop a freeze), and no temp file is left behind.
    #[test]
    fn an_agent_s_declare_keeps_a_standing_freeze() {
        let s = store("agent-excl", Liveness::ALL_ALIVE);
        let leftovers = |s: &FreezeStore| -> Vec<String> {
            std::fs::read_dir(s.holders_dir())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "persistent.json")
                .collect()
        };

        s.declare_persistent("release window", true).unwrap();
        let before = std::fs::read(s.persistent_file()).unwrap();
        match s
            .declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap()
        {
            AgentDeclare::Held(m) => {
                assert_eq!(m.reason, "release window");
                assert!(m.incident);
                assert_eq!(m.agent, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(s.persistent_file()).unwrap(), before);
        assert_eq!(leftovers(&s), Vec::<String>::new());

        std::fs::write(s.persistent_file(), "{not json").unwrap();
        assert_eq!(
            s.declare_persistent_by_agent("bad deploy", false, "claude-code")
                .unwrap(),
            AgentDeclare::Declared
        );
        let m = s.active().expect("frozen");
        assert_eq!(
            (m.reason.as_str(), m.agent.as_deref()),
            ("bad deploy", Some("claude-code"))
        );

        let _ = s.thaw();
        s.expire_thaw_for_test();
        assert_eq!(
            s.declare_persistent_by_agent("fresh", false, "claude-code")
                .unwrap(),
            AgentDeclare::Declared
        );
        assert_eq!(s.active().expect("frozen").reason, "fresh");
        assert_eq!(leftovers(&s), Vec::<String>::new());

        // A link that fails for any other reason is that failure: not
        // "one stood", and nothing is written another way.
        let _ = s.thaw();
        s.expire_thaw_for_test();
        let fail = |kind| {
            LINK_FAILS.with(|f| f.set(Some(kind)));
            let r = s.declare_persistent_by_agent("disk full", false, "claude-code");
            LINK_FAILS.with(|f| f.set(None));
            r
        };
        let err = fail(std::io::ErrorKind::PermissionDenied).expect_err("the link failed");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert!(s.active().is_none(), "nothing written another way");
        assert_eq!(leftovers(&s), Vec::<String>::new());

        // One stood when linked, and a thaw lifted it before it was read:
        // an error to retry, never a write that is not exclusive.
        let err = fail(std::io::ErrorKind::AlreadyExists).expect_err("lifted meanwhile");
        assert!(err.to_string().contains("lifted while"), "{err}");
        assert!(s.active().is_none(), "nothing written another way");
        done(&s);
    }

    /// One rule for every declare: one line, at most 200 characters.
    #[test]
    fn a_freeze_reason_is_one_short_line() {
        assert_eq!(check_reason(""), Ok(()));
        assert_eq!(check_reason(&"x".repeat(REASON_MAX)), Ok(()));
        assert_eq!(
            check_reason(&"é".repeat(REASON_MAX)),
            Ok(()),
            "characters, not bytes"
        );
        assert_eq!(
            check_reason(&"x".repeat(REASON_MAX + 1)),
            Err(BadReason::TooLong)
        );
        for bad in [
            "a\nb",
            "a\tb",
            "a\u{2028}b",
            "a\u{202E}b",
            "a\u{2067}b",
            "a\u{200F}b",
            // The dialog sanitiser's set: a zero-width joiner, a BOM and
            // a TAG character passed here once, to be shown raw.
            "a\u{200D}b",
            "a\u{FEFF}b",
            "a\u{E0041}b",
        ] {
            assert_eq!(check_reason(bad), Err(BadReason::BreaksTheLine), "{bad:?}");
        }
        assert_eq!(declare_label(true), "IncidentStart");
        assert_eq!(declare_label(false), "FreezeDeploys");
    }

    /// A thaw holds for [`THAW_HOLDS_MINS`], to the second, and its
    /// record carries what was lifted.
    #[test]
    fn a_thaw_holds_for_thirty_minutes() {
        let s = store("thawed", Liveness::ALL_ALIVE);
        assert_eq!(s.recently_thawed(), None, "nothing lifted yet");
        s.declare_persistent_by_agent("bad deploy", true, "claude-code")
            .unwrap();
        assert!(matches!(s.thaw(), Thaw::Lifted { .. }));
        let t = s.recently_thawed().expect("the thaw holds");
        assert_eq!(t.marker.reason, "bad deploy");
        assert!(t.marker.incident);
        assert_eq!(t.marker.agent.as_deref(), Some("claude-code"));
        assert_eq!(t.holds_until() - t.thawed_at, chrono::Duration::minutes(30));

        let at = t.thawed_at;
        let path = s.thawed_file();
        let just_inside = at + chrono::Duration::seconds(30 * 60 - 1);
        let at_the_end = at + chrono::Duration::minutes(30);
        assert!(read_thawed_at(&path, just_inside).is_some(), "29:59 holds");
        let slack = chrono::Duration::seconds(START_SLACK_SECS);
        assert!(
            read_thawed_at(&path, at - slack).is_some(),
            "a clock a little behind still holds"
        );
        assert!(
            read_thawed_at(&path, at - slack - chrono::Duration::seconds(1)).is_none(),
            "a record from further in the future does not hold forever"
        );
        assert!(
            read_thawed_at(&path, at_the_end).is_none(),
            "30:00 does not"
        );

        // A thaw that found nothing to lift records nothing new.
        s.write_test_thawed(&t.marker, at - chrono::Duration::hours(1));
        assert_eq!(s.thaw(), Thaw::Nothing);
        assert_eq!(s.recently_thawed(), None);
        done(&s);
    }

    /// An operator's freeze, persistent or a session's, is a new
    /// ruling and ends the hold; an agent's declaring never does.
    #[test]
    fn only_an_operator_s_freeze_ends_a_thaw_s_hold() {
        let s = store("thawed-clear", Liveness::ALL_ALIVE);
        let lift = |s: &FreezeStore| {
            s.declare_persistent("x", false).unwrap();
            assert!(matches!(s.thaw(), Thaw::Lifted { .. }));
            assert!(s.recently_thawed().is_some());
        };

        lift(&s);
        s.declare_persistent_by_agent("again", false, "claude-code")
            .unwrap();
        assert!(s.recently_thawed().is_some(), "an agent's never clears it");

        lift(&s);
        s.declare_persistent("operator", false).unwrap();
        assert_eq!(s.recently_thawed(), None, "ebman freeze clears it");
        let _ = s.thaw();

        lift(&s);
        s.declare_session(4242, "session", false).unwrap();
        assert_eq!(s.recently_thawed(), None, ":freeze-deploys clears it");
        done(&s);
    }

    /// One the reader cannot make sense of holds nothing off.
    #[test]
    fn an_unreadable_thaw_record_holds_nothing() {
        let s = store("thawed-bad", Liveness::ALL_ALIVE);
        std::fs::write(s.thawed_file(), "{not json").unwrap();
        assert_eq!(s.recently_thawed(), None);
        std::fs::write(
            s.thawed_file(),
            r#"{"pid":1,"reason":"x","thawed_at":"yesterday"}"#,
        )
        .unwrap();
        assert_eq!(s.recently_thawed(), None);
        done(&s);
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
            agent: None,
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

    /// The real store round-trips a session freeze through the real
    /// path and the real probes: this process is alive, so its own freeze
    /// reads back, and clearing it lifts it.
    ///
    /// The one test that touches the store every process shares; see
    /// `no_other_test_reads_the_shared_store`.
    #[test]
    fn the_real_store_round_trips_through_the_real_path() {
        let s = FreezeStore::real();
        assert!(
            s.marker().ends_with("freeze.json"),
            "{}",
            s.marker().display()
        );
        s.clear_session(std::process::id());
        s.declare_session(std::process::id(), "incident #4321", true)
            .expect("marker must be written");
        assert!(s.session_file(std::process::id()).exists());
        let found = s.active().expect(
            "a freeze declared by THIS live process must read back as active — \
             otherwise it fails open while the operator believes the fleet is frozen",
        );
        assert_eq!(found.reason, "incident #4321");
        assert!(found.incident);
        assert_eq!(found.pid, std::process::id());
        s.clear_session(std::process::id());
        assert!(s.active().is_none(), "clearing our own freeze must lift it");
    }

    /// No test but the round trip above names the shared store.
    ///
    /// Under `cfg(test)` the cache directory is per process, not per
    /// test, so the shared store is one marker for every test in the
    /// binary, on parallel threads. Tests that wrote it took a lock; the
    /// ones that only read it did not, and every MCP write test and CLI
    /// write gate was a reader, so one test's freeze could refuse
    /// another's write. Every surface now takes its store as a value and
    /// a test passes `FreezeStore::isolated()`, which leaves nothing for
    /// the lock to serialise. This keeps it that way.
    /// An isolated store, a `--demo` App's among them, lives outside
    /// the cache, where its files would outlive the session.
    #[test]
    fn an_isolated_store_is_not_in_the_cache() {
        let s = FreezeStore::isolated();
        assert!(
            !s.marker.starts_with(crate::util::cache_dir()),
            "{:?}",
            s.marker
        );
        assert!(s.marker.starts_with(std::env::temp_dir()), "{:?}", s.marker);
    }

    #[test]
    fn no_other_test_reads_the_shared_store() {
        // Each entry names why the call is not a read of the store.
        const ALLOWED: &[(&str, usize, &str)] = &[
            ("src/freeze.rs", 1, "the round trip above"),
            ("src/cli/mod.rs", 1, "a source guard's expected text"),
            ("src/cli/mcp/tests.rs", 1, "a source guard's expected text"),
        ];
        // Built, so these lines are not matches of their own. A test
        // that builds the real write env reads the shared store too.
        let needles = [
            concat!("FreezeStore::", "real()"),
            concat!("WriteEnv::", "real()"),
        ];
        let mut found = Vec::new();
        for (path, src) in crate::app::tests::scan::source_files() {
            let test_part: Vec<&str> = if crate::app::tests::scan::is_test_path(&path) {
                src.lines().collect()
            } else {
                // The lines `production_half` cut: it removes whole test
                // modules and keeps the rest in order, so walking both
                // together leaves exactly those.
                let production = crate::app::tests::scan::production_half(&src);
                let mut kept = production.lines().peekable();
                src.lines()
                    .filter(|line| {
                        if kept.peek() == Some(line) {
                            kept.next();
                            false
                        } else {
                            true
                        }
                    })
                    .collect()
            };
            let n: usize = test_part
                .iter()
                .map(|l| {
                    let code = crate::app::tests::scan::strip_line_comment(l);
                    needles
                        .iter()
                        .map(|n| code.matches(n).count())
                        .sum::<usize>()
                })
                .sum();
            if n > 0 {
                found.push((path, n));
            }
        }
        let unexpected: Vec<_> = found
            .iter()
            .filter(|(path, n)| !ALLOWED.iter().any(|(p, k, _)| path == p && n == k))
            .collect();
        assert!(
            unexpected.is_empty(),
            "a test reads the shared freeze store; give it `FreezeStore::isolated()` \
             or the surface's own store: {unexpected:?}"
        );
        // Canary: the round trip is found, so the scan sees test code.
        assert!(
            found.iter().any(|(p, _)| p == "src/freeze.rs"),
            "the scan found no test code at all: {found:?}"
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
