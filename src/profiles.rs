use std::{collections::BTreeSet, path::PathBuf};

/// Resolve the path to `~/.aws/config` (or whatever `AWS_CONFIG_FILE`
/// points at). Mirrors the AWS SDK provider chain so the `p` picker
/// and `:profile NAME` pre-check see the same files the SDK would
/// resolve against. Without this, operators using `aws-vault`, corp
/// env wrappers, or work-vs-personal splits via `AWS_CONFIG_FILE`
/// had their valid profiles refused by the 0.17.2 pre-check.
pub fn config_file_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AWS_CONFIG_FILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aws/config"))
}

/// Same shape as [`config_file_path`] but for the credentials file —
/// honours `AWS_SHARED_CREDENTIALS_FILE` with the standard
/// `~/.aws/credentials` fallback.
pub fn credentials_file_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aws/credentials"))
}

pub fn load_profiles() -> Vec<String> {
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

pub const REGIONS: &[&str] = &[
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
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_file_path_honours_aws_config_file_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("AWS_CONFIG_FILE");
        std::env::set_var("AWS_CONFIG_FILE", "/tmp/custom-aws-config");
        let p = config_file_path().expect("AWS_CONFIG_FILE set should yield Some path");
        assert_eq!(p, PathBuf::from("/tmp/custom-aws-config"));
        if let Some(v) = prev {
            std::env::set_var("AWS_CONFIG_FILE", v);
        } else {
            std::env::remove_var("AWS_CONFIG_FILE");
        }
    }

    #[test]
    fn credentials_file_path_honours_aws_shared_credentials_file_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE");
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", "/tmp/custom-aws-creds");
        let p = credentials_file_path()
            .expect("AWS_SHARED_CREDENTIALS_FILE set should yield Some path");
        assert_eq!(p, PathBuf::from("/tmp/custom-aws-creds"));
        if let Some(v) = prev {
            std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", v);
        } else {
            std::env::remove_var("AWS_SHARED_CREDENTIALS_FILE");
        }
    }

    #[test]
    fn config_file_path_falls_back_to_home_when_no_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("AWS_CONFIG_FILE");
        std::env::remove_var("AWS_CONFIG_FILE");
        std::env::set_var("HOME", "/tmp/fake-home");
        let p = config_file_path().expect("HOME set should yield Some path");
        assert_eq!(p, PathBuf::from("/tmp/fake-home/.aws/config"));
        if let Some(v) = prev {
            std::env::set_var("AWS_CONFIG_FILE", v);
        }
    }
}
