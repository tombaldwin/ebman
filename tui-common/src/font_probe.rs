//! Detect whether the current terminal's font renders Powerline / Nerd
//! glyphs as a single cell. We can't actually inspect the font from a TUI,
//! but we can write a known Powerline triangle (`U+E0B0`) and ask the
//! terminal where the cursor ended up. A patched font draws the glyph in
//! one cell — cursor advances by 1. An unpatched font usually substitutes
//! the placeholder block which most terminals render as a "wide" character,
//! advancing the cursor by 2 — or, more commonly, falls back to the
//! tofu/replacement character which most terminals render in one cell but
//! at an obviously wrong baseline (we can't tell from here, but the cell
//! count is at least stable). We treat advance-by-1 as "supported" and
//! anything else as "not supported".
//!
//! The probe is best-effort: any I/O error or unexpected response falls
//! back to `false` ("not supported"), which keeps the `unicode` glyph set
//! in play.
//!
//! The actual cursor query has to run *before* we enter the alternate
//! screen / raw mode, so the probe is wired into `main()` at startup, in
//! front of `enter_tui()`.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor;
use crossterm::style::Print;
use crossterm::terminal;
use crossterm::ExecutableCommand;

/// Probe sequence: write a Powerline right-triangle, ask the terminal for
/// the cursor column, then erase the glyph. Returns true if the glyph
/// advanced the cursor by exactly one column (the patched-font signature).
///
/// The probe enables raw mode briefly so `cursor::position()` can read the
/// response; it disables raw mode before returning regardless of outcome.
pub fn detect_powerline_support() -> bool {
    // Don't probe if stdout isn't a TTY — piped output / CI shouldn't open
    // raw mode just to read a glyph back.
    if !std::io::stdout().is_terminal_like() {
        return false;
    }
    probe_glyph_width_one("\u{E0B0}").unwrap_or(false)
}

/// Probe a Nerd Font MDI codepoint (`U+F048B`, mdi-server — what the
/// `Instances` tab icon uses) to verify it renders at width 1. The basic
/// E0Bx Powerline range and the Fxxxx Nerd Font MDI range come from
/// *different* glyph blocks in the same patched font; some terminals or
/// fallback fonts ship one without the other, in which case the tab strip
/// silently misaligns. Treated as advisory: we still pick `powerline` when
/// E0B0 works, but log a warning so the user sees the cause in the log
/// file when they spot misaligned tabs.
pub fn detect_tab_icon_support() -> bool {
    if !std::io::stdout().is_terminal_like() {
        return false;
    }
    probe_glyph_width_one("\u{F048B}").unwrap_or(false)
}

fn probe_glyph_width_one(glyph: &str) -> io::Result<bool> {
    let mut stdout = io::stdout();
    // Save where the cursor is so we can restore it; record the column
    // before the probe write to compute the advance.
    terminal::enable_raw_mode()?;
    let restore = ProbeGuard;
    stdout.execute(cursor::SavePosition)?;
    let (col_before, _row) = cursor::position().unwrap_or((0, 0));
    stdout.execute(Print(glyph))?;
    stdout.flush()?;
    // Give the terminal a moment to process before we ask for the cursor.
    std::thread::sleep(Duration::from_millis(20));
    let (col_after, _row) = cursor::position().unwrap_or((col_before, 0));
    // Restore cursor + clear the probe glyph so it never reaches the user.
    stdout.execute(cursor::RestorePosition)?;
    stdout.execute(Print("  "))?;
    stdout.execute(cursor::RestorePosition)?;
    drop(restore);
    let advance = col_after.saturating_sub(col_before);
    Ok(classify_advance(advance))
}

/// Pure: a one-cell advance signals a patched font. Anything else (zero
/// when the glyph was dropped entirely; two when a wide replacement fired;
/// large when the terminal swallowed the probe and gave us a stale answer)
/// is treated as unsupported.
fn classify_advance(advance: u16) -> bool {
    advance == 1
}

/// Resolved-from-`auto` outcome. Carries the final icons string plus a flag
/// that says "we picked powerline, but the MDI block looks broken — warn".
/// Pure structure so the decision logic is testable without terminal I/O.
#[derive(Debug, PartialEq, Eq)]
pub struct AutoResolved {
    pub icons: &'static str,
    pub warn_tab_icons_missing: bool,
}

fn classify_auto(powerline: bool, tab_icons: bool) -> AutoResolved {
    if powerline {
        AutoResolved {
            icons: "powerline",
            warn_tab_icons_missing: !tab_icons,
        }
    } else {
        AutoResolved {
            icons: "unicode",
            warn_tab_icons_missing: false,
        }
    }
}

/// Resolve a configured icon style. `"auto"` triggers [`detect_powerline_support`]
/// and resolves to `"powerline"` on a yes / `"unicode"` on a no; any other
/// value is passed through unchanged so the regular [`crate::theme`] parser
/// handles it (and surfaces typos as the existing fallback to unicode).
///
/// When `"auto"` picks `"powerline"` but the Nerd Font MDI block (used by
/// the Detail-view tab strip) probes as missing, logs a one-shot warning
/// via `tracing::warn!`. The warning is advisory — the rest of Powerline
/// mode still works; only the per-tab icons (`Instances`, `Metrics`, etc.)
/// will visually misalign.
///
/// Pure-with-side-effects: only does I/O when the input is literally
/// `"auto"`. Run once at startup, before TUI init.
pub fn resolve_icons_setting(raw: &str) -> String {
    if raw.eq_ignore_ascii_case("auto") {
        let resolved = classify_auto(detect_powerline_support(), detect_tab_icon_support());
        if resolved.warn_tab_icons_missing {
            tracing::warn!(
                target: "ebman::font_probe",
                "Powerline glyph (U+E0B0) renders, but Nerd Font MDI codepoint \
                 (U+F048B) does not — Detail-view tab strip may misalign. Install \
                 a Nerd Font (`brew install font-meslo-lg-nerd-font`) or set \
                 `icons = \"unicode\"` in ~/.config/ebman/config.toml."
            );
        }
        resolved.icons.to_string()
    } else {
        raw.to_string()
    }
}

struct ProbeGuard;
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

// `std::io::IsTerminal` lives behind an unstable cfg on old stdlibs and
// behind `is_terminal` on newer ones. We avoid the dance by sniffing the
// std env directly — no TTY when stdin/stdout is piped.
trait IsTerminalLike {
    fn is_terminal_like(&self) -> bool;
}

impl IsTerminalLike for std::io::Stdout {
    fn is_terminal_like(&self) -> bool {
        use std::io::IsTerminal;
        self.is_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_one_cell_advance_is_supported() {
        assert!(classify_advance(1));
    }

    #[test]
    fn classify_other_advances_are_unsupported() {
        assert!(!classify_advance(0));
        assert!(!classify_advance(2));
        assert!(!classify_advance(7));
    }

    #[test]
    fn classify_auto_powerline_with_tab_icons_works_cleanly() {
        let r = classify_auto(true, true);
        assert_eq!(r.icons, "powerline");
        assert!(!r.warn_tab_icons_missing);
    }

    #[test]
    fn classify_auto_powerline_without_tab_icons_warns() {
        // Common case: Powerline-aware terminal font (E0B0 works) but no
        // MDI codepoints (F048B doesn't). The auto-resolve still picks
        // powerline because most of the chrome works, but the warning
        // flag fires so the user gets a tracing entry pointing at the
        // tab-strip misalignment.
        let r = classify_auto(true, false);
        assert_eq!(r.icons, "powerline");
        assert!(r.warn_tab_icons_missing);
    }

    #[test]
    fn classify_auto_no_powerline_picks_unicode_and_does_not_warn() {
        // No Powerline support at all → unicode fallback; tab-icon probe
        // is irrelevant because we never use those glyphs in unicode mode.
        // Warning would be confusing in this case.
        let r = classify_auto(false, false);
        assert_eq!(r.icons, "unicode");
        assert!(!r.warn_tab_icons_missing);

        // Same outcome if tab_icons somehow probes true but Powerline
        // didn't — the icons setting is the gatekeeper, no warning needed.
        let r = classify_auto(false, true);
        assert_eq!(r.icons, "unicode");
        assert!(!r.warn_tab_icons_missing);
    }

    #[test]
    fn resolve_passes_through_non_auto_values() {
        assert_eq!(resolve_icons_setting("unicode"), "unicode");
        assert_eq!(resolve_icons_setting("ascii"), "ascii");
        assert_eq!(resolve_icons_setting("powerline"), "powerline");
        // Unknown values are passed through untouched; the theme parser
        // has the fallback logic for those.
        assert_eq!(resolve_icons_setting("nerd"), "nerd");
        assert_eq!(resolve_icons_setting("bogus"), "bogus");
    }
}
