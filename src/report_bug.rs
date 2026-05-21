//! Bug-report payload builder.
//!
//! `:report-bug` shows the operator a scrubbed report containing
//! version / OS / recent-log-lines / recent activity, and offers two
//! ways to actually file it:
//!
//!   - `y` — copy to clipboard (paste into a GitHub issue manually)
//!   - `b` — open a new GitHub issue in the browser, body pre-filled
//!     via URL query params
//!   - `esc` — cancel
//!
//! Ebman itself never sends the payload anywhere. The friction
//! (operator pastes into an issue) is the feature: it keeps the
//! tool defensible to operators running against regulated workloads.
//!
//! The scrubber runs over the assembled payload to redact the
//! obvious leaks: account IDs, ARNs, env names from the live env
//! list, profile / role names from the active context, and CNAMEs
//! / FQDNs. It's not bulletproof — a freeform error message could
//! still embed a customer name — which is why the operator sees
//! the exact payload before it leaves their machine.

use std::collections::BTreeSet;

/// Sensitive tokens supplied by the caller — pulled from the live App
/// state at report-build time. `account_id` is the operator's own
/// account, but it's still PII for "this is the org running ebman" so
/// gets the same treatment as the others.
#[derive(Debug, Clone, Default)]
pub struct ScrubContext {
    /// Captured but currently unused — the 12-digit-number pass
    /// already redacts the account ID from any payload text. Kept
    /// on the struct because future scrubbing rules (e.g. exact
    /// match against a `friendly_account_name`) will want it.
    #[allow(dead_code)]
    pub account_id: Option<String>,
    pub profile: Option<String>,
    pub region: Option<String>,
    /// Every env name currently in the in-memory table. Replaced
    /// with `[env]` so a stack trace that happened to format an env
    /// name doesn't leak it.
    pub env_names: BTreeSet<String>,
    /// Every application name currently in the in-memory table.
    pub app_names: BTreeSet<String>,
    /// CNAMEs / FQDNs from the env list — these are operator-domain
    /// and leak company names ("api.foo-corp.com").
    pub cnames: BTreeSet<String>,
}

/// Markdown-ish report payload, ready to be displayed in the
/// overlay or pasted into a GitHub issue. Caller passes pre-collected
/// pieces of context so this module stays free of `App` /
/// `tokio` dependencies — pure string assembly, testable in
/// isolation.
pub struct ReportInput<'a> {
    pub ebman_version: &'a str,
    pub os: &'a str,
    pub os_release: &'a str,
    pub icons: &'a str,
    pub theme: &'a str,
    pub refresh_interval_secs: u64,
    /// Last ~30 lines of `~/.cache/ebman/ebman.log`. Caller reads
    /// the file; this module assembles + scrubs.
    pub recent_log_lines: Vec<String>,
    /// Last ~10 operator-visible status / error messages. Picked
    /// from `App.message_log` so we mirror what the operator just
    /// saw on screen, not whatever internal tracing fired.
    pub recent_messages: Vec<String>,
    /// Most recent crash backtrace, if any. Pulled from
    /// `~/.cache/ebman/crash-*.log` by `latest_crash_log()`.
    pub recent_crash: Option<String>,
    /// `(tier, env_count, app_count, multi_regions)` summary —
    /// abstract numbers, not identifiers.
    pub env_count: usize,
    pub app_count: usize,
    pub multi_regions_count: usize,
    pub multi_account_enabled: bool,
}

/// Build the full report. Returns the assembled + scrubbed payload.
/// Two passes:
///   1. Format the structured sections into a markdown body.
///   2. Run `scrub` over the result so any payload-side
///      identifiers (e.g. ARNs in log lines) get redacted.
///
/// Caller decides what to do with the payload — render in an
/// overlay, copy to clipboard, or hand to a browser URL.
pub fn build_report(input: &ReportInput<'_>, ctx: &ScrubContext) -> String {
    let mut body = String::new();
    body.push_str("## ebman bug report\n\n");
    body.push_str("(Account IDs / ARNs / env names / profiles / CNAMEs are scrubbed.\n");
    body.push_str("Review before pasting; some freeform errors may still embed identifiers.)\n\n");

    body.push_str("### Environment\n");
    body.push_str(&format!("ebman: {}\n", input.ebman_version));
    body.push_str(&format!("os: {} / {}\n", input.os, input.os_release));
    body.push_str(&format!("icons: {}\n", input.icons));
    body.push_str(&format!("theme: {}\n", input.theme));
    body.push_str(&format!(
        "refresh_interval: {}s\n",
        input.refresh_interval_secs
    ));
    body.push_str(&format!(
        "scope: envs={}, apps={}, multi_regions={}, multi_account={}\n",
        input.env_count, input.app_count, input.multi_regions_count, input.multi_account_enabled
    ));
    body.push('\n');

    if !input.recent_messages.is_empty() {
        body.push_str("### Recent on-screen messages\n");
        body.push_str("```\n");
        for msg in &input.recent_messages {
            body.push_str(msg);
            body.push('\n');
        }
        body.push_str("```\n\n");
    }

    if !input.recent_log_lines.is_empty() {
        body.push_str("### Last 30 lines of ebman.log\n");
        body.push_str("```\n");
        for line in &input.recent_log_lines {
            body.push_str(line);
            body.push('\n');
        }
        body.push_str("```\n\n");
    }

    if let Some(crash) = &input.recent_crash {
        body.push_str("### Most recent panic backtrace\n");
        body.push_str("```\n");
        body.push_str(crash);
        if !crash.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n\n");
    }

    body.push_str("### What were you doing?\n");
    body.push_str("<!-- Describe the action that triggered the bug — what command,\n");
    body.push_str("     what env, what was the expected result. -->\n\n");

    scrub(&body, ctx)
}

/// Apply identifier scrubbing to `text`. Order matters: longer /
/// more-specific patterns first so the shorter ones don't eat
/// substrings of them. Pure; tested below.
pub fn scrub(text: &str, ctx: &ScrubContext) -> String {
    let mut out = text.to_string();

    // 1. ARNs — `arn:aws:<service>:<region>:<account>:<resource>`.
    // Catch-all regex would be cleaner but adding a regex pass for
    // one shape is overkill; iterate char-by-char.
    out = scrub_pattern(&out, "arn:aws:", "[arn]");
    out = scrub_pattern(&out, "arn:aws-us-gov:", "[arn]");
    out = scrub_pattern(&out, "arn:aws-cn:", "[arn]");

    // 2. Specific env names from the live list. Reverse-length-sort
    // so `prod-api-canary` doesn't get half-replaced by a shorter
    // `prod-api` match.
    let mut env_names: Vec<&String> = ctx.env_names.iter().collect();
    env_names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for name in &env_names {
        if !name.is_empty() {
            out = out.replace(name.as_str(), "[env]");
        }
    }

    // 3. Application names — same treatment.
    let mut app_names: Vec<&String> = ctx.app_names.iter().collect();
    app_names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for name in &app_names {
        if !name.is_empty() {
            out = out.replace(name.as_str(), "[app]");
        }
    }

    // 4. CNAMEs — typically `*.elb.amazonaws.com` patterns, but
    // EB also lets operators set arbitrary CNAMEs.
    let mut cnames: Vec<&String> = ctx.cnames.iter().collect();
    cnames.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for cname in &cnames {
        if !cname.is_empty() {
            out = out.replace(cname.as_str(), "[cname]");
        }
    }

    // 5. Account ID — 12 consecutive ASCII digits. Iterate byte-by-byte
    // so we don't pull in regex-machinery for one numeric pattern.
    out = scrub_12_digit_numbers(&out);

    // 6. The operator's own account / profile from context. The
    // 12-digit pass already caught the account, but the profile
    // name needs a literal replace.
    if let Some(p) = ctx.profile.as_ref() {
        if !p.is_empty() && p != "default" {
            // Skip "default" — replacing the literal word would
            // mangle every default-* token in unrelated output.
            out = out.replace(p, "[profile]");
        }
    }

    // 7. Region — informational only; replacing identifies country
    // code more crudely than the operator probably wants. Leave it.
    // (Reasoning: a bug report that says "us-east-1" doesn't tell
    // an attacker much; replacing it costs more useful context
    // for debugging than it buys in privacy.)
    let _ = &ctx.region;

    out
}

/// Replace any 12-digit ASCII number with `[account]`. Linear pass;
/// matches `\d{12}` style without pulling in the `regex` crate's
/// machinery for this single pattern.
fn scrub_12_digit_numbers(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            // Lookahead: 12 consecutive digits?
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let run = j - i;
            if run == 12 {
                // Replace. Only when the run is exactly 12 — longer
                // numeric strings (timestamps, sizes) are different
                // shape; shorter ones aren't account IDs.
                out.push_str("[account]");
                i = j;
                continue;
            } else {
                // Copy the digit run verbatim.
                out.push_str(&text[i..j]);
                i = j;
                continue;
            }
        }
        // SAFETY: text is &str, byte boundary aligned at i because
        // we only advance over ASCII digits above.
        out.push(c as char);
        i += 1;
    }
    out
}

/// Replace anything matching `prefix<continuation-until-whitespace>`
/// with `replacement`. The continuation gobbles non-whitespace,
/// non-quote, non-comma characters — sufficient for ARN-style tokens
/// that end at the next space / quote / comma in a log line.
fn scrub_pattern(text: &str, prefix: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(prefix) {
        out.push_str(&rest[..pos]);
        // Walk until a terminator.
        let after = &rest[pos..];
        let mut end = 0;
        for (i, c) in after.char_indices() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '"' | '\'' | ',' | ')' | ']') {
                end = i;
                break;
            }
            end = i + c.len_utf8();
        }
        out.push_str(replacement);
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// URL-encode a body for GitHub's `issues/new?body=` URL. Lighter
/// than pulling in `urlencoding` for one call site; only encodes
/// the characters GitHub's URL parser is sensitive to.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// Build the GitHub `issues/new` URL with the report pre-filled.
/// GitHub caps URL length at ~8192 chars; truncate the body if it
/// would push us past 7900 so the title + URL params still fit.
pub fn github_issue_url(repo: &str, title: &str, body: &str) -> String {
    let max_body = 7900_usize.saturating_sub(title.len());
    let truncated = if body.len() > max_body {
        let truncated_at = body
            .char_indices()
            .take_while(|(i, _)| *i < max_body.saturating_sub(64))
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        let mut s = body[..truncated_at].to_string();
        s.push_str("\n\n…[body truncated for URL length; paste the full payload from the overlay]");
        s
    } else {
        body.to_string()
    };
    format!(
        "https://github.com/{repo}/issues/new?title={t}&body={b}",
        t = url_encode(title),
        b = url_encode(&truncated)
    )
}

/// Read the most recent crash log written by the panic hook. Returns
/// `None` when no crash logs exist. Helper so the report builder can
/// stay synchronous + pure.
pub fn latest_crash_log() -> Option<String> {
    let dir = crate::util::cache_dir();
    let mut crashes: Vec<std::fs::DirEntry> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .collect();
    crashes.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));
    let latest = crashes.last()?;
    std::fs::read_to_string(latest.path()).ok()
}

/// Read the tail of `~/.cache/ebman/ebman.log` — up to `n` lines.
/// Errors silently to an empty Vec; the report still ships, just
/// without log context.
pub fn tail_ebman_log(n: usize) -> Vec<String> {
    let mut path = crate::util::cache_dir();
    path.push("ebman.log");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(env_names: &[&str], app_names: &[&str], profile: Option<&str>) -> ScrubContext {
        ScrubContext {
            account_id: None,
            profile: profile.map(String::from),
            region: Some("eu-west-2".into()),
            env_names: env_names.iter().map(|s| (*s).to_string()).collect(),
            app_names: app_names.iter().map(|s| (*s).to_string()).collect(),
            cnames: BTreeSet::new(),
        }
    }

    #[test]
    fn scrub_redacts_12_digit_account_ids() {
        let ctx = ctx_with(&[], &[], None);
        let scrubbed = scrub("account 123456789012 hit a limit", &ctx);
        assert_eq!(scrubbed, "account [account] hit a limit");
    }

    #[test]
    fn scrub_leaves_short_numbers_alone() {
        let ctx = ctx_with(&[], &[], None);
        let scrubbed = scrub("port 8080 / 11 digits 12345678901", &ctx);
        assert!(scrubbed.contains("8080"));
        assert!(scrubbed.contains("12345678901"));
        assert!(!scrubbed.contains("[account]"));
    }

    #[test]
    fn scrub_redacts_arns() {
        let ctx = ctx_with(&[], &[], None);
        let scrubbed = scrub(
            "Role arn:aws:iam::123456789012:role/EbmanReadOnly does not have perms",
            &ctx,
        );
        assert!(scrubbed.contains("[arn]"));
        assert!(!scrubbed.contains("arn:aws"));
        assert!(!scrubbed.contains("EbmanReadOnly"));
    }

    #[test]
    fn scrub_redacts_env_names_longest_first() {
        let ctx = ctx_with(&["prod-api", "prod-api-canary"], &[], None);
        let scrubbed = scrub("prod-api-canary went red, prod-api is yellow", &ctx);
        // `prod-api-canary` matches first because it's longer —
        // ensures the canary substring doesn't get half-replaced.
        assert_eq!(scrubbed, "[env] went red, [env] is yellow");
    }

    #[test]
    fn scrub_redacts_profile_name_but_skips_default() {
        let ctx = ctx_with(&[], &[], Some("prod-aws"));
        let scrubbed = scrub("profile=prod-aws region=eu-west-2", &ctx);
        assert!(scrubbed.contains("[profile]"));
        assert!(!scrubbed.contains("prod-aws"));

        let ctx_default = ctx_with(&[], &[], Some("default"));
        let scrubbed = scrub("profile=default region=eu-west-2", &ctx_default);
        // 'default' is too generic to redact — leaving it alone.
        assert!(scrubbed.contains("default"));
    }

    #[test]
    fn url_encode_handles_special_chars() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("ARN: arn:aws"), "ARN%3A%20arn%3Aaws");
    }

    #[test]
    fn github_issue_url_pre_fills_title_and_body() {
        let url = github_issue_url("tombaldwin/ebman", "Crash on :why", "stack trace here");
        assert!(url.starts_with("https://github.com/tombaldwin/ebman/issues/new?"));
        assert!(url.contains("title=Crash"));
        assert!(url.contains("body=stack"));
    }

    #[test]
    fn github_issue_url_truncates_long_body() {
        let long_body = "x".repeat(20_000);
        let url = github_issue_url("tombaldwin/ebman", "Bug", &long_body);
        assert!(url.len() < 8500, "URL must stay under GitHub's ~8k limit");
        // Decode it back conceptually: truncated marker should be there.
        assert!(url.contains("body%20truncated"));
    }
}
