//! `ebman ctl <op> [args] [--socket PATH]` — Unix-socket client for
//! driving a running ebman session. Pair with `--control-socket
//! PATH` on the running instance. See `src/control.rs` for the
//! server side + the supported op vocabulary.

use color_eyre::eyre::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::control;

pub async fn run(args: &[String]) -> Result<()> {
    let mut socket_path = control::default_socket_path();
    let mut rest: Vec<&str> = Vec::new();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            if let Some(p) = iter.next() {
                socket_path = std::path::PathBuf::from(p);
            } else {
                eprintln!("ebman ctl: --socket requires a path");
                std::process::exit(2);
            }
        } else {
            rest.push(arg.as_str());
        }
    }
    if rest.is_empty() {
        eprintln!(
            "usage: ebman ctl <screen|key|cmd|state> [args]  [--socket PATH]\n\
             examples:\n  ebman ctl screen\n  ebman ctl key Down\n  ebman ctl key Ctrl+R\n  \
             ebman ctl cmd region eu-west-2\n  ebman ctl state"
        );
        std::process::exit(2);
    }
    let head = rest[0].to_ascii_uppercase();
    let body = rest[1..].join(" ");
    let request = if body.is_empty() {
        head
    } else {
        format!("{head} {body}")
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
