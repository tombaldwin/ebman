//! Option-setting diffing between two environments (`:diff`) and the
//! overlay bodies that present the result.

use super::*;

/// Pure: should this env-metadata diff row be suppressed given the
/// operator's `--ignore-keys` list? Matches the row's field label
/// case-insensitively against the (already-lowercased) ignore set. The
/// `Version` row also matches `version_label` — that's the key
/// `:config-diff` uses for the same field, and the backlog's stated use
/// case ("hide the noisy version_label differences"), so both spellings
/// work. Empty `ignore_keys` is a no-op.
pub fn diff_field_ignored(field: &str, ignore_keys: &[String]) -> bool {
    if ignore_keys.is_empty() {
        return false;
    }
    let f = field.to_ascii_lowercase();
    ignore_keys
        .iter()
        .any(|k| *k == f || (f == "version" && k == "version_label"))
}

pub(crate) fn diff_envs(
    left: &Environment,
    right: &Environment,
    redact_on: bool,
    ignore_keys: &[String],
) -> String {
    let cn = |s: &str| {
        if redact_on {
            redact_block(s)
        } else {
            s.to_string()
        }
    };
    let updated = |e: &Environment| {
        e.updated
            .map(|u| u.to_rfc3339())
            .unwrap_or_else(|| "—".into())
    };
    let mut rows: Vec<(&str, String, String)> = vec![
        ("Name", left.name.clone(), right.name.clone()),
        (
            "Application",
            left.application.clone(),
            right.application.clone(),
        ),
        ("Tier", left.tier.clone(), right.tier.clone()),
        ("Status", left.status.clone(), right.status.clone()),
        ("Health", left.health.clone(), right.health.clone()),
        ("Platform", left.platform.clone(), right.platform.clone()),
        (
            "Version",
            left.version_label.clone(),
            right.version_label.clone(),
        ),
        ("CNAME", cn(&left.cname), cn(&right.cname)),
        ("Updated", updated(left), updated(right)),
    ];
    // Drop rows the operator asked to ignore (e.g. `--ignore-keys
    // "version,updated"`). Done before width math so the layout stays
    // tight around what's left.
    rows.retain(|(field, _, _)| !diff_field_ignored(field, ignore_keys));

    // Width-aware truncation so long values don't blow out the popup.
    let width: usize = 28;
    let truncate = |s: &str| -> String {
        if s.chars().count() > width {
            let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
            t.push('…');
            t
        } else {
            s.to_string()
        }
    };

    let left_label = truncate(&format!("◄ {}", left.name));
    let right_label = truncate(&format!("{} ►", right.name));
    let mut out = String::new();
    out.push_str(&format!(
        "{:<14}    {:<width$}    {}\n",
        "", left_label, right_label,
    ));
    out.push_str(&"─".repeat(14 + 4 + width + 4 + width));
    out.push('\n');
    for (field, l, r) in rows {
        let differs = l != r;
        let marker = if differs { "≠" } else { " " };
        out.push_str(&format!(
            "{marker} {:<12}  {:<width$}    {}\n",
            field,
            truncate(&l),
            truncate(&r),
        ));
    }
    out
}

/// Pure render of the `:options` overlay body. Groups `rows` by
/// namespace; within each group, operator-set rows come first
/// (marked `▸`), defaults follow (marked `•`). Optional
/// `filter_ns` restricts to one namespace.
///
/// Format per row:
///   `<marker> NAME[<padding>]  = VALUE       (default: X, type: T, ...)`
///
/// The metadata trailer (`default:`, `type:`, `severity:`, ranges,
/// value_options) only renders when the field is set — keeps the
/// line lean. Long value-option lists get truncated to "first 5 +
/// …" to avoid one option blowing past the popup width.
///
/// Top of the body carries a one-line legend so the operator
/// doesn't have to learn the marker convention from `?`.
/// One option-setting that differs between two envs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiff {
    pub namespace: String,
    pub name: String,
    pub left: Option<String>,
    pub right: Option<String>,
}

/// Pure: option-settings that differ between two envs. Compares the
/// operator-set `value` per `(namespace, name)`; rows where both
/// sides agree — including both unset — are dropped. EB's
/// `Some("")` and `None` both mean "unset", so they're normalised
/// to equal. Result is sorted `(namespace, name)` for a stable
/// overlay.
/// Pure: parse the `--ignore-keys "k1,k2"` argument. Splits on
/// commas, trims whitespace, drops empty entries, normalises to
/// lowercase so the filter is case-insensitive. Operators can also
/// use `namespace:name` form for precise matches against a specific
/// namespace; the filter compares both forms below.
pub fn parse_ignore_keys(csv: Option<&str>) -> Vec<String> {
    let Some(csv) = csv else {
        return Vec::new();
    };
    csv.split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pure: filter `diffs` to drop entries whose option name (or the
/// full `namespace:name` form) matches any entry in `ignore_keys`.
/// Match is case-insensitive (`ignore_keys` is pre-normalised).
/// Empty `ignore_keys` is a no-op (pass through).
pub fn filter_config_diffs(diffs: Vec<ConfigDiff>, ignore_keys: &[String]) -> Vec<ConfigDiff> {
    if ignore_keys.is_empty() {
        return diffs;
    }
    diffs
        .into_iter()
        .filter(|d| {
            let name_lower = d.name.to_lowercase();
            let qualified_lower = format!("{}:{}", d.namespace.to_lowercase(), name_lower);
            !ignore_keys
                .iter()
                .any(|k| k == &name_lower || k == &qualified_lower)
        })
        .collect()
}

pub fn diff_config_options(
    left: &[crate::aws::ConfigOption],
    right: &[crate::aws::ConfigOption],
) -> Vec<ConfigDiff> {
    use std::collections::{BTreeMap, BTreeSet};
    let norm = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    let to_map = |opts: &[crate::aws::ConfigOption]| -> BTreeMap<(String, String), Option<String>> {
        opts.iter()
            .map(|o| ((o.namespace.clone(), o.name.clone()), norm(&o.value)))
            .collect()
    };
    let lmap = to_map(left);
    let rmap = to_map(right);
    let mut keys: BTreeSet<(String, String)> = lmap.keys().cloned().collect();
    keys.extend(rmap.keys().cloned());
    keys.into_iter()
        .filter_map(|k| {
            let l = lmap.get(&k).cloned().flatten();
            let r = rmap.get(&k).cloned().flatten();
            if l == r {
                None
            } else {
                Some(ConfigDiff {
                    namespace: k.0,
                    name: k.1,
                    left: l,
                    right: r,
                })
            }
        })
        .collect()
}

/// Render the `:config-diff` overlay — the option-settings that
/// differ between `left_env` and `right_env`, grouped by namespace.
pub(crate) fn render_config_diff_overlay(
    left_env: &str,
    right_env: &str,
    diffs: &[ConfigDiff],
) -> String {
    if diffs.is_empty() {
        return format!(
            "Config diff — {left_env}  ↔  {right_env}\n\n\
             ✓ identical: every operator-set option-setting matches.\n\n\
             esc / q to close"
        );
    }
    let mut body = format!(
        "Config diff — {left_env}  ↔  {right_env}\n\
         {n} option-setting(s) differ.  L = {left_env}   R = {right_env}\n\
         (unset = at the platform default)\n\n",
        n = diffs.len()
    );
    let mut current_ns: Option<&str> = None;
    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unset)".into());
    for d in diffs {
        if Some(d.namespace.as_str()) != current_ns {
            if current_ns.is_some() {
                body.push('\n');
            }
            body.push_str(&format!("── {} ──\n", d.namespace));
            current_ns = Some(d.namespace.as_str());
        }
        body.push_str(&format!(
            "  {}\n      L: {}\n      R: {}\n",
            d.name,
            show(&d.left),
            show(&d.right),
        ));
    }
    body.push_str("\nesc / q to close");
    body
}

pub(crate) fn render_options_overlay(
    rows: &[crate::aws::ConfigOption],
    filter_ns: Option<&str>,
    env_name: &str,
) -> String {
    let filtered: Vec<&crate::aws::ConfigOption> = rows
        .iter()
        .filter(|r| filter_ns.is_none_or(|ns| r.namespace == ns))
        .collect();
    if filtered.is_empty() {
        return match filter_ns {
            Some(ns) => format!(
                "No options found for namespace '{ns}' on env '{env_name}'.\n\n\
                 Spelling? Try `:options` (no arg) to see the full list of\n\
                 namespaces available for this env's platform.\n\n\
                 esc / q to close"
            ),
            None => format!(
                "No configuration options returned for env '{env_name}'.\n\n\
                 This usually means the env's platform doesn't expose an option\n\
                 vocabulary (custom platform or stale solution-stack). Try\n\
                 `:set-option` directly if you know what you want to change.\n\n\
                 esc / q to close"
            ),
        };
    }
    // Compute the longest name within each namespace so the `= value`
    // columns line up per group. Walking once first; second pass renders.
    let mut max_name_per_ns: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for r in &filtered {
        let e = max_name_per_ns.entry(r.namespace.as_str()).or_insert(0);
        *e = (*e).max(r.name.chars().count()).min(38);
    }

    let user_set = filtered.iter().filter(|r| r.value.is_some()).count();
    let mut body = String::new();
    body.push_str(&format!(
        "Configuration vocabulary for {env_name}\n\
         {user_set}/{total} options are operator-set; the rest are at default.\n\n\
         ▸ = operator-set    • = default    severity warns when changing rolls instances\n\n",
        total = filtered.len()
    ));

    let mut current_ns: Option<&str> = None;
    for r in &filtered {
        if Some(r.namespace.as_str()) != current_ns {
            if current_ns.is_some() {
                body.push('\n');
            }
            body.push_str(&format!("── {} ──\n", r.namespace));
            current_ns = Some(r.namespace.as_str());
        }
        let marker = if r.value.is_some() { "▸" } else { "•" };
        let name_width = max_name_per_ns
            .get(r.namespace.as_str())
            .copied()
            .unwrap_or(20);
        let name_padded = if r.name.chars().count() < name_width {
            format!("{name:<width$}", name = r.name, width = name_width)
        } else {
            r.name.clone()
        };
        let value_str = match &r.value {
            Some(v) => format!(" = {v}"),
            None => String::new(),
        };
        // Trailing metadata — only emit what's set so short-form
        // rows stay short.
        let mut meta: Vec<String> = Vec::new();
        if let Some(d) = &r.default_value {
            if !d.is_empty() {
                meta.push(format!("default: {d}"));
            }
        }
        if !r.value_type.is_empty() && r.value_type != "Scalar" {
            // Scalar is the default; only call out non-scalars
            // (`List`) which surprise the operator.
            meta.push(format!("type: {}", r.value_type));
        }
        if let Some(s) = &r.change_severity {
            if s != "NoInterruption" && s != "Unknown" {
                meta.push(format!("severity: {s}"));
            }
        }
        match (r.min_value, r.max_value) {
            (Some(min), Some(max)) => meta.push(format!("range: {min}-{max}")),
            (Some(min), None) => meta.push(format!("min: {min}")),
            (None, Some(max)) => meta.push(format!("max: {max}")),
            (None, None) => {}
        }
        if let Some(maxlen) = r.max_length {
            meta.push(format!("max_len: {maxlen}"));
        }
        if !r.value_options.is_empty() {
            let preview: Vec<&str> = r.value_options.iter().take(5).map(String::as_str).collect();
            let more = r.value_options.len().saturating_sub(5);
            let suffix = if more > 0 {
                format!(", … +{more}")
            } else {
                String::new()
            };
            meta.push(format!("oneof: {}{suffix}", preview.join(", ")));
        }
        let meta_str = if meta.is_empty() {
            String::new()
        } else {
            format!("  ({})", meta.join(", "))
        };
        body.push_str(&format!("  {marker} {name_padded}{value_str}{meta_str}\n"));
    }
    body.push_str(
        "\n`:set-option NAMESPACE NAME VALUE` to change a setting.\n\
         `:options NAMESPACE` to filter to one family.\n\
         esc / q to close",
    );
    body
}
