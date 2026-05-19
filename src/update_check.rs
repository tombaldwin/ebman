//! Check crates.io for a newer release of `ebman`. The check fires once at
//! startup and writes its result to the App via an `AppMsg::UpdateCheck`. We
//! don't pull in `reqwest` for this — `curl` is already a dependency of the
//! log-tail feature, so a one-shot subprocess fits in the same budget.

/// Result returned by `check_async`. `None` means "no newer release" /
/// "couldn't reach crates.io" / "version string didn't parse" — anything that
/// shouldn't bother the user.
#[derive(Debug, Clone)]
pub struct LatestRelease {
    pub version: String,
}

/// Fire-and-forget update check. Spawns `curl`, parses the JSON response, and
/// compares against `CARGO_PKG_VERSION`. Returns `Some(latest)` only when a
/// strictly-newer semver is available. Anything that goes wrong silently maps
/// to `None`.
pub async fn check_async() -> Option<LatestRelease> {
    use tokio::process::Command;
    // 10s cap so a stalled DNS / network doesn't keep the task alive.
    let out = Command::new("curl")
        .args([
            "-s",
            "-S",
            "--max-time",
            "10",
            "-H",
            "Accept: application/json",
            "-H",
            concat!("User-Agent: ebman/", env!("CARGO_PKG_VERSION")),
            "https://crates.io/api/v1/crates/ebman",
        ])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let latest = extract_max_stable_version(&body)?;
    let current = env!("CARGO_PKG_VERSION");
    if is_newer(&latest, current) {
        Some(LatestRelease { version: latest })
    } else {
        None
    }
}

/// Pull `crate.max_stable_version` out of the JSON response without bringing
/// in a JSON parser. Crates.io's response shape is well-known and won't shift
/// silently — and we already silently degrade on any mismatch.
fn extract_max_stable_version(body: &str) -> Option<String> {
    let key = "\"max_stable_version\"";
    let i = body.find(key)?;
    let after = &body[i + key.len()..];
    let colon = after.find(':')?;
    let after = &after[colon + 1..];
    let q1 = after.find('"')?;
    let rest = &after[q1 + 1..];
    let q2 = rest.find('"')?;
    Some(rest[..q2].to_string())
}

/// Semver-style ordering on dotted decimal versions. "0.2.0" > "0.1.5".
/// Non-numeric tails (e.g. `-rc1`) are sorted lexicographically as a fallback;
/// we don't ship pre-releases ourselves so this isn't load-bearing.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| {
        s.split('.')
            .map(|p| p.split('-').next().unwrap_or(p).parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let a = parse(candidate);
    let b = parse(current);
    for i in 0..a.len().max(b.len()) {
        let ai = *a.get(i).unwrap_or(&0);
        let bi = *b.get(i).unwrap_or(&0);
        if ai > bi {
            return true;
        }
        if ai < bi {
            return false;
        }
    }
    false
}

/// Channel the running binary was installed through, inferred from its path
/// on disk. The notice in the header always says "an upgrade exists"; this
/// is how we tell the operator *what to run* to apply it. None of the
/// patterns are bulletproof — we lean on the most-common defaults and fall
/// back to a "download from releases page" hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChannel {
    /// `/<prefix>/Cellar/ebman/<ver>/bin/ebman` — Homebrew. `brew upgrade ebman`.
    Homebrew,
    /// `~/.cargo/bin/ebman` — `cargo install ebman`.
    Cargo,
    /// Anywhere else (downloaded tarball on $PATH, `cargo install --path .`
    /// from a checkout, custom layout). Suggest the GH releases page.
    Standalone,
}

impl InstallChannel {
    /// Classify a binary path. Pure for testability.
    pub fn from_path(path: &std::path::Path) -> Self {
        let s = path.to_string_lossy();
        if s.contains("/Cellar/ebman/") {
            return InstallChannel::Homebrew;
        }
        // Cargo's default install dir is $CARGO_HOME/bin (often ~/.cargo/bin).
        // Look for the `.cargo/bin/` segment — covers default + custom
        // $CARGO_HOME layouts.
        if s.contains("/.cargo/bin/") || s.contains("/cargo/bin/") {
            return InstallChannel::Cargo;
        }
        InstallChannel::Standalone
    }

    /// Shell command that upgrades the binary in-place for this channel.
    /// Returned as a single-line string suitable for the operator to copy.
    pub fn upgrade_command(self) -> &'static str {
        match self {
            InstallChannel::Homebrew => "brew upgrade ebman",
            InstallChannel::Cargo => "cargo install ebman --force",
            // Tarball install — no in-place upgrade; point at the latest
            // release page. Operator downloads + verifies + replaces.
            InstallChannel::Standalone => {
                "download the latest binary from https://github.com/tombaldwin/ebman/releases/latest"
            }
        }
    }
}

/// Detect the running binary's install channel. Best-effort; returns
/// `Standalone` if we can't read our own path.
pub fn detect_install_channel() -> InstallChannel {
    std::env::current_exe()
        .map(|p| InstallChannel::from_path(&p))
        .unwrap_or(InstallChannel::Standalone)
}

#[cfg(test)]
mod tests {
    use super::{extract_max_stable_version, is_newer, InstallChannel};
    use std::path::Path;

    #[test]
    fn is_newer_compares_dotted_semver() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn is_newer_handles_prerelease_tails() {
        // Pre-release tails on the candidate are stripped; we compare numeric only.
        assert!(is_newer("0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
    }

    #[test]
    fn extract_max_stable_version_finds_field() {
        let body = r#"{"crate":{"id":"ebman","max_stable_version":"0.4.2","other":"x"}}"#;
        assert_eq!(extract_max_stable_version(body).as_deref(), Some("0.4.2"));
    }

    #[test]
    fn extract_max_stable_version_missing_returns_none() {
        assert!(extract_max_stable_version(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    fn install_channel_detects_homebrew_cellar() {
        let p = Path::new("/opt/homebrew/Cellar/ebman/0.1.1/bin/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Homebrew);
        let p = Path::new("/usr/local/Cellar/ebman/0.1.1/bin/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Homebrew);
    }

    #[test]
    fn install_channel_detects_cargo_bin() {
        let p = Path::new("/Users/tom/.cargo/bin/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Cargo);
        // Alternative CARGO_HOME outside the home dir.
        let p = Path::new("/var/lib/cargo/bin/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Cargo);
    }

    #[test]
    fn install_channel_falls_back_to_standalone() {
        let p = Path::new("/usr/local/bin/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Standalone);
        let p = Path::new("/home/tom/Downloads/ebman");
        assert_eq!(InstallChannel::from_path(p), InstallChannel::Standalone);
    }

    #[test]
    fn upgrade_command_is_concrete_per_channel() {
        assert!(InstallChannel::Homebrew
            .upgrade_command()
            .contains("brew upgrade"));
        assert!(InstallChannel::Cargo
            .upgrade_command()
            .contains("cargo install"));
        assert!(InstallChannel::Standalone
            .upgrade_command()
            .contains("releases/latest"));
    }
}
