# Fonts (optional, for the prettier glyph set)

Ebman runs fine in any terminal with the default `icons = "unicode"` config. For the Powerline-style pill chain, tab ribbon, and per-tab MDI icons (`icons = "powerline"` or `icons = "auto"`), your terminal needs a Nerd Font installed — vanilla Powerline fonts give you the triangles but tofu/boxes where the tab icons should be.

## 1. Install a Nerd Font

```bash
brew install font-meslo-lg-nerd-font           # Powerlevel10k crowd; safe default
brew install font-jetbrains-mono-nerd-font     # modern monospace, no ligature surprises
```

## 2. Set your terminal's font

Pick one of the `Mono` variants — they're sized for fixed-width TUIs (e.g. `MesloLGS Nerd Font Mono`, `JetBrainsMono Nerd Font Mono`):

- iTerm2: Preferences → Profiles → Text → Font
- Terminal.app: Preferences → Profiles → Font → Change
- Ghostty / Alacritty / WezTerm: `font-family` in the relevant config file
- VS Code / Cursor terminal: `terminal.integrated.fontFamily` in settings

## 3. Tell ebman to use the new glyphs

Either run `:settings` in ebman and pick `auto` (or `powerline`) from the Icons field, or add this to `~/.config/ebman/config.toml`:

```toml
icons = "auto"   # probes the terminal at startup; falls back to "unicode"
```

Restart ebman (or use `ebman ctl reload` if you're driving via the control socket) so the startup probe runs against your new font. `icons = "powerline"` skips the probe and forces the Nerd glyph set unconditionally.

Without a Nerd Font, stick to `icons = "unicode"` (the default) — everything still works, you just don't get the per-tab MDI icons.
