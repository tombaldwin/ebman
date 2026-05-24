# tb-tui-common

Shared TUI helpers reused across [`ebman`](https://github.com/tombaldwin/ebman) and [`pgman`](https://github.com/tombaldwin/pgman) — k9s-style terminal UIs for AWS Elastic Beanstalk and PostgreSQL respectively.

## What's in it

- **`font_probe`** — Powerline-glyph terminal probe (CSI device-status interrogation; returns whether the active font advances `U+E0B0` to a single cell).
- **`overlay`** — `OverlaySize` enum + `centered_overlay(category, frame)` helper so every modal overlay (text dump, picker, action confirm, palette) sizes from one table.
- **`theme`** — `IconStyle` enum + `contrast_text_for(bg)` WCAG-aware foreground picker so themed pills stay readable against arbitrary backgrounds.
- **`splash`** — pixel-art `render_frame(frame, palette)` loop for ASCII-grid splash animations driven by a per-character palette closure.
- **`util`** — `parse_bool(&str)` (loose form: `"yes"` / `"on"` / `"1"` etc.) and `write_atomic(path, content)` (writes via a sibling temp file + rename so a crash mid-write doesn't corrupt the existing file).

## Stability

Pre-1.0. The API mostly stabilised when ebman 0.8.0 / pgman 0.0.x shipped against it; expect non-breaking additions and occasional renames if a consumer surfaces a friction point. Pin a specific minor version if you care about that.

## License

MIT OR Apache-2.0.
