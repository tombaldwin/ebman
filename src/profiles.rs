use std::{collections::BTreeSet, path::PathBuf};

pub fn load_profiles() -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        read_profiles(&home.join(".aws/config"), true, &mut names);
        read_profiles(&home.join(".aws/credentials"), false, &mut names);
    }
    if names.is_empty() {
        names.insert("default".into());
    }
    names.into_iter().collect()
}

fn read_profiles(path: &PathBuf, config_style: bool, out: &mut BTreeSet<String>) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
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
