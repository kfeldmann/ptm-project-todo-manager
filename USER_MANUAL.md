# ptm — Project & Task Manager: User Manual

**ptm** is a terminal-based (TUI) project and task manager. It stores all data locally in a SQLite database and is controlled entirely by the keyboard.

---

## Table of Contents

1. [Installation & Launch](#1-installation--launch)
2. [Core Concepts](#2-core-concepts)
3. [Data Storage](#3-data-storage)
4. [Navigation Overview](#4-navigation-overview)
5. [Project List](#5-project-list)
6. [Project Detail](#6-project-detail)
7. [Global Todo List](#7-global-todo-list)
8. [Overlays](#8-overlays)
   - [Quick Capture](#81-quick-capture)
   - [Tag Picker](#82-tag-picker)
   - [Move-to-Project Picker](#83-move-to-project-picker)
   - [Reminder Input](#84-reminder-input)
   - [Spell Check](#85-spell-check)
   - [Key Help](#86-key-help)
   - [Confirm Delete](#87-confirm-delete)
9. [Tags](#9-tags)
10. [Todo Statuses](#10-todo-statuses)
11. [Reminders](#11-reminders)
12. [External Editor Integration](#12-external-editor-integration)
13. [Quick Reference: All Keybindings](#13-quick-reference-all-keybindings)

---

## 1. Installation & Launch

### Build from source

```bash
cargo build --release
```

The binary is placed at `target/release/ptm`.

### Run

```bash
ptm
```

Or run the debug build during development:

```bash
cargo run
```

No configuration is required on first launch. The database and data directory are created automatically.

---

## 2. Core Concepts

| Concept | Description |
|---|---|
| **Project** | A named container that holds todos and log updates. Projects have an optional description and can be tagged. |
| **Todo** | A single task. Every todo belongs to either one project or the **Inbox** (no project). |
| **Inbox** | A virtual project for todos not yet assigned to a project. Always shown at the top of the global todo view. |
| **Update** | A timestamped journal entry logged against a project (e.g. meeting notes, progress notes). |
| **Tag** | A short label that can be applied to projects. Tags are global — the same tag pool is shared across all projects. |
| **Reminder** | An ISO date (`YYYY-MM-DD`) attached to a todo. Surfaced as a color-coded badge on the project list. |

---

## 3. Data Storage

All data is stored in a single SQLite database:

```
~/.local/share/ptm/ptm.db
```

If the `XDG_DATA_HOME` environment variable is set, that path is used instead of `~/.local/share`.

The database is created on first launch. No manual setup is required.

---

## 4. Navigation Overview

ptm has three **screens**. You are always on exactly one screen. Overlays (pickers, modals) can appear on top of any screen.

```
ProjectList  ──Enter──▶  ProjectDetail
     │                        │
     t                       ESC
     │                        │
     ▼                        ▼
  TodoList  ────p────▶  ProjectList
```

| Key | Action |
|---|---|
| `t` (Project List) | Switch to **Global Todo List** |
| `p` (Todo List) | Switch to **Project List** |
| `Enter` (on a project) | Open **Project Detail** for the selected project |
| `ESC` (in Project Detail) | Return to **Project List** |
| `Enter` (on a todo in Todo List) | Jump to that todo's **Project Detail** |

### Global keys (any screen, outside overlays)

| Key | Action |
|---|---|
| `i` | Open **Quick Capture** (add todo to Inbox) |
| `?` | Open **Key Help** overlay |

---

## 5. Project List

This is the default screen when you start ptm.

```
┌──────────────────────────────────────────────────────────┐
│ Projects                                                 │
├──────────────────────────────────────────────────────────┤
│   Build analytics dashboard                              │
│   Rewrite auth service          [work]  2025-07-20       │  ← selected row
│   Migrate database                                       │
│   [2 archived — A to show]                               │
├──────────────────────────────────────────────────────────┤
│ Q:quit  ?:help                                           │
└──────────────────────────────────────────────────────────┘
```

### Cursor movement

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |

### Reordering

| Key | Action |
|---|---|
| `J` (Shift+j) | Move selected project **down** one position |
| `K` (Shift+k) | Move selected project **up** one position |

The cursor follows the project as it moves.

### Creating a project

Press `n`. A text input appears at the bottom of the list. Type the project title and press `Enter` to save, or `ESC` to cancel. The new project is appended at the end of the list.

### Opening a project

Press `Enter` to open the **Project Detail** screen for the selected project.

### Renaming a project title

Press `e`. The project title becomes an inline text input. Edit it and press `Enter` to save, or `ESC` to discard.

### Search

Press `/` to enter search mode. The list filters in real time as you type. Press `Enter` to lock the filter in place (cursor movement continues to work), or `ESC` to clear the search and show all projects.

While searching, the title bar shows `/your-query`.

### Tag filtering

Press `T` (Shift+t) to open the **Tag Picker** in filter mode. Select a tag and press `Enter` to show only projects carrying that tag. The active filter is shown in the title bar as `[tagname]`.

Selecting the already-active tag a second time clears the filter.

### Archiving

| Key | Action |
|---|---|
| `a` | Archive the selected project (hides it from the default view) |
| `A` (Shift+a) | Toggle display of archived projects |

Archived projects are dimmed when shown. Archiving does not delete data.

### Deleting a project

Press `D` (Shift+d). A confirmation dialog appears. Press `y` or `Enter` to confirm, `n` or `ESC` to cancel.

> **Warning:** Deleting a project permanently removes it along with all its todos and updates.

### Navigating to the Todo List

Press `t` to switch to the **Global Todo List**.

### Reminder badge

If any active todo in a project has a reminder date set, the earliest date is shown next to the project title, color-coded by urgency:

| Color | Meaning |
|---|---|
| Red | Overdue (past today's date) |
| Yellow | Due within 7 days |
| Default | Further away |

---

## 6. Project Detail

Press `Enter` on a project in the Project List to open its detail view.

The Project Detail screen is organized into four **tabs**, switched with single-key shortcuts:

| Key | Tab |
|---|---|
| `d` | Description |
| `u` | Updates |
| `t` | Todos |
| `f` | Files (placeholder) |

```
┌──────────────────────────────────────────────────────────┐
│ Rewrite auth service                             [work]  │
├──────────────────────────────────────────────────────────┤
│  Description   Updates   Todos   Files                   │
├──────────────────────────────────────────────────────────┤
│  ...tab content...                                       │
├──────────────────────────────────────────────────────────┤
│ ESC:back  d/u/t/f:tabs  ?:help                           │
└──────────────────────────────────────────────────────────┘
```

### Universal keys (work in every tab)

| Key | Action |
|---|---|
| `ESC` | Return to Project List |
| `E` (Shift+e) | Edit project title inline |
| `T` (Shift+t) | Open Tag Picker to edit tags on this project |
| `/` | Search / highlight text across all tabs |

### Editing the project title

Press `E` (Shift+e). The title becomes an inline text input pre-filled with the current title. Edit it and press `Enter` to save, or `ESC` to discard.

---

### Description tab (`d`)

Shows the project's long-form description, rendered with markdown.

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `e` or `n` | Open `$EDITOR` to edit the description |
| `S` (Shift+s) | Spell check the description |

---

### Updates tab (`u`)

Shows a list of timestamped journal entries for the project.

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` (Shift+j) | Move selected update **down** |
| `K` (Shift+k) | Move selected update **up** |
| `Enter` | Open selected update in the **update viewer** |
| `n` | Add a new update (opens `$EDITOR`) |
| `e` | Edit selected update (opens `$EDITOR`) |
| `S` (Shift+s) | Spell check the selected update |
| `D` (Shift+d) | Delete selected update (with confirmation) |

#### Update editor format

When adding or editing an update, the editor buffer contains:

```
Date: YYYY-MM-DD
---
Your update text here.
```

Edit the text after `---`. The `Date:` line sets the update label shown in the list.

#### Update viewer

Press `Enter` on a selected update to open it in a scrollable full-screen viewer within the Updates tab.

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `e` | Edit this update in `$EDITOR` |
| `S` (Shift+s) | Spell check this update |
| `/` | Search / highlight text within the viewer |
| `ESC` | Close viewer, return to Updates list |

---

### Todos tab (`t`)

Lists all todos for the project. Archived and done/canceled todos can be hidden.

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` (Shift+j) | Move selected todo **down** (reorder) |
| `K` (Shift+k) | Move selected todo **up** (reorder) |
| `n` | Add a new todo (inline title entry) |
| `e` | Edit selected todo title (inline) |
| `s` | Cycle status of selected todo |
| `S` (Shift+s) | Spell check the selected todo title |
| `r` | Set/edit/clear reminder on selected todo |
| `m` | Move selected todo to another project or Inbox |
| `a` | Archive / unarchive the selected todo |
| `A` (Shift+a) | Toggle show/hide archived todos |
| `C` (Shift+c) | Toggle hide/show done and canceled todos |
| `D` (Shift+d) | Delete selected todo (with confirmation) |

#### Adding a todo

Press `n`. An inline text input appears at the bottom of the todo list. Type the title and press `Enter` to save, or `ESC` to cancel.

#### Cycling todo status

Press `s` on the selected todo to advance its status one step:

```
New (-)  →  In Progress (o)  →  Done (✓)  →  Canceled (x)  →  New (-)
```

---

### Archiving the project

Press `a` from any tab. The project is archived and you are returned to the Project List.

---

## 7. Global Todo List

Press `t` from the Project List to switch to the Global Todo List.

```
┌──────────────────────────────────────────────────────────┐
│ All Todos                                                │
├──────────────────────────────────────────────────────────┤
│ ── Inbox ─────────────────────────────────────────────── │
│   - Call dentist                                         │
│ ── Rewrite auth service ──────────────────────────────── │
│   o Audit existing JWT usage         2025-07-20          │  ← cursor
│   - Design Paseto schema                                 │
│ ── Build analytics dashboard ─────────────────────────── │
│   - Define KPI dashboard layout                          │
│   - Hook up BigQuery source          2025-07-25          │
├──────────────────────────────────────────────────────────┤
│ Q:quit  ?:help                                           │
└──────────────────────────────────────────────────────────┘
```

Todos are grouped by project. The **Inbox** (todos with no project) is always shown first.

### Cursor movement

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |

The cursor moves through individual todo items only (skipping group headers).

### Reordering

| Key | Action |
|---|---|
| `J` (Shift+j) | Move selected todo down within its project group |
| `K` (Shift+k) | Move selected todo up within its project group |

Todos cannot be dragged across project group boundaries — use `m` (move) for that.

### Status cycling

Press `s` to cycle the selected todo's status (same cycle as in Project Detail).

### Editing a todo title

Press `e`. The todo title becomes an inline text input. Edit and press `Enter` to save, or `ESC` to discard.

### Spell checking a todo title

Press `S` (Shift+s) to open the **Spell Check** overlay for the selected todo's title.

### Creating a new todo (Inbox)

Press `n`. The **Quick Capture** overlay opens, adding the new todo directly to the Inbox.

### Setting a reminder

Press `r` to open the **Reminder Input** overlay for the selected todo.

### Moving a todo

Press `m` to open the **Move-to-Project Picker** overlay.

### Archiving a todo

| Key | Action |
|---|---|
| `a` | Archive / unarchive the selected todo |
| `A` (Shift+a) | Toggle show/hide archived todos |

### Jumping to a project

Press `Enter` on a todo to navigate to the **Project Detail** screen for its owning project and place the cursor on that same todo in the Todos tab. (Inbox todos have no project, so `Enter` has no effect on them.)

### Filtering

| Key | Action |
|---|---|
| `/` | Search — filters todo titles in real time |
| `ESC` (in search) | Clear search and exit search mode |
| `C` (Shift+c) | Toggle hide/show completed (`✓`) and canceled (`x`) todos |
| `T` (Shift+t) | Open Tag Picker to filter by project tag |

Selecting the already-active tag in the picker clears the filter.

### Deleting a todo

Press `D` (Shift+d). A confirmation dialog appears.

### Navigating to Project List

Press `p` to return to the Project List.

### Quit

Press `Q` (Shift+q) to quit ptm.

---

## 8. Overlays

Overlays appear on top of the current screen. All keys are captured by the overlay until it is dismissed.

### 8.1 Quick Capture

**Trigger:** `i` from any screen (outside overlays); or `n` from the Global Todo List.

```
┌──────────────────────────────┐
│ Quick capture → Inbox        │
│ > _                          │
│ Enter:save  ESC:cancel       │
└──────────────────────────────┘
```

Type the todo title and press `Enter` to save it to the Inbox. Press `ESC` to cancel without saving.

The current screen is not changed; ptm resumes exactly where you were.

---

### 8.2 Tag Picker

**Trigger:** `T` (Shift+t) from Project List or Todo List (filter mode); or `T` from Project Detail (edit mode).

```
┌──────────────────────────┐
│ Tags  > _                │
├──────────────────────────┤
│ ● work                   │  ← applied (●) vs. available (○)
│ ○ personal               │
│ ○ customer-a             │
│   + Create "work-new"    │  ← shown if no exact match
└──────────────────────────┘
```

| Key | Action |
|---|---|
| Type characters | Filter the tag list |
| `Backspace` | Delete last character; closes picker if input is empty |
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `Enter` (edit mode) | Toggle the highlighted tag on/off for the current project |
| `Enter` (filter mode) | Apply the highlighted tag as a filter (selecting the active tag clears it) |
| `Enter` on "Create" row | Create the new tag and immediately apply it |
| `r` (search empty, edit or filter mode) | Rename the highlighted tag |
| `ESC` | Close the picker with no further changes |

**Behavior:**
- Tags are matched case-insensitively.
- If the typed text does not match any existing tag exactly, a **"+ Create"** row appears at the bottom (edit mode only). Pressing `Enter` on it creates the tag and applies it in one step.
- Removing the last association of a tag automatically deletes the tag from the database (garbage-collected silently).

---

### 8.3 Move-to-Project Picker

**Trigger:** `m` from Project Detail (Todos tab) or `m` from Global Todo List.

```
┌──────────────────────────────────┐
│ Move to…  > auth_                │
├──────────────────────────────────┤
│   ── Inbox ──                    │
│   Rewrite auth service           │
│   Build analytics dashboard      │
└──────────────────────────────────┘
```

| Key | Action |
|---|---|
| Type characters | Filter the project list |
| `Backspace` | Delete last character |
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `Enter` | Move the todo to the highlighted destination |
| `ESC` | Cancel — no change |

**Behavior:**
- The todo's current project (or Inbox) is excluded from the list.
- **Inbox** is always available as a destination (sets the todo's project to none).
- Archived projects do not appear.
- The todo is appended at the end of the destination project's todo list.

---

### 8.4 Reminder Input

**Trigger:** `r` from Project Detail (Todos tab) or `r` from Global Todo List.

```
┌──────────────────────────────────┐
│ Reminder (YYYY-MM-DD)            │
│ > 2025-08-01_                    │
│ Enter:save  ESC:cancel           │
└──────────────────────────────────┘
```

Type a date in `YYYY-MM-DD` format and press `Enter` to save. Leave the input blank and press `Enter` to **clear** an existing reminder. Press `ESC` to cancel.

The input is pre-filled with the existing reminder date (if any). An error message is shown if the format is invalid.

---

### 8.5 Spell Check

**Trigger:** `S` (Shift+s) on a selected todo (Project Detail or Todo List), on a selected update (Updates tab or update viewer), or in the Description tab.

```
┌────────────────────────────────────────────────┐
│ Spell Check  (1 of 2 flagged)                  │
├────────────────────────────────────────────────┤
│ ...context around the misspelled word...       │
│                                                │
│  Misspelled: "Authh"                           │
│                                                │
│  1. Auth                                       │
│  2. Auth.                                      │
│  3. Auths                                      │
│                                                │
│  1-9:replace  s:skip  a:accept-all  i:add-dict │
│  ESC/q:finish                                  │
└────────────────────────────────────────────────┘
```

The overlay presents flagged words one at a time.

| Key | Action |
|---|---|
| `1`–`9` | Replace the current word with numbered suggestion |
| `s` | Skip this occurrence |
| `a` | Accept all occurrences of this word for the session (ignore) |
| `i` | Add word to personal dictionary (persisted) |
| `ESC` / `q` | Finish session and save all corrections made so far |

When all words are handled, the overlay dismisses automatically. Corrections are saved back to the database immediately on close.

> **Note:** Spell check requires a Hunspell dictionary (`en_US.aff` / `en_US.dic`). Install with `apt install hunspell-en-us` or point `PTM_DICT_DIR` at a directory containing those files. If no dictionary is found, a warning is shown instead.

---

### 8.6 Key Help

**Trigger:** `?` from any screen (outside overlays).

Displays a scrollable reference of all keybindings.

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `ESC` / `?` | Close |

---

### 8.7 Confirm Delete

**Trigger:** `D` (Shift+d) on a project, todo, or update.

```
┌────────────────────────────────────────────────────────┐
│ Delete 'Rewrite auth service'?                         │
│ All todos and updates will be lost.                    │
│                                                        │
│  y / Enter : confirm       n / ESC : cancel            │
└────────────────────────────────────────────────────────┘
```

| Key | Action |
|---|---|
| `y`, `Y`, or `Enter` | Confirm and permanently delete |
| `n`, `N`, or `ESC` | Cancel, nothing is deleted |

---

## 9. Tags

Tags are global labels shared across all projects. There is no separate "manage tags" screen.

### Creating a tag
Open the Tag Picker on any project (`T` in Project Detail), type a new name, and press `Enter` on the **"+ Create"** row (or press `Enter` when the typed name has no exact match).

### Applying / removing a tag
In the Tag Picker (edit mode), press `Enter` on any tag to toggle it on or off for the current project. Applied tags are shown with a filled indicator (●).

### Renaming a tag
Open the Tag Picker (with `T`), clear the search input so it is empty, navigate to the tag you want to rename, and press `r`. An inline rename input appears; press `Enter` to save, `ESC` to cancel.

### Tag filter
From the Project List or Global Todo List, press `T` (Shift+t) to open the Tag Picker in filter mode. Selecting a tag narrows the visible items to those associated with that tag. Selecting the same tag again clears the filter.

### Automatic cleanup
When a tag is removed from its last project, it is automatically deleted from the database. You never need to manually clean up unused tags.

### Todo tag inheritance
Todos do not have their own tags. They inherit the tags of their parent project. Filtering by tag in the Global Todo List shows todos whose project carries that tag. Inbox todos (no project) appear only when no tag filter is active.

---

## 10. Todo Statuses

Each todo has one of four statuses, displayed as a leading character:

| Character | Status | Meaning |
|---|---|---|
| `-` | **New** | Not yet started |
| `o` | **In Progress** | Actively being worked on |
| `✓` | **Done** | Completed |
| `x` | **Canceled** | Abandoned |

Press `s` on a selected todo to advance one step through the cycle:

```
New (-)  →  In Progress (o)  →  Done (✓)  →  Canceled (x)  →  New (-)  →  …
```

Press `C` (Shift+c) in the Global Todo List or the Todos tab to hide/show todos in the Done or Canceled states.

---

## 11. Reminders

A reminder is an ISO date (`YYYY-MM-DD`) associated with a todo. To set or change a reminder, press `r` on the selected todo from either Project Detail (Todos tab) or the Global Todo List.

Reminder dates are surfaced in two places:

1. **Inline on the todo** — displayed as `  YYYY-MM-DD` to the right of the todo title, color-coded.
2. **On the Project List** — the earliest active-todo reminder date bubbles up to appear next to the project title.

Only reminders on active (non-done, non-canceled) todos are surfaced.

### Color coding

| Color | Meaning |
|---|---|
| 🔴 Red | Date is in the past (overdue) |
| 🟡 Yellow | Date is within the next 7 days |
| Default | Further away |

ptm does **not** send active notifications. Reminders are informational only and must be checked by opening the app.

To clear a reminder, press `r`, delete the date text so the input is blank, and press `Enter`.

---

## 12. External Editor Integration

Long-form text (project descriptions and update entries) is edited in your system's `$EDITOR`. When you trigger an editor action (`e` or `n` in Description tab; `n` or `e` in Updates tab), ptm:

1. Suspends the TUI and restores the normal terminal.
2. Writes the current content to a temporary file.
3. Launches `$EDITOR` with that file.
4. Waits for the editor process to exit.
5. Reads the saved file content back.
6. Resumes the TUI with the updated content.

If `$EDITOR` is not set, ptm falls back to `vi`.

---

## 13. Quick Reference: All Keybindings

### Global (any screen, outside overlays)

| Key | Action |
|---|---|
| `i` | Open Quick Capture (add todo to Inbox) |
| `?` | Open Key Help overlay |

---

### Project List

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` | Move selected project down (reorder) |
| `K` | Move selected project up (reorder) |
| `Enter` | Open Project Detail |
| `n` | New project (inline title entry) |
| `e` | Rename selected project title (inline) |
| `/` | Search project titles |
| `T` | Tag filter picker |
| `a` | Archive / unarchive selected project |
| `A` | Toggle show/hide archived projects |
| `D` | Delete selected project (with confirmation) |
| `t` | Switch to Global Todo List |
| `Q` | Quit |

---

### Project Detail — Universal

| Key | Action |
|---|---|
| `ESC` | Return to Project List |
| `E` | Edit project title (inline) |
| `T` | Edit project tags |
| `/` | Search / highlight text |
| `d` | Switch to Description tab |
| `u` | Switch to Updates tab |
| `t` | Switch to Todos tab |
| `f` | Switch to Files tab |

---

### Project Detail — Description tab

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `e` / `n` | Edit description in `$EDITOR` |
| `S` | Spell check description |

---

### Project Detail — Updates tab

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` | Move selected update down (reorder) |
| `K` | Move selected update up (reorder) |
| `Enter` | Open selected update in viewer |
| `n` | Add new update (opens `$EDITOR`) |
| `e` | Edit selected update (opens `$EDITOR`) |
| `S` | Spell check selected update |
| `D` | Delete selected update (with confirmation) |

#### Update viewer (within Updates tab)

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `e` | Edit this update in `$EDITOR` |
| `S` | Spell check this update |
| `/` | Search / highlight |
| `ESC` | Close viewer |

---

### Project Detail — Todos tab

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` | Move selected todo down (reorder) |
| `K` | Move selected todo up (reorder) |
| `n` | Add new todo (inline) |
| `e` | Edit selected todo title (inline) |
| `s` | Cycle todo status |
| `S` | Spell check selected todo title |
| `r` | Set/edit/clear reminder |
| `m` | Move todo to another project or Inbox |
| `a` | Archive / unarchive selected todo |
| `A` | Toggle show/hide archived todos |
| `C` | Toggle hide/show done and canceled todos |
| `D` | Delete selected todo (with confirmation) |

---

### Global Todo List

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `J` | Move selected todo down within its project group (reorder) |
| `K` | Move selected todo up within its project group (reorder) |
| `s` | Cycle status of selected todo |
| `e` | Edit selected todo title (inline) |
| `S` | Spell check selected todo title |
| `r` | Set/edit/clear reminder |
| `n` | Quick Capture (add todo to Inbox) |
| `m` | Move selected todo to another project or Inbox |
| `a` | Archive / unarchive selected todo |
| `A` | Toggle show/hide archived todos |
| `Enter` | Jump to owning project's Project Detail |
| `/` | Search todo titles |
| `C` | Toggle hide/show done and canceled todos |
| `T` | Tag filter picker |
| `D` | Delete selected todo (with confirmation) |
| `p` | Switch to Project List |
| `Q` | Quit |

---

### Overlays (all)

| Key | Action |
|---|---|
| `ESC` | Close overlay, cancel any change |

### Quick Capture

| Key | Action |
|---|---|
| `Enter` | Save todo to Inbox |
| `ESC` | Cancel |

### Tag Picker

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `Enter` | Toggle tag (edit mode) or apply filter (filter mode) |
| `r` | Rename highlighted tag (when search input is empty) |
| `Backspace` | Delete last character; closes picker if input is empty |
| `ESC` | Close without changes |

### Move-to-Project Picker

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `Enter` | Move todo to selected destination |
| `ESC` | Cancel |

### Reminder Input

| Key | Action |
|---|---|
| `Enter` | Save reminder; blank input clears the reminder |
| `ESC` | Cancel |

### Spell Check

| Key | Action |
|---|---|
| `1`–`9` | Replace with numbered suggestion |
| `s` | Skip this occurrence |
| `a` | Accept all occurrences (ignore for session) |
| `i` | Add word to personal dictionary |
| `ESC` / `q` | Finish and save corrections |

### Key Help

| Key | Action |
|---|---|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `ESC` / `?` | Close |

### Confirm Delete

| Key | Action |
|---|---|
| `y`, `Y`, `Enter` | Confirm deletion |
| `n`, `N`, `ESC` | Cancel |

---

*Created using Anthropic Claude — keep this note on internal versions until a human has reviewed and verified the content.*
