// Release metadata derived at build time from `CHANGELOG.md`.
//
// The changelog is the single source of truth for when a version
// shipped, so the release date is parsed out of it at compile time
// rather than kept in a constant somebody has to remember to bump.
// This repo has twice shown what the alternative costs: `server.json`
// sat at `0.32.0` while the crate was at `0.34.2`, and the changelog's
// own link references rotted for seventeen releases.
//
// `build.rs` `include!`s this file so the build script and the tested
// code are the same source, not a copy of each other.

/// Extract the release date for `version` from a Keep-a-Changelog body.
///
/// Matches a heading of the form `## [0.35.0] — 2026-08-27` and returns
/// the date. Returns `None` when the version has no heading yet (a
/// working tree whose `Cargo.toml` was bumped before the changelog
/// section was cut) or when the date is not an ISO `YYYY-MM-DD` — the
/// caller degrades to showing no date rather than rendering rubbish
/// into the header.
///
/// Deliberately tolerant about the separator: the file uses an em dash,
/// but a hyphen or a bare space would be a reasonable thing for a
/// future editor to type, and refusing those would silently blank the
/// date rather than fail loudly.
pub(crate) fn release_date_for<'a>(changelog: &'a str, version: &str) -> Option<&'a str> {
    let needle = format!("## [{version}]");
    let rest = changelog
        .lines()
        .find_map(|line| line.trim_end().strip_prefix(needle.as_str()))?;
    let date = rest
        .trim_start_matches([' ', '\u{2014}', '\u{2013}', '-'])
        .trim();
    is_iso_date(date).then_some(date)
}

/// `YYYY-MM-DD`, digits and hyphens in the right places. Not a calendar
/// check — this exists to stop a malformed heading reaching the header,
/// not to validate that February had 30 days.
pub(crate) fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Changelog

## [Unreleased]

## [0.35.0] — 2026-08-27

### Fixed

## [0.34.2] — 2026-08-25
";

    #[test]
    fn finds_the_date_for_the_named_version() {
        assert_eq!(release_date_for(SAMPLE, "0.35.0"), Some("2026-08-27"));
        assert_eq!(release_date_for(SAMPLE, "0.34.2"), Some("2026-08-25"));
    }

    #[test]
    fn a_version_with_no_heading_yet_has_no_date() {
        // Cargo.toml bumped, changelog section not cut yet. The header
        // shows the version alone rather than a wrong date.
        assert_eq!(release_date_for(SAMPLE, "0.36.0"), None);
    }

    #[test]
    fn the_unreleased_heading_is_not_a_date() {
        assert_eq!(release_date_for(SAMPLE, "Unreleased"), None);
    }

    #[test]
    fn a_version_that_is_a_prefix_of_another_does_not_match_it() {
        // `## [0.3.5]` must not be answered by `## [0.3.50]`, and the
        // `]` in the needle is what stops it. Worth pinning: the two
        // coexisted in this project's tag history.
        let cl = "## [0.3.50] — 2026-01-02\n";
        assert_eq!(release_date_for(cl, "0.3.5"), None);
        assert_eq!(release_date_for(cl, "0.3.50"), Some("2026-01-02"));
    }

    #[test]
    fn a_malformed_date_is_rejected_rather_than_rendered() {
        for bad in [
            "## [1.0.0] — soon\n",
            "## [1.0.0] — 2026-8-27\n",
            "## [1.0.0] — 26-08-2027\n",
            "## [1.0.0]\n",
        ] {
            assert_eq!(release_date_for(bad, "1.0.0"), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn a_hyphen_or_plain_space_separator_also_works() {
        assert_eq!(
            release_date_for("## [1.0.0] - 2026-08-27\n", "1.0.0"),
            Some("2026-08-27")
        );
        assert_eq!(
            release_date_for("## [1.0.0] 2026-08-27\n", "1.0.0"),
            Some("2026-08-27")
        );
    }

    #[test]
    fn iso_date_shape() {
        assert!(is_iso_date("2026-08-27"));
        assert!(!is_iso_date("2026-08-2"));
        assert!(!is_iso_date("2026/08/27"));
        assert!(!is_iso_date(""));
    }
}
