//! `ViewState` — the presentation layer over `App::environments`, and the
//! one place the "rebuild the view after you change it" rule is enforced.
//!
//! The table `ui` draws is not `environments` directly: it is a filtered,
//! optionally grouped projection of it, plus two lookup maps the render hot
//! path needs per row. Recomputing that per frame was measurably wasteful,
//! so it is cached — and a cache with hand-written invalidation is exactly
//! the kind of thing that goes quietly stale.
//!
//! So the derived fields are private. Reading one goes through an accessor
//! that checks the cache has been rebuilt since its inputs last changed:
//!
//! - `filter` and `grouped` are private too, and the only way to change
//!   them ([`ViewState::filter_mut`], [`ViewState::set_grouped`]) marks the
//!   cache stale as a side effect. You cannot forget.
//! - the remaining inputs (`App::environments`, `aliases`, `latest_stacks`,
//!   the theme palette) are owned by `App` and used far too widely to hide,
//!   so those sites call [`ViewState::invalidate`] explicitly — but a miss
//!   is now a loud assertion on the next read rather than a silently wrong
//!   table.
//!
//! [`App::rebuild_view`] is the only caller of [`ViewState::store`], which
//! is the only thing that can clear the stale flag.

use std::collections::{BTreeSet, HashMap};

use ratatui::style::Color;
use tui_common::TextInput;

use super::{DisplayRow, SortKey, ViewMode};

pub struct ViewState {
    /// Sort column for the environments table.
    pub sort_key: SortKey,
    /// Descending when set. Toggled by pressing the same sort key twice.
    pub sort_desc: bool,
    /// Row density (`:compact`).
    pub mode: ViewMode,
    /// Columns hidden via `:cols`.
    pub hidden_cols: BTreeSet<String>,
    /// Mask secret-ish values in overlays and the table (`:redact`).
    pub redact: bool,

    // ---- cache inputs (private: mutating one invalidates the cache) ----
    filter: TextInput,
    grouped: bool,

    // ---- derived (private: reading one asserts the cache is fresh) ----
    filtered: Vec<usize>,
    display: Vec<DisplayRow>,
    app_colors: HashMap<String, Color>,
    stale_platforms: HashMap<String, String>,
    stale: bool,
}

impl ViewState {
    pub fn new(
        filter: TextInput,
        grouped: bool,
        sort_key: SortKey,
        sort_desc: bool,
        redact: bool,
        hidden_cols: BTreeSet<String>,
    ) -> Self {
        Self {
            sort_key,
            sort_desc,
            mode: ViewMode::Default,
            hidden_cols,
            redact,
            filter,
            grouped,
            filtered: Vec::new(),
            display: Vec::new(),
            app_colors: HashMap::new(),
            stale_platforms: HashMap::new(),
            // Empty caches over an empty `environments` are correct, not
            // stale — the first refresh rebuilds them anyway.
            stale: false,
        }
    }

    /// The `/` filter buffer. Read-only — see [`Self::filter_mut`].
    pub fn filter(&self) -> &TextInput {
        &self.filter
    }

    /// Mutable access to the filter buffer, which marks the cache stale.
    ///
    /// Deliberately conservative: taking `&mut` counts as a change even if
    /// the caller ends up not editing anything. An extra `rebuild_view()`
    /// costs a filter pass over `environments`; a missed one shows the
    /// operator the wrong rows.
    pub fn filter_mut(&mut self) -> &mut TextInput {
        self.stale = true;
        &mut self.filter
    }

    /// Replace the filter buffer wholesale (`:filter`, saved views,
    /// clearing on escape). Marks the cache stale.
    pub fn set_filter(&mut self, filter: impl Into<TextInput>) {
        self.filter = filter.into();
        self.stale = true;
    }

    /// Whether rows are grouped by application, with separators between.
    pub fn grouped(&self) -> bool {
        self.grouped
    }

    pub fn set_grouped(&mut self, grouped: bool) {
        if self.grouped != grouped {
            self.grouped = grouped;
            self.stale = true;
        }
    }

    /// Mark the derived slices as needing a rebuild.
    ///
    /// For the inputs `ViewState` does not own — `App::environments`,
    /// `aliases`, `latest_stacks`, the theme palette. Callers that change
    /// `filter` or `grouped` do not need this.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Indexes into `App::environments` that survive the current filter.
    pub fn filtered(&self) -> &[usize] {
        self.assert_fresh();
        &self.filtered
    }

    /// Rows as drawn, including group separators when `grouped` is set.
    pub fn display(&self) -> &[DisplayRow] {
        self.assert_fresh();
        &self.display
    }

    /// `application → palette colour`, assigned by order of first
    /// appearance in the filtered view.
    pub fn app_colors(&self) -> &HashMap<String, Color> {
        self.assert_fresh();
        &self.app_colors
    }

    /// `env name → newer platform version`, for envs on a superseded
    /// solution stack. Empty until `latest_stacks` has been fetched.
    pub fn stale_platforms(&self) -> &HashMap<String, String> {
        self.assert_fresh();
        &self.stale_platforms
    }

    /// Install a freshly computed view. The only way to clear the stale
    /// flag, and called from exactly one place: `App::rebuild_view`.
    pub(super) fn store(
        &mut self,
        filtered: Vec<usize>,
        display: Vec<DisplayRow>,
        app_colors: HashMap<String, Color>,
        stale_platforms: HashMap<String, String>,
    ) {
        self.filtered = filtered;
        self.display = display;
        self.app_colors = app_colors;
        self.stale_platforms = stale_platforms;
        self.stale = false;
    }

    /// Panics in debug builds, logs in release.
    ///
    /// A stale read is a real bug — the operator is looking at rows that do
    /// not match what they filtered for — but panicking inside the alternate
    /// screen would take the whole TUI down and scribble over the terminal,
    /// which is worse than one wrong frame. Tests run in debug, so this
    /// fails loudly where it can be fixed.
    fn assert_fresh(&self) {
        if self.stale {
            debug_assert!(
                false,
                "read a stale view cache: an input changed without a following \
                 App::rebuild_view()"
            );
            tracing::error!("stale view cache read — rows may not match the filter");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ViewState {
        ViewState::new(
            TextInput::new(),
            false,
            SortKey::App,
            false,
            false,
            BTreeSet::new(),
        )
    }

    fn store_empty(v: &mut ViewState) {
        v.store(Vec::new(), Vec::new(), HashMap::new(), HashMap::new());
    }

    #[test]
    fn starts_fresh_over_an_empty_fleet() {
        assert!(!view().is_stale());
    }

    #[test]
    fn taking_the_filter_mutably_marks_the_cache_stale() {
        let mut v = view();
        v.filter_mut().insert_str("prod");
        assert!(v.is_stale());
    }

    #[test]
    fn reading_the_filter_leaves_the_cache_fresh() {
        let mut v = view();
        v.filter_mut().insert_str("prod");
        store_empty(&mut v);
        assert_eq!(v.filter().text(), "prod");
        assert!(!v.is_stale());
    }

    #[test]
    fn set_filter_marks_the_cache_stale() {
        let mut v = view();
        store_empty(&mut v);
        v.set_filter("web");
        assert!(v.is_stale());
        assert_eq!(v.filter().text(), "web");
    }

    #[test]
    fn set_grouped_only_invalidates_on_an_actual_change() {
        let mut v = view();
        store_empty(&mut v);
        v.set_grouped(false);
        assert!(!v.is_stale(), "no-op set should not force a rebuild");
        v.set_grouped(true);
        assert!(v.is_stale());
    }

    #[test]
    fn invalidate_covers_the_inputs_view_state_does_not_own() {
        let mut v = view();
        store_empty(&mut v);
        v.invalidate();
        assert!(v.is_stale());
    }

    #[test]
    fn store_is_the_only_thing_that_clears_stale() {
        let mut v = view();
        v.invalidate();
        v.store(
            vec![0, 2],
            vec![DisplayRow::Env(0), DisplayRow::Env(2)],
            HashMap::new(),
            HashMap::new(),
        );
        assert!(!v.is_stale());
        assert_eq!(v.filtered(), &[0, 2]);
        assert_eq!(v.display().len(), 2);
    }

    #[test]
    #[should_panic(expected = "read a stale view cache")]
    fn reading_a_stale_cache_is_caught() {
        let mut v = view();
        v.filter_mut().insert_str("prod");
        let _ = v.display();
    }
}
