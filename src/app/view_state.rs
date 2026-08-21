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

use crossterm::event::KeyEvent;
use ratatui::style::Color;
use tui_common::TextInput;

use super::{DisplayRow, SortKey, ViewMode};

pub struct ViewState {
    /// Row density (`:compact`).
    pub mode: ViewMode,
    /// Columns hidden via `:cols`.
    pub hidden_cols: BTreeSet<String>,
    /// Mask secret-ish values in overlays and the table (`:redact`).
    pub redact: bool,

    // ---- cache inputs (private: mutating one invalidates the cache) ----
    filter: TextInput,
    grouped: bool,
    /// Sort column, and descending when set. Private because they have to
    /// stay in step with the order of `App::environments` — see
    /// [`Self::set_sort`].
    sort_key: SortKey,
    sort_desc: bool,

    // ---- derived (private: reading one asserts the cache is fresh) ----
    filtered: Vec<usize>,
    display: Vec<DisplayRow>,
    app_colors: HashMap<String, Color>,
    stale_platforms: HashMap<String, String>,
    stale: bool,
    /// Whether the current stale episode has already been logged. `Cell`
    /// because `assert_fresh` runs from `&self` accessors on the render
    /// path.
    logged_stale: std::cell::Cell<bool>,
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
            mode: ViewMode::Default,
            hidden_cols,
            redact,
            filter,
            grouped,
            sort_key,
            sort_desc,
            filtered: Vec::new(),
            display: Vec::new(),
            app_colors: HashMap::new(),
            stale_platforms: HashMap::new(),
            // Empty caches over an empty `environments` are correct, not
            // stale — the first refresh rebuilds them anyway.
            stale: false,
            logged_stale: std::cell::Cell::new(false),
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

    pub fn sort_key(&self) -> SortKey {
        self.sort_key
    }

    pub fn sort_desc(&self) -> bool {
        self.sort_desc
    }

    /// Record a new sort. These fields describe the order
    /// `App::environments` is *already* in, so setting them without
    /// re-sorting leaves the header arrow disagreeing with the rows —
    /// call `App::set_sort`, which does both, and is the only caller.
    ///
    /// `pub(super)` narrows that but doesn't guarantee it: `App`'s impl is
    /// spread across the sibling modules under `app/`, and every one of
    /// them is a descendant that can reach this. It's no longer a bare
    /// `pub` field assignable from `ui.rs`; the last step is convention.
    pub(super) fn set_sort(&mut self, key: SortKey, desc: bool) {
        self.sort_key = key;
        self.sort_desc = desc;
        self.stale = true;
    }

    /// Offer a key to the filter buffer. Returns whether it was consumed,
    /// and marks the cache stale only if it was.
    ///
    /// This exists so callers don't have to reach for [`Self::filter_mut`]
    /// just to *ask* — `TextInput::handle_key` returns false for every key
    /// it doesn't handle (`Down`, `PageUp`, most Ctrl chords), and taking
    /// `&mut` to find that out would mark the cache stale with nothing
    /// changed and no rebuild to follow.
    pub fn filter_handle_key(&mut self, key: KeyEvent) -> bool {
        let consumed = self.filter.handle_key(key);
        self.stale |= consumed;
        consumed
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
    ///
    /// Same caveat as [`Self::set_sort`] — `pub(super)` reaches every
    /// module under `app/`, so "one place" is a fact about the code, not
    /// something the visibility enforces.
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
        self.logged_stale.set(false);
    }

    /// Panics in debug builds, logs once in release.
    ///
    /// A stale read is a real bug: the operator is looking at rows that
    /// don't match what they filtered for. Debug builds — `cargo test` and
    /// `cargo run` / `--demo` — panic, because that's where it can still be
    /// fixed and a developer needs to see it. Shipped release builds don't:
    /// a panic inside the alternate screen takes the TUI down and scribbles
    /// over the operator's terminal, which is worse than one wrong frame.
    ///
    /// The log fires once per stale episode, not once per read. All four
    /// accessors funnel through here and `draw_table` hits several of them
    /// per row per frame, so an unguarded `error!` would write thousands of
    /// lines to `~/.cache/ebman/ebman.log` for as long as the flag is set.
    /// [`Self::store`] re-arms it.
    fn assert_fresh(&self) {
        if self.stale {
            debug_assert!(
                false,
                "read a stale view cache: an input changed without a following \
                 App::rebuild_view()"
            );
            if !self.logged_stale.replace(true) {
                tracing::error!(
                    "stale view cache read — the table may not match the active \
                     filter until the next rebuild"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn textinput_never_mutates_on_a_key_it_reports_as_unconsumed() {
        // `filter_handle_key` marks the cache stale iff `handle_key`
        // returns true, which is only sound because a `false` return
        // means the buffer is untouched. That contract lives in
        // `tb-tui-common` and isn't stated in its docs, and the
        // dependency is a caret range — so pin it here. If a future
        // 0.1.x adds a handler that mutates and returns false, this
        // fails instead of silently resurrecting the stale-cache bug.
        let unconsumed = [
            key(KeyCode::Down, KeyModifiers::NONE),
            key(KeyCode::Up, KeyModifiers::NONE),
            key(KeyCode::PageUp, KeyModifiers::NONE),
            key(KeyCode::PageDown, KeyModifiers::NONE),
            key(KeyCode::Tab, KeyModifiers::NONE),
            key(KeyCode::BackTab, KeyModifiers::SHIFT),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::F(1), KeyModifiers::NONE),
            key(KeyCode::Insert, KeyModifiers::NONE),
            key(KeyCode::Null, KeyModifiers::NONE),
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            key(KeyCode::Char('g'), KeyModifiers::CONTROL),
            key(KeyCode::Char('x'), KeyModifiers::ALT),
            key(KeyCode::Char('p'), KeyModifiers::SUPER),
        ];
        for k in unconsumed {
            let mut v = view();
            v.set_filter("prod");
            v.store(Vec::new(), Vec::new(), HashMap::new(), HashMap::new());
            let before = v.filter().text().to_string();
            let before_col = v.filter().cursor_col();
            let consumed = v.filter_handle_key(k);
            assert!(
                !consumed,
                "{k:?} should not be consumed by the filter buffer"
            );
            assert_eq!(v.filter().text(), before, "{k:?} mutated the text");
            assert_eq!(v.filter().cursor_col(), before_col, "{k:?} moved the caret");
            assert!(
                !v.is_stale(),
                "{k:?} was not consumed, so it must not dirty the cache"
            );
        }
    }

    #[test]
    fn a_consumed_key_does_dirty_the_cache() {
        let mut v = view();
        v.store(Vec::new(), Vec::new(), HashMap::new(), HashMap::new());
        assert!(v.filter_handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE)));
        assert!(v.is_stale());
        assert_eq!(v.filter().text(), "p");
    }

    #[test]
    #[should_panic(expected = "read a stale view cache")]
    fn reading_a_stale_cache_is_caught() {
        let mut v = view();
        v.filter_mut().insert_str("prod");
        let _ = v.display();
    }
}
