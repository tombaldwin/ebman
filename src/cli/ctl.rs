//! `ebman ctl <op> [args] [--socket PATH]` — Unix-socket client for
//! driving a running ebman session. Pair with `--control-socket
//! PATH` on the running instance. See `src/control.rs` for the
//! server side + the supported op vocabulary.

use color_eyre::eyre::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::control;

const CTL_USAGE: &str = "usage: ebman ctl <screen|key|cmd|state> [args]  [--socket PATH]\n\
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
            if let Some(p) = iter.next() {
                socket_path = std::path::PathBuf::from(p);
            } else {
                return Err("ebman ctl: --socket requires a path".into());
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

pub async fn run(args: &[String]) -> Result<()> {
    let CtlArgs {
        socket_path,
        request,
    } = match parse_ctl_args(args, control::default_socket_path()) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        color_eyre::eyre::eyre!(
            "ebman ctl: connect to {} failed: {e}\n  hint: start ebman with `--control-socket {}`",
            socket_path.display(),
            socket_path.display()
        )
    })?;
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{response}");
    if !response.ends_with('\n') {
        println!();
    }
    if response.starts_with("ERR ") {
        std::process::exit(1);
    }
    Ok(())
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
}
