use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use rusqlite::Connection;
use std::path::PathBuf;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::time::SystemTime;

use crate::data::{db, models::*, repo::{self, TodoGroup}};
use crate::input::{InputState, InputKeyResult, handle_input_key};

// ─── Navigation ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    ProjectList,
    ProjectDetail,
    TodoList,
}

/// Which tab of the Project Detail view is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PdTab {
    Description,
    Updates,
    Todos,
    Files,
}

// ─── Spell-check target ──────────────────────────────────────────────────────

/// Identifies which piece of data is being spell-checked so the corrected
/// text can be written back to the database when the overlay closes.
#[derive(Debug, Clone)]
pub enum SpellTarget {
    /// A todo's title field.  Argument is the todo ID.
    TodoTitle(String),
    /// An update's body field.  Arguments are the update ID and its
    /// current label (date), captured at open time so a reload mid-session
    /// cannot lose the label.
    UpdateBody { id: String, label: String },
    /// The active project's description field.
    ProjectDescription,
}

// ─── Overlays ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Overlay {
    QuickCapture {
        input: InputState,
    },
    TagPicker {
        all_tags: Vec<Tag>,
        applied_ids: Vec<String>,
        search: String,
        cursor: usize,
        project_id: String,   // empty string = filter mode (no project)
        filter_mode: bool,
    },
    ProjectPicker {
        projects: Vec<Project>,
        search: String,
        cursor: usize,
        todo_id: String,
        current_project_id: Option<String>,
    },
    ConfirmDelete {
        message: String,
        target: DeleteTarget,
    },
    /// Inline reminder date entry (YYYY-MM-DD).
    ReminderInput {
        input: InputState,
        todo_id: String,
        /// Non-empty when the last Enter press had a format error.
        error: Option<String>,
    },
    /// Inline rename input for an existing tag.
    RenameTag {
        tag_id: String,
        input: InputState,
        error: Option<String>,
        /// Stored so we can reopen the TagPicker after a successful rename.
        return_project_id: String,
        return_filter_mode: bool,
    },
    /// Global key bindings reference. `scroll` tracks vertical offset.
    KeyHelp {
        scroll: u16,
    },
    /// Generic dismissible warning/error notice.
    /// Dismissed by any key press.
    Warn {
        title: String,
        body: String,
    },
    /// Spell-check session for a single text field.
    SpellCheck {
        /// Text split into (text, is_word) segments; word segments are
        /// mutated in place as replacements are accepted.
        segments: Vec<(String, bool)>,
        /// Sorted indices into `segments` that are still flagged as
        /// misspelled.  Elements are removed as words are handled.
        bad_words: Vec<usize>,
        /// Position in `bad_words` of the word currently under review.
        current: usize,
        /// Replacement candidates for `segments[bad_words[current]]`.
        suggestions: Vec<String>,
        /// Where to write the corrected text on close.
        target: SpellTarget,
        /// Set once every entry in `bad_words` has been handled.
        done: bool,
    },
}

#[derive(Debug, Clone)]
pub enum DeleteTarget {
    Project(String),
    Update(String),
    Todo(String),
}

// ─── Pending $EDITOR ──────────────────────────────────────────────────────────

pub struct PendingEditor {
    pub content: String,
    pub target: EditorTarget,
    /// Stable identifier used as the temp-file name (e.g. `desc_<uuid>`).
    /// Two ptm sessions editing the same record will collide on this name.
    pub slot: String,
}

pub enum EditorTarget {
    ProjectDescription,
    AddUpdate,
    EditUpdate(String),    // update_id
}

// ─── App ──────────────────────────────────────────────────────────────────────

pub struct App {
    pub conn: Connection,
    pub should_quit: bool,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub pending_editor: Option<PendingEditor>,
    /// Live spell-checker backed by the system Hunspell dictionary.
    /// `None` when no dictionary was found at startup.
    pub spell: Option<crate::spell::SpellChecker>,

    // ── Project List state ──────────────────────────────────────────────────
    pub pl_projects: Vec<Project>,
    pub pl_cursor: usize,
    pub pl_show_archived: bool,
    pub pl_archived_count: u32,
    pub pl_search: String,
    pub pl_searching: bool,
    pub tag_filter: Option<String>,
    /// `Some(state)` when inline project creation is active.
    pub pl_creating: Option<InputState>,
    /// `Some(state)` when inline project title editing is active.
    pub pl_editing_title: Option<InputState>,

    // ── Project Detail state ────────────────────────────────────────────────
    pub pd_project: Option<Project>,
    pub pd_todos: Vec<Todo>,
    pub pd_updates: Vec<Update>,
    pub pd_todo_cursor: usize,
    pub pd_tab: PdTab,
    pub pd_update_cursor: usize,
    /// Scroll offset (lines) for the Description tab.
    pub pd_desc_scroll: u16,
    /// Max scroll for the Description tab; set by render so the key handler can clamp.
    pub pd_desc_max_scroll: std::cell::Cell<u16>,
    /// ID of the update currently open in full-screen viewer (`None` = list view).
    pub pd_open_update: Option<String>,
    /// Scroll offset for the open update viewer.
    pub pd_update_view_scroll: u16,
    /// Max scroll for the open update viewer; set by render so the key handler can clamp.
    pub pd_update_view_max_scroll: std::cell::Cell<u16>,
    /// Max scroll for the Key Help overlay; set by render so the key handler can clamp.
    pub key_help_max_scroll: std::cell::Cell<u16>,
    /// `Some(state)` when adding a todo inline.
    pub pd_adding_todo: Option<InputState>,
    /// `Some(state)` when editing the title inline.
    pub pd_editing_title: Option<InputState>,
    /// `Some((todo_id, state))` when editing a todo title inline.
    pub pd_editing_todo: Option<(String, InputState)>,
    /// Hide done/canceled todos globally (Project Detail Todos tab + Global Todo List).
    pub hide_done: bool,
    /// Search query active in the Project Detail screen.
    pub pd_search: String,
    /// `true` while the user is actively typing the PD search query.
    pub pd_searching: bool,

    // ── Global Todo List state ───────────────────────────────────────────────
    pub tl_groups: Vec<TodoGroup>,
    /// Flat cursor index (counts only todo rows, not group headers).
    pub tl_cursor: usize,
    pub tl_search: String,
    pub tl_searching: bool,
    /// `Some((todo_id, state))` when editing a todo title inline.
    pub tl_editing: Option<(String, InputState)>,
    /// Show archived todos in the Global Todo List (default: false = hidden).
    pub tl_show_archived: bool,
    /// Count of archived todos visible in the global list scope (non-archived projects + inbox).
    pub tl_archived_count: u32,
    /// Show archived todos in the Project Detail Todos tab (default: false = hidden).
    pub pd_show_archived_todos: bool,
    /// Count of archived todos in the current project.
    pub pd_archived_todo_count: u32,
    /// Count of done/canceled non-archived todos hidden in the global todo list.
    pub tl_done_count: u32,
    /// Count of done/canceled non-archived todos hidden in the current project detail.
    pub pd_done_count: u32,
}

fn data_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".local")
                .join("share")
        });
    base.join("ptm").join("ptm.db")
}

impl App {
    pub fn new() -> Result<Self> {
        let path = data_path();
        let conn = db::open(&path)?;

        let projects       = repo::list_projects(&conn, false, None, None)?;
        let archived_count = repo::count_archived(&conn)?;

        // Try to load the spell checker; failure is non-fatal.
        let data_dir = path.parent().unwrap_or(&path);
        let spell = crate::spell::SpellChecker::new(data_dir);

        Ok(App {
            conn,
            should_quit: false,
            screen: Screen::ProjectList,
            overlay: None,
            pending_editor: None,
            spell,

            pl_projects: projects,
            pl_cursor: 0,
            pl_show_archived: false,
            pl_archived_count: archived_count,
            pl_search: String::new(),
            pl_searching: false,
            tag_filter: None,
            pl_creating: None,
            pl_editing_title: None,

            pd_project: None,
            pd_todos: Vec::new(),
            pd_updates: Vec::new(),
            pd_todo_cursor: 0,
            pd_tab: PdTab::Todos,
            pd_update_cursor: 0,
            pd_desc_scroll: 0,
            pd_desc_max_scroll: std::cell::Cell::new(0),
            pd_open_update: None,
            pd_update_view_scroll: 0,
            pd_update_view_max_scroll: std::cell::Cell::new(0),
            key_help_max_scroll: std::cell::Cell::new(0),
            pd_adding_todo: None,
            pd_editing_title: None,
            pd_editing_todo: None,
            hide_done: false,
            pd_search: String::new(),
            pd_searching: false,

            tl_groups: Vec::new(),
            tl_cursor: 0,
            tl_search: String::new(),
            tl_searching: false,
            tl_editing: None,
            tl_show_archived: false,
            tl_archived_count: 0,
            pd_show_archived_todos: false,
            pd_archived_todo_count: 0,
            tl_done_count: 0,
            pd_done_count: 0,
        })
    }

    // ─── Top-level key handler ─────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // If an overlay is active, send all keys there first.
        if self.overlay.is_some() {
            return self.handle_key_overlay(key);
        }

        // Global: i → quick capture (only when not in any inline-edit mode).
        if key.code == KeyCode::Char('i') && !self.is_inline_editing() {
            self.overlay = Some(Overlay::QuickCapture { input: InputState::new() });
            return Ok(());
        }

        // Global: ? → key help overlay.
        if key.code == KeyCode::Char('?') && !self.is_inline_editing() {
            self.overlay = Some(Overlay::KeyHelp { scroll: 0 });
            return Ok(());
        }

        match self.screen {
            Screen::ProjectList      => self.handle_key_project_list(key),
            Screen::ProjectDetail    => self.handle_key_project_detail(key),
            Screen::TodoList         => self.handle_key_todo_list(key),
        }
    }

    // ─── Overlay dispatcher ────────────────────────────────────────────────

    fn handle_key_overlay(&mut self, key: KeyEvent) -> Result<()> {
        // Take the overlay out so we can freely mutate self in sub-handlers.
        let Some(overlay) = self.overlay.take() else {
            return Ok(());
        };

        match overlay {
            Overlay::QuickCapture { input } => {
                self.overlay = self.handle_quick_capture(key, input)?;
            }
            ov @ Overlay::TagPicker { .. } => {
                self.overlay = self.handle_tag_picker(key, ov)?;
            }
            Overlay::ProjectPicker { projects, search, cursor, todo_id, current_project_id } => {
                self.overlay = self.handle_project_picker(
                    key, projects, search, cursor, todo_id, current_project_id,
                )?;
            }
            Overlay::ConfirmDelete { message, target } => {
                self.overlay = self.handle_confirm_delete(key, message, target)?;
            }
            Overlay::ReminderInput { input, todo_id, .. } => {
                self.overlay = self.handle_reminder_input(key, input, todo_id)?;
            }
            Overlay::RenameTag { tag_id, input, error: _, return_project_id, return_filter_mode } => {
                self.overlay = self.handle_rename_tag(key, tag_id, input, return_project_id, return_filter_mode)?;
            }
            Overlay::KeyHelp { scroll } => {
                self.overlay = self.handle_key_help(key, scroll)?;
            }
            Overlay::SpellCheck { segments, bad_words, current, suggestions, target, done } => {
                self.overlay = self.handle_spell_check(
                    key, segments, bad_words, current, suggestions, target, done,
                )?;
            }
            Overlay::Warn { .. } => {
                self.overlay = None;
            }
        }
        Ok(())
    }

    // ─── Quick Capture ────────────────────────────────────────────────────

    fn handle_quick_capture(
        &mut self, key: KeyEvent, mut input: InputState,
    ) -> Result<Option<Overlay>> {
        match handle_input_key(&mut input, key) {
            InputKeyResult::Escape => return Ok(None),
            InputKeyResult::Enter => {
                let title = input.trimmed().to_string();
                if !title.is_empty() {
                    repo::create_todo(&self.conn, None, &title)?;
                    if self.screen == Screen::TodoList {
                        self.reload_todo_list()?;
                    }
                }
                return Ok(None);
            }
            InputKeyResult::Continue => {}
        }
        Ok(Some(Overlay::QuickCapture { input }))
    }

    // ─── Tag Picker ───────────────────────────────────────────────────────
    //
    // Navigation uses Up/Down arrows only so that j and k can be typed freely
    // into the search field (e.g. for tag names like "java" or "kotlin").

    fn handle_tag_picker(
        &mut self,
        key: KeyEvent,
        state: Overlay,
    ) -> Result<Option<Overlay>> {
        let Overlay::TagPicker { mut all_tags, mut applied_ids, mut search, mut cursor, project_id, filter_mode } = state
        else { unreachable!() };
        // Build visible item list (filtered tags + optional "create" entry).
        let filtered: Vec<usize> = all_tags
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                search.is_empty()
                    || t.name.to_lowercase().contains(&search.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();

        let exact_match = all_tags
            .iter()
            .any(|t| t.name.to_lowercase() == search.to_lowercase());
        let has_create = !search.is_empty() && !filter_mode && !exact_match;
        let item_count = filtered.len() + if has_create { 1 } else { 0 };

        match key.code {
            KeyCode::Esc => return Ok(None),

            // Arrow-key-only navigation; j/k fall through to the Char branch.
            KeyCode::Down => {
                if cursor + 1 < item_count { cursor += 1; }
            }
            KeyCode::Up => {
                cursor = cursor.saturating_sub(1);
            }

            KeyCode::Enter => {
                if filter_mode {
                    if let Some(&idx) = filtered.get(cursor) {
                        let tag = &all_tags[idx];
                        // Toggle: selecting the active filter clears it.
                        if self.tag_filter.as_deref() == Some(tag.name.as_str()) {
                            self.tag_filter = None;
                        } else {
                            self.tag_filter = Some(tag.name.clone());
                        }
                    }
                    self.reload_project_list()?;
                    self.reload_todo_list()?;
                    return Ok(None);
                }

                // Edit mode: toggle tag on the project.
                if cursor < filtered.len() {
                    let tag = &all_tags[filtered[cursor]];
                    let tag_id   = tag.id.clone();
                    let tag_name = tag.name.clone();
                    if applied_ids.contains(&tag_id) {
                        repo::untag_project(&self.conn, &project_id, &tag_id)?;
                        applied_ids.retain(|id| id != &tag_id);
                    } else {
                        repo::tag_project(&self.conn, &project_id, &tag_name)?;
                        applied_ids.push(tag_id);
                    }
                    self.reload_project_detail()?;
                    all_tags = repo::list_all_tags(&self.conn)?;
                    // Refresh applied_ids from DB
                    if let Some(ref p) = self.pd_project {
                        applied_ids = p.tags.iter().map(|t| t.id.clone()).collect();
                    }
                } else if has_create {
                    // Create new tag and apply it.
                    repo::tag_project(&self.conn, &project_id, &search)?;
                    self.reload_project_detail()?;
                    all_tags = repo::list_all_tags(&self.conn)?;
                    if let Some(ref p) = self.pd_project {
                        applied_ids = p.tags.iter().map(|t| t.id.clone()).collect();
                    }
                    search.clear();
                    cursor = 0;
                }
            }

            // Rename the highlighted tag (only available when search is clear
            // and the cursor is on a real tag row, not the "+ Create" row).
            KeyCode::Char('r') if search.is_empty() => {
                if let Some(&idx) = filtered.get(cursor) {
                    let tag = &all_tags[idx];
                    let input = InputState::from_str(&tag.name);
                    return Ok(Some(Overlay::RenameTag {
                        tag_id: tag.id.clone(),
                        input,
                        error: None,
                        return_project_id: project_id,
                        return_filter_mode: filter_mode,
                    }));
                }
            }

            KeyCode::Char(c) => {
                search.push(c);
                cursor = 0;
            }
            KeyCode::Backspace => {
                if search.is_empty() {
                    return Ok(None);
                }
                search.pop();
                cursor = 0;
            }
            _ => {}
        }

        Ok(Some(Overlay::TagPicker {
            all_tags, applied_ids, search, cursor, project_id, filter_mode,
        }))
    }

    // ─── Rename Tag ───────────────────────────────────────────────────────

    fn handle_rename_tag(
        &mut self,
        key: KeyEvent,
        tag_id: String,
        mut input: InputState,
        return_project_id: String,
        return_filter_mode: bool,
    ) -> Result<Option<Overlay>> {
        match handle_input_key(&mut input, key) {
            InputKeyResult::Escape => {
                // Reopen the TagPicker the user came from.
                return self.reopen_tag_picker(return_project_id, return_filter_mode);
            }
            InputKeyResult::Enter => {
                let new_name = input.trimmed().to_string();
                if new_name.is_empty() {
                    return Ok(Some(Overlay::RenameTag {
                        tag_id,
                        input,
                        error: Some("Name cannot be empty".into()),
                        return_project_id,
                        return_filter_mode,
                    }));
                }
                match repo::rename_tag(&self.conn, &tag_id, &new_name) {
                    Ok(()) => {
                        self.reload_project_list()?;
                        self.reload_todo_list()?;
                        if self.pd_project.is_some() {
                            self.reload_project_detail()?;
                        }
                        // Reopen the TagPicker with refreshed data.
                        return self.reopen_tag_picker(return_project_id, return_filter_mode);
                    }
                    Err(_) => {
                        return Ok(Some(Overlay::RenameTag {
                            tag_id,
                            input,
                            error: Some(format!("'{}' already exists", new_name)),
                            return_project_id,
                            return_filter_mode,
                        }));
                    }
                }
            }
            InputKeyResult::Continue => {}
        }
        Ok(Some(Overlay::RenameTag { tag_id, input, error: None, return_project_id, return_filter_mode }))
    }

    /// Reconstruct a `TagPicker` overlay from stored return context.
    fn reopen_tag_picker(
        &self,
        project_id: String,
        filter_mode: bool,
    ) -> Result<Option<Overlay>> {
        let all_tags = repo::list_all_tags(&self.conn)?;
        let applied_ids = if filter_mode || project_id.is_empty() {
            Vec::new()
        } else {
            self.pd_project.as_ref()
                .map(|p| p.tags.iter().map(|t| t.id.clone()).collect())
                .unwrap_or_default()
        };
        Ok(Some(Overlay::TagPicker {
            all_tags,
            applied_ids,
            search: String::new(),
            cursor: 0,
            project_id,
            filter_mode,
        }))
    }

    // ─── Project Picker ───────────────────────────────────────────────────
    //
    // Navigation uses Up/Down arrows only (same rationale as TagPicker).

    fn handle_project_picker(
        &mut self,
        key: KeyEvent,
        projects: Vec<Project>,
        mut search: String,
        mut cursor: usize,
        todo_id: String,
        current_project_id: Option<String>,
    ) -> Result<Option<Overlay>> {
        // Build destination list.
        let items = self.build_picker_items(&projects, &search, &current_project_id);
        let count = items.len();

        match key.code {
            KeyCode::Esc => return Ok(None),

            KeyCode::Down => {
                if cursor + 1 < count { cursor += 1; }
            }
            KeyCode::Up => {
                cursor = cursor.saturating_sub(1);
            }

            KeyCode::Enter => {
                if let Some((dest_pid, _)) = items.get(cursor) {
                    repo::move_todo(&self.conn, &todo_id, dest_pid.as_deref())?;
                    match self.screen {
                        Screen::ProjectDetail => self.reload_project_detail()?,
                        Screen::TodoList      => self.reload_todo_list()?,
                        _ => {}
                    }
                }
                return Ok(None);
            }

            KeyCode::Char(c) => {
                search.push(c);
                cursor = 0;
            }
            KeyCode::Backspace => {
                search.pop();
                cursor = 0;
            }
            _ => {}
        }

        Ok(Some(Overlay::ProjectPicker {
            projects, search, cursor, todo_id, current_project_id,
        }))
    }

    /// Build the ordered list of `(project_id, display_name)` destination items for the picker.
    pub fn build_picker_items(
        &self,
        projects: &[Project],
        search: &str,
        current_project_id: &Option<String>,
    ) -> Vec<(Option<String>, String)> {
        let mut items: Vec<(Option<String>, String)> = Vec::new();

        // Inbox (unless current location IS the inbox).
        if current_project_id.is_some() {
            items.push((None, "── Inbox ──".to_string()));
        }

        for p in projects {
            let is_current = current_project_id.as_deref() == Some(p.id.as_str());
            if is_current { continue; }
            if search.is_empty() || p.title.to_lowercase().contains(&search.to_lowercase()) {
                items.push((Some(p.id.clone()), p.title.clone()));
            }
        }
        items
    }

    // ─── Confirm Delete ───────────────────────────────────────────────────

    fn handle_confirm_delete(
        &mut self,
        key: KeyEvent,
        message: String,
        target: DeleteTarget,
    ) -> Result<Option<Overlay>> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                match &target {
                    DeleteTarget::Project(id) => {
                        let id = id.clone();
                        repo::delete_project(&self.conn, &id)?;
                        if self.screen == Screen::ProjectDetail {
                            self.screen = Screen::ProjectList;
                            self.pd_project = None;
                        }
                        self.reload_project_list()?;
                    }
                    DeleteTarget::Todo(id) => {
                        let id = id.clone();
                        repo::delete_todo(&self.conn, &id)?;
                        match self.screen {
                            Screen::ProjectDetail => {
                                self.pd_todo_cursor = self.pd_todo_cursor.saturating_sub(1);
                                self.reload_project_detail()?;
                            }
                            Screen::TodoList => {
                                self.tl_cursor = self.tl_cursor.saturating_sub(1);
                                self.reload_todo_list()?;
                            }
                            _ => {}
                        }
                    }
                    DeleteTarget::Update(id) => {
                        let id = id.clone();
                        repo::delete_update(&self.conn, &id)?;
                        self.reload_project_detail()?;
                    }
                }
                Ok(None)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Ok(None),
            _ => Ok(Some(Overlay::ConfirmDelete { message, target })),
        }
    }

    // ─── Reminder Input ───────────────────────────────────────────────────

    fn handle_reminder_input(
        &mut self,
        key: KeyEvent,
        mut input: InputState,
        todo_id: String,
    ) -> Result<Option<Overlay>> {
        match handle_input_key(&mut input, key) {
            InputKeyResult::Escape => return Ok(None),
            InputKeyResult::Enter => {
                let val = input.trimmed().to_string();
                if val.is_empty() {
                    repo::set_todo_reminder(&self.conn, &todo_id, None)?;
                    match self.screen {
                        Screen::ProjectDetail => self.reload_project_detail()?,
                        Screen::TodoList      => self.reload_todo_list()?,
                        _ => {}
                    }
                    return Ok(None);
                }
                match NaiveDate::parse_from_str(&val, "%Y-%m-%d") {
                    Ok(_) => {
                        repo::set_todo_reminder(&self.conn, &todo_id, Some(val.as_str()))?;
                        match self.screen {
                            Screen::ProjectDetail => self.reload_project_detail()?,
                            Screen::TodoList      => self.reload_todo_list()?,
                            _ => {}
                        }
                        return Ok(None);
                    }
                    Err(_) => {
                        return Ok(Some(Overlay::ReminderInput {
                            input,
                            todo_id,
                            error: Some("Invalid date — use YYYY-MM-DD (e.g. 2025-12-31)".into()),
                        }));
                    }
                }
            }
            InputKeyResult::Continue => {}
        }
        Ok(Some(Overlay::ReminderInput { input, todo_id, error: None }))
    }

    // ─── Project List ─────────────────────────────────────────────────────

    fn handle_key_project_list(&mut self, key: KeyEvent) -> Result<()> {
        // ── Inline title-edit mode ──
        if let Some(mut state) = self.pl_editing_title.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        if let Some(id) = self.pl_selected_id() {
                            repo::update_project_title(&self.conn, &id, &title)?;
                            self.reload_project_list()?;
                        }
                    }
                }
                InputKeyResult::Continue => { self.pl_editing_title = Some(state); }
            }
            return Ok(());
        }

        // ── Inline create mode ──
        if let Some(mut state) = self.pl_creating.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        repo::create_project(&self.conn, &title)?;
                        self.reload_project_list()?;
                        self.pl_cursor = self.pl_projects.len().saturating_sub(1);
                    }
                }
                InputKeyResult::Continue => { self.pl_creating = Some(state); }
            }
            return Ok(());
        }

        // ── Search mode ──
        if self.pl_searching {
            match key.code {
                KeyCode::Esc => {
                    self.pl_searching = false;
                    self.pl_search.clear();
                    self.reload_project_list()?;
                }
                KeyCode::Enter => { self.pl_searching = false; }
                KeyCode::Char(c) => {
                    self.pl_search.push(c);
                    self.reload_project_list()?;
                    self.pl_cursor = 0;
                }
                KeyCode::Backspace => {
                    self.pl_search.pop();
                    self.reload_project_list()?;
                    self.pl_cursor = 0;
                }
                _ => {}
            }
            return Ok(());
        }

        // ── Normal mode ──
        match key.code {
            KeyCode::Char('Q') => { self.should_quit = true; }

            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.pl_projects.len().saturating_sub(1);
                if self.pl_cursor < max { self.pl_cursor += 1; }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.pl_cursor > 0 { self.pl_cursor -= 1; }
            }

            KeyCode::Char('J') => {
                if let Some(id) = self.pl_selected_id() {
                    let next_pos = self.pl_cursor + 1;
                    if let Some(neighbor) = self.pl_projects.get(next_pos) {
                        repo::swap_project_order(&self.conn, &id, &neighbor.id)?;
                        self.reload_project_list()?;
                        self.pl_cursor = next_pos;
                    }
                }
            }
            KeyCode::Char('K') => {
                if let Some(id) = self.pl_selected_id() {
                    if self.pl_cursor > 0 {
                        let prev_pos = self.pl_cursor - 1;
                        if let Some(neighbor) = self.pl_projects.get(prev_pos) {
                            repo::swap_project_order(&self.conn, &id, &neighbor.id)?;
                            self.reload_project_list()?;
                            self.pl_cursor = prev_pos;
                        }
                    }
                }
            }

            KeyCode::Enter => {
                if let Some(id) = self.pl_selected_id() {
                    self.open_project_detail(&id)?;
                }
            }

            KeyCode::Char('n') => {
                self.pl_creating = Some(InputState::new());
            }

            KeyCode::Char('e') => {
                if let Some(p) = self.pl_projects.get(self.pl_cursor) {
                    self.pl_editing_title = Some(InputState::from_str(&p.title));
                }
            }

            KeyCode::Char('/') => {
                self.pl_searching = true;
            }

            KeyCode::Char('T') => {
                let all_tags = repo::list_all_tags(&self.conn)?;
                let applied_ids = self.tag_filter.as_ref()
                    .and_then(|name| all_tags.iter().find(|t| t.name == *name))
                    .map(|t| vec![t.id.clone()])
                    .unwrap_or_default();
                self.overlay = Some(Overlay::TagPicker {
                    all_tags,
                    applied_ids,
                    search: String::new(),
                    cursor: 0,
                    project_id: String::new(),
                    filter_mode: true,
                });
            }

            KeyCode::Char('a') => {
                if let Some(id) = self.pl_selected_id() {
                    repo::toggle_archive_project(&self.conn, &id)?;
                    self.reload_project_list()?;
                    self.pl_cursor = self.pl_cursor.min(self.pl_projects.len().saturating_sub(1));
                }
            }

            KeyCode::Char('A') => {
                self.pl_show_archived = !self.pl_show_archived;
                self.reload_project_list()?;
            }

            KeyCode::Char('D') => {
                if let Some(id) = self.pl_selected_id() {
                    let title = self.pl_projects[self.pl_cursor].title.clone();
                    self.overlay = Some(Overlay::ConfirmDelete {
                        message: format!("Delete '{}'? All todos and updates will be lost.", title),
                        target: DeleteTarget::Project(id),
                    });
                }
            }

            KeyCode::Char('t') => {
                self.go_to_todo_list()?;
            }

            _ => {}
        }
        Ok(())
    }

    fn pl_selected_id(&self) -> Option<String> {
        self.pl_projects.get(self.pl_cursor).map(|p| p.id.clone())
    }

    // ─── Project Detail ───────────────────────────────────────────────────

    fn handle_key_project_detail(&mut self, key: KeyEvent) -> Result<()> {
        // ── Edit todo mode ──
        if let Some((todo_id, mut state)) = self.pd_editing_todo.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        repo::update_todo_title(&self.conn, &todo_id, &title)?;
                        self.reload_project_detail()?;
                    }
                }
                InputKeyResult::Continue => { self.pd_editing_todo = Some((todo_id, state)); }
            }
            return Ok(());
        }

        // ── Add todo mode ──
        if let Some(mut state) = self.pd_adding_todo.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        if let Some(pid) = self.pd_project.as_ref().map(|p| p.id.clone()) {
                            repo::create_todo(&self.conn, Some(&pid), &title)?;
                            self.reload_project_detail()?;
                            self.pd_todo_cursor = self.pd_todos.len().saturating_sub(1);
                        } else {
                            crate::log::warn("pd_adding_todo: pd_project is None — todo discarded");
                        }
                    }
                }
                InputKeyResult::Continue => { self.pd_adding_todo = Some(state); }
            }
            return Ok(());
        }

        // ── Edit title mode ──
        if let Some(mut state) = self.pd_editing_title.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        if let Some(pid) = self.pd_project.as_ref().map(|p| p.id.clone()) {
                            repo::update_project_title(&self.conn, &pid, &title)?;
                            self.reload_project_detail()?;
                        } else {
                            crate::log::warn("pd_editing_title: pd_project is None — title edit discarded");
                        }
                    }
                }
                InputKeyResult::Continue => { self.pd_editing_title = Some(state); }
            }
            return Ok(());
        }

        // ── Search mode ──
        if self.pd_searching {
            match key.code {
                KeyCode::Esc => {
                    self.pd_searching = false;
                    self.pd_search.clear();
                }
                KeyCode::Enter => {
                    self.pd_searching = false;
                }
                KeyCode::Char(c) => {
                    self.pd_search.push(c);
                }
                KeyCode::Backspace => {
                    self.pd_search.pop();
                }
                _ => {}
            }
            return Ok(());
        }

        // ── Update viewer mode ──
        if self.pd_open_update.is_some() {
            return self.handle_key_pd_update_viewer(key);
        }

        // ── Universal keys (work in every tab) ──
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::ProjectList;
                self.pd_project = None;
                self.pd_search.clear();
                self.pd_searching = false;
                self.reload_project_list()?;
                return Ok(());
            }
            KeyCode::Char('E') => {
                if let Some(ref p) = self.pd_project {
                    self.pd_editing_title = Some(InputState::from_str(&p.title));
                }
                return Ok(());
            }
            KeyCode::Char('T') => {
                if let Some(ref p) = self.pd_project {
                    let pid         = p.id.clone();
                    let all_tags    = repo::list_all_tags(&self.conn)?;
                    let applied_ids = p.tags.iter().map(|t| t.id.clone()).collect();
                    self.overlay = Some(Overlay::TagPicker {
                        all_tags, applied_ids,
                        search: String::new(), cursor: 0,
                        project_id: pid, filter_mode: false,
                    });
                }
                return Ok(());
            }
            KeyCode::Char('/') => {
                self.pd_searching = true;
                return Ok(());
            }
            // Tab switching
            KeyCode::Char('d') => { self.pd_tab = PdTab::Description; self.pd_desc_scroll = 0; return Ok(()); }
            KeyCode::Char('u') => { self.pd_tab = PdTab::Updates;       return Ok(()); }
            KeyCode::Char('t') => { self.pd_tab = PdTab::Todos;         return Ok(()); }
            KeyCode::Char('f') => { self.pd_tab = PdTab::Files;         return Ok(()); }
            _ => {}
        }

        // ── Dispatch to tab-specific handler ──
        match self.pd_tab {
            PdTab::Description => self.handle_key_pd_description(key),
            PdTab::Updates     => self.handle_key_pd_updates(key),
            PdTab::Todos       => self.handle_key_pd_todos(key),
            PdTab::Files       => Ok(()),
        }
    }

    // ─── Project Detail – Description tab ──────────────────────────────────

    fn handle_key_pd_description(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('e') | KeyCode::Char('n') => {
                if let Some(ref p) = self.pd_project {
                    self.pending_editor = Some(PendingEditor {
                        content: p.description.clone(),
                        target: EditorTarget::ProjectDescription,
                        slot: format!("desc_{}", p.id),
                    });
                }
            }
            KeyCode::Char('S') => {
                if let Some(ref p) = self.pd_project {
                    let desc = p.description.clone();
                    self.open_spell_check(desc, SpellTarget::ProjectDescription)?;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.pd_desc_max_scroll.get();
                if self.pd_desc_scroll < max {
                    self.pd_desc_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.pd_desc_scroll = self.pd_desc_scroll.saturating_sub(1);
            }
            _ => {}
        }
        Ok(())
    }

    // ─── Project Detail – Update viewer ────────────────────────────────────

    fn handle_key_pd_update_viewer(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.pd_open_update = None;
                self.pd_update_view_scroll = 0;
                // Keep pd_search so highlights re-apply if the user reopens a viewer;
                // the user can clear it with / → ESC while inside the viewer.
            }
            KeyCode::Char('/') => {
                self.pd_searching = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.pd_update_view_max_scroll.get();
                if self.pd_update_view_scroll < max {
                    self.pd_update_view_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.pd_update_view_scroll = self.pd_update_view_scroll.saturating_sub(1);
            }
            KeyCode::Char('e') => {
                let open_id = self.pd_open_update.clone();
                if let Some(ref id) = open_id {
                    if let Some(upd) = self.pd_updates.iter().find(|u| &u.id == id) {
                        let label = if upd.label.is_empty() {
                            upd.created_at.chars().take(10).collect::<String>()
                        } else {
                            upd.label.clone()
                        };
                        let content = format!("Date: {}\n---\n{}", label, upd.body);
                        self.pending_editor = Some(PendingEditor {
                            content,
                            target: EditorTarget::EditUpdate(upd.id.clone()),
                            slot: format!("update_{}", upd.id),
                        });
                    }
                }
            }
            KeyCode::Char('S') => {
                let open_id = self.pd_open_update.clone();
                if let Some(ref id) = open_id {
                    if let Some(upd) = self.pd_updates.iter().find(|u| &u.id == id) {
                        let body  = upd.body.clone();
                        let label = upd.label.clone();
                        let id    = upd.id.clone();
                        self.open_spell_check(body, SpellTarget::UpdateBody { id, label })?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ─── Project Detail – Updates tab ─────────────────────────────────────

    fn handle_key_pd_updates(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            // Enter → open the selected update in full-screen viewer.
            KeyCode::Enter => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    self.pd_open_update = Some(upd.id.clone());
                    self.pd_update_view_scroll = 0;
                }
            }

            // n → open $EDITOR to add a new update.
            KeyCode::Char('n') => {
                let offset_minutes = crate::tz::get_tz_offset_minutes();
                let today = (DateTime::<Utc>::from(SystemTime::now()) + Duration::minutes(offset_minutes))
                    .format("%Y-%m-%d")
                    .to_string();
                let project_id = self.pd_project.as_ref()
                    .map(|p| p.id.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                self.pending_editor = Some(PendingEditor {
                    content: format!("Date: {}\n---\n", today),
                    target: EditorTarget::AddUpdate,
                    slot: format!("new_update_{}", project_id),
                });
            }

            // e → open the selected update in $EDITOR for editing.
            KeyCode::Char('e') => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    // Fall back to created_at date if label is empty (legacy records).
                    let label = if upd.label.is_empty() {
                        upd.created_at.chars().take(10).collect::<String>()
                    } else {
                        upd.label.clone()
                    };
                    let content = format!("Date: {}\n---\n{}", label, upd.body);
                    self.pending_editor = Some(PendingEditor {
                        content,
                        target: EditorTarget::EditUpdate(upd.id.clone()),
                        slot: format!("update_{}", upd.id),
                    });
                }
            }

            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.pd_updates.len().saturating_sub(1);
                if self.pd_update_cursor < max {
                    self.pd_update_cursor += 1;
                }
            }

            KeyCode::Char('k') | KeyCode::Up => {
                if self.pd_update_cursor > 0 {
                    self.pd_update_cursor -= 1;
                }
            }

            KeyCode::Char('J') => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    let id = upd.id.clone();
                    let next_pos = self.pd_update_cursor + 1;
                    if let Some(neighbor) = self.pd_updates.get(next_pos) {
                        repo::swap_update_order(&self.conn, &id, &neighbor.id)?;
                        self.reload_project_detail()?;
                        self.pd_update_cursor = next_pos;
                    }
                }
            }

            KeyCode::Char('K') => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    let id = upd.id.clone();
                    if self.pd_update_cursor > 0 {
                        let prev_pos = self.pd_update_cursor - 1;
                        if let Some(neighbor) = self.pd_updates.get(prev_pos) {
                            repo::swap_update_order(&self.conn, &id, &neighbor.id)?;
                            self.reload_project_detail()?;
                            self.pd_update_cursor = prev_pos;
                        }
                    }
                }
            }

            // D → confirm-delete the selected update.
            KeyCode::Char('D') => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    let preview = upd.body.lines().next().unwrap_or("")
                        .chars().take(40).collect::<String>();
                    self.overlay = Some(Overlay::ConfirmDelete {
                        message: format!("Delete update '{}'?", preview),
                        target: DeleteTarget::Update(upd.id.clone()),
                    });
                }
            }

            KeyCode::Char('S') => {
                if let Some(upd) = self.pd_updates.get(self.pd_update_cursor) {
                    let body  = upd.body.clone();
                    let label = upd.label.clone();
                    let id    = upd.id.clone();
                    self.open_spell_check(body, SpellTarget::UpdateBody { id, label })?;
                }
            }

            _ => {}
        }
        Ok(())
    }

    // ─── Project Detail – Todos tab ────────────────────────────────────────

    fn handle_key_pd_todos(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            // n → start inline add-todo entry.
            KeyCode::Char('n') => {
                self.pd_adding_todo = Some(InputState::new());
            }

            // e → inline-edit the selected todo title.
            KeyCode::Char('e') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    let title = self.pd_todos.iter()
                        .find(|t| t.id == id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    self.pd_editing_todo = Some((id, InputState::from_str(&title)));
                }
            }


            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.pd_todos.len().saturating_sub(1);
                if self.pd_todo_cursor < max { self.pd_todo_cursor += 1; }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.pd_todo_cursor > 0 { self.pd_todo_cursor -= 1; }
            }

            KeyCode::Char('J') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    let next_pos = self.pd_todo_cursor + 1;
                    if let Some(neighbor) = self.pd_todos.get(next_pos) {
                        repo::swap_todo_order(&self.conn, &id, &neighbor.id)?;
                        self.reload_project_detail()?;
                        self.pd_todo_cursor = next_pos;
                    }
                }
            }
            KeyCode::Char('K') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    if self.pd_todo_cursor > 0 {
                        let prev_pos = self.pd_todo_cursor - 1;
                        if let Some(neighbor) = self.pd_todos.get(prev_pos) {
                            repo::swap_todo_order(&self.conn, &id, &neighbor.id)?;
                            self.reload_project_detail()?;
                            self.pd_todo_cursor = prev_pos;
                        }
                    }
                }
            }

            KeyCode::Char('s') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    repo::cycle_todo_status(&self.conn, &id)?;
                    self.reload_project_detail()?;
                }
            }

            KeyCode::Char('r') => {
                if let Some((id, reminder)) = self.pd_selected_todo_id()
                    .map(|id| {
                        let r = self.pd_todos.iter()
                            .find(|t| t.id == id)
                            .and_then(|t| t.reminder.clone())
                            .unwrap_or_default();
                        (id, r)
                    })
                {
                    self.overlay = Some(Overlay::ReminderInput { input: InputState::from_str(&reminder), todo_id: id, error: None });
                }
            }

            KeyCode::Char('m') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    let current_pid = self.pd_todos.iter()
                        .find(|t| t.id == id)
                        .and_then(|t| t.project_id.clone());
                    let projects = repo::list_projects(&self.conn, false, None, None)?;
                    self.overlay = Some(Overlay::ProjectPicker {
                        projects, search: String::new(), cursor: 0,
                        todo_id: id, current_project_id: current_pid,
                    });
                }
            }

            KeyCode::Char('D') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    let title = self.pd_todos.iter()
                        .find(|t| t.id == id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    self.overlay = Some(Overlay::ConfirmDelete {
                        message: format!("Delete todo '{}'?", title),
                        target: DeleteTarget::Todo(id),
                    });
                }
            }

            KeyCode::Char('a') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    repo::toggle_archive_todo(&self.conn, &id)?;
                    self.reload_project_detail()?;
                    let max = self.pd_todos.len().saturating_sub(1);
                    if self.pd_todo_cursor > max { self.pd_todo_cursor = max; }
                }
            }

            KeyCode::Char('A') => {
                self.pd_show_archived_todos = !self.pd_show_archived_todos;
                self.reload_project_detail()?;
            }

            KeyCode::Char('C') => {
                self.hide_done = !self.hide_done;
                self.reload_project_detail()?;
                // Keep cursor in bounds after filtering.
                let max = self.pd_todos.len().saturating_sub(1);
                if self.pd_todo_cursor > max { self.pd_todo_cursor = max; }
            }

            KeyCode::Char('S') => {
                if let Some(id) = self.pd_selected_todo_id() {
                    let title = self.pd_todos.iter()
                        .find(|t| t.id == id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    self.open_spell_check(title, SpellTarget::TodoTitle(id))?;
                }
            }

            _ => {}
        }
        Ok(())
    }
    fn pd_selected_todo_id(&self) -> Option<String> {
        self.pd_todos.get(self.pd_todo_cursor).map(|t| t.id.clone())
    }

    // ─── Global Todo List ─────────────────────────────────────────────────

    fn handle_key_todo_list(&mut self, key: KeyEvent) -> Result<()> {
        // ── Inline edit mode ──
        if let Some((todo_id, mut state)) = self.tl_editing.take() {
            match handle_input_key(&mut state, key) {
                InputKeyResult::Escape => { /* discard */ }
                InputKeyResult::Enter => {
                    let title = state.trimmed().to_string();
                    if !title.is_empty() {
                        repo::update_todo_title(&self.conn, &todo_id, &title)?;
                        self.reload_todo_list()?;
                    }
                }
                InputKeyResult::Continue => { self.tl_editing = Some((todo_id, state)); }
            }
            return Ok(());
        }

        // ── Search mode ──
        if self.tl_searching {
            match key.code {
                KeyCode::Esc => {
                    self.tl_searching = false;
                    self.tl_search.clear();
                    self.reload_todo_list()?;
                }
                KeyCode::Enter => { self.tl_searching = false; }
                KeyCode::Char(c) => {
                    self.tl_search.push(c);
                    self.reload_todo_list()?;
                    self.tl_cursor = 0;
                }
                KeyCode::Backspace => {
                    self.tl_search.pop();
                    self.reload_todo_list()?;
                    self.tl_cursor = 0;
                }
                _ => {}
            }
            return Ok(());
        }

        // ── Normal mode ──
        let total = self.flat_todo_count();

        match key.code {
            KeyCode::Char('Q') => { self.should_quit = true; }

            KeyCode::Char('p') => { self.go_to_project_list()?; }

            KeyCode::Char('j') | KeyCode::Down => {
                if self.tl_cursor + 1 < total { self.tl_cursor += 1; }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.tl_cursor > 0 { self.tl_cursor -= 1; }
            }

            KeyCode::Char('J') => {
                if let Some(cur) = self.tl_current_todo() {
                    let id = cur.id.clone();
                    let cur_proj = cur.project_id.clone();
                    let next_pos = self.tl_cursor + 1;
                    // Only swap within the same project; J/K never crosses group boundaries.
                    if let Some(neighbor) = self.tl_todo_at(next_pos) {
                        if neighbor.project_id == cur_proj {
                            let neighbor_id = neighbor.id.clone();
                            repo::swap_todo_order(&self.conn, &id, &neighbor_id)?;
                            self.reload_todo_list()?;
                            self.tl_cursor = next_pos;
                        }
                    }
                }
            }
            KeyCode::Char('K') => {
                if let Some(cur) = self.tl_current_todo() {
                    let id = cur.id.clone();
                    let cur_proj = cur.project_id.clone();
                    if self.tl_cursor > 0 {
                        let prev_pos = self.tl_cursor - 1;
                        if let Some(neighbor) = self.tl_todo_at(prev_pos) {
                            if neighbor.project_id == cur_proj {
                                let neighbor_id = neighbor.id.clone();
                                repo::swap_todo_order(&self.conn, &id, &neighbor_id)?;
                                self.reload_todo_list()?;
                                self.tl_cursor = prev_pos;
                            }
                        }
                    }
                }
            }

            KeyCode::Char('s') => {
                if let Some(id) = self.tl_current_todo_id() {
                    repo::cycle_todo_status(&self.conn, &id)?;
                    self.reload_todo_list()?;
                }
            }

            KeyCode::Char('e') => {
                if let Some((id, title)) = self.tl_current_todo_id_and_title() {
                    self.tl_editing = Some((id, InputState::from_str(&title)));
                }
            }


            KeyCode::Char('r') => {
                if let Some(todo) = self.tl_current_todo() {
                    let id = todo.id.clone();
                    let reminder = todo.reminder.clone().unwrap_or_default();
                    self.overlay = Some(Overlay::ReminderInput { input: InputState::from_str(&reminder), todo_id: id, error: None });
                }
            }

            KeyCode::Char('n') => {
                self.overlay = Some(Overlay::QuickCapture { input: InputState::new() });
            }

            KeyCode::Char('/') => {
                self.tl_searching = true;
            }

            KeyCode::Char('a') => {
                if let Some(id) = self.tl_current_todo_id() {
                    repo::toggle_archive_todo(&self.conn, &id)?;
                    self.reload_todo_list()?;
                    // Clamp cursor after a potential removal.
                    let total = self.flat_todo_count();
                    if total > 0 && self.tl_cursor >= total {
                        self.tl_cursor = total - 1;
                    }
                }
            }

            KeyCode::Char('A') => {
                self.tl_show_archived = !self.tl_show_archived;
                self.reload_todo_list()?;
            }

            KeyCode::Char('C') => {
                self.hide_done = !self.hide_done;
                self.reload_todo_list()?;
            }

            KeyCode::Char('T') => {
                let all_tags = repo::list_all_tags(&self.conn)?;
                let applied_ids = self.tag_filter.as_ref()
                    .and_then(|name| all_tags.iter().find(|t| t.name == *name))
                    .map(|t| vec![t.id.clone()])
                    .unwrap_or_default();
                self.overlay = Some(Overlay::TagPicker {
                    all_tags, applied_ids,
                    search: String::new(), cursor: 0,
                    project_id: String::new(), filter_mode: true,
                });
            }

            KeyCode::Char('m') => {
                if let Some(todo) = self.tl_current_todo() {
                    let id  = todo.id.clone();
                    let pid = todo.project_id.clone();
                    let projects = repo::list_projects(&self.conn, false, None, None)?;
                    self.overlay = Some(Overlay::ProjectPicker {
                        projects, search: String::new(), cursor: 0,
                        todo_id: id, current_project_id: pid,
                    });
                }
            }

            KeyCode::Enter => {
                // Jump to the owning project's detail view and place the
                // cursor on the same todo inside Project Detail.
                if let Some((tid, pid)) = self.tl_current_todo()
                    .and_then(|t| t.project_id.as_ref().map(|p| (t.id.clone(), p.clone())))
                {
                    self.open_project_detail(&pid)?;
                    if let Some(pos) = self.pd_todos.iter().position(|t| t.id == tid) {
                        self.pd_todo_cursor = pos;
                    }
                }
            }

            KeyCode::Char('D') => {
                if let Some(todo) = self.tl_current_todo() {
                    let id    = todo.id.clone();
                    let title = todo.title.clone();
                    self.overlay = Some(Overlay::ConfirmDelete {
                        message: format!("Delete todo '{}'?", title),
                        target: DeleteTarget::Todo(id),
                    });
                }
            }

            KeyCode::Char('S') => {
                if let Some((id, title)) = self.tl_current_todo_id_and_title() {
                    self.open_spell_check(title, SpellTarget::TodoTitle(id))?;
                }
            }

            _ => {}
        }
        Ok(())
    }

    // ─── Todo list helpers ────────────────────────────────────────────────

    pub fn flat_todo_count(&self) -> usize {
        self.tl_groups.iter().map(|g| g.todos.len()).sum()
    }

    pub fn tl_current_todo(&self) -> Option<&Todo> {
        let mut count = 0usize;
        for group in &self.tl_groups {
            for todo in &group.todos {
                if count == self.tl_cursor { return Some(todo); }
                count += 1;
            }
        }
        None
    }

    fn tl_todo_at(&self, flat_idx: usize) -> Option<&Todo> {
        let mut count = 0usize;
        for group in &self.tl_groups {
            for todo in &group.todos {
                if count == flat_idx { return Some(todo); }
                count += 1;
            }
        }
        None
    }

    fn tl_current_todo_id(&self) -> Option<String> {
        self.tl_current_todo().map(|t| t.id.clone())
    }

    fn tl_current_todo_id_and_title(&self) -> Option<(String, String)> {
        self.tl_current_todo().map(|t| (t.id.clone(), t.title.clone()))
    }

    // ─── Navigation helpers ───────────────────────────────────────────────

    pub fn open_project_detail(&mut self, project_id: &str) -> Result<()> {
        self.pd_show_archived_todos = false;
        let project = repo::get_project(&self.conn, project_id)?;
        let todos   = repo::list_todos(&self.conn, Some(project_id), self.hide_done, self.pd_show_archived_todos)?;
        let updates = repo::list_updates(&self.conn, project_id)?;
        self.pd_project      = Some(project);
        self.pd_todos        = todos;
        self.pd_updates      = updates;
        self.pd_todo_cursor  = 0;
        self.pd_tab          = PdTab::Todos;
        self.pd_update_cursor = 0;
        self.pd_desc_scroll  = 0;
        self.pd_open_update  = None;
        self.pd_update_view_scroll = 0;
        self.pd_adding_todo  = None;
        self.pd_editing_title = None;
        self.pd_editing_todo = None;
        self.pd_search.clear();
        self.pd_searching   = false;
        self.pd_archived_todo_count = repo::count_archived_todos(&self.conn, Some(project_id))?;
        self.pd_done_count = repo::count_done_todos(&self.conn, Some(project_id))?;
        self.screen = Screen::ProjectDetail;
        Ok(())
    }

    fn go_to_todo_list(&mut self) -> Result<()> {
        self.reload_todo_list()?;
        self.screen = Screen::TodoList;
        Ok(())
    }

    fn go_to_project_list(&mut self) -> Result<()> {
        self.reload_project_list()?;
        self.screen = Screen::ProjectList;
        Ok(())
    }

    // ─── Data reload helpers ──────────────────────────────────────────────

    pub fn reload_project_list(&mut self) -> Result<()> {
        let search = if self.pl_search.is_empty() { None } else { Some(self.pl_search.as_str()) };
        self.pl_projects = repo::list_projects(
            &self.conn, self.pl_show_archived, self.tag_filter.as_deref(), search,
        )?;
        self.pl_archived_count = repo::count_archived(&self.conn)?;
        let len = self.pl_projects.len();
        if len > 0 && self.pl_cursor >= len {
            self.pl_cursor = len - 1;
        }
        Ok(())
    }

    pub fn reload_project_detail(&mut self) -> Result<()> {
        let pid = match self.pd_project.as_ref().map(|p| p.id.clone()) {
            Some(id) => id,
            None => return Ok(()),
        };
        let project = repo::get_project(&self.conn, &pid)?;
        let todos   = repo::list_todos(&self.conn, Some(&pid), self.hide_done, self.pd_show_archived_todos)?;
        let updates = repo::list_updates(&self.conn, &pid)?;
        self.pd_archived_todo_count = repo::count_archived_todos(&self.conn, Some(&pid))?;
        self.pd_done_count = repo::count_done_todos(&self.conn, Some(&pid))?;
        self.pd_project = Some(project);
        self.pd_todos   = todos;
        self.pd_updates = updates;
        // Clamp todo cursor.
        let tlen = self.pd_todos.len();
        if tlen > 0 && self.pd_todo_cursor >= tlen {
            self.pd_todo_cursor = tlen - 1;
        }
        // Clamp update cursor.
        let ulen = self.pd_updates.len();
        if ulen == 0 {
            self.pd_update_cursor = 0;
        } else if self.pd_update_cursor >= ulen {
            self.pd_update_cursor = ulen - 1;
        }
        // If the currently open update was deleted, close the viewer.
        if let Some(ref oid) = self.pd_open_update.clone() {
            if !self.pd_updates.iter().any(|u| &u.id == oid) {
                self.pd_open_update = None;
                self.pd_update_view_scroll = 0;
            }
        }
        Ok(())
    }

    pub fn reload_todo_list(&mut self) -> Result<()> {
        let search = if self.tl_search.is_empty() { None } else { Some(self.tl_search.as_str()) };
        self.tl_groups = repo::list_todos_global(
            &self.conn, self.hide_done, search, self.tag_filter.as_deref(), self.tl_show_archived,
        )?;
        self.tl_archived_count = repo::count_archived_todos_global(&self.conn)?;
        self.tl_done_count = repo::count_done_todos_global(&self.conn)?;
        let total = self.flat_todo_count();
        if total > 0 && self.tl_cursor >= total {
            self.tl_cursor = total - 1;
        }
        Ok(())
    }

    // ─── Spell Check overlay ──────────────────────────────────────────────

    /// Open a spell-check session for `text`, writing corrections back to
    /// `target` when the overlay closes.  Does nothing when the text is empty.
    /// Shows a [`Overlay::Warn`] modal when no dictionary is loaded.
    fn open_spell_check(&mut self, text: String, target: SpellTarget) -> Result<()> {
        if text.trim().is_empty() { return Ok(()); }
        let Some(checker) = self.spell.as_ref() else {
            self.overlay = Some(Overlay::Warn {
                title: " Spell Check Unavailable ".into(),
                body: "No spelling dictionary found.\
\nTo enable spell check, install a dictionary file:\
\n    apt install hunspell-en-us\
\nOr set PTM_DICT_DIR to a directory containing\
\nen_US.aff and en_US.dic.".into(),
            });
            return Ok(());
        };
        let segments = crate::spell::tokenize(&text);
        let bad_words: Vec<usize> = segments
            .iter()
            .enumerate()
            .filter(|(_, (w, is_word))| *is_word && !checker.check(w))
            .map(|(i, _)| i)
            .collect();
        let done = bad_words.is_empty();
        let suggestions = if done {
            Vec::new()
        } else {
            checker.suggest(&segments[bad_words[0]].0)
        };
        self.overlay = Some(Overlay::SpellCheck {
            segments, bad_words, current: 0, suggestions, target, done,
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_spell_check(
        &mut self,
        key: KeyEvent,
        mut segments: Vec<(String, bool)>,
        mut bad_words: Vec<usize>,
        mut current: usize,
        mut suggestions: Vec<String>,
        target: SpellTarget,
        done: bool,
    ) -> Result<Option<Overlay>> {
        if done {
            // Session complete — any key dismisses.
            return Ok(None);
        }

        match key.code {
            // ── Close and save ─────────────────────────────────────────────
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.save_spell_result(&segments, &target)?;
                return Ok(None);
            }

            // ── Replace once with suggestion N ─────────────────────────────
            KeyCode::Char(c) if matches!(c, '1'..='9') => {
                let n = (c as u8 - b'1') as usize;
                if n < suggestions.len() {
                    segments[bad_words[current]].0 = suggestions[n].clone();
                    bad_words.remove(current);
                    // `current` now indexes the next word (or is out-of-bounds).
                } else {
                    return Ok(Some(Overlay::SpellCheck {
                        segments, bad_words, current, suggestions, target, done: false,
                    }));
                }
            }

            // ── Skip this occurrence ───────────────────────────────────────
            KeyCode::Char('s') => {
                current += 1;
            }

            // ── Accept all occurrences for this session ────────────────────
            KeyCode::Char('a') => {
                let word = segments[bad_words[current]].0.clone();
                let to_remove: std::collections::HashSet<usize> = bad_words[current..]
                    .iter()
                    .filter(|&&idx| segments[idx].0 == word)
                    .copied()
                    .collect();
                bad_words.retain(|idx| !to_remove.contains(idx));
                // `current` now indexes the next unhandled word.
            }

            // ── Add to personal dictionary ─────────────────────────────────
            KeyCode::Char('i') => {
                let word = segments[bad_words[current]].0.clone();
                if let Some(ref mut checker) = self.spell {
                    checker.add_word(&word);
                }
                let to_remove: std::collections::HashSet<usize> = bad_words[current..]
                    .iter()
                    .filter(|&&idx| segments[idx].0 == word)
                    .copied()
                    .collect();
                bad_words.retain(|idx| !to_remove.contains(idx));
            }

            // ── Ignore everything else ─────────────────────────────────────
            _ => {
                return Ok(Some(Overlay::SpellCheck {
                    segments, bad_words, current, suggestions, target, done: false,
                }));
            }
        }

        // All words handled?
        if current >= bad_words.len() {
            self.save_spell_result(&segments, &target)?;
            return Ok(Some(Overlay::SpellCheck {
                segments, bad_words, current, suggestions: Vec::new(), target, done: true,
            }));
        }

        // Fetch suggestions for the new current word.
        suggestions = self.spell
            .as_ref()
            .map(|c| c.suggest(&segments[bad_words[current]].0))
            .unwrap_or_default();

        Ok(Some(Overlay::SpellCheck {
            segments, bad_words, current, suggestions, target, done: false,
        }))
    }

    /// Write the corrected text back to the database and refresh in-memory
    /// state.  Called whenever the spell-check overlay closes with changes.
    fn save_spell_result(
        &mut self,
        segments: &[(String, bool)],
        target: &SpellTarget,
    ) -> Result<()> {
        let text: String = segments.iter().map(|(t, _)| t.as_str()).collect();
        match target {
            SpellTarget::TodoTitle(id) => {
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    repo::update_todo_title(&self.conn, id, &trimmed)?;
                }
            }
            SpellTarget::UpdateBody { id, label } => {
                repo::update_update(&self.conn, id, label, text.trim())?;
            }
            SpellTarget::ProjectDescription => {
                if let Some(pid) = self.pd_project.as_ref().map(|p| p.id.clone()) {
                    repo::update_project_description(&self.conn, &pid, text.trim())?;
                } else {
                    crate::log::warn(
                        "save_spell_result: ProjectDescription with no active project — discarded",
                    );
                }
            }
        }
        self.reload_project_detail()?;
        self.reload_todo_list()?;
        Ok(())
    }

    // ─── Key Help overlay ────────────────────────────────────────────────

    fn handle_key_help(&mut self, key: KeyEvent, mut scroll: u16) -> Result<Option<Overlay>> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => Ok(None),
            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.key_help_max_scroll.get();
                if scroll < max { scroll += 1; }
                Ok(Some(Overlay::KeyHelp { scroll }))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                scroll = scroll.saturating_sub(1);
                Ok(Some(Overlay::KeyHelp { scroll }))
            }
            _ => Ok(Some(Overlay::KeyHelp { scroll })),
        }
    }

    // ─── Inline-edit guard ─────────────────────────────────────────────────

    fn is_inline_editing(&self) -> bool {
        self.pd_editing_todo.is_some()
            || self.pd_adding_todo.is_some()
            || self.pd_editing_title.is_some()
            || self.pd_searching
            || self.tl_editing.is_some()
            || self.pl_creating.is_some()
            || self.pl_editing_title.is_some()
            || self.pl_searching
            || self.tl_searching
    }

    // ─── Editor result ────────────────────────────────────────────────────

    pub fn handle_editor_result(&mut self, target: EditorTarget, content: String) -> Result<()> {
        match target {
            EditorTarget::ProjectDescription => {
                let trimmed = content.trim().to_string();
                if let Some(pid) = self.pd_project.as_ref().map(|p| p.id.clone()) {
                    repo::update_project_description(&self.conn, &pid, &trimmed)?;
                } else {
                    crate::log::warn("handle_editor_result: ProjectDescription called with no active project — edit discarded");
                }
                self.reload_project_detail()?
            }
            EditorTarget::AddUpdate => {
                if !content.trim().is_empty() {
                    if let Some(pid) = self.pd_project.as_ref().map(|p| p.id.clone()) {
                        let (label, body) = parse_update_template(&content);
                        if !body.trim().is_empty() {
                            repo::create_update(&self.conn, &pid, &label, &body)?;
                        }
                    } else {
                        crate::log::warn("handle_editor_result: AddUpdate called with no active project — update discarded");
                    }
                }
                self.reload_project_detail()?;
            }
            EditorTarget::EditUpdate(id) => {
                if !content.trim().is_empty() {
                    let (label, body) = parse_update_template(&content);
                    repo::update_update(&self.conn, &id, &label, &body)?;
                }
                self.reload_project_detail()?;
            }
        }
        Ok(())
    }
}

// ─── Free helpers ──────────────────────────────────────────────────────────────────────

/// Parse the $EDITOR template for update entries.
///
/// Expected format:
/// ```text
/// Date: <label>
/// ---
/// <body>
/// ```
/// The `Date: ` prefix is stripped from the first line to obtain the label.
/// The `---` separator is consumed; everything after it is the body.
/// If the separator is missing the remaining lines are treated as the body.
fn parse_update_template(content: &str) -> (String, String) {
    let mut lines = content.lines();
    // First line: "Date: <value>" — strip the prefix to get the label.
    let label = lines
        .next()
        .unwrap_or("")
        .trim()
        .strip_prefix("Date: ")
        .unwrap_or("")
        .trim()
        .to_string();
    // Next line should be the "---" separator; skip it.
    // If it is not the separator, include it in the body.
    let next = lines.next().unwrap_or("");
    let body = if next.trim() == "---" {
        lines.collect::<Vec<_>>().join("\n")
    } else {
        // No separator — treat this line and the rest as body.
        let mut parts = vec![next];
        parts.extend(lines);
        parts.join("\n")
    };
    (label, body)
}
