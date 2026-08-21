//! Saved-view encoding: serialising the current filter/sort/group
//! state to the compact string persisted in `state.toml`, and applying
//! one back onto a live `App`.

use super::*;

pub(crate) fn encode_view(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !app.filter.is_empty() {
        parts.push(format!("filter={}", app.filter.text()));
    }
    parts.push(format!(
        "sort={}:{}",
        app.sort_key.label(),
        if app.sort_desc { "desc" } else { "asc" }
    ));
    parts.push(format!("grouped={}", app.grouped));
    let scope = match app.scope {
        Scope::Envs => "envs",
        Scope::Apps => "apps",
    };
    parts.push(format!("scope={scope}"));
    parts.join(";")
}

/// Encode a filter-only saved view — the value `:save NAME` writes
/// to `saved_views`. Omits `sort=`, `grouped=`, `scope=` so loading
/// the view doesn't perturb those — `apply_view` only touches a
/// field when its `KEY=` part is present in the snapshot (sort /
/// grouped / scope are no-op when absent). `filter=` is the
/// exception: `apply_view` always sets `app.filter` (snapshot
/// semantics — restore the filter that was active at save time,
/// including empty). A filter-only view from this encoder always
/// has `filter=` present, so the asymmetry doesn't bite here; the
/// case it matters is loading a full snapshot taken when the
/// filter was empty (which clears the current filter on purpose).
///
/// Used by the legacy `:save` command (filter-only save) and by
/// the state.toml backward-compat path that promotes old
/// `filter.NAME = "..."` lines into saved_views.
pub fn encode_filter_only_view(filter: &str) -> String {
    format!("filter={filter}")
}

/// Pure: extract the filter portion of an encoded saved view.
/// Returns the empty string when the view doesn't include a
/// `filter=` part (which means "no filter" — operator wanted the
/// view to clear whatever filter was set). Used by the chip-bar
/// active-check + the cycle keybind.
pub fn view_filter_value(encoded: &str) -> &str {
    for part in encoded.split(';') {
        if let Some(rest) = part.trim().strip_prefix("filter=") {
            return rest;
        }
    }
    ""
}

/// Snapshot semantics: `filter` always restores (defaults to empty
/// when `filter=` is absent — the save-time state was "no filter"),
/// while `sort` / `grouped` / `scope` only restore when explicitly
/// present (so a filter-only view from `encode_filter_only_view`
/// doesn't perturb them). See `encode_filter_only_view`'s docstring
/// for the operator-visible consequence.
pub(crate) fn apply_view(app: &mut App, snap: &str) {
    let mut new_filter = String::new();
    for part in snap.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim() {
            "filter" => new_filter = v.trim().to_string(),
            "sort" => {
                let (key, desc) = parse_sort(Some(v.trim()));
                app.sort_key = key;
                app.sort_desc = desc;
            }
            "grouped" => app.grouped = v.trim().eq_ignore_ascii_case("true"),
            "scope" => {
                app.scope = match v.trim() {
                    "apps" => Scope::Apps,
                    _ => Scope::Envs,
                };
            }
            _ => {}
        }
    }
    app.filter = new_filter.into();
    app.resort_envs(); // also rebuilds the view.
}
