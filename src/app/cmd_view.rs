//! View / filter / column management — pure-state commands that
//! manipulate `App` fields (`hidden_cols`, `saved_views`,
//! `named_filters`, `filter`) and persist the result to `state.toml`.
//! No AWS calls, no async spawns; ideal candidate for the third slice
//! of the `execute_command` split.
//!
//! Same parent-module-visibility pattern as `cmd_overlay` and
//! `cmd_write`. `encode_view` and `apply_view` are private free
//! functions in `app.rs`; the sub-module reaches them via `super::*`.

use super::{apply_view, encode_view, App};

impl App {
    /// `:cols [list | hide NAME | show NAME | reset]` — manage which
    /// columns of the env table are visible. NAME stays unhideable —
    /// the table is unusable without it.
    pub(crate) fn cmd_cols(&mut self, rest: &[&str]) {
        const KNOWN: &[&str] = &[
            "NAME",
            "APPLICATION",
            "TIER",
            "STATUS",
            "HEALTH",
            "TREND",
            "PLATFORM",
            "VERSION",
            "CNAME",
            "AGE",
        ];
        match rest.first().copied() {
            None | Some("list") => {
                let listing: Vec<String> = KNOWN
                    .iter()
                    .map(|c| {
                        if self.hidden_cols.contains(*c) {
                            format!("{c} (hidden)")
                        } else {
                            c.to_string()
                        }
                    })
                    .collect();
                self.status_message = Some(format!("cols: {}", listing.join(", ")));
            }
            Some("hide") => match rest.get(1) {
                Some(name) => {
                    let upper = name.to_uppercase();
                    if upper == "NAME" {
                        self.error_message = Some("NAME cannot be hidden".into());
                    } else if !KNOWN.contains(&upper.as_str()) {
                        self.error_message = Some(format!("unknown column '{name}'"));
                    } else {
                        self.hidden_cols.insert(upper.clone());
                        self.persist_state();
                        self.status_message = Some(format!("hid column {upper}"));
                    }
                }
                None => self.error_message = Some("usage: :cols hide <name>".into()),
            },
            Some("show") => match rest.get(1) {
                Some(name) => {
                    let upper = name.to_uppercase();
                    if self.hidden_cols.remove(&upper) {
                        self.persist_state();
                        self.status_message = Some(format!("showed column {upper}"));
                    } else {
                        self.status_message = Some(format!("column {upper} already visible"));
                    }
                }
                None => self.error_message = Some("usage: :cols show <name>".into()),
            },
            Some("reset") => {
                self.hidden_cols.clear();
                self.persist_state();
                self.status_message = Some("all columns visible".into());
            }
            Some(other) => {
                self.error_message = Some(format!(
                    "unknown :cols subcommand '{other}'  (try: list / hide NAME / show NAME / reset)"
                ));
            }
        }
    }

    /// `:save-view NAME` — snapshot the current filter + sort + group +
    /// scope into `state.toml` under `NAME`. Restored via `:view NAME`.
    pub(crate) fn cmd_save_view(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(name) => {
                let snap = encode_view(self);
                self.saved_views.insert((*name).to_string(), snap.clone());
                self.persist_state();
                self.status_message = Some(format!("saved view '{name}'  ({snap})"));
            }
            None => self.error_message = Some("usage: :save-view <name>".into()),
        }
    }

    pub(crate) fn cmd_view(&mut self, rest: &[&str]) {
        match rest.first() {
            None => {
                self.error_message = Some("usage: :view <name>  — see :views".into());
            }
            Some(name) => {
                if let Some(snap) = self.saved_views.get(*name).cloned() {
                    apply_view(self, &snap);
                    self.status_message = Some(format!("loaded view '{name}'"));
                } else {
                    self.error_message = Some(format!("no view '{name}'"));
                }
            }
        }
    }

    pub(crate) fn cmd_views(&mut self) {
        if self.saved_views.is_empty() {
            self.status_message = Some("no saved views — :save-view <name> to create one".into());
        } else {
            let listing: Vec<String> = self.saved_views.keys().cloned().collect();
            self.status_message = Some(format!("views: {}", listing.join(", ")));
        }
    }

    pub(crate) fn cmd_view_drop(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(name) => {
                if self.saved_views.remove(*name).is_some() {
                    self.persist_state();
                    self.status_message = Some(format!("dropped view '{name}'"));
                } else {
                    self.error_message = Some(format!("no view '{name}'"));
                }
            }
            None => self.error_message = Some("usage: :view-drop <name>".into()),
        }
    }

    /// `:filter NAME` / `:f NAME` — load a saved view by name.
    /// Empty arg clears the current filter (operator escape hatch
    /// when stuck behind a filter that hides everything). Reads
    /// from the unified `saved_views` store — same source `]`/`[`
    /// cycle through and `:view NAME` loads from. Loading via
    /// `apply_view` so a full view (with sort + group + scope)
    /// applies entirely, while a filter-only view only changes
    /// the filter — same `apply_view` "missing fields untouched"
    /// semantics.
    pub(crate) fn cmd_filter_load(&mut self, rest: &[&str]) {
        match rest.first() {
            None => {
                self.filter.clear();
                self.rebuild_view();
                self.status_message = Some("filter cleared".into());
            }
            Some(name) if self.saved_views.contains_key(*name) => {
                if let Some(snap) = self.saved_views.get(*name).cloned() {
                    super::apply_view(self, &snap);
                    self.status_message = Some(format!("filter: {name} → \"{}\"", self.filter));
                }
            }
            Some(name) => {
                self.error_message = Some(format!(
                    "no saved view named '{name}' — try :views (or :save <name>)"
                ));
            }
        }
    }

    /// `:save NAME` — snapshot only the current FILTER under
    /// `NAME` in the unified saved-views store. For a full
    /// filter+sort+group+scope snapshot, use `:save-view NAME`.
    /// Both go to the same store; the difference is the encoded
    /// payload — a filter-only view leaves the other surfaces
    /// alone on load, a full view applies everything.
    pub(crate) fn cmd_save_filter(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(name) => {
                if self.filter.is_empty() {
                    self.error_message = Some("nothing to save — set a filter with / first".into());
                } else {
                    let encoded = super::encode_filter_only_view(&self.filter);
                    self.saved_views.insert((*name).to_string(), encoded);
                    self.status_message =
                        Some(format!("saved filter '{name}' = \"{}\"", self.filter));
                    self.persist_state();
                }
            }
            None => self.error_message = Some("usage: :save <name>".into()),
        }
    }

    /// `:drop NAME` — alias for `:view-drop NAME`. Same store, same
    /// operation. Kept as a separate verb because operators
    /// muscle-memory `:save` ↔ `:drop` and `:save-view` ↔
    /// `:view-drop` independently.
    pub(crate) fn cmd_drop_filter(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(name) => {
                if self.saved_views.remove(*name).is_some() {
                    self.status_message = Some(format!("dropped saved filter '{name}'"));
                    self.persist_state();
                } else {
                    self.error_message = Some(format!("no saved filter named '{name}'"));
                }
            }
            None => self.error_message = Some("usage: :drop <name>".into()),
        }
    }

    /// `:filters` — list saved views, showing the filter portion
    /// for each (since this is the filter-flavored listing command).
    /// `:views` is the same listing but without the filter detail.
    pub(crate) fn cmd_filters(&mut self) {
        if self.saved_views.is_empty() {
            self.status_message = Some("no saved filters — :save <name> to create one".into());
        } else {
            let listing: Vec<String> = self
                .saved_views
                .iter()
                .map(|(k, encoded)| format!("{k}=\"{}\"", super::view_filter_value(encoded)))
                .collect();
            self.status_message = Some(format!("filters: {}", listing.join("  ")));
        }
    }
}
