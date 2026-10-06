use std::str::FromStr;

/// Returned by [`TodoStatus`]'s [`FromStr`] impl when the input string does
/// not match any known variant.
#[derive(Debug)]
pub struct UnknownTodoStatus(pub String);

impl std::fmt::Display for UnknownTodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown todo status: {:?}", self.0)
    }
}

impl std::error::Error for UnknownTodoStatus {}

/// Completion/progress status of a todo item.
#[derive(Debug, Clone, PartialEq)]
pub enum TodoStatus {
    New,
    InProgress,
    Canceled,
    Done,
}

impl TodoStatus {
    /// Advance to the next status in the cycle: New → InProgress → Done → Canceled → New.
    pub fn cycle(&self) -> Self {
        match self {
            Self::New        => Self::InProgress,
            Self::InProgress => Self::Done,
            Self::Done       => Self::Canceled,
            Self::Canceled   => Self::New,
        }
    }

    /// Single-character representation used in list views.
    pub fn char_repr(&self) -> char {
        match self {
            Self::New        => '-',
            Self::InProgress => 'o',
            Self::Canceled   => 'x',
            Self::Done       => '✓',
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New        => "new",
            Self::InProgress => "in_progress",
            Self::Canceled   => "canceled",
            Self::Done       => "done",
        }
    }

    #[allow(dead_code)] // Filtering is currently done in SQL; kept for potential Rust-side use.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::New | Self::InProgress)
    }
}

impl FromStr for TodoStatus {
    type Err = UnknownTodoStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "new"         => Ok(Self::New),
            "in_progress" => Ok(Self::InProgress),
            "done"        => Ok(Self::Done),
            "canceled"    => Ok(Self::Canceled),
            _             => Err(UnknownTodoStatus(s.to_owned())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub id: String,
    pub name: String,
    #[allow(dead_code)] // Color display not yet implemented in the TUI; mirrors the DB schema.
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub description: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub sort_order: f64,
    pub archived: bool,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub created_at: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub updated_at: String,
    pub tags: Vec<Tag>,
    /// ISO date of the earliest active reminder across all todos in this project.
    pub soonest_reminder: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Todo {
    pub id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub status: TodoStatus,
    pub reminder: Option<String>,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub sort_order: f64,
    pub archived: bool,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub created_at: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TodoStatus::cycle ──────────────────────────────────────────────────

    #[test]
    fn cycle_new_to_in_progress() {
        assert_eq!(TodoStatus::New.cycle(), TodoStatus::InProgress);
    }

    #[test]
    fn cycle_in_progress_to_done() {
        assert_eq!(TodoStatus::InProgress.cycle(), TodoStatus::Done);
    }

    #[test]
    fn cycle_done_to_canceled() {
        assert_eq!(TodoStatus::Done.cycle(), TodoStatus::Canceled);
    }

    #[test]
    fn cycle_canceled_back_to_new() {
        assert_eq!(TodoStatus::Canceled.cycle(), TodoStatus::New);
    }

    // ── TodoStatus::char_repr ──────────────────────────────────────────────

    #[test]
    fn char_repr_all_variants() {
        assert_eq!(TodoStatus::New.char_repr(), '-');
        assert_eq!(TodoStatus::InProgress.char_repr(), 'o');
        assert_eq!(TodoStatus::Canceled.char_repr(), 'x');
        assert_eq!(TodoStatus::Done.char_repr(), '\u{2713}'); // ✓
    }

    // ── TodoStatus::as_str ────────────────────────────────────────────────

    #[test]
    fn as_str_all_variants() {
        assert_eq!(TodoStatus::New.as_str(), "new");
        assert_eq!(TodoStatus::InProgress.as_str(), "in_progress");
        assert_eq!(TodoStatus::Done.as_str(), "done");
        assert_eq!(TodoStatus::Canceled.as_str(), "canceled");
    }

    // ── TodoStatus::is_active ─────────────────────────────────────────────

    #[test]
    fn is_active_new_and_in_progress() {
        assert!(TodoStatus::New.is_active());
        assert!(TodoStatus::InProgress.is_active());
    }

    #[test]
    fn is_not_active_done_and_canceled() {
        assert!(!TodoStatus::Done.is_active());
        assert!(!TodoStatus::Canceled.is_active());
    }

    // ── TodoStatus FromStr ────────────────────────────────────────────────

    #[test]
    fn parse_all_valid_statuses() {
        assert_eq!("new".parse::<TodoStatus>().unwrap(), TodoStatus::New);
        assert_eq!("in_progress".parse::<TodoStatus>().unwrap(), TodoStatus::InProgress);
        assert_eq!("done".parse::<TodoStatus>().unwrap(), TodoStatus::Done);
        assert_eq!("canceled".parse::<TodoStatus>().unwrap(), TodoStatus::Canceled);
    }

    #[test]
    fn parse_invalid_status_returns_err_containing_input() {
        let err = "bogus".parse::<TodoStatus>().unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn as_str_roundtrips_via_parse() {
        for status in [
            TodoStatus::New,
            TodoStatus::InProgress,
            TodoStatus::Done,
            TodoStatus::Canceled,
        ] {
            assert_eq!(status.as_str().parse::<TodoStatus>().unwrap(), status);
        }
    }

    // ── UnknownTodoStatus display ─────────────────────────────────────────

    #[test]
    fn unknown_status_display_includes_the_value() {
        let err = UnknownTodoStatus("weird_value".to_string());
        assert!(err.to_string().contains("weird_value"));
    }
}

#[derive(Debug, Clone)]
pub struct Update {
    pub id: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub project_id: String,
    /// Editable date/label column (auto-populated with today's date on creation).
    pub label: String,
    pub body: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub sort_order: f64,
    pub created_at: String,
    #[allow(dead_code)] // Schema field; not yet surfaced by the UI.
    pub updated_at: String,
}
