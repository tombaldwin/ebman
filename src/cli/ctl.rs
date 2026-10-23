//! `ebman ctl <op> [args] [--socket PATH]` — Unix-socket client for
//! driving a running ebman session. Pair with `--control-socket
//! PATH` on the running instance. See `src/control.rs` for the
//! server side + the supported op vocabulary.

use color_eyre::eyre::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::control;

const CTL_USAGE: &str = "usage: ebman ctl <screen|key|cmd|state|reload> [args]  [--socket PATH]\n\
     examples:\n  ebman ctl screen\n  ebman ctl key Down\n  ebman ctl key Ctrl+R\n  \
     ebman ctl cmd region eu-west-2\n  ebman ctl state";

/// Parsed `ebman ctl` invocation: the resolved socket path and the
/// assembled wire request (`HEAD [body]`). Pulling the parse out of
/// [`run`] lets the `--socket` handling + request assembly be tested
/// without opening a real Unix socket.
#[derive(Debug, PartialEq, Eq)]
struct CtlArgs {
    socket_path: std::path::PathBuf,
    request: String,
}

/// Pure parser for `ebman ctl`. `default_socket` is injected (rather
/// than calling `control::default_socket_path()` inline) so the parser
/// has no ambient dependency and tests can pin an exact path. Returns
/// `Err(msg)` for the two exit-2 usage paths: `--socket` without a
/// value, or no operation given.
fn parse_ctl_args(args: &[String], default_socket: std::path::PathBuf) -> Result<CtlArgs, String> {
    let mut socket_path = default_socket;
    let mut rest: Vec<&str> = Vec::new();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            match iter.next() {
                Some(p) if !p.starts_with("--") => socket_path = std::path::PathBuf::from(p),
                Some(p) => {
                    return Err(format!(
                        "ebman ctl: --socket expects a path, got flag '{p}'"
                    ))
                }
                None => return Err("ebman ctl: --socket requires a path".into()),
            }
        } else {
            rest.push(arg.as_str());
        }
    }
    if rest.is_empty() {
        return Err(CTL_USAGE.into());
    }
    let head = rest[0].to_ascii_uppercase();
    let body = rest[1..].join(" ");
    // The wire protocol is one request per line — an embedded newline
    // would silently truncate the request at the server's read_line.
    if head.contains('\n') || body.contains('\n') {
        return Err("ebman ctl: arguments must not contain newlines".into());
    }
    let request = if body.is_empty() {
        head
    } else {
        format!("{head} {body}")
    };
    Ok(CtlArgs {
        socket_path,
        request,
    })
}

/// `ebman ctl` — drive a running TUI over its control socket.
///
/// Exit 2 on a usage error, 1 when no TUI is listening or the command
/// is refused. The thin shell over [`ctl_body`].
pub async fn run(args: &[String]) -> Result<()> {
    let code = ctl_body(
        args,
        control::default_socket_path(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// `ebman ctl`'s whole behaviour against any default socket and any
/// writers. Returns the exit code.
pub(crate) async fn ctl_body(
    args: &[String],
    default_socket: std::path::PathBuf,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let CtlArgs {
        socket_path,
        request,
    } = match parse_ctl_args(args, default_socket) {
        Ok(parsed) => parsed,
        Err(msg) => {
            let _ = writeln!(err, "{msg}");
            return 2;
        }
    };
    let mut stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                err,
                "ebman ctl: connect to {} failed: {e}\n  hint: start ebman with `--control-socket {}`",
                socket_path.display(),
                socket_path.display()
            );
            return 1;
        }
    };
    let mut response = String::new();
    let exchanged = async {
        stream.write_all(request.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.read_to_string(&mut response).await
    }
    .await;
    if let Err(e) = exchanged {
        let _ = writeln!(err, "ebman ctl: {}: {e}", socket_path.display());
        return 1;
    }
    let _ = write!(out, "{response}");
    if !response.ends_with('\n') {
        let _ = writeln!(out);
    }
    if response.starts_with("ERR ") {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn sock() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/default.sock")
    }

    #[test]
    fn single_op_uppercases_head_and_uses_default_socket() {
        let p = parse_ctl_args(&argv(&["ctl", "screen"]), sock()).unwrap();
        assert_eq!(p.request, "SCREEN");
        assert_eq!(p.socket_path, sock());
    }

    #[test]
    fn op_with_body_joins_with_spaces() {
        let p = parse_ctl_args(&argv(&["ctl", "cmd", "region", "eu-west-2"]), sock()).unwrap();
        assert_eq!(p.request, "CMD region eu-west-2");
    }

    #[test]
    fn socket_flag_overrides_default_and_is_stripped_from_request() {
        let p = parse_ctl_args(
            &argv(&["ctl", "--socket", "/run/ebman.sock", "key", "Ctrl+R"]),
            sock(),
        )
        .unwrap();
        assert_eq!(p.socket_path, std::path::PathBuf::from("/run/ebman.sock"));
        assert_eq!(p.request, "KEY Ctrl+R");
    }

    #[test]
    fn socket_without_value_is_usage_error() {
        let err = parse_ctl_args(&argv(&["ctl", "--socket"]), sock()).unwrap_err();
        assert!(err.contains("--socket requires a path"), "got: {err}");
    }

    #[test]
    fn no_operation_is_usage_error() {
        let err = parse_ctl_args(&argv(&["ctl"]), sock()).unwrap_err();
        assert!(err.contains("usage:"), "got: {err}");
        // ...also when only --socket is given (rest ends up empty).
        let err2 = parse_ctl_args(&argv(&["ctl", "--socket", "/x.sock"]), sock()).unwrap_err();
        assert!(err2.contains("usage:"), "got: {err2}");
    }

    /// A one-shot server at a socket of its own that answers `reply`, and
    /// the request it saw.
    async fn serve_once(
        reply: &'static str,
    ) -> (std::path::PathBuf, tokio::task::JoinHandle<String>) {
        use tokio::io::AsyncBufReadExt;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ebman-ctl-{}-{n}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut line = String::new();
            tokio::io::BufReader::new(r)
                .read_line(&mut line)
                .await
                .unwrap();
            w.write_all(reply.as_bytes()).await.unwrap();
            line
        });
        (path, task)
    }

    async fn body(args: &[&str], socket: std::path::PathBuf) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        // Bounded: a request the server never sees whole (a missing
        // newline) leaves both ends waiting, and a hung test reports
        // nothing at all.
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ctl_body(&argv(args), socket, &mut out, &mut err),
        )
        .await
        .expect("ctl answered within 5s");
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn a_request_reaches_the_tui_and_its_answer_is_printed() {
        let (path, served) = serve_once("OK state follows").await;
        let (code, out, _) = body(&["ctl", "cmd", "region", "eu-west-2"], path.clone()).await;
        assert_eq!(code, 0);
        assert_eq!(out, "OK state follows\n", "a newline is added when missing");
        assert_eq!(served.await.unwrap(), "CMD region eu-west-2\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn exit_codes_follow_the_table() {
        let (path, _served) = serve_once("ERR unknown command\n").await;
        let (code, out, _) = body(&["ctl", "cmd", "nope"], path.clone()).await;
        assert_eq!(code, 1, "a refused command");
        assert_eq!(out, "ERR unknown command\n");
        let _ = std::fs::remove_file(&path);

        let (code, _, err) = body(&["ctl", "state"], path.clone()).await;
        assert_eq!(code, 1, "nothing listening");
        assert!(err.contains("--control-socket"), "names the fix: {err}");

        let (code, _, _) = body(&["ctl"], path).await;
        assert_eq!(code, 2, "usage");
    }
}
