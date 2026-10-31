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
    let at = ended.ended_at;
    let path = s.ended_file();
    let day = chrono::Duration::hours(ENDED_REPORT_HOURS);
    let second = chrono::Duration::seconds(1);
    assert!(
        read_ended_at(&path, at + day - second).is_some(),
        "reported for a day"
    );
    assert!(read_ended_at(&path, at + day).is_none(), "and no longer");
    let slack = chrono::Duration::seconds(START_SLACK_SECS);
    assert!(
        read_ended_at(&path, at - slack).is_some(),
        "a clock a little behind"
    );
    assert!(
        read_ended_at(&path, at - slack - second).is_none(),
        "a record from further in the future is not reported for days"
    );
    // The record carries the marker whole, as the thaw record does.
    let text = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["persistent"], false, "{text}");
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

/// A temp file a dead writer left is swept by the next agent declare
/// once it is over a minute old, and a fresh one (a write in flight)
/// is left alone.
#[test]
fn stale_temp_files_are_swept_and_fresh_ones_kept() {
    let s = store("sweep", Liveness::ALL_ALIVE);
    std::fs::create_dir_all(s.holders_dir()).unwrap();
    let old = s.holders_dir().join("persistent.excl.99999.0");
    let tmp = s.holders_dir().join("persistent.tmp.99999.1");
    let fresh = s.holders_dir().join("persistent.excl.99999.2");
    for p in [&old, &tmp, &fresh] {
        std::fs::write(p, "x").unwrap();
    }
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for p in [&old, &tmp] {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
    }
    s.declare_persistent_by_agent("bad deploy", false, "claude-code")
        .unwrap();
    assert!(!old.exists() && !tmp.exists(), "stale ones swept");
    assert!(fresh.exists(), "a fresh one may be in flight");

    // A minute old exactly is not yet stale.
    let edge = s.holders_dir().join("persistent.excl.99999.3");
    std::fs::write(&edge, "x").unwrap();
    let written = std::fs::metadata(&edge).unwrap().modified().unwrap();
    sweep_stale_temps(
        &s.holders_dir(),
        written + std::time::Duration::from_secs(60),
    );
    assert!(edge.exists(), "60s is the limit, not past it");
    sweep_stale_temps(
        &s.holders_dir(),
        written + std::time::Duration::from_secs(61),
    );
    assert!(!edge.exists());

    // The marker itself is never a temp file, however old.
    let _ = s.thaw();
    s.expire_thaw_for_test();
    s.declare_persistent("release window", false).unwrap();
    std::fs::File::options()
        .write(true)
        .open(s.persistent_file())
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    assert!(matches!(
        s.declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap(),
        AgentDeclare::Held(_)
    ));
    assert_eq!(s.active().expect("kept").reason, "release window");
    done(&s);
}

/// A marker that cannot be read for any reason but "gone" is that
/// failure, not "lifted while declaring".
#[test]
fn an_unreadable_marker_path_is_its_own_error() {
    let s = store("marker-dir", Liveness::ALL_ALIVE);
    std::fs::create_dir_all(s.persistent_file()).unwrap();
    let err = s
        .declare_persistent_by_agent("bad deploy", false, "claude-code")
        .expect_err("a directory is no marker");
    assert!(!err.to_string().contains("lifted while"), "{err}");
    done(&s);
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
    let after = |f: fn(&Path)| {
        s.with_link(LinkSeam {
            after: Some(f),
            ..LinkSeam::default()
        })
    };
    let r = after(thaw_now).declare_persistent_by_agent("bad deploy", false, "claude-code");
    assert!(matches!(r, Ok(AgentDeclare::Thawed(_))), "{r:?}");
    assert!(s.active().is_none(), "the agent's marker was taken back");

    // Only the agent's own: an operator's freeze with the same reason,
    // there by then, is kept.
    s.expire_thaw_for_test();
    fn thaw_then_operator_freezes(marker: &Path) {
        thaw_now(marker);
        write_marker_full(
            marker,
            PERSISTENT_PID,
            "bad deploy",
            false,
            true,
            false,
            None,
        )
        .unwrap();
    }
    let r = after(thaw_then_operator_freezes).declare_persistent_by_agent(
        "bad deploy",
        false,
        "claude-code",
    );
    assert!(matches!(r, Ok(AgentDeclare::Thawed(_))), "{r:?}");
    let kept = s.active().expect("the operator's freeze stands");
    assert_eq!(kept.agent, None);
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
        s.with_link(LinkSeam {
            fails: Some(kind),
            ..LinkSeam::default()
        })
        .declare_persistent_by_agent("disk full", false, "claude-code")
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

/// The sweep removes a dead process's isolated store, in either
/// layout, and nothing else: not a live one's, not this process's,
/// not a name that only starts the same way.
#[test]
fn the_isolated_sweep_removes_only_dead_processes_stores() {
    let tmp = std::env::temp_dir().join(format!("ebman-freeze-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let mk_dir = |n: &str| {
        std::fs::create_dir_all(tmp.join(n).join("0.d")).unwrap();
        std::fs::write(tmp.join(n).join("0.thawed.json"), "{}").unwrap();
    };
    let mk_file = |n: &str| std::fs::write(tmp.join(n), "{}").unwrap();
    mk_dir("ebman-freeze-isolated-41");
    mk_file("ebman-freeze-isolated-41-3.ended.json");
    mk_dir("ebman-freeze-isolated-41-3.d");
    mk_dir("ebman-freeze-isolated-42");
    mk_dir("ebman-freeze-isolated-7");
    mk_file("ebman-freeze-isolated-x.json");
    mk_file("ebman-freeze-41.json");
    sweep_isolated(&tmp, 7, |pid| pid == 42);
    let mut left: Vec<String> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "ebman-freeze-41.json",
            "ebman-freeze-isolated-42",
            "ebman-freeze-isolated-7",
            "ebman-freeze-isolated-x.json",
        ]
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Every isolated store a process makes is under its one directory,
/// the one the sweep judges by its pid.
#[test]
fn an_isolated_store_lives_under_its_process_s_directory() {
    let s = FreezeStore::isolated();
    let dir = std::env::temp_dir().join(format!("{ISOLATED_PREFIX}{}", std::process::id()));
    assert_eq!(s.marker.parent(), Some(dir.as_path()), "{:?}", s.marker);
    assert!(dir.is_dir());
}

#[test]
fn no_other_test_reads_the_shared_store() {
    // Each entry names why the call is not a read of the store.
    const ALLOWED: &[(&str, usize, &str)] = &[
        ("src/freeze/tests.rs", 1, "the round trip above"),
        ("src/cli/write_env.rs", 1, "a source guard's expected text"),
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
        found.iter().any(|(p, _)| p == "src/freeze/tests.rs"),
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
