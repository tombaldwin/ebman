//! Small string, parsing and formatting helpers used across the app.
//!
//! Pure functions only: no `App`, no I/O. Anything that grows a
//! dependency on app state belongs in `app.rs` or a `cmd_*` module.

use super::*;

/// Pure: expand a typed command line through the operator's
/// alias map. If the first whitespace-separated token matches a
/// key, swap it for the alias's expansion and keep any remaining
/// args (appended after the expansion). Single-level only — the
/// expanded line is NOT re-checked for further aliases, so
/// `alias.x = "x ..."` is safe (degenerates to "x ..." dispatched
/// once). Non-alias lines pass through unchanged.
///
/// Owned `String` return so the caller can borrow `.as_str()`
/// without lifetime gymnastics around the input slice.
pub(crate) fn expand_command_alias(
    line: &str,
    aliases: &std::collections::HashMap<String, String>,
) -> String {
    let line = line.trim();
    // A fast path, not a correctness guard — `||` vs `&&` here is
    // equivalent. With no aliases the lookup below misses and returns
    // `line` anyway; with an empty line the lookup is for `""`, which no
    // sane alias table holds, and that returns `line` too.
    if aliases.is_empty() || line.is_empty() {
        return line.to_string();
    }
    let mut parts = line.splitn(2, char::is_whitespace);
    let first = match parts.next() {
        Some(s) => s,
        None => return line.to_string(),
    };
    let Some(expansion) = aliases.get(first) else {
        return line.to_string();
    };
    match parts.next() {
        Some(rest) => format!("{expansion} {rest}"),
        None => expansion.clone(),
    }
}

pub(crate) fn humanize_short_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Parse a `:tag KEY [value tokens…]` argument list. Returns `Some((key,
/// value))` when there's at least a key and one value token. Value tokens
/// are joined with a single space — there's no shell-style quoting, since
/// we trust the operator and want the command bar to stay typeable.
pub(crate) fn parse_tag_args(rest: &[&str]) -> Option<(String, String)> {
    let key = (*rest.first()?).to_string();
    if rest.len() < 2 {
        return None;
    }
    let value = rest[1..].join(" ");
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Extract a "delta toast key" from text shaped like `▲2 Red` / `▼1 Yellow`.
/// Returns `Some(bucket_name)` when the text is a status-delta toast and we
/// want subsequent updates for the same bucket to replace rather than stack.
/// Pure function so it's easy to pin down in tests.
pub(crate) fn delta_toast_key(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if first != '▲' && first != '▼' {
        return None;
    }
    let rest: String = chars.collect();
    // Require at least one digit immediately after the arrow.
    let first_rest = rest.chars().next()?;
    if !first_rest.is_ascii_digit() {
        return None;
    }
    let bucket_start = rest.find(|c: char| !c.is_ascii_digit())?;
    let after_digits = &rest[bucket_start..];
    let bucket = after_digits.trim_start();
    if bucket.is_empty() || !bucket.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let word: String = bucket
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    Some(word)
}

/// Pure: redact a free-form string for display in the `:history` overlay
/// context header. Matches the `redact` helper in `ui.rs` (full-block
/// shaded chars preserving length) so the look is consistent — duplicated
/// rather than imported because the ui module's `redact` is private.
pub(crate) fn redact_for_log(value: &str, on: bool) -> String {
    if !on || value.is_empty() || value == "—" {
        return value.to_string();
    }
    "▓".repeat(value.chars().count())
}

/// Pure: extract the first single-quoted string that appears after
/// `needle` in `msg` (case-insensitive needle match). Returns None if
/// the needle isn't found or there's no quoted substring after it. Used
/// to pull `'build-142'` out of "Updating environment to use version
/// label 'build-142'.".
pub(crate) fn extract_quoted_after(msg: &str, needle: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let after = lower.find(&needle_lower)? + needle_lower.len();
    let tail = msg.get(after..)?;
    let start = tail.find('\'')?;
    let body = &tail[start + 1..];
    let end = body.find('\'')?;
    Some(body[..end].to_string())
}

pub(crate) fn parse_sort(raw: Option<&str>) -> (SortKey, bool) {
    let Some(s) = raw else {
        return (SortKey::App, false);
    };
    let (k, dir) = s.split_once(':').unwrap_or((s, "asc"));
    let key = SortKey::parse(k.trim()).unwrap_or(SortKey::App);
    let desc = dir.trim().eq_ignore_ascii_case("desc");
    (key, desc)
}

pub(crate) fn health_rank(h: &str) -> u8 {
    match h.to_lowercase().as_str() {
        "green" | "ok" => 0,
        "grey" | "gray" | "info" | "no data" | "pending" => 1,
        "yellow" | "warning" => 2,
        "red" | "severe" | "degraded" => 3,
        _ => 4,
    }
}

pub(crate) fn parse_toggle(arg: Option<&str>, current: bool) -> bool {
    match arg.map(str::to_ascii_lowercase).as_deref() {
        Some("on") | Some("true") | Some("yes") | Some("1") => true,
        Some("off") | Some("false") | Some("no") | Some("0") => false,
        _ => !current,
    }
}

pub(crate) fn scroll_apply(current: u16, delta: i32) -> u16 {
    let next = current as i32 + delta;
    next.max(0) as u16
}

/// Parse the optional trailing args of `:metric add LABEL NS NAME ...`.
/// Args after `NAME` are either a stat name (`Average`, `Sum`, ...) or a
/// dimension list (`InstanceId=i-abc,Foo=bar`). Any token containing `=`
/// is treated as dims; the other is stat. Returns `(stat, dims)` with
/// `stat` defaulting to `Average` and `dims` to empty when absent. Pure.
pub(crate) fn parse_metric_extra_args(args: &[&str]) -> (String, Vec<(String, String)>) {
    let mut stat: Option<String> = None;
    let mut dims: Vec<(String, String)> = Vec::new();
    for tok in args {
        if tok.contains('=') {
            for kv in tok.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    let k = k.trim();
                    let v = v.trim();
                    if !k.is_empty() && !v.is_empty() {
                        dims.push((k.to_string(), v.to_string()));
                    }
                }
            }
        } else if stat.is_none() {
            stat = Some(tok.to_string());
        }
    }
    (stat.unwrap_or_else(|| "Average".into()), dims)
}

/// Parse an `s3://bucket/key/with/slashes` URL into a `(bucket, key)`
/// tuple. Returns `None` if the input isn't an `s3://` URL or the bucket
/// or key is empty. Pure.
pub(crate) fn parse_s3_url(raw: &str) -> Option<(String, String)> {
    let rest = raw.strip_prefix("s3://")?;
    let (bucket, key) = rest.split_once('/')?;
    if bucket.is_empty() || key.is_empty() {
        return None;
    }
    Some((bucket.to_string(), key.to_string()))
}

/// Expand a leading `~/` to `$HOME/`. Other tilde forms (e.g. `~user`) are
/// left as-is; the operator gets a clear "can't read" error if they pass
/// something obscure. Pure for ease of testing.
pub(crate) fn expand_tilde(path: &str) -> String {
    expand_tilde_from(std::env::var_os("HOME"), path)
}

/// The pure half of [`expand_tilde`], with `$HOME` passed in.
///
/// Split out so the test doesn't have to mutate the environment. It did,
/// under a `// SAFETY: tests run single-threaded by default` comment
/// that was simply untrue — `cargo test` is parallel by default, and
/// `profiles.rs` says so in its own comment two files away while racing
/// this one for the same variable. Several production paths read `HOME`
/// live, so the race was reachable, not theoretical. `set_var` is also
/// `unsafe` under the 2024 env API and a hard error on that edition.
pub(crate) fn expand_tilde_from(home: Option<std::ffi::OsString>, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home {
            let mut p = std::path::PathBuf::from(home);
            p.push(rest);
            return p.display().to_string();
        }
    }
    path.to_string()
}

/// Derive a version label from a file path + a timestamp. Uses the
/// filename stem (everything before the last `.`) so `./build.zip` becomes
/// `build_1684512345`. Sanitises any chars EB rejects in version labels
/// (anything outside `[A-Za-z0-9_.-]`). Pure for testability.
pub(crate) fn derive_version_label(path: &str, unix_ts: i64) -> String {
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bundle");
    let sanitised: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitised}_{unix_ts}")
}

/// Pick the most useful CloudWatch Logs group for an env's `:logs-tail`
/// default. EB streams to a handful of groups per env (web.stdout.log,
/// nginx access, eb-engine.log, …); we prefer the app stdout because that's
/// where deploy / runtime output lives. Falls back to the first by name.
/// Pure for testability.
pub(crate) fn pick_default_log_group(groups: &[String]) -> Option<String> {
    const PRIORITIES: &[&str] = &[
        "/var/log/web.stdout.log",
        "/var/log/eb-engine.log",
        "/var/log/eb-hooks.log",
        "/var/log/nginx/access.log",
    ];
    for needle in PRIORITIES {
        if let Some(g) = groups.iter().find(|g| g.ends_with(needle)) {
            return Some(g.clone());
        }
    }
    groups.first().cloned()
}

/// Pull a `--flag VALUE` style named argument out of a `:command` `rest`
/// slice and parse it. Returns `None` if the flag is absent, the value is
/// missing, or parsing fails. Used by commands like `:logs-stream` that
/// take optional flags alongside their positional args. Pure.
pub(crate) fn parse_named_arg<T: std::str::FromStr>(rest: &[&str], flag: &str) -> Option<T> {
    let pos = rest.iter().position(|s| *s == flag)?;
    rest.get(pos + 1).and_then(|v| v.parse().ok())
}

/// Map a friendly env-metric "kind" to a `(metric_name, default_op, default_stat)`
/// triple. The user can override the operator on the CLI but the defaults
/// reflect "what you'd reasonably alarm on for this metric" — e.g. drop in
/// health (LE) vs spike in 5xx (GT). Pure so the unit tests don't need
/// AWS.
pub(crate) fn alarm_kind_to_metric(
    kind: &str,
) -> Option<(&'static str, &'static str, &'static str)> {
    match kind {
        "health" => Some(("EnvironmentHealth", "LessThanOrEqualToThreshold", "Maximum")),
        "4xx" | "req4xx" => Some(("ApplicationRequests4xx", "GreaterThanThreshold", "Sum")),
        "5xx" | "req5xx" => Some(("ApplicationRequests5xx", "GreaterThanThreshold", "Sum")),
        "latency" | "p90" => Some(("ApplicationLatencyP90", "GreaterThanThreshold", "Average")),
        _ => None,
    }
}

/// Wrap `text` at `width` columns, prefixing the first line with `lead` and
/// subsequent lines with `cont` so continuation visually flows under the
/// leader (e.g. `"↳ "` followed by aligned continuation). Greedy
/// word-wrap; falls back to hard-break inside a word that won't fit on its
/// own line. Pure for testability.
pub(crate) fn wrap_with_hanging_indent(text: &str, width: usize, lead: &str, cont: &str) -> String {
    if text.is_empty() {
        return lead.to_string();
    }
    let body_width = width.saturating_sub(lead.chars().count()).max(1);
    let mut out = String::new();
    let mut first = true;
    let mut current = String::new();
    let prefix = |first: bool| if first { lead } else { cont };
    for word in text.split_whitespace() {
        // If a single word is longer than the body width, hard-break it.
        //
        // `>` vs `>=` is equivalent. A word of exactly `body_width` goes
        // down the hard-break path as a single chunk and down the normal
        // path as a full line, and both emit it alone with the same
        // prefix — the trailing newline the two disagree about is popped
        // at the end.
        if word.chars().count() > body_width {
            if !current.is_empty() {
                out.push_str(prefix(first));
                out.push_str(&current);
                out.push('\n');
                first = false;
                current.clear();
            }
            let mut chars = word.chars();
            loop {
                let chunk: String = (&mut chars).take(body_width).collect();
                if chunk.is_empty() {
                    break;
                }
                out.push_str(prefix(first));
                out.push_str(&chunk);
                out.push('\n');
                first = false;
            }
            continue;
        }
        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > body_width {
            out.push_str(prefix(first));
            out.push_str(&current);
            out.push('\n');
            first = false;
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push_str(prefix(first));
        out.push_str(&current);
        out.push('\n');
    }
    out.pop(); // remove trailing newline (caller adds its own)
    out
}

pub(crate) fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        s.to_string()
    } else {
        // POSIX-safe single-quote: replace ' with '\'' and wrap.
        let escaped = s.replace('\'', "'\\''");
        format!("'{escaped}'")
    }
}

pub(crate) fn md_escape(s: &str) -> String {
    // Escape '|' (table separator) and backslash. Other Markdown specials are
    // safe inside a table cell.
    s.replace('\\', "\\\\").replace('|', "\\|")
}

/// Pure: edit (Levenshtein) distance between two strings, counting
/// single-character insertions / deletions / substitutions. Used by
/// the unknown-command `did-you-mean` path; small enough that
/// pulling in the `strsim` crate would be over-spec.
///
/// Implemented as the standard O(m·n) DP table with byte-level
/// iteration. ASCII-only paths get exact answers; multi-byte
/// UTF-8 still terminates but the distance is counted in bytes,
/// not graphemes. Acceptable for the command-name use case
/// (every built-in is ASCII).
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.is_empty() {
        return b_bytes.len();
    }
    if b_bytes.is_empty() {
        return a_bytes.len();
    }
    // Two-row rolling DP: only the previous row's distances are
    // needed to compute the current row. Saves O(m·n) memory →
    // O(min(m,n)) without changing the answer.
    // Which side is `short` is a memory optimisation, not a decision:
    // the row DP is correct for either ordering and Levenshtein is
    // symmetric, so `<`, `<=`, `==` and `>` all yield the same distance.
    // All three mutations of this comparison are equivalent; the
    // symmetry is pinned by a test rather than assumed.
    let (short, long) = if a_bytes.len() < b_bytes.len() {
        (a_bytes, b_bytes)
    } else {
        (b_bytes, a_bytes)
    };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr: Vec<usize> = vec![0; short.len() + 1];
    for (i, lc) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, sc) in short.iter().enumerate() {
            let cost = if lc == sc { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[short.len()]
}

/// Suggest the closest registry name to `input` within an
/// edit-distance threshold. Returns `None` when no candidate is
/// close enough — a wild guess would mislead rather than help.
///
/// Threshold is length-dependent: short inputs (`:q`, `:r`) get
/// distance ≤ 1; longer ones tolerate up to 2 typos. The
/// length-aware threshold prevents a 2-char miss like `:xy`
/// from "matching" every 3-char name in the registry.
pub(crate) fn suggest_command(input: &str) -> Option<String> {
    suggest_from(input, crate::commands::all_names())
}

/// The candidate-selection half of [`suggest_command`], with the name
/// list passed in.
///
/// Split out so the tie-break can be tested. Against the live registry a
/// test has to find two real commands equidistant from a made-up input
/// and rely on their relative order in the table — which pins the
/// registry's contents, not this function, and breaks the next time a
/// command is added.
///
/// The rule: strictly-better wins, so the FIRST of several equally-close
/// names is kept. `<=` here would hand the suggestion to whichever
/// happened to be last in the registry.
pub(crate) fn suggest_from<'a, I>(input: &str, names: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let threshold = if input.len() <= 3 { 1 } else { 2 };
    let mut best: Option<(usize, String)> = None;
    for name in names {
        let d = edit_distance(input, name);
        if d <= threshold && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, name.to_string()));
        }
    }
    best.map(|(_, name)| name)
}

/// Pure: return the built-in command names + aliases that begin
/// with `prefix`. Sorted alphabetically. Empty prefix returns every
/// name (still alpha-sorted). De-duplicated so a command's
/// canonical name and any aliases don't both surface for the same
/// dispatch arm — first occurrence wins.
///
/// Used by command-mode Tab cycling. Plugins (`commands.toml`) are
/// not included here because plugins are operator-specific and
/// can change without a registry update; future enhancement could
/// merge them in but the registry-driven first cut keeps the
/// behaviour predictable.
pub(crate) fn completion_candidates(prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = crate::commands::all_names()
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether `cmd` (a command name or alias) takes an existing
/// environment name as its first positional argument — the
/// unambiguous cases where Tab-completing an env name in the command
/// bar makes sense (`:diff`, `:config-diff`, `:rds-detach`; `:diff`
/// also takes a second env name, and completing the trailing token
/// covers both slots). Sourced from the command registry
/// (`CommandSpec::env_arg`, set via `commands::cmd_env_arg`) so it
/// can't drift from the command definitions, and resolves aliases.
pub(crate) fn command_takes_env_arg(cmd: &str) -> bool {
    crate::commands::COMMANDS
        .iter()
        .any(|c| c.env_arg && (c.name == cmd || c.aliases.contains(&cmd)))
}

/// Format an "age" against now. Pure; keeps the secrets renderer
/// from depending on ui.rs's private `humanize_age`.
pub(crate) fn format_age(
    now: chrono::DateTime<chrono::Utc>,
    t: chrono::DateTime<chrono::Utc>,
) -> String {
    let d = now.signed_duration_since(t);
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hrs = mins / 60;
    if hrs < 48 {
        return format!("{hrs}h ago");
    }
    let days = hrs / 24;
    if days < 60 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 24 {
        return format!("~{months}mo ago");
    }
    format!("~{}y ago", days / 365)
}

pub(crate) fn urlencode(s: &str) -> String {
    // Minimal URL-encode of the characters that appear in EB app / env names.
    // EB names are restricted to a–z A–Z 0–9 - _ so most input passes through;
    // we still encode space and any non-ASCII for safety.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}
