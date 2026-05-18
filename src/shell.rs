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

/// A live embedded shell session. `parser` is the virtual terminal state,
/// `writer` is the PTY master input side, `_child` keeps the subprocess
/// alive for the lifetime of this struct.
pub struct ShellSession {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Human label shown in the pane title (e.g. the instance id).
    pub label: String,
    /// Output-reader background task. `Some` until the subprocess exits
    /// and the reader returns; then the run loop can decide to close.
    pub reader_alive: Arc<std::sync::atomic::AtomicBool>,
}

impl ShellSession {
    /// Spawn `command` with the given `args` inside a fresh PTY sized for
    /// `rows × cols`. Returns once the subprocess has been launched; the
    /// background reader task continues feeding `vt100::Parser`.
    pub fn spawn(
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
            writer,
            master,
            child,
            label,
            reader_alive,
        })
    }

    /// Forward bytes from a keyboard event to the PTY master.
    pub fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Resize the PTY to match a new pane size. No-op on failure.
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// True when the subprocess has exited and the reader thread has
    /// returned. The run loop checks this each frame and tears down the
    /// session when the user's `exit` / ^D propagates.
    pub fn is_dead(&self) -> bool {
        !self.reader_alive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Best-effort kill of the subprocess. Called when the user explicitly
    /// closes the pane (vs. F12 detach which keeps the session live).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

/// Translate a crossterm KeyEvent into the byte sequence a Unix terminal
/// emulator would send. Covers the common keys; falls back to UTF-8
/// encoding of the printable character. Modifier handling:
///   Ctrl-A..Z → 0x01..0x1A (xterm convention)
///   Alt-K     → ESC then K
///   Plain     → the character bytes
pub fn key_event_to_bytes(key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
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
