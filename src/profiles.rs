use std::{collections::BTreeSet, path::PathBuf};

/// Resolve the path to `~/.aws/config` (or whatever `AWS_CONFIG_FILE`
/// points at). Mirrors the AWS SDK provider chain so the `p` picker
/// and `:profile NAME` pre-check see the same files the SDK would
/// resolve against. Without this, operators using `aws-vault`, corp
/// env wrappers, or work-vs-personal splits via `AWS_CONFIG_FILE`
/// had their valid profiles refused by the 0.17.2 pre-check.
pub(crate) fn config_file_path() -> Option<PathBuf> {
    aws_file_path(
        std::env::var_os("AWS_CONFIG_FILE"),
        std::env::var_os("HOME"),
        ".aws/config",
    )
}

/// The pure half of [`config_file_path`] / [`credentials_file_path`]:
/// an explicit override wins, otherwise `$HOME` joined with `rel`.
///
/// Split out so the tests stop mutating the environment. The test below
/// set `HOME=/tmp/fake-home` and **never restored it**, so every test
/// that ran afterwards in the same process saw the fake — and several
/// production paths read `HOME` live. `ENV_LOCK` serialised the tests
/// that knew to take it; it could not undo a value left behind.
pub(crate) fn aws_file_path(
    override_var: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    rel: &str,
) -> Option<PathBuf> {
    if let Some(p) = override_var {
        return Some(PathBuf::from(p));
    }
    home.map(|h| PathBuf::from(h).join(rel))
}

/// Same shape as [`config_file_path`] but for the credentials file —
/// honours `AWS_SHARED_CREDENTIALS_FILE` with the standard
/// `~/.aws/credentials` fallback.
pub(crate) fn credentials_file_path() -> Option<PathBuf> {
    aws_file_path(
        std::env::var_os("AWS_SHARED_CREDENTIALS_FILE"),
        std::env::var_os("HOME"),
        ".aws/credentials",
    )
}

pub(crate) fn load_profiles() -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(p) = config_file_path() {
        read_profiles(&p, true, &mut names);
    }
    if let Some(p) = credentials_file_path() {
        read_profiles(&p, false, &mut names);
    }
    if names.is_empty() {
        names.insert("default".into());
    }
    names.into_iter().collect()
}

fn read_profiles(path: &PathBuf, config_style: bool, out: &mut BTreeSet<String>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with('[') || !line.ends_with(']') {
            continue;
        }
        let inner = &line[1..line.len() - 1].trim();
        // ~/.aws/config:        [default] or [profile foo] or [sso-session bar]
        // ~/.aws/credentials:   [default] or [foo]
        let name = if config_style {
            if let Some(rest) = inner.strip_prefix("profile ") {
                rest.trim().to_string()
            } else if *inner == "default" {
                "default".to_string()
            } else {
                continue; // skip [sso-session ...], [services ...], etc.
            }
        } else {
            inner.to_string()
        };
        if !name.is_empty() {
            out.insert(name);
        }
    }
}

pub(crate) const REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "af-south-1",
    "ap-east-1",
    "ap-south-1",
    "ap-south-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-southeast-5",
    "ca-central-1",
    "ca-west-1",
    "eu-central-1",
    "eu-central-2",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "il-central-1",
    "me-central-1",
    "me-south-1",
    "sa-east-1",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that mutate process-wide env vars. `cargo test`
    /// runs tests in parallel by default and `set_var` / `remove_var`
    /// are not thread-safe; the mutex keeps the env-var-touching tests
    /// from racing each other.

    #[test]
    fn an_explicit_override_wins_over_home() {
        // Both overrides, tested through the pure half. These used to
        // mutate the real environment under `ENV_LOCK`; the lock kept
        // them from racing each other, but not from racing anything
        // that didn't know to take it — and `app/tests/parsing.rs` was
        // mutating HOME with no lock at all, claiming in a `// SAFETY`
        // comment that `cargo test` is single-threaded. It isn't.
        let os = std::ffi::OsString::from;
        assert_eq!(
            aws_file_path(Some(os("/tmp/custom-aws-config")), None, ".aws/config"),
            Some(PathBuf::from("/tmp/custom-aws-config"))
        );
        assert_eq!(
            aws_file_path(
                Some(os("/tmp/custom-aws-creds")),
                Some(os("/home/someone")),
                ".aws/credentials"
            ),
            Some(PathBuf::from("/tmp/custom-aws-creds")),
            "the override wins even when HOME is set"
        );
    }

    #[test]
    fn config_file_path_falls_back_to_home_when_no_override() {
        // No env mutation, and no lock needed. The previous version set
        // HOME=/tmp/fake-home and never put it back, so every test that
        // ran after it in the same process saw the fake value.
        let os = std::ffi::OsString::from;
        assert_eq!(
            aws_file_path(None, Some(os("/tmp/fake-home")), ".aws/config"),
            Some(PathBuf::from("/tmp/fake-home/.aws/config"))
        );
        // An explicit override wins over HOME.
        assert_eq!(
            aws_file_path(
                Some(os("/custom/cfg")),
                Some(os("/tmp/fake-home")),
                ".aws/config"
            ),
            Some(PathBuf::from("/custom/cfg"))
        );
        // No HOME and no override → no path at all, rather than a
        // relative one rooted at the cwd.
        assert_eq!(aws_file_path(None, None, ".aws/config"), None);
    }
}
