//! App-specific path helpers for ebman. The generic bits
//! (`parse_bool`, `write_atomic`) live in `tui-common::util` and are
//! re-exported here so existing `crate::util::*` call sites keep
//! working unchanged.

use std::path::PathBuf;

pub use tui_common::util::{parse_bool, write_atomic};

/// XDG-style user config directory for ebman: `~/.config/ebman/`.
/// Falls back to the current working directory when `$HOME` is
/// unset (rare; mostly affects sandboxed test environments).
pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/ebman");
        return p;
    }
    PathBuf::from(".")
}

/// XDG-style user cache directory for ebman: `~/.cache/ebman/`.
/// Used for the application log, audit log, crash reports, and the
/// cost-explorer cache. Same fallback shape as `config_dir`.
pub fn cache_dir() -> PathBuf {
    // Tests must never touch the developer's real cache. This is not
    // hypothetical: a test that exercised the cost handler's persist
    // branch wrote a fabricated $1.00 row for a non-existent env into
    // `~/.cache/ebman/cost-unknown-us-east-1.toml` with a fresh
    // timestamp — and because the cache is only stale after 24 hours,
    // the next real session would have rendered that fiction and
    // skipped the fetch that would have corrected it.
    #[cfg(test)]
    {
        let mut p = std::env::temp_dir();
        p.push(format!("ebman-test-cache-{}", std::process::id()));
        p
    }
    #[cfg(not(test))]
    {
        match std::env::var_os("HOME") {
            Some(home) => {
                let mut p = PathBuf::from(home);
                p.push(".cache/ebman");
                p
            }
            None => PathBuf::from("."),
        }
    }
}

/// Convenience: `config_dir().join(name)`.
pub fn config_file(name: &str) -> PathBuf {
    config_dir().join(name)
}

/// Escape a string per the JSON string-escape spec. Returns the
/// escaped INNER content (no surrounding `"`) so callers can embed
/// the result inside a larger hand-rolled JSON body. Pair with
/// [`json_string`] when you want the value wrapped + escaped in
/// one call.
///
/// One canonical helper for the whole crate (lib + bin). Pre-0.16
/// there were six near-identical variants scattered across
/// `audit.rs` / `cli/mod.rs` / `lint.rs` / `app.rs` / `llm.rs`;
/// they're all routed through this now.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Escape + wrap in `"..."` for use as a complete JSON string
/// literal. Same escape semantics as [`json_escape`]; convenience
/// wrapper that adds the surrounding quotes.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&json_escape(s));
    out.push('"');
    out
}

/// Redaction policy shared by the MCP `get_option_settings`/`drift`/
/// `audit_log` tools, `ebman drift`, and the TUI `:drift` overlay:
/// env-var VALUES are secrets (`aws:elasticbeanstalk:application:
/// environment` carries DB URLs, API keys); keys stay visible so
/// config shape is inspectable. `DBPassword` matches the `:rds`
/// precedent. Everything else passes through.
pub fn redact_option_value(namespace: &str, name: &str, value: &str, redact: bool) -> String {
    if !redact {
        return value.to_string();
    }
    if namespace == "aws:elasticbeanstalk:application:environment"
        || name.eq_ignore_ascii_case("DBPassword")
    {
        return "(redacted)".to_string();
    }
    value.to_string()
}

/// Open (create-if-missing) a file for appending with 0600 perms —
/// operator-only. The cache artifacts this guards (audit.log with SSM
/// command strings, ebman.log, crash reports, explain cache) were
/// previously created with the umask default (usually 0644,
/// world-readable). Unix-only mode; other platforms get the default.
pub fn open_append_secure(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    // `mode()` applies only on CREATE — a pre-0.27 install's existing
    // 0644 file would stay world-readable forever without this
    // migration chmod (verified live on this machine's ebman.log).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(f)
}

/// `std::fs::write` with 0600 perms on create (see
/// [`open_append_secure`]).
pub fn write_secure(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(contents)
}

#[cfg(test)]
mod tests {
    use super::{json_escape, json_string};

    #[test]
    fn json_escape_escapes_quotes_backslashes_newlines_tabs() {
        assert_eq!(json_escape(""), "");
        assert_eq!(json_escape("hello"), "hello");
        assert_eq!(json_escape("with \"quotes\""), "with \\\"quotes\\\"");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\tb"), "a\\tb");
        assert_eq!(json_escape("a\rb"), "a\\rb");
        // Sub-0x20 control chars get \uXXXX escapes.
        assert_eq!(json_escape("\x01"), "\\u0001");
        assert_eq!(json_escape("\x07"), "\\u0007");
    }

    #[test]
    fn json_string_wraps_in_quotes() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(json_string("hello"), "\"hello\"");
        assert_eq!(json_string("with \"quotes\""), "\"with \\\"quotes\\\"\"");
    }

    #[test]
    fn json_string_round_trips_via_yaml_parser() {
        // YAML is a JSON superset; serde_yml parses both. Useful
        // cross-check that our hand-rolled escape is spec-compliant.
        let inputs = [
            "",
            "plain",
            "with \"quotes\" and \\ backslashes",
            "line1\nline2\twith tab",
            "control \x01\x02 chars",
        ];
        for input in inputs {
            let escaped = json_string(input);
            let parsed: String = serde_yml::from_str(&escaped)
                .unwrap_or_else(|e| panic!("json_string({input:?}) = {escaped} failed: {e}"));
            assert_eq!(parsed, input);
        }
    }
}

/// Split a comma-separated value into a clean `Vec<String>`: trim each
/// entry, drop the empties.
///
/// This shape was written out by hand in seventeen places — config
/// parsing, saved state, CLI flags, form input, EB option settings —
/// with identical semantics every time. One implementation means one
/// place to be sure about what happens to `"a,,b"` and `" a , b "`.
pub fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod split_csv_tests {
    use super::split_csv;

    #[test]
    fn trims_entries_and_drops_empties() {
        assert_eq!(
            split_csv("subnet-a,subnet-b, subnet-c, ,subnet-d"),
            vec!["subnet-a", "subnet-b", "subnet-c", "subnet-d"]
        );
    }

    #[test]
    fn empty_and_separator_only_input_yields_nothing() {
        assert!(split_csv("").is_empty());
        assert!(split_csv(",,,").is_empty());
        assert!(split_csv("  ,  ").is_empty());
    }

    #[test]
    fn a_single_entry_needs_no_separator() {
        assert_eq!(split_csv(" solo "), vec!["solo"]);
    }

    #[test]
    fn interior_whitespace_is_preserved() {
        assert_eq!(split_csv("a b, c d"), vec!["a b", "c d"]);
    }
}

/// Compare two dotted version strings by semver precedence.
///
/// Numeric core compared component-wise; a pre-release ranks below the
/// release it precedes, and pre-release identifiers compare left to
/// right (numeric numerically, numeric below alphanumeric, fewer fields
/// below more). Returns `Ordering` so it can drive `sort_by`.
///
/// Shared on purpose: this used to exist twice, once for EB platform
/// versions and once in `update_check`, and only one of them learned
/// the pre-release rule — so a binary running `0.30.0-rc1` was never
/// told that `0.30.0` had shipped.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Split off any pre-release suffix at the first `-`. Solution-stack
    // versions never have one (`stack_family_version` only accepts
    // all-digit dot parts), so this only matters for operator-authored
    // custom platform versions.
    fn split(s: &str) -> (&str, Option<&str>) {
        match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        }
    }
    let (a_core, a_pre) = split(a);
    let (b_core, b_pre) = split(b);

    let parse = |s: &str| {
        s.split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    let av = parse(a_core);
    let bv = parse(b_core);
    for i in 0..av.len().max(bv.len()) {
        let aa = av.get(i).and_then(|x| *x);
        let bb = bv.get(i).and_then(|x| *x);
        match (aa, bb) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                o => return o,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => break,
        }
    }
    // Cores tie. Semver: a pre-release ranks BELOW the release it
    // precedes, so `1.0.0-rc1` must not be offered as newer than
    // `1.0.0` in the platform-upgrade picker. The old code fell
    // straight through to `a.cmp(b)` here, and lexicographically
    // "1.0.0-rc1" > "1.0.0" because it's a prefix extension.
    match compare_prerelease(a_pre, b_pre) {
        Ordering::Equal => a.cmp(b),
        o => o,
    }
}

/// Semver pre-release precedence, for two versions whose cores are equal.
///
/// Absent beats present (a release outranks its own pre-release), then
/// dot-separated identifiers compare left to right: numeric ones
/// numerically, numeric below alphanumeric, alphanumeric ASCII-wise, and
/// a shorter identifier list below a longer one when all else ties.
fn compare_prerelease(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Greater,
        (Some(_), None) => return Ordering::Less,
        (Some(a), Some(b)) => (a, b),
    };
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            // Fewer identifiers ranks below more, all else equal.
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

#[cfg(test)]
mod compare_versions_tests {
    use super::compare_versions;

    #[test]
    fn compare_versions_ranks_a_prerelease_below_its_release() {
        use std::cmp::Ordering;
        // The bug: cores tie, so the old code fell through to `a.cmp(b)`,
        // and lexicographically "1.0.0-rc1" > "1.0.0" because it's a prefix
        // extension. `:upgrade-platform` then offered an rc as the newest.
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_orders_prereleases_among_themselves() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0-rc2"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-alpha", "1.0.0-beta"),
            Ordering::Less
        );
        // Dot-separated identifiers compare left to right, numerically
        // where both are numeric.
        assert_eq!(
            compare_versions("1.0.0-rc.2", "1.0.0-rc.10"),
            Ordering::Less,
            "numeric identifiers compare as numbers, not strings"
        );
        // Numeric ranks below alphanumeric.
        assert_eq!(compare_versions("1.0.0-1", "1.0.0-alpha"), Ordering::Less);
        // Fewer identifiers ranks below more, all else equal.
        assert_eq!(compare_versions("1.0.0-rc", "1.0.0-rc.1"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0-rc1"), Ordering::Equal);
    }

    #[test]
    fn compare_versions_still_orders_release_cores() {
        use std::cmp::Ordering;
        // Regression guard: the pre-release work must not disturb the
        // ordering solution stacks rely on.
        assert_eq!(compare_versions("1.0.1", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("4.0.1", "4.0.1"), Ordering::Equal);
    }

    #[test]
    fn platform_picker_sorts_the_release_above_its_rc() {
        // The end-to-end shape: `list_compatible_platforms` sorts
        // descending with `compare_versions(&b.version, &a.version)`.
        let mut versions = vec!["1.0.0", "1.0.0-rc1", "1.0.1", "1.0.0-rc2"];
        versions.sort_by(|a, b| compare_versions(b, a));
        assert_eq!(versions, vec!["1.0.1", "1.0.0", "1.0.0-rc2", "1.0.0-rc1"]);
    }
}

// ── AWS partitions ─────────────────────────────────────────────────
//
// Partition knowledge was scattered: `aws.rs` mapped region prefixes to
// global-service endpoints, `report_bug.rs` listed ARN prefixes to
// scrub, `parse_access_denied` string-matched `arn:aws:sts::` and so
// silently failed everywhere else, and three console URLs hardcoded the
// commercial host. One table now, so adding a partition is one edit and
// the pieces can't disagree.

/// One AWS partition: how its regions are named, how its ARNs are
/// prefixed, where its global services endpoint, and where its console
/// lives (if we can name it).
pub struct Partition {
    /// The `arn:PARTITION:...` segment.
    pub arn: &'static str,
    /// Region-name prefixes belonging to this partition. Empty for the
    /// commercial partition, which is the fallback.
    prefixes: &'static [&'static str],
    /// Where global services (IAM, Cost Explorer) endpoint. They have
    /// one endpoint per partition, not per region.
    pub global_region: &'static str,
    /// Console hostname template, `{region}` substituted. `None` where
    /// we can't name it — the ISO partitions have consoles, but on
    /// networks whose hostnames aren't ours to guess, and a link that
    /// definitely doesn't resolve is worse than an honest refusal.
    pub console_host: Option<&'static str>,
}

/// Matching is first-match over `prefixes`, so ORDER MATTERS whenever
/// one prefix is a prefix of another. It happens not to be true of the
/// current set — `us-isob-` and `us-iso-` differ before the trailing
/// `-` — but "they all end in `-`" does not guarantee it (`us-` would
/// swallow every commercial `us-*` region), so a new entry has to be
/// checked against the others rather than appended blindly. The test
/// `no_prefix_shadows_another` does that check.
///
/// The commercial entry carries no prefixes and is reached through
/// `commercial()`, not by position.
pub const PARTITIONS: &[Partition] = &[
    Partition {
        arn: "aws-us-gov",
        prefixes: &["us-gov-"],
        global_region: "us-gov-west-1",
        console_host: Some("{region}.console.amazonaws-us-gov.com"),
    },
    Partition {
        arn: "aws-cn",
        prefixes: &["cn-"],
        global_region: "cn-north-1",
        console_host: Some("{region}.console.amazonaws.cn"),
    },
    Partition {
        arn: "aws-iso-b",
        prefixes: &["us-isob-"],
        global_region: "us-isob-east-1",
        console_host: None,
    },
    Partition {
        arn: "aws-iso-f",
        prefixes: &["us-isof-"],
        global_region: "us-isof-south-1",
        console_host: None,
    },
    Partition {
        arn: "aws-iso-e",
        prefixes: &["eu-isoe-"],
        global_region: "eu-isoe-west-1",
        console_host: None,
    },
    Partition {
        arn: "aws-iso",
        prefixes: &["us-iso-"],
        global_region: "us-iso-east-1",
        console_host: None,
    },
    Partition {
        arn: "aws-eusc",
        prefixes: &["eusc-"],
        // The only region the pinned SDK's partition data lists for
        // this partition; staying inside it is the property that
        // matters. Console host deliberately unset — the European
        // Sovereign Cloud has one, but not on a hostname worth
        // guessing at.
        global_region: "eusc-de-east-1",
        console_host: None,
    },
    Partition {
        arn: "aws",
        prefixes: &[],
        global_region: "us-east-1",
        console_host: Some("{region}.console.aws.amazon.com"),
    },
];

/// The commercial partition — the fallback for regions we don't
/// recognise, and the only entry with no prefixes of its own.
fn commercial() -> &'static Partition {
    PARTITIONS
        .iter()
        .find(|p| p.arn == "aws")
        .expect("commercial partition present in PARTITIONS")
}

impl Partition {
    /// The region prefixes this partition claims.
    #[cfg(test)]
    fn prefixes(&self) -> &'static [&'static str] {
        self.prefixes
    }
}

/// The partition a region belongs to. Unknown regions fall back to the
/// commercial partition, which is both the common case and the least
/// surprising guess.
pub fn partition_for_region(region: &str) -> &'static Partition {
    // Resolved by identity, not by position. This used to fall back to
    // `PARTITIONS.last()`, so appending a new partition — the "one
    // edit" the table's own header advertises — would silently redirect
    // every unrecognised region into it, breaking `:explain`, `:cost on`
    // and every console link for ordinary commercial operators.
    PARTITIONS
        .iter()
        .find(|p| p.prefixes.iter().any(|pre| region.starts_with(pre)))
        .unwrap_or_else(commercial)
}

/// The partition segment of an ARN — `aws` from `arn:aws:iam::…`.
pub fn arn_partition(arn: &str) -> Option<&str> {
    let rest = arn.strip_prefix("arn:")?;
    let seg = rest.split(':').next()?;
    (!seg.is_empty()).then_some(seg)
}

/// Every `arn:PARTITION:` prefix, for scrubbing ARNs out of text.
pub fn arn_prefixes() -> impl Iterator<Item = String> {
    PARTITIONS.iter().map(|p| format!("arn:{}:", p.arn))
}

/// The AWS console URL for a region, or `None` when the partition's
/// console host isn't one we can name.
pub fn console_base_url(region: &str) -> Option<String> {
    let host = partition_for_region(region).console_host?;
    Some(format!("https://{}", host.replace("{region}", region)))
}

#[cfg(test)]
mod partition_tests {
    use super::*;

    #[test]
    fn regions_map_to_their_partition() {
        assert_eq!(partition_for_region("eu-west-2").arn, "aws");
        assert_eq!(partition_for_region("us-gov-east-1").arn, "aws-us-gov");
        assert_eq!(partition_for_region("cn-northwest-1").arn, "aws-cn");
        assert_eq!(partition_for_region("us-iso-east-1").arn, "aws-iso");
        assert_eq!(partition_for_region("us-isob-east-1").arn, "aws-iso-b");
        assert_eq!(partition_for_region("us-isof-south-1").arn, "aws-iso-f");
        assert_eq!(partition_for_region("eu-isoe-west-1").arn, "aws-iso-e");
        // European Sovereign Cloud — carried by the pinned SDK's own
        // partition data, and missed by the first version of this table.
        assert_eq!(partition_for_region("eusc-de-east-1").arn, "aws-eusc");
        // Unknown regions fall back to commercial rather than failing.
        assert_eq!(partition_for_region("mars-central-1").arn, "aws");
        assert_eq!(partition_for_region("").arn, "aws");
    }

    #[test]
    fn arn_partition_reads_the_segment() {
        assert_eq!(arn_partition("arn:aws:iam::1:role/r"), Some("aws"));
        assert_eq!(
            arn_partition("arn:aws-us-gov:sts::1:assumed-role/R/S"),
            Some("aws-us-gov")
        );
        assert_eq!(arn_partition("arn:aws-cn:s3:::bucket"), Some("aws-cn"));
        assert_eq!(arn_partition("not-an-arn"), None);
        assert_eq!(arn_partition("arn:"), None);
    }

    #[test]
    fn console_urls_follow_the_partition() {
        assert_eq!(
            console_base_url("eu-west-2").as_deref(),
            Some("https://eu-west-2.console.aws.amazon.com")
        );
        assert_eq!(
            console_base_url("us-gov-west-1").as_deref(),
            Some("https://us-gov-west-1.console.amazonaws-us-gov.com")
        );
        assert_eq!(
            console_base_url("cn-north-1").as_deref(),
            Some("https://cn-north-1.console.amazonaws.cn")
        );
        // ISO consoles exist, but not on hostnames we can assert — a
        // link that definitely doesn't resolve is worse than saying so.
        assert!(console_base_url("us-iso-east-1").is_none());
    }

    #[test]
    fn arn_prefixes_covers_every_partition() {
        let prefixes: Vec<String> = arn_prefixes().collect();
        assert_eq!(prefixes.len(), PARTITIONS.len());
        assert!(prefixes.contains(&"arn:aws:".to_string()));
        assert!(prefixes.contains(&"arn:aws-us-gov:".to_string()));
        assert!(prefixes.contains(&"arn:aws-iso-b:".to_string()));
        assert!(
            prefixes.contains(&"arn:aws-eusc:".to_string()),
            "report_bug scrubs ARNs from this list — a missing partition \
             leaks account IDs into a public issue"
        );
    }
}

#[cfg(test)]
mod partition_ordering_tests {
    use super::PARTITIONS;

    #[test]
    fn no_prefix_shadows_another() {
        // Matching is first-match, so a prefix that is itself a prefix
        // of a later entry's would swallow it. The header used to claim
        // this was impossible because every prefix ends in `-`; it
        // isn't ("us-" would capture every commercial us-* region), so
        // check the cross-product rather than assert a rule of thumb.
        for (i, a) in PARTITIONS.iter().enumerate() {
            for pa in a.prefixes() {
                for (j, b) in PARTITIONS.iter().enumerate() {
                    if i >= j {
                        continue;
                    }
                    for pb in b.prefixes() {
                        assert!(
                            !pb.starts_with(pa),
                            "{} (entry {i}, prefix {pa:?}) shadows {} (entry {j}, prefix {pb:?}) \
                             — reorder so the more specific prefix comes first",
                            a.arn,
                            b.arn
                        );
                    }
                }
            }
        }
    }
}
