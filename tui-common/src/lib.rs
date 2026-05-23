//! tui-common — shared helpers used by ratatui-based TUIs `ebman` and
//! `pgman` (and any future siblings).
//!
//! What lives here:
//! - [`font_probe`] — pre-TUI terminal probe that decides whether the
//!   user's font renders Powerline / Nerd Font glyphs at one cell each.
//! - [`overlay`] — centred-popup sizing (`OverlaySize` categories +
//!   `centered_overlay` / `centered_rect` helpers).
//!
//! What does *not* live here: anything app-specific (AWS SDK clients,
//! Postgres connection code, EB-flavoured theme palettes, …). Pure
//! reusable plumbing only.

pub mod font_probe;
pub mod overlay;
pub mod theme;
pub mod util;
