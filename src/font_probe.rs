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
    detect_inner().unwrap_or(false)
}

fn detect_inner() -> io::Result<bool> {
    let mut stdout = io::stdout();
    // Save where the cursor is so we can restore it; record the column
    // before the probe write to compute the advance.
    terminal::enable_raw_mode()?;
    let restore = ProbeGuard;
    stdout.execute(cursor::SavePosition)?;
    let (col_before, _row) = cursor::position().unwrap_or((0, 0));
    // Powerline triangle. If the font supports it, this is a single cell.
    stdout.execute(Print("\u{E0B0}"))?;
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

/// Resolve a configured icon style. `"auto"` triggers [`detect_powerline_support`]
/// and resolves to `"powerline"` on a yes / `"unicode"` on a no; any other
/// value is passed through unchanged so the regular [`crate::theme`] parser
/// handles it (and surfaces typos as the existing fallback to unicode).
///
/// Pure-with-side-effects: only does I/O when the input is literally
/// `"auto"`. Run once at startup, before TUI init.
pub fn resolve_icons_setting(raw: &str) -> String {
    if raw.eq_ignore_ascii_case("auto") {
        if detect_powerline_support() {
            "powerline".into()
        } else {
            "unicode".into()
        }
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
