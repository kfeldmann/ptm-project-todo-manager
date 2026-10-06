pub mod markdown;
pub mod theme;
pub mod views;

use ratatui::Frame;
use crate::app::{App, Screen};

/// Top-level render: draws the active screen then any overlay on top.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    match app.screen {
        Screen::ProjectList      => views::project_list::render(frame, area, app),
        Screen::ProjectDetail    => views::project_detail::render(frame, area, app),
        Screen::TodoList         => views::todo_list::render(frame, area, app),
    }

    if app.overlay.is_some() {
        views::overlay::render_overlay(frame, area, app);
    }
}
