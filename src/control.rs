//! Optional Unix-socket control plane. When `ebman` is launched with
//! `--control-socket PATH`, this module opens a listener at PATH and accepts
//! one-shot requests:
//!
//! - `SCREEN\n` → returns a plain-text rendering of the current TUI frame.
//! - `KEY <SPEC>\n` → injects a synthesised key event into the run loop.
//!   Spec syntax: `Down`, `Up`, `Enter`, `Esc`, `Tab`, `BackTab`,
//!   `Backspace`, `Home`, `End`, `PageUp`, `PageDown`, `Space`, `F1`–`F12`,
//!   a single character, or `Char(j)`. Combine with `Ctrl+`, `Shift+`, `Alt+`.
//! - `CMD <text>\n` → runs the given `:command` (leading colon optional).
//! - `STATE\n` → returns a flat JSON object with current mode / profile /
//!   region / env count / selected env / load state.
//!
//! Each TCP connection is a single request → response → close cycle, so the
//! `ebman ctl …` subcommand can stay stateless (and so the server is robust
//! against half-disconnected clients).
//!
//! Security: the listener creates the socket with `0600` permissions so only
//! the current user can connect. Anyone with read access to that socket has
//! full control of the running ebman process, including dispatch of
//! destructive AWS actions — keep the socket path private.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

/// One request received over the control socket. The main run loop drains
/// these from an mpsc channel and dispatches them inside `tokio::select!`.
#[derive(Debug)]
pub enum ControlOp {
    /// Request a plain-text dump of the current TUI buffer. Reply via the
    /// oneshot with the rendered text (newline-separated rows).
    Screen(oneshot::Sender<String>),
    /// Inject a synthesised key event. The run loop dispatches it through
    /// the usual `handle_event(Event::Key(_))` path so all bindings apply.
    Key(KeyEvent),
    /// Run a `:command` body (with or without the leading colon).
    Command(String),
    /// Request a JSON snapshot of high-level App state.
    State(oneshot::Sender<String>),
    /// Re-exec the binary at `std::env::current_exe()` with the original
    /// argv. The run loop exits cleanly and `main()` then performs the
    /// `exec`, so the parent shell's terminal is reused by the new process.
    /// Pair with a prior `cargo build --release` to pick up source changes.
    Reload,
}

/// Open the Unix socket at `path` and spawn a listener task that translates
/// inbound text requests into `ControlOp` messages on `tx`. Silently returns
/// on bind failure after logging the error — the TUI must keep running.
pub fn spawn_listener(path: PathBuf, tx: mpsc::UnboundedSender<ControlOp>) {
    tokio::spawn(async move {
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, path = %path.display(), "control socket bind failed");
                return;
            }
        };
        restrict_socket_perms(&path);
        tracing::info!(path = %path.display(), "control socket listening");
        // Our own uid, read off the socket file we just created —
        // avoids a libc dependency for geteuid().
        let own_uid = socket_owner_uid(&path);
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "accept on control socket failed");
                    continue;
                }
            };
            // Peer-credential check on EVERY connection: the 0600
            // chmod happens after bind, and a connection racing that
            // window could sit in the backlog with the umask-default
            // perms. SO_PEERCRED closes the race (and hardens the
            // socket beyond file perms generally — the socket drives
            // arbitrary TUI commands including `readonly off`).
            if !peer_is_owner(&stream, own_uid) {
                tracing::warn!("control socket: rejected connection from another uid");
                continue;
            }
            let tx2 = tx.clone();
            tokio::spawn(async move {
                let _ = handle_connection(stream, tx2).await;
            });
        }
    });
}

#[cfg(unix)]
fn restrict_socket_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_socket_perms(_path: &Path) {}

/// The uid owning the freshly-bound socket file — i.e. our own uid.
/// `None` (metadata failed / non-unix) fails open to the perms-only
/// posture that shipped before the peer check.
#[cfg(unix)]
fn socket_owner_uid(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn socket_owner_uid(_path: &Path) -> Option<u32> {
    None
}

/// Whether a peer uid may drive this socket: the owner, or root.
///
/// Root is allowed because it could read the socket regardless, so
/// refusing it buys nothing.
///
/// Split out of [`peer_is_owner`] because it is the whole authorisation
/// decision and the socket around it made it unreachable. The
/// 2026-08-26 mutation sweep left every operator in it alive, including
/// `==` flipped to `!=` — which admits everyone EXCEPT the owner, on a
/// socket whose own comment notes it "drives arbitrary TUI commands
/// including `readonly off`".
pub(crate) fn uid_is_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid || peer_uid == 0
}

/// SO_PEERCRED check: the connecting process must run as the same uid
/// that owns the socket. Root (uid 0) is also allowed — it could read
/// the socket regardless.
#[cfg(unix)]
fn peer_is_owner(stream: &tokio::net::UnixStream, own_uid: Option<u32>) -> bool {
    let Some(own) = own_uid else { return true };
    match stream.peer_cred() {
        Ok(cred) => uid_is_allowed(cred.uid(), own),
        // Can't read peer creds: refuse rather than trust.
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn peer_is_owner(_stream: &tokio::net::UnixStream, _own_uid: Option<u32>) -> bool {
    true
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::UnboundedSender<ControlOp>,
) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let line = line.trim();
    if line.is_empty() {
        write_half.write_all(b"ERR empty request\n").await?;
        return Ok(());
    }
    let (head, tail) = match line.split_once(' ') {
        Some((h, t)) => (h, t),
        None => (line, ""),
    };
    match head.to_ascii_uppercase().as_str() {
        "SCREEN" => {
            let (otx, orx) = oneshot::channel();
            if tx.send(ControlOp::Screen(otx)).is_err() {
                write_half.write_all(b"ERR app dropped channel\n").await?;
                return Ok(());
            }
            match orx.await {
                Ok(text) => {
                    write_half.write_all(text.as_bytes()).await?;
                    if !text.ends_with('\n') {
                        write_half.write_all(b"\n").await?;
                    }
                }
                Err(_) => {
                    write_half.write_all(b"ERR snapshot cancelled\n").await?;
                }
            }
        }
        "STATE" => {
            let (otx, orx) = oneshot::channel();
            if tx.send(ControlOp::State(otx)).is_err() {
                write_half.write_all(b"ERR app dropped channel\n").await?;
                return Ok(());
            }
            match orx.await {
                Ok(text) => {
                    write_half.write_all(text.as_bytes()).await?;
                    write_half.write_all(b"\n").await?;
                }
                Err(_) => {
                    write_half.write_all(b"ERR state cancelled\n").await?;
                }
            }
        }
        "KEY" => match parse_key_spec(tail) {
            Some(ke) => {
                let _ = tx.send(ControlOp::Key(ke));
                write_half.write_all(b"OK\n").await?;
            }
            None => {
                write_half
                    .write_all(format!("ERR invalid key spec: {tail}\n").as_bytes())
                    .await?;
            }
        },
        "RELOAD" => {
            // Reply OK *before* the run loop tears down the TUI so the
            // client sees the exit signal cleanly. Best-effort; if mpsc
            // send fails the app is already shutting down.
            let _ = tx.send(ControlOp::Reload);
            write_half.write_all(b"OK\n").await?;
        }
        "CMD" => {
            let cmd = tail.trim().trim_start_matches(':').to_string();
            if cmd.is_empty() {
                write_half.write_all(b"ERR empty command\n").await?;
            } else {
                let _ = tx.send(ControlOp::Command(cmd));
                write_half.write_all(b"OK\n").await?;
            }
        }
        other => {
            write_half
                .write_all(
                    format!(
                        "ERR unknown op '{other}' (try: SCREEN | KEY <spec> | CMD <text> | STATE)\n"
                    )
                    .as_bytes(),
                )
                .await?;
        }
    }
    Ok(())
}

/// Render a ratatui [`Buffer`] to plain text by walking its cells row by row.
/// Trailing whitespace per line is stripped so the output is grep-friendly.
pub(crate) fn render_buffer_as_text(buf: &Buffer) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            row.push_str(cell.symbol());
        }
        lines.push(row.trim_end().to_string());
    }
    lines.join("\n")
}

/// Default control-socket path if the user doesn't pass one explicitly.
/// `~/.cache/ebman/control.sock`. The `ebman ctl` subcommand uses the same
/// default so the two halves rendezvous without any flag.
pub(crate) fn default_socket_path() -> PathBuf {
    let mut p = crate::util::cache_dir();
    p.push("control.sock");
    p
}

/// Parse a key spec into a crossterm `KeyEvent`. See the module-level docs
/// for the grammar. Returns `None` if no terminal key code could be parsed.
pub(crate) fn parse_key_spec(spec: &str) -> Option<KeyEvent> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut mods = KeyModifiers::NONE;
    let mut code: Option<KeyCode> = None;
    for piece in trimmed.split('+') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let lower = piece.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" | "^" => mods |= KeyModifiers::CONTROL,
            "shift" => mods |= KeyModifiers::SHIFT,
            "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
            "up" => code = Some(KeyCode::Up),
            "down" => code = Some(KeyCode::Down),
            "left" => code = Some(KeyCode::Left),
            "right" => code = Some(KeyCode::Right),
            "enter" | "return" => code = Some(KeyCode::Enter),
            "esc" | "escape" => code = Some(KeyCode::Esc),
            "tab" => code = Some(KeyCode::Tab),
            "backtab" | "shift+tab" => code = Some(KeyCode::BackTab),
            "backspace" => code = Some(KeyCode::Backspace),
            "delete" | "del" => code = Some(KeyCode::Delete),
            "home" => code = Some(KeyCode::Home),
            "end" => code = Some(KeyCode::End),
            "pageup" => code = Some(KeyCode::PageUp),
            "pagedown" => code = Some(KeyCode::PageDown),
            "space" => code = Some(KeyCode::Char(' ')),
            _ => {
                // Function keys: F1..F12 (case-insensitive)
                if let Some(num) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=12).contains(&num) {
                        code = Some(KeyCode::F(num));
                        continue;
                    }
                }
                // `Char(x)` explicit form preserves case.
                if let Some(inner) = piece
                    .strip_prefix("Char(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some(c) = inner.chars().next() {
                        code = Some(KeyCode::Char(c));
                        continue;
                    }
                }
                // Single-character fallback preserves original case so the
                // caller can distinguish `J` (events cursor) from `j` (table move).
                if piece.chars().count() == 1 {
                    let c = piece.chars().next()?;
                    code = Some(KeyCode::Char(c));
                }
            }
        }
    }
    code.map(|c| KeyEvent::new(c, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_char_is_case_sensitive() {
        let k = parse_key_spec("j").unwrap();
        assert_eq!(k.code, KeyCode::Char('j'));
        let k = parse_key_spec("J").unwrap();
        assert_eq!(k.code, KeyCode::Char('J'));
    }

    #[test]
    fn parse_arrow_keys() {
        assert_eq!(parse_key_spec("Down").unwrap().code, KeyCode::Down);
        assert_eq!(parse_key_spec("up").unwrap().code, KeyCode::Up);
    }

    #[test]
    fn parse_ctrl_combinations() {
        let k = parse_key_spec("Ctrl+R").unwrap();
        assert_eq!(k.code, KeyCode::Char('R'));
        assert!(k.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parse_function_keys() {
        assert_eq!(parse_key_spec("F2").unwrap().code, KeyCode::F(2));
        assert_eq!(parse_key_spec("f12").unwrap().code, KeyCode::F(12));
        // Out of range → no parse.
        assert!(parse_key_spec("F13").is_none());
    }

    #[test]
    fn parse_explicit_char_form() {
        let k = parse_key_spec("Char(:)").unwrap();
        assert_eq!(k.code, KeyCode::Char(':'));
    }

    #[test]
    fn parse_space_keyword() {
        assert_eq!(parse_key_spec("Space").unwrap().code, KeyCode::Char(' '));
    }

    #[test]
    fn parse_empty_is_none() {
        assert!(parse_key_spec("").is_none());
        assert!(parse_key_spec("   ").is_none());
    }
}

#[cfg(test)]
mod peer_auth_tests {
    use super::{peer_is_owner, uid_is_allowed};

    // ── mutation-sweep triage, 2026-08-26 ────────────────────────────
    //
    // The peer-credential check is the authorisation on a socket that,
    // by its own comment, "drives arbitrary TUI commands including
    // `readonly off`". Every operator in it survived the sweep.

    /// The owner and root, and nobody else.
    #[test]
    fn only_the_owner_and_root_may_drive_the_socket() {
        assert!(uid_is_allowed(501, 501), "the owner");
        assert!(uid_is_allowed(0, 501), "root could read the socket anyway");
        assert!(uid_is_allowed(0, 0), "root owning it is still root");

        // `==` flipped to `!=` on the first comparison admits everyone
        // EXCEPT the owner. This is the case that catches it.
        assert!(
            !uid_is_allowed(502, 501),
            "another user must not drive this socket"
        );
        assert!(!uid_is_allowed(1, 501), "nor another system account");
        // `||` flipped to `&&` would refuse the owner — checked by the
        // first assertion — and `== 0` flipped to `!= 0` would admit
        // every non-root uid, checked by these.
        assert!(!uid_is_allowed(65534, 501), "nor nobody(65534)");
    }

    /// The wiring: a real socket pair, with the check reading real peer
    /// credentials rather than a number we passed in.
    #[tokio::test]
    async fn peer_is_owner_reads_real_peer_credentials() {
        let (a, _b) = tokio::net::UnixStream::pair().expect("socket pair");
        // Safe: getuid() cannot fail and takes no arguments.
        let me = unsafe { libc::getuid() };

        assert!(peer_is_owner(&a, Some(me)), "our own uid owns this socket");
        assert!(
            peer_is_owner(&a, None),
            "no owner recorded (non-unix socket_owner_uid) means no check \
             to make — the file permissions are the only gate there"
        );

        // A different uid is refused. Skipped when running as root,
        // where the `uid == 0` arm legitimately allows everything.
        if me != 0 {
            assert!(
                !peer_is_owner(&a, Some(me.wrapping_add(1))),
                "a socket owned by someone else must refuse us"
            );
        }
    }

    /// The listener must still refuse before handing the connection on.
    ///
    /// `if !peer_is_owner(..)` had its `!` deletable, which inverts the
    /// gate: only *non*-owners would get through. Neither test above
    /// notices — they exercise the decision, not the call site.
    #[test]
    fn the_listener_refuses_before_serving() {
        let src = std::fs::read_to_string("src/control.rs").expect("read control.rs");
        // Anchor on the definition at column zero. The first version of
        // this guard searched for `pub(crate) fn spawn_listener` — the
        // wrong visibility — and `split_once` happily matched the
        // occurrence inside THIS test, so the slice it checked was its
        // own assertion string. It passed against the very mutation it
        // exists to catch.
        let listener = src
            .split_once("\npub fn spawn_listener")
            .expect("spawn_listener moved or was renamed")
            .1;
        let listener = listener.split("\n}\n").next().unwrap_or(listener);
        assert!(
            !listener.contains("mod peer_auth_tests"),
            "the slice ran past the function into this test module, so it \
             would be checking its own source"
        );
        assert!(
            listener.contains("if !peer_is_owner(&stream, own_uid) {"),
            "spawn_listener must refuse a connection whose peer is not the \
             socket owner, BEFORE spawning handle_connection. Dropping the \
             `!` inverts the gate and serves only other users."
        );
        assert!(
            listener.contains("continue;"),
            "and the refusal must skip the connection rather than fall \
             through to serving it"
        );
    }
}
