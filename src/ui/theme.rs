use ratatui::style::Color;

/// Central colour palette for the application.
pub struct Theme {
    pub highlight_bg: Color,
    pub highlight_fg: Color,
    pub header_fg: Color,
    pub hint_fg: Color,
    pub tag_fg: Color,

    pub done_fg: Color,
    pub canceled_fg: Color,
    pub in_progress_fg: Color,
    pub new_fg: Color,
    pub overlay_border_fg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            highlight_bg:    Color::DarkGray,
            highlight_fg:    Color::White,
            header_fg:       Color::Blue,
            hint_fg:         Color::DarkGray,
            tag_fg:          Color::Blue,

            done_fg:         Color::Green,
            canceled_fg:     Color::DarkGray,
            in_progress_fg:  Color::Yellow,
            new_fg:          Color::Reset,
            overlay_border_fg: Color::Yellow,
        }
    }
}
