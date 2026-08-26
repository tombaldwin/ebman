//! Embedded shell pane — spawns a subprocess inside a pseudo-terminal we
//! own, feeds its output through `vt100` to maintain a virtual terminal
//! buffer, and exposes a render path that paints that buffer into a
//! ratatui `Buffer`. The user types into ebman, ebman writes the bytes to
//! the PTY master, the subprocess sees them as if they were typed directly.
//!
//! Used today for SSM Session Manager sessions (`aws ssm start-session`),
//! but the API is generic — anything that runs in a TTY can be hosted.
//!
//! Limits:
//! - vt100 implements enough of xterm to handle interactive shells, but
//!   it's not a full xterm. Heavy TUIs (full-screen vim, mosh) may
//!   render imperfectly.
//! - Bracketed paste / focus events / mouse passthrough not forwarded.
//!
//! Detach key: **F12**. Sent neither to the PTY nor to the normal key
//! dispatch — it returns control to ebman without killing the subprocess
//! (the session keeps running; the user can come back). A second F12 from
//! Detail / Normal mode resumes the same session.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize};

/// A live embedded shell session. `parser` is the virtual terminal state.
/// The PTY-side fields (`writer`, `master`, `child`) are `Option` because
/// `--demo` mode constructs a fake session that pre-loads canned content
/// into the parser without spawning a real subprocess. For real sessions
/// all three are populated; for demo sessions all three are `None`.
pub(crate) struct ShellSession {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Option<Box<dyn Write + Send>>,
    pub master: Option<Box<dyn MasterPty + Send>>,
    pub child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// Human label shown in the pane title (e.g. the instance id).
    pub label: String,
    /// Output-reader background task. `Some` until the subprocess exits
    /// and the reader returns; then the run loop can decide to close.
    /// Demo sessions keep this `true` for the session's lifetime — they
    /// can't "die" because there's no subprocess.
    pub reader_alive: Arc<std::sync::atomic::AtomicBool>,
    /// "Typewriter" state for demo sessions — bytes the
    /// `tick_demo_typer` call drains into `parser` incrementally so
    /// the pane animates as if a real shell were echoing typed input
    /// and producing output. `None` for real sessions (which get
    /// bytes from the PTY reader thread).
    pub demo_typer: Option<Mutex<DemoTyperState>>,
}

/// Bookkeeping for the demo-mode typewriter. Bytes get fed into
/// `parser` by `ShellSession::tick_demo_typer`, called from the run
/// loop's 30 fps `shell_tick`. The pacing model is simple: drain
/// `CHARS_PER_TICK` bytes per tick, and after a chunk that contained a
/// newline, hold for `NEWLINE_PAUSE_TICKS` ticks before resuming.
/// Tuning targets ~3-5 seconds total for a typical session transcript.
pub(crate) struct DemoTyperState {
    bytes: Vec<u8>,
    pos: usize,
    skip_ticks: u8,
}

impl DemoTyperState {
    /// Characters to drain into the parser per shell_tick.
    /// 2 chars @ ~30fps ≈ 60 cps — feels like a real fast typist's
    /// pace. Higher (e.g. 6) looks robot-fast; lower (e.g. 1) looks
    /// labored.
    const CHARS_PER_TICK: usize = 2;
    /// Extra ticks (no emit) to hold after a newline. Gives each
    /// command/output line a beat of dwell before the next starts.
    /// 6 ticks * 33ms ≈ 200ms — closer to a natural "command landed,
    /// reading the output" beat than the prior 100ms.
    const NEWLINE_PAUSE_TICKS: u8 = 6;
}

impl ShellSession {
    /// Spawn `command` with the given `args` inside a fresh PTY sized for
    /// `rows × cols`. Returns once the subprocess has been launched; the
    /// background reader task continues feeding `vt100::Parser`.
    pub(crate) fn spawn(
        command: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        label: String,
    ) -> std::io::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let PtyPair { master, slave } = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);
        // Inherit current dir + relevant env vars. portable-pty starts with
        // an empty env by default, which would break AWS profile / region.
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        for (k, v) in std::env::vars() {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");

        let child = slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(format!("spawn: {e}")))?;
        drop(slave);

        let writer = master
            .take_writer()
            .map_err(|e| std::io::Error::other(format!("take_writer: {e}")))?;
        let mut reader = master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(format!("try_clone_reader: {e}")))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let parser_for_thread = parser.clone();
        let alive_for_thread = reader_alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser_for_thread.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
            alive_for_thread.store(false, std::sync::atomic::Ordering::Release);
        });

        Ok(Self {
            parser,
            writer: Some(writer),
            master: Some(master),
            child: Some(child),
            label,
            reader_alive,
            demo_typer: None,
        })
    }

    /// Build a fake demo-mode session: a `vt100::Parser` that will be
    /// fed `content` incrementally by `tick_demo_typer`, no real PTY
    /// behind it. The typewriter pacing makes the pane animate as
    /// the operator-realistic commands type themselves out — VHS
    /// captures look like a real session rather than a static dump.
    /// Keystrokes into a demo session are silently dropped (`send()`
    /// no-ops when `writer` is `None`); F12 (and Esc, demo-only)
    /// detaches as usual.
    pub(crate) fn demo(label: String, content: &str, rows: u16, cols: u16) -> Self {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let demo_typer = Some(Mutex::new(DemoTyperState {
            bytes: content.as_bytes().to_vec(),
            pos: 0,
            skip_ticks: 0,
        }));
        Self {
            parser,
            writer: None,
            master: None,
            child: None,
            label,
            // Demo sessions stay alive until the operator detaches.
            reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            demo_typer,
        }
    }

    /// Drain a chunk of the demo session's pending bytes into the
    /// parser. No-op for real sessions (which have `demo_typer = None`)
    /// and for demo sessions that have already played out their full
    /// content. The run loop's `shell_tick` (~30 fps when a shell is
    /// open) calls this every frame so the typewriter animates at the
    /// expected pace.
    pub(crate) fn tick_demo_typer(&self) {
        let Some(typer_mtx) = self.demo_typer.as_ref() else {
            return;
        };
        let Ok(mut s) = typer_mtx.lock() else { return };
        if s.skip_ticks > 0 {
            s.skip_ticks -= 1;
            return;
        }
        if s.pos >= s.bytes.len() {
            return;
        }
        let end = (s.pos + DemoTyperState::CHARS_PER_TICK).min(s.bytes.len());
        let chunk: Vec<u8> = s.bytes[s.pos..end].to_vec();
        s.pos = end;
        let had_newline = chunk.contains(&b'\n');
        // Drop the typer lock before grabbing parser to avoid a
        // theoretical deadlock if anything ever takes them in the
        // opposite order. Today nothing does, but cheap insurance.
        drop(s);
        if let Ok(mut p) = self.parser.lock() {
            p.process(&chunk);
        }
        if had_newline {
            if let Ok(mut s) = typer_mtx.lock() {
                s.skip_ticks = DemoTyperState::NEWLINE_PAUSE_TICKS;
            }
        }
    }

    /// Forward bytes from a keyboard event to the PTY master. No-op on a
    /// demo session (no PTY behind it).
    pub(crate) fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.write_all(bytes)?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Resize the PTY to match a new pane size. No-op on failure. For a
    /// demo session, only the parser's `set_size` runs (no PTY to
    /// resize).
    pub(crate) fn resize(&self, rows: u16, cols: u16) {
        if let Some(master) = self.master.as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// True when the subprocess has exited and the reader thread has
    /// returned. The run loop checks this each frame and tears down the
    /// session when the user's `exit` / ^D propagates. Demo sessions
    /// never report dead (they're closed explicitly via F12 + the
    /// existing `close_shell_session` path).
    pub(crate) fn is_dead(&self) -> bool {
        !self.reader_alive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Best-effort kill of the subprocess. Called when the user explicitly
    /// closes the pane (vs. F12 detach which keeps the session live).
    /// No-op on a demo session.
    pub(crate) fn kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

/// Translate a crossterm KeyEvent into the byte sequence a Unix terminal
/// emulator would send. Covers the common keys; falls back to UTF-8
/// encoding of the printable character. Modifier handling:
///   Ctrl-A..Z → 0x01..0x1A (xterm convention)
///   Alt-K     → ESC then K
///   Plain     → the character bytes
pub(crate) fn key_event_to_bytes(key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mods = key.modifiers;
    let mut out = Vec::with_capacity(4);
    match key.code {
        KeyCode::Char(c) => {
            if mods.contains(KeyModifiers::CONTROL) {
                let upper = c.to_ascii_uppercase() as u32;
                if (b'A' as u32..=b'Z' as u32).contains(&upper) {
                    out.push((upper - b'A' as u32 + 1) as u8);
                } else if c == ' ' {
                    out.push(0);
                } else {
                    return None;
                }
            } else if mods.contains(KeyModifiers::ALT) {
                out.push(0x1b);
                out.extend(c.to_string().as_bytes());
            } else {
                out.extend(c.to_string().as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out.extend(b"\x1b[A"),
        KeyCode::Down => out.extend(b"\x1b[B"),
        KeyCode::Right => out.extend(b"\x1b[C"),
        KeyCode::Left => out.extend(b"\x1b[D"),
        KeyCode::Home => out.extend(b"\x1b[H"),
        KeyCode::End => out.extend(b"\x1b[F"),
        KeyCode::PageUp => out.extend(b"\x1b[5~"),
        KeyCode::PageDown => out.extend(b"\x1b[6~"),
        KeyCode::Delete => out.extend(b"\x1b[3~"),
        KeyCode::Insert => out.extend(b"\x1b[2~"),
        KeyCode::F(n) => match n {
            1 => out.extend(b"\x1bOP"),
            2 => out.extend(b"\x1bOQ"),
            3 => out.extend(b"\x1bOR"),
            4 => out.extend(b"\x1bOS"),
            5 => out.extend(b"\x1b[15~"),
            6 => out.extend(b"\x1b[17~"),
            7 => out.extend(b"\x1b[18~"),
            8 => out.extend(b"\x1b[19~"),
            9 => out.extend(b"\x1b[20~"),
            10 => out.extend(b"\x1b[21~"),
            11 => out.extend(b"\x1b[23~"),
            _ => return None,
        },
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::key_event_to_bytes;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn plain_char_passes_through() {
        let k = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(key_event_to_bytes(&k).unwrap(), b"a");
    }

    #[test]
    fn ctrl_c_is_0x03() {
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_event_to_bytes(&k).unwrap(), vec![0x03]);
    }

    #[test]
    fn alt_x_is_esc_prefixed() {
        let k = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(key_event_to_bytes(&k).unwrap(), vec![0x1b, b'x']);
    }

    #[test]
    fn arrow_keys_emit_csi_sequences() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key_event_to_bytes(&up).unwrap(), b"\x1b[A");
    }

    #[test]
    fn backspace_is_0x7f() {
        let k = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(key_event_to_bytes(&k).unwrap(), vec![0x7f]);
    }
}

#[cfg(test)]
mod key_bytes_tests {
    use super::key_event_to_bytes;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // ── mutation-sweep triage, 2026-08-26 ────────────────────────────
    //
    // 26 survivors, every one a deletable arm. The fallback is
    // `_ => return None`, so a deleted arm means the key is silently
    // SWALLOWED — press Home in the embedded shell and nothing happens,
    // with no error and no clue why.
    //
    // Asserting 26 key→sequence pairs would be a copy of the table.
    // These are published xterm/VT100 sequences with real structure, so
    // the properties are pinned instead, and only the handful whose
    // exact value is load-bearing are spelled out.

    /// Every key the embedded shell forwards.
    const FORWARDED: &[KeyCode] = &[
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Esc,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Delete,
        KeyCode::Insert,
        KeyCode::F(1),
        KeyCode::F(2),
        KeyCode::F(3),
        KeyCode::F(4),
        KeyCode::F(5),
        KeyCode::F(6),
        KeyCode::F(7),
        KeyCode::F(8),
        KeyCode::F(9),
        KeyCode::F(10),
        KeyCode::F(11),
    ];

    fn bytes(code: KeyCode) -> Vec<u8> {
        key_event_to_bytes(&KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap_or_else(|| panic!("{code:?} produced nothing — the shell would swallow it"))
    }

    /// FORWARDED must cover every key the function actually handles.
    ///
    /// It catches the one-sided cases: an arm added to production
    /// without a list entry, and a list entry whose arm has gone.
    ///
    /// It does NOT catch a coordinated removal — deleting a key from
    /// both sides keeps the counts equal. That is inherent, not an
    /// oversight: no test comparing the code against a list derived
    /// from the code can tell "we dropped support for Insert
    /// deliberately" from "we dropped it by accident". Only a spec
    /// outside the code can, which is why the `ctl key` vocabulary in
    /// `control.rs` is pinned to `docs/headless.md` instead. These
    /// escape sequences have no such doc, so this is the available
    /// half. The price table in `app/tests/cost.rs` has the same shape
    /// and the same limit.
    #[test]
    fn forwarded_covers_every_arm_that_emits_bytes() {
        let src = std::fs::read_to_string("src/shell.rs").expect("read shell.rs");
        let body = src
            .split_once("\npub(crate) fn key_event_to_bytes")
            .expect("key_event_to_bytes moved or was renamed")
            .1;
        let body = body.split("\n#[cfg(test)]").next().unwrap_or(body);
        assert!(
            !body.contains("mod key_bytes_tests"),
            "the slice ran past the function into this test module"
        );
        // Every arm that emits bytes directly, at either level of the
        // match. The `Char(c)` arm is a block rather than an expression
        // and is covered by its own tests below.
        let arms = body.matches("=> out.").count();
        assert_eq!(
            arms,
            FORWARDED.len(),
            "key_event_to_bytes has {arms} byte-emitting arms and \
             FORWARDED lists {}. Add the new key to FORWARDED (or drop \
             the stale one) so it is actually checked.",
            FORWARDED.len()
        );
    }

    /// No forwarded key may be swallowed. This is what catches all 26
    /// deletable arms: a deleted arm falls to `_ => return None`.
    #[test]
    fn every_forwarded_key_produces_bytes() {
        for &code in FORWARDED {
            let b = bytes(code);
            assert!(!b.is_empty(), "{code:?} produced an empty sequence");
        }
    }

    /// And no two produce the SAME bytes — the shell could not tell them
    /// apart, so Home and End doing the same thing would look like a
    /// terminal bug rather than ours.
    #[test]
    fn no_two_forwarded_keys_collide() {
        let mut seen: Vec<(KeyCode, Vec<u8>)> = Vec::new();
        for &code in FORWARDED {
            let b = bytes(code);
            if let Some((other, _)) = seen.iter().find(|(_, prev)| *prev == b) {
                panic!("{code:?} and {other:?} both send {b:?}");
            }
            seen.push((code, b));
        }
        assert_eq!(seen.len(), FORWARDED.len());
    }

    /// The handful whose exact value is load-bearing, each with why.
    #[test]
    fn the_sequences_that_have_to_be_exact() {
        // CR, not LF. A shell's line discipline submits on carriage
        // return; `\n` leaves the line sitting there unexecuted.
        assert_eq!(bytes(KeyCode::Enter), b"\r");
        // DEL (0x7f), not BS (0x08) — what terminals actually send, and
        // what readline binds to backward-delete-char.
        assert_eq!(bytes(KeyCode::Backspace), vec![0x7f]);
        // Bare ESC introduces every other sequence below, so it must be
        // exactly one byte or they all shift.
        assert_eq!(bytes(KeyCode::Esc), vec![0x1b]);
        assert_eq!(bytes(KeyCode::Tab), b"\t");
    }

    /// Structure rather than values: everything but Enter/Tab/Backspace
    /// is an escape sequence, F1–F4 use SS3 and F5 upward use CSI. That
    /// split is the xterm convention, and getting it backwards sends a
    /// function key no shell recognises.
    #[test]
    fn escape_sequences_follow_the_xterm_shape() {
        for &code in FORWARDED {
            let b = bytes(code);
            match code {
                KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace => {
                    assert_eq!(b.len(), 1, "{code:?} is a bare control byte");
                }
                KeyCode::Esc => assert_eq!(b, vec![0x1b]),
                _ => assert_eq!(b[0], 0x1b, "{code:?} must start with ESC: {b:?}"),
            }
        }
        for n in 1..=4u8 {
            assert_eq!(&bytes(KeyCode::F(n))[..2], b"\x1bO", "F{n} uses SS3");
        }
        for n in 5..=11u8 {
            assert_eq!(&bytes(KeyCode::F(n))[..2], b"\x1b[", "F{n} uses CSI");
        }
    }

    /// Ctrl-Space is NUL, and Ctrl with anything that isn't a letter or
    /// space sends nothing rather than a wrong byte.
    #[test]
    fn ctrl_combinations_outside_the_alphabet() {
        let ctrl =
            |c: char| key_event_to_bytes(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        assert_eq!(ctrl(' ').unwrap(), vec![0x00], "Ctrl-Space is NUL");
        assert_eq!(ctrl('a').unwrap(), vec![0x01], "Ctrl-A is 0x01");
        assert_eq!(ctrl('z').unwrap(), vec![0x1a], "Ctrl-Z is 0x1a");
        assert_eq!(ctrl('A').unwrap(), vec![0x01], "case-insensitive");
        // Not in A..Z and not space → nothing, rather than a byte the
        // shell would act on.
        assert!(ctrl('1').is_none());
        assert!(ctrl('/').is_none());
    }

    /// Alt prefixes with ESC, which is how a terminal encodes Meta.
    #[test]
    fn alt_prefixes_with_escape() {
        let k = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(key_event_to_bytes(&k).unwrap(), b"\x1bb");
    }
}
