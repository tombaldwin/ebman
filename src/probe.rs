//! Pre-deploy / lint health-check probe: compose the URL from a
//! CNAME + health-check path and run a bounded `curl` HEAD against
//! it. Extracted from `app.rs` (0.27 architecture pass) — pure/IO
//! helpers with no TUI state, shared by the Deploy confirm modal,
//! `ebman lint --probe-live`, and (via the shared lint seams) the
//! MCP server.

/// Pure: compose a probe URL from a CNAME + a health-check path.
/// EB CNAMEs are bare hostnames (`api-prod.eba.amazonaws.com`);
/// the path may or may not start with a slash. We always emit a
/// `http://` URL because EB envs aren't HTTPS by default and a
/// missing TLS cert is a separate operator concern (the probe
/// shouldn't false-positive on that). Operators with custom TLS
/// can put their HTTPS CNAME directly into their LB listener
/// config; the probe is a development-mode best-effort signal.
pub(crate) fn build_health_check_probe_url(cname: &str, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{path}")
    };
    format!("http://{cname}{path}")
}

/// Run the pre-deploy probe via curl: HEAD + 2s timeout + follow
/// redirects (operators sometimes set the health-check-url to a
/// path that 301s to the real handler). Returns `Ok(())` for any
/// 2xx; `Err(<short reason>)` for non-2xx, timeout, or transport
/// errors. The reason string is surfaced in the modal so the
/// operator can decide whether the warning matters.
pub(crate) async fn run_health_check_probe(url: &str) -> Result<(), String> {
    use tokio::process::Command;
    let out = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-L",
            "--max-time",
            "2",
            "-w",
            "%{http_code}",
            "-I",
        ])
        .arg(url)
        .output()
        .await
        .map_err(|e| format!("could not invoke curl: {e}"))?;
    if !out.status.success() {
        // curl exit code 28 is the timeout; everything else is some
        // form of connect/resolve/protocol error. Surface the
        // stderr message when present so the operator gets a hint.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            return Err(stderr
                .lines()
                .next()
                .unwrap_or("transport error")
                .to_string());
        }
        return Err(format!("curl exit {}", out.status.code().unwrap_or(-1)));
    }
    let code_str = String::from_utf8_lossy(&out.stdout);
    let code: u16 = code_str
        .trim()
        .parse()
        .map_err(|_| format!("unparseable status `{}`", code_str.trim()))?;
    classify_health_check_status(code)
}

/// Pure status-code classifier — surfaced as a separate helper so
/// the matrix is unit-testable without invoking curl.
pub(crate) fn classify_health_check_status(code: u16) -> Result<(), String> {
    match code {
        200..=299 => Ok(()),
        0 => Err("no response (transport error)".into()),
        300..=399 => Err(format!("HTTP {code} (redirect — curl was told to follow)")),
        // 4xx / 5xx — the live URL responded but with an error.
        // The most common offender is 404 (path not configured on
        // the new app version) which is exactly the auto-rollback
        // footgun we're trying to warn about.
        400..=599 => Err(format!("HTTP {code}")),
        // Forward-compat: any other range is a server doing
        // something unusual. Surface it verbatim.
        _ => Err(format!("HTTP {code}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_health_check_probe_url_normalises_path() {
        // Path starting with `/` passes through.
        assert_eq!(
            super::build_health_check_probe_url("api.example.com", "/healthz"),
            "http://api.example.com/healthz"
        );
        // Path without leading `/` gets one prepended (covers operators
        // who configure `healthz` rather than `/healthz` in EB).
        assert_eq!(
            super::build_health_check_probe_url("api.example.com", "healthz"),
            "http://api.example.com/healthz"
        );
        // Empty path collapses to `/` — EB's default health check.
        assert_eq!(
            super::build_health_check_probe_url("api.example.com", ""),
            "http://api.example.com/"
        );
        // Root path round-trips.
        assert_eq!(
            super::build_health_check_probe_url("api.example.com", "/"),
            "http://api.example.com/"
        );
    }

    #[test]
    fn classify_health_check_status_treats_2xx_as_ok_and_others_as_warning() {
        // 2xx range — all clear.
        assert!(super::classify_health_check_status(200).is_ok());
        assert!(super::classify_health_check_status(201).is_ok());
        assert!(super::classify_health_check_status(299).is_ok());
        // 0 means curl couldn't even connect.
        let err = super::classify_health_check_status(0).unwrap_err();
        assert!(err.contains("no response"));
        // 3xx still warns because we already pass -L to curl;
        // seeing a redirect in the final code means a loop.
        let err = super::classify_health_check_status(301).unwrap_err();
        assert!(err.contains("301"));
        // 404 — the canonical auto-rollback footgun.
        let err = super::classify_health_check_status(404).unwrap_err();
        assert!(err.contains("404"));
        // 5xx — server is up but failing.
        let err = super::classify_health_check_status(503).unwrap_err();
        assert!(err.contains("503"));
    }
}
