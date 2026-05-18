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
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "accept on control socket failed");
                    continue;
                }
            };
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
pub fn render_buffer_as_text(buf: &Buffer) -> String {
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
pub fn default_socket_path() -> PathBuf {
    let mut p = crate::util::cache_dir();
    p.push("control.sock");
    p
}

/// Parse a key spec into a crossterm `KeyEvent`. See the module-level docs
/// for the grammar. Returns `None` if no terminal key code could be parsed.
pub fn parse_key_spec(spec: &str) -> Option<KeyEvent> {
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
