use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,

    // Severity colours
    pub health_green: Color,
    pub health_yellow: Color,
    pub health_red: Color,
    pub health_grey: Color,

    // Status colours
    pub status_ready: Color,
    pub status_updating: Color,
    pub status_terminating: Color,

    // Chrome
    pub border_idle: Color,
    pub border_active: Color,
    pub title: Color,
    pub title_alt: Color,
    pub muted: Color,
    pub text: Color,
    pub accent: Color,

    // Backgrounds
    pub row_alt_bg: Color,
    pub row_red_bg: Color,
    pub row_yellow_bg: Color,
    pub row_selected_bg: Color,
    pub row_hover_bg: Color,

    // App palette (16 distinct colours; sequential assignment in UI code)
    pub app_palette: Vec<Color>,

    // Icons preference
    pub icons: IconStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconStyle {
    Unicode,
    Ascii,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark",
            health_green: Color::Rgb(140, 220, 160),
            health_yellow: Color::Rgb(240, 210, 130),
            health_red: Color::Rgb(240, 130, 130),
            health_grey: Color::Rgb(150, 150, 160),

            status_ready: Color::Rgb(140, 220, 160),
            status_updating: Color::Rgb(240, 210, 130),
            status_terminating: Color::Rgb(240, 130, 130),

            border_idle: Color::Rgb(80, 90, 110),
            border_active: Color::Rgb(120, 200, 240),
            title: Color::Rgb(140, 200, 240),
            title_alt: Color::Rgb(220, 160, 240),
            muted: Color::Rgb(150, 155, 170),
            text: Color::Rgb(220, 222, 230),
            accent: Color::Rgb(255, 200, 120),

            row_alt_bg: Color::Rgb(18, 22, 28),
            row_red_bg: Color::Rgb(48, 18, 22),
            row_yellow_bg: Color::Rgb(40, 36, 16),
            row_selected_bg: Color::Rgb(40, 60, 90),
            row_hover_bg: Color::Rgb(30, 38, 50),

            app_palette: vec![
                Color::Rgb(120, 200, 240),
                Color::Rgb(220, 160, 240),
                Color::Rgb(140, 220, 200),
                Color::Rgb(220, 180, 140),
                Color::Rgb(180, 220, 240),
                Color::Rgb(240, 180, 220),
                Color::Rgb(180, 140, 230),
                Color::Rgb(140, 200, 220),
                Color::Rgb(220, 160, 200),
                Color::Rgb(150, 220, 200),
                Color::Rgb(120, 180, 255),
                Color::Rgb(200, 180, 255),
                Color::Rgb(255, 180, 230),
                Color::Rgb(180, 220, 255),
                Color::Rgb(170, 230, 230),
                Color::Rgb(220, 200, 250),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// Lighter palette suited to light-background terminals.
    pub fn light() -> Self {
        Self {
            name: "light",
            health_green: Color::Rgb(40, 130, 70),
            health_yellow: Color::Rgb(160, 110, 0),
            health_red: Color::Rgb(170, 30, 40),
            health_grey: Color::Rgb(100, 100, 110),

            status_ready: Color::Rgb(40, 130, 70),
            status_updating: Color::Rgb(160, 110, 0),
            status_terminating: Color::Rgb(170, 30, 40),

            border_idle: Color::Rgb(160, 165, 175),
            border_active: Color::Rgb(40, 100, 170),
            title: Color::Rgb(40, 100, 170),
            title_alt: Color::Rgb(120, 60, 170),
            muted: Color::Rgb(110, 115, 125),
            text: Color::Rgb(30, 32, 40),
            accent: Color::Rgb(180, 90, 0),

            row_alt_bg: Color::Rgb(238, 240, 244),
            row_red_bg: Color::Rgb(250, 220, 220),
            row_yellow_bg: Color::Rgb(252, 240, 200),
            row_selected_bg: Color::Rgb(210, 224, 240),
            row_hover_bg: Color::Rgb(228, 234, 246),

            app_palette: vec![
                Color::Rgb(40, 100, 170),
                Color::Rgb(120, 60, 170),
                Color::Rgb(20, 130, 130),
                Color::Rgb(170, 100, 40),
                Color::Rgb(80, 80, 170),
                Color::Rgb(170, 80, 130),
                Color::Rgb(90, 60, 170),
                Color::Rgb(20, 110, 140),
                Color::Rgb(170, 90, 130),
                Color::Rgb(40, 130, 110),
                Color::Rgb(20, 90, 200),
                Color::Rgb(100, 80, 200),
                Color::Rgb(190, 80, 170),
                Color::Rgb(60, 110, 200),
                Color::Rgb(40, 150, 150),
                Color::Rgb(140, 80, 200),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// High-contrast palette for accessibility. Pure black bg, pure white text,
    /// fully saturated severity colours, no greys-on-grey.
    pub fn high_contrast() -> Self {
        Self {
            name: "high-contrast",
            health_green: Color::Rgb(0, 255, 80),
            health_yellow: Color::Rgb(255, 230, 0),
            health_red: Color::Rgb(255, 60, 60),
            health_grey: Color::Rgb(220, 220, 220),

            status_ready: Color::Rgb(0, 255, 80),
            status_updating: Color::Rgb(255, 230, 0),
            status_terminating: Color::Rgb(255, 60, 60),

            border_idle: Color::Rgb(200, 200, 200),
            border_active: Color::Rgb(255, 255, 255),
            title: Color::Rgb(120, 220, 255),
            title_alt: Color::Rgb(255, 160, 255),
            muted: Color::Rgb(220, 220, 220),
            text: Color::Rgb(255, 255, 255),
            accent: Color::Rgb(255, 180, 0),

            row_alt_bg: Color::Rgb(20, 20, 20),
            row_red_bg: Color::Rgb(80, 0, 0),
            row_yellow_bg: Color::Rgb(60, 50, 0),
            row_selected_bg: Color::Rgb(0, 80, 160),
            row_hover_bg: Color::Rgb(40, 40, 40),

            app_palette: vec![
                Color::Rgb(0, 220, 255),
                Color::Rgb(255, 100, 255),
                Color::Rgb(0, 255, 200),
                Color::Rgb(255, 180, 0),
                Color::Rgb(150, 200, 255),
                Color::Rgb(255, 200, 220),
                Color::Rgb(190, 130, 255),
                Color::Rgb(80, 220, 240),
                Color::Rgb(255, 150, 200),
                Color::Rgb(80, 255, 200),
                Color::Rgb(120, 180, 255),
                Color::Rgb(200, 180, 255),
                Color::Rgb(255, 180, 240),
                Color::Rgb(180, 220, 255),
                Color::Rgb(170, 255, 230),
                Color::Rgb(220, 200, 255),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// Parse a theme by name. Returns the matched theme plus an optional warning
    /// when the input didn't match a known preset.
    pub fn resolve(name: &str) -> (Self, Option<String>) {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "dark" => (Self::dark(), None),
            "light" => (Self::light(), None),
            "high-contrast" | "highcontrast" | "hc" => (Self::high_contrast(), None),
            other => (
                Self::dark(),
                Some(format!("unknown theme {other:?} — using 'dark'")),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_names() {
        let (t, w) = Theme::resolve("dark");
        assert_eq!(t.name, "dark");
        assert!(w.is_none());
        let (t, w) = Theme::resolve("LIGHT");
        assert_eq!(t.name, "light");
        assert!(w.is_none());
        let (t, w) = Theme::resolve("  Dark ");
        assert_eq!(t.name, "dark");
        assert!(w.is_none());
    }

    #[test]
    fn resolve_high_contrast_variants() {
        for n in ["high-contrast", "highcontrast", "HC", "Hc"] {
            let (t, w) = Theme::resolve(n);
            assert_eq!(t.name, "high-contrast", "for input {n:?}");
            assert!(w.is_none());
        }
    }

    #[test]
    fn resolve_unknown_falls_back_with_warning() {
        let (t, w) = Theme::resolve("dracula");
        assert_eq!(t.name, "dark");
        let msg = w.expect("expected a warning");
        assert!(msg.to_lowercase().contains("dracula"));
    }

    #[test]
    fn resolve_empty_uses_default() {
        let (t, w) = Theme::resolve("");
        assert_eq!(t.name, "dark");
        assert!(w.is_none());
    }

    #[test]
    fn palettes_are_distinct_per_theme() {
        // Sanity: light theme is darker text on lighter bg; dark theme inverts.
        assert_ne!(Theme::dark().text, Theme::light().text);
        assert_ne!(Theme::dark().row_alt_bg, Theme::light().row_alt_bg);
    }
}
