//! The `:env` editor round-trip: render current environment variables
//! as an editable `KEY=VALUE` buffer, parse the operator's edits back,
//! and diff the two into option-setting set/remove lists.

/// Render env vars as `KEY=VALUE` lines, aligned on the `=` for easy scan.
/// Empty values render as `""` so operators can distinguish "explicitly
/// empty" from "not set". Pure.
pub fn format_env_vars(vars: &[(String, String)]) -> String {
    if vars.is_empty() {
        return "(no env vars set)".into();
    }
    let key_width = vars
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(8, 40);
    let mut out = String::new();
    for (k, v) in vars {
        let rendered = if v.is_empty() {
            "\"\"".to_string()
        } else {
            v.clone()
        };
        out.push_str(&format!("{k:<key_width$} = {rendered}\n"));
    }
    out
}

/// Pure: format the temp-file body the `:env-edit` flow opens
/// in `$EDITOR`. Header comment explains the contract (lines look
/// like `KEY=VALUE`; `#` comments and blank lines are ignored;
/// save+quit applies; quit-without-save / unchanged-body cancels).
/// Existing env vars are sorted alphabetically so the operator
/// gets a stable target for diffs across runs.
pub(crate) fn build_env_edit_body(env_name: &str, vars: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# ebman env-var editor — {env_name}\n"));
    out.push_str("#\n");
    out.push_str("# Lines that look like KEY=VALUE are interpreted as env vars.\n");
    out.push_str("# Lines starting with # are comments.\n");
    out.push_str("# Blank lines are ignored.\n");
    out.push_str("#\n");
    out.push_str("# Save and quit to apply changes. Saving an unchanged file is a clean\n");
    out.push_str("# no-op. To reference a Secrets Manager value, store the ARN here\n");
    out.push_str("# (e.g. `DB_PASSWORD_SECRET_ARN=arn:aws:secretsmanager:...`) and have\n");
    out.push_str("# your app's bootstrap call GetSecretValue at runtime — EB does not\n");
    out.push_str("# resolve secretsmanager:// references natively.\n\n");
    let mut sorted: Vec<&(String, String)> = vars.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in sorted {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

/// Pure: parse the operator's edited `:env-edit` body back into a
/// `KEY -> VALUE` map. Splits each non-comment line on the *first*
/// `=` so values containing `=` (common for query-string-style
/// settings or base64-encoded secrets) pass through intact.
/// Keys that fail to validate (empty after trim, contain whitespace)
/// are dropped — EB's option-settings API would reject them anyway,
/// and the operator gets the diff feedback after save.
pub(crate) fn parse_env_edit_body(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            continue;
        }
        // Trailing whitespace + a single optional carriage return
        // (Windows line endings) get stripped from the value, but
        // intentional internal whitespace is preserved.
        let value = value.trim_end_matches('\r').trim_end_matches('\n');
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Pure: produce `(to_set, to_remove)` deltas from two env-var
/// snapshots. `to_set` carries the EB option-settings triple
/// `(namespace, name, value)`; `to_remove` carries `(namespace,
/// name)`. Caller-supplied namespace because the same shape is
/// reusable beyond `aws:elasticbeanstalk:application:environment`
/// (e.g. future `:options-edit` could feed any namespace).
/// `(namespace, key, value)` triples — the shape EB's option-settings
/// update API expects for "set these". Aliased so [`diff_env_vars`]'s
/// signature isn't tripping the complex-type clippy lint.
type OptionSet = Vec<(String, String, String)>;

/// `(namespace, key)` pairs — "remove these" shape.
type OptionRemove = Vec<(String, String)>;

pub(crate) fn diff_env_vars(
    namespace: &str,
    original: &std::collections::BTreeMap<String, String>,
    edited: &std::collections::BTreeMap<String, String>,
) -> (OptionSet, OptionRemove) {
    let mut to_set: OptionSet = Vec::new();
    let mut to_remove: OptionRemove = Vec::new();
    // Set or update: any key present in `edited` whose value
    // differs from the original (or was missing entirely).
    for (k, v) in edited {
        match original.get(k) {
            Some(prev) if prev == v => continue,
            _ => to_set.push((namespace.to_string(), k.clone(), v.clone())),
        }
    }
    // Remove: keys present in `original` but absent from `edited`.
    for k in original.keys() {
        if !edited.contains_key(k) {
            to_remove.push((namespace.to_string(), k.clone()));
        }
    }
    (to_set, to_remove)
}
