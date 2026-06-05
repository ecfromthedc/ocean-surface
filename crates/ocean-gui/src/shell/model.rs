use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workspace::{Workspace, WorkspaceEntry, WorkspaceEntryKind, WorkspaceTree};

use super::editor_buffer::{EditorCursor, TextBuffer};
use super::vault_index::{self, Backlink, VaultIndex};

const MAX_SCAN_DEPTH: usize = 6;
pub const MAX_RENDERED_LINES: usize = 72;
const CURSOR_SCROLL_MARGIN: usize = 8;
const LIVE_METADATA_CHAR_LIMIT: usize = 200_000;
const APP_SUPPORT_DIR: &str = "Library/Application Support/Ocean GUI";
const LAST_WORKSPACE_FILE: &str = "last-workspace";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    Folder,
    Markdown,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: usize,
    pub path: PathBuf,
    pub label: String,
    pub depth: usize,
    pub kind: FileKind,
    pub expanded: bool,
    pub has_children: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteSearchResult {
    pub path: PathBuf,
    pub label: String,
    pub parent_label: String,
}

#[derive(Clone, Debug)]
pub struct EditorTab {
    pub path: PathBuf,
    pub label: String,
    pub dirty: bool,
    buffer: TextBuffer,
}

impl EditorTab {
    fn clean(path: PathBuf, label: String, text: String) -> Self {
        Self {
            path,
            label,
            dirty: false,
            buffer: TextBuffer::new_saved(&text),
        }
    }

    fn reset_document(&mut self, text: &str, dirty: bool) {
        self.buffer = TextBuffer::new_saved(text);
        if dirty {
            self.buffer.mark_dirty();
        }
        self.dirty = dirty;
    }

    fn sync_document(&mut self, buffer: &TextBuffer) {
        self.buffer = buffer.clone();
        self.dirty = self.buffer.is_dirty();
    }

    fn mark_saved(&mut self) {
        self.buffer.mark_saved();
        self.dirty = false;
    }
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub label: String,
    pub level: u8,
    pub line_number: usize,
}

#[derive(Clone, Debug, Default)]
pub struct DocumentStatus {
    pub words: usize,
    pub lines: usize,
    pub links: usize,
    pub backlinks: usize,
    pub rendered_lines: usize,
}

#[derive(Clone, Debug)]
pub struct ShellState {
    workspace: Workspace,
    tree: Option<WorkspaceTree>,
    vault_index: VaultIndex,
    expanded_dirs: HashSet<PathBuf>,
    pub root: PathBuf,
    pub active_path: Option<PathBuf>,
    pub selected_path: Option<PathBuf>,
    pub files: Vec<FileEntry>,
    pub tabs: Vec<EditorTab>,
    document: TextBuffer,
    marked_text_range: Option<Range<usize>>,
    pub document_start_line: usize,
    pub document_lines: Vec<String>,
    pub outline: Vec<OutlineItem>,
    pub links: Vec<String>,
    pub backlinks: Vec<Backlink>,
    pub status: DocumentStatus,
    pub status_message: String,
}

impl ShellState {
    #[must_use]
    pub fn seed() -> Self {
        let mut state = Self::seed_with_root(initial_workspace_root());
        if let Err(error) = write_last_workspace(&state.root) {
            state.status_message =
                format!("{}; vault not remembered: {error}", state.status_message);
        }
        state
    }

    #[must_use]
    pub fn seed_with_root(root: PathBuf) -> Self {
        let workspace = Workspace::new(&root)
            .or_else(|_| Workspace::new(PathBuf::from(".")))
            .expect("current directory or fallback workspace should be readable");
        let root = workspace.root().to_path_buf();

        let mut state = Self {
            workspace,
            tree: None,
            vault_index: VaultIndex::empty(),
            expanded_dirs: HashSet::new(),
            root,
            active_path: None,
            selected_path: None,
            files: Vec::new(),
            tabs: Vec::new(),
            document: TextBuffer::new_saved(""),
            marked_text_range: None,
            document_start_line: 0,
            document_lines: Vec::new(),
            outline: Vec::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
            status: DocumentStatus::default(),
            status_message: String::from("Ready"),
        };

        state.refresh_files();
        state.open_first_markdown();
        state
    }

    #[must_use]
    pub fn active_label(&self) -> String {
        self.active_path
            .as_deref()
            .map(|path| self.display_path(path))
            .unwrap_or_else(|| String::from("scratch"))
    }

    #[must_use]
    pub fn root_label(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    pub fn refresh_files(&mut self) {
        match self.workspace.scan(MAX_SCAN_DEPTH) {
            Ok(tree) => {
                let vault_index = self.index_for_tree(&tree);
                seed_expanded_dirs(&tree, &mut self.expanded_dirs);
                self.tree = Some(tree);
                self.vault_index = vault_index;
                self.rebuild_visible_files();
                self.refresh_document_metadata();
                self.status_message = format!("{} visible entries", self.files.len());
            }
            Err(error) => {
                self.files.clear();
                self.tree = None;
                self.vault_index = VaultIndex::empty();
                self.status_message = error.to_string();
            }
        }
    }

    pub fn set_active_file(&mut self, file_id: usize) {
        let Some(file) = self.files.iter().find(|entry| entry.id == file_id).cloned() else {
            return;
        };

        self.selected_path = Some(file.path.clone());
        match file.kind {
            FileKind::Folder => self.toggle_folder(file.path),
            FileKind::Markdown => self.open_path(file.path),
        }
    }

    pub fn set_active_tab(&mut self, tab_index: usize) {
        if self.tabs.get(tab_index).is_none() {
            return;
        }

        self.sync_active_tab_buffer();

        self.open_tab_buffer(tab_index);
    }

    pub fn close_tab(&mut self, tab_index: usize) {
        if tab_index >= self.tabs.len() {
            return;
        }

        let closed_path = self.tabs.remove(tab_index).path;
        if self.active_path.as_ref() == Some(&closed_path) {
            if let Some(next_tab) = self
                .tabs
                .get(tab_index)
                .or_else(|| self.tabs.last())
                .cloned()
            {
                self.open_path(next_tab.path);
            } else {
                self.load_document(None, String::new(), String::from("No file open"));
            }
        }
    }

    pub fn set_workspace_root(&mut self, root: PathBuf) {
        match self.workspace.set_root(root) {
            Ok(()) => {
                self.root = self.workspace.root().to_path_buf();
                self.active_path = None;
                self.selected_path = None;
                self.tree = None;
                self.vault_index = VaultIndex::empty();
                self.expanded_dirs.clear();
                self.tabs.clear();
                self.refresh_files();
                self.open_first_markdown();
                if let Err(error) = write_last_workspace(&self.root) {
                    self.status_message =
                        format!("{}; vault not remembered: {error}", self.status_message);
                }
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn create_note(&mut self) {
        let creation_dir = self.note_creation_dir();
        let path = self.next_note_path_in(&creation_dir);
        let title = path
            .file_stem()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| String::from("Untitled"));
        let text = format!("# {title}\n\n");

        match self.workspace.write_file(&path, &text) {
            Ok(()) => {
                self.refresh_files();
                if let Some(parent) = path.parent() {
                    self.expanded_dirs.insert(parent.to_path_buf());
                    self.rebuild_visible_files();
                }
                self.open_path(path);
                self.move_cursor_to_end();
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn rename_selected_to(&mut self, target: PathBuf) {
        let Some(source) = self.selected_note_path() else {
            self.status_message = String::from("Select a note to rename");
            return;
        };
        let target = normalize_markdown_target(target);

        if !target.starts_with(&self.root) {
            self.status_message = String::from("Rename target must stay inside vault");
            return;
        }

        if source == target {
            self.status_message = String::from("Rename target is unchanged");
            return;
        }

        self.sync_active_tab_buffer();
        match self.workspace.rename_path(&source, &target) {
            Ok(()) => {
                if let Some(parent) = target.parent() {
                    self.expanded_dirs.insert(parent.to_path_buf());
                }
                self.update_open_paths_after_rename(&source, &target);
                self.refresh_files();
                self.selected_path = Some(target.clone());
                self.status_message = format!("Renamed {}", self.display_path(&target));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn delete_selected_note(&mut self) {
        let Some(path) = self.selected_note_path() else {
            self.status_message = String::from("Select a note to delete");
            return;
        };

        if self.open_tab_dirty(&path) {
            self.status_message = String::from("Save or close dirty note before deleting");
            return;
        }

        let was_active = self.active_path.as_ref() == Some(&path);
        let label = self.display_path(&path);
        match self.workspace.remove_file(&path) {
            Ok(()) => {
                self.tabs.retain(|tab| tab.path != path);
                self.selected_path = None;
                if was_active {
                    if let Some(last_index) = self.tabs.len().checked_sub(1) {
                        self.open_tab_buffer(last_index);
                    } else {
                        self.active_path = None;
                        self.refresh_files();
                        self.open_first_markdown();
                    }
                } else {
                    self.refresh_files();
                }
                self.status_message = format!("Deleted {label}");
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn reveal_selected(&mut self) {
        let Some(path) = self.selected_path.as_ref().or(self.active_path.as_ref()) else {
            self.status_message = String::from("No file selected");
            return;
        };

        match Command::new("open").arg("-R").arg(path).spawn() {
            Ok(_) => {
                self.status_message = format!("Revealed {}", self.display_path(path));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn apply_external_vault_change(&mut self, paths: &[PathBuf]) {
        self.refresh_files();

        let Some(active_path) = self.active_path.clone() else {
            self.status_message = String::from("Vault updated");
            return;
        };

        if !paths.is_empty()
            && !paths
                .iter()
                .any(|path| path_matches_active(path, &active_path))
        {
            self.status_message = String::from("Vault updated");
            return;
        }

        if self.open_tab_dirty(&active_path) {
            self.status_message = String::from("External change detected; save or reload manually");
            return;
        }

        if active_path.exists() {
            self.reload_active();
        } else {
            self.close_missing_active_path(active_path);
        }
    }

    pub fn reload_active(&mut self) {
        let Some(path) = self.active_path.clone() else {
            self.status_message = String::from("No file open");
            return;
        };

        match self.workspace.read_file(&path) {
            Ok(text) => {
                let label = self.display_path(&path);
                self.replace_active_tab_buffer(&path, &text, false);
                self.load_document(Some(path), text, format!("Reloaded {label}"));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn open_active_external(&mut self) {
        let Some(path) = self.active_path.clone() else {
            self.status_message = String::from("No file open");
            return;
        };

        match Command::new("open").arg(&path).spawn() {
            Ok(_) => {
                self.status_message = format!("Editing {}", self.display_path(&path));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn save_active(&mut self) {
        let Some(path) = self.active_path.clone() else {
            self.status_message = String::from("No file open");
            return;
        };

        match self.workspace.write_file(&path, &self.document.text()) {
            Ok(()) => {
                self.document.mark_saved();
                for tab in &mut self.tabs {
                    if tab.path == path {
                        tab.sync_document(&self.document);
                        tab.mark_saved();
                    }
                }
                self.rebuild_vault_index();
                self.refresh_document_metadata();
                self.status_message = format!("Saved {}", self.display_path(&path));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() || self.active_path.is_none() {
            return;
        }

        if self.document.insert_text(text) {
            self.after_edit();
        }
    }

    pub fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    pub fn insert_tab(&mut self) {
        self.insert_text("    ");
    }

    pub fn delete_backward(&mut self) {
        if self.active_path.is_none() {
            return;
        }

        if self.document.delete_backward() {
            self.after_edit();
        }
    }

    pub fn delete_forward(&mut self) {
        if self.active_path.is_none() {
            return;
        }

        if self.document.delete_forward() {
            self.after_edit();
        }
    }

    pub fn undo(&mut self) {
        if self.active_path.is_none() {
            self.status_message = String::from("No file open");
            return;
        }

        if !self.document.undo() {
            self.status_message = String::from("Nothing to undo");
            return;
        }

        self.after_history_edit("Undid edit");
    }

    pub fn redo(&mut self) {
        if self.active_path.is_none() {
            self.status_message = String::from("No file open");
            return;
        }

        if !self.document.redo() {
            self.status_message = String::from("Nothing to redo");
            return;
        }

        self.after_history_edit("Redid edit");
    }

    pub fn move_cursor_left(&mut self) {
        self.document.move_left();
        self.after_cursor_move();
    }

    pub fn move_cursor_right(&mut self) {
        self.document.move_right();
        self.after_cursor_move();
    }

    pub fn move_cursor_up(&mut self) {
        self.document.move_up(false);
        self.after_cursor_move();
    }

    pub fn move_cursor_down(&mut self) {
        self.document.move_down(false);
        self.after_cursor_move();
    }

    pub fn move_cursor_to_start(&mut self) {
        self.document.move_to_start();
        self.after_cursor_move();
    }

    pub fn move_cursor_to_end(&mut self) {
        self.document.move_to_end();
        self.after_cursor_move();
    }

    pub fn move_cursor_to_line_column(&mut self, line: usize, column: usize) {
        self.document.move_to_line_column(line, column);
        self.after_cursor_move();
    }

    pub fn jump_to_outline_item(&mut self, outline_index: usize) -> bool {
        let Some(item) = self.outline.get(outline_index) else {
            self.status_message = String::from("Heading not found");
            return false;
        };

        let line = item.line_number.saturating_sub(1);
        let label = item.label.clone();
        self.document.move_to_line_column(line, 0);
        self.after_cursor_move();
        self.status_message = format!("Jumped to {label}");
        true
    }

    pub fn extend_cursor_to_line_column(&mut self, line: usize, column: usize) {
        self.document.extend_to_line_column(line, column);
        self.after_cursor_move();
    }

    pub fn scroll_document_by_lines(&mut self, line_delta: isize) -> bool {
        let line_count = self.document.line_count();
        if line_count == 0 {
            return false;
        }

        let max_start = max_document_start_line(line_count);
        let next_start = if line_delta.is_negative() {
            self.document_start_line
                .saturating_sub(line_delta.unsigned_abs())
        } else {
            self.document_start_line
                .saturating_add(line_delta as usize)
                .min(max_start)
        };

        if next_start == self.document_start_line {
            return false;
        }

        self.document_start_line = next_start;
        self.refresh_document_view();
        true
    }

    pub fn set_document_start_line(&mut self, line: usize) -> bool {
        let line_count = self.document.line_count();
        let next_start = line.min(max_document_start_line(line_count));

        if next_start == self.document_start_line {
            return false;
        }

        self.document_start_line = next_start;
        self.refresh_document_view();
        true
    }

    pub fn extend_cursor_left(&mut self) {
        self.document.extend_left();
        self.after_cursor_move();
    }

    pub fn extend_cursor_right(&mut self) {
        self.document.extend_right();
        self.after_cursor_move();
    }

    pub fn extend_cursor_up(&mut self) {
        self.document.extend_up();
        self.after_cursor_move();
    }

    pub fn extend_cursor_down(&mut self) {
        self.document.extend_down();
        self.after_cursor_move();
    }

    pub fn select_all(&mut self) {
        self.document.select_all();
    }

    pub fn select_word_at_line_column(&mut self, line: usize, column: usize) -> bool {
        let selected = self.document.select_word_at_line_column(line, column);
        self.marked_text_range = None;
        self.after_cursor_move();
        selected
    }

    #[must_use]
    pub fn selection_range(&self) -> Option<Range<usize>> {
        self.document.selection_range()
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        self.document.selected_text()
    }

    #[must_use]
    pub fn selected_columns_for_line(&self, line: usize) -> Option<Range<usize>> {
        self.document.selected_columns_for_line(line)
    }

    #[must_use]
    pub fn selected_utf16_range(&self) -> (Range<usize>, bool) {
        self.document.utf16_selection_range()
    }

    #[must_use]
    pub fn marked_utf16_range(&self) -> Option<Range<usize>> {
        self.marked_text_range
            .as_ref()
            .map(|range| self.document.char_range_to_utf16(range))
    }

    #[must_use]
    pub fn text_for_utf16_range(
        &self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
    ) -> String {
        self.document.text_for_utf16_range(range, adjusted_range)
    }

    pub fn replace_text_in_utf16_range(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let replacement_range = range
            .map(|range| self.document.utf16_range_to_char(&range))
            .or_else(|| self.marked_text_range.clone())
            .or_else(|| self.selection_range())
            .unwrap_or_else(|| {
                let cursor = self.document.cursor_char_offset();
                cursor..cursor
            });

        self.marked_text_range = None;
        let changed = self
            .document
            .replace_range(replacement_range, text)
            .is_some();
        if changed {
            self.after_edit();
        }
        changed
    }

    pub fn replace_and_mark_text_in_utf16_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected_range: Option<Range<usize>>,
    ) -> bool {
        let replacement_range = range
            .map(|range| self.document.utf16_range_to_char(&range))
            .or_else(|| self.marked_text_range.clone())
            .or_else(|| self.selection_range())
            .unwrap_or_else(|| {
                let cursor = self.document.cursor_char_offset();
                cursor..cursor
            });

        let Some(inserted_range) = self.document.replace_range(replacement_range, text) else {
            self.marked_text_range = None;
            return false;
        };

        self.marked_text_range = (!text.is_empty()).then_some(inserted_range.clone());
        if let Some(selected_range) = selected_range {
            let selected_range = utf16_range_in_text_to_char_range(text, selected_range);
            self.document.set_selection_range(
                inserted_range.start + selected_range.start
                    ..inserted_range.start + selected_range.end,
                false,
            );
        }

        self.after_edit();
        true
    }

    pub fn unmark_text(&mut self) {
        self.marked_text_range = None;
    }

    #[must_use]
    pub fn editor_cursors_for_utf16_range(
        &self,
        range: Range<usize>,
    ) -> (EditorCursor, EditorCursor) {
        let char_range = self.document.utf16_range_to_char(&range);
        let start = self.document.line_column_for_char_offset(char_range.start);
        let end = self.document.line_column_for_char_offset(char_range.end);

        (start, end)
    }

    #[must_use]
    pub fn utf16_index_for_line_column(&self, line: usize, column: usize) -> usize {
        self.document.utf16_offset_for_line_column(line, column)
    }

    pub fn take_selected_text(&mut self) -> Option<String> {
        let selected = self.document.take_selected_text()?;
        self.marked_text_range = None;
        self.after_edit();
        Some(selected)
    }

    #[must_use]
    pub fn cursor_position(&self) -> EditorCursor {
        self.document.cursor_position()
    }

    #[must_use]
    pub fn document_text(&self) -> String {
        self.document.text()
    }

    fn open_first_markdown(&mut self) {
        let first_markdown = self.tree.as_ref().and_then(first_markdown_path);

        if let Some(path) = first_markdown {
            self.open_path(path);
        } else {
            self.load_document(
                None,
                String::from("# Ocean GUI\n\nNo markdown files found in this workspace."),
                String::from("No markdown files found"),
            );
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        self.sync_active_tab_buffer();

        if let Some(tab_index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.open_tab_buffer(tab_index);
            return;
        }

        match self.workspace.read_file(&path) {
            Ok(text) => {
                let label = self.display_path(&path);
                self.selected_path = Some(path.clone());
                self.tabs
                    .push(EditorTab::clean(path.clone(), label.clone(), text.clone()));

                self.load_document(Some(path), text, format!("Opened {label}"));
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    fn open_tab_buffer(&mut self, tab_index: usize) {
        let Some(tab) = self.tabs.get(tab_index).cloned() else {
            return;
        };

        self.selected_path = Some(tab.path.clone());
        self.load_document_buffer(Some(tab.path), tab.buffer, format!("Opened {}", tab.label));
    }

    fn replace_active_tab_buffer(&mut self, path: &Path, text: &str, dirty: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == path) {
            tab.reset_document(text, dirty);
        } else {
            let label = self.display_path(path);
            let mut tab = EditorTab::clean(path.to_path_buf(), label, text.to_string());
            if dirty {
                tab.dirty = true;
            }
            self.tabs.push(tab);
        }
    }

    fn sync_active_tab_buffer(&mut self) {
        let Some(path) = self.active_path.as_ref() else {
            return;
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.path == path) {
            tab.sync_document(&self.document);
        }
    }

    fn after_edit(&mut self) {
        self.refresh_document_after_edit();
        self.update_active_tab_dirty();
        self.status_message = format!("Modified {}", self.active_label());
    }

    fn after_history_edit(&mut self, action: &str) {
        self.refresh_document_after_edit();
        self.update_active_tab_dirty();
        self.status_message = format!("{action} {}", self.active_label());
    }

    fn after_cursor_move(&mut self) {
        self.ensure_cursor_visible();
    }

    fn refresh_document_after_edit(&mut self) {
        if self.document.len_chars() <= LIVE_METADATA_CHAR_LIMIT {
            self.refresh_document_metadata();
        } else {
            self.refresh_document_view();
        }
    }

    fn refresh_document_metadata(&mut self) {
        self.refresh_document_view();
        if self.document.len_chars() > LIVE_METADATA_CHAR_LIMIT {
            self.outline.clear();
            self.links.clear();
            self.backlinks.clear();
            self.status.words = 0;
            self.status.links = 0;
            self.status.backlinks = 0;
            return;
        }

        let all_lines = self.document.all_lines();
        let text = self.document.text();
        self.outline = extract_outline(&all_lines);
        self.links = extract_links(&text);
        self.backlinks = self
            .active_path
            .as_deref()
            .map(|path| self.vault_index.backlinks_for(path))
            .unwrap_or_default();
        self.status.words = self.document.word_count();
        self.status.links = self.links.len();
        self.status.backlinks = self.backlinks.len();
    }

    fn refresh_document_view(&mut self) {
        self.clamp_document_start_line();
        self.document_lines = self
            .document
            .rendered_lines_from(self.document_start_line, MAX_RENDERED_LINES);
        self.status = DocumentStatus {
            words: self.status.words,
            lines: self.document.line_count(),
            links: self.status.links,
            backlinks: self.status.backlinks,
            rendered_lines: self.document_lines.len().min(MAX_RENDERED_LINES),
        };
    }

    fn ensure_cursor_visible(&mut self) {
        let cursor_line = self.document.cursor_position().line;
        let line_count = self.document.line_count();
        let visible_budget = MAX_RENDERED_LINES.min(line_count);
        if visible_budget == 0 {
            self.document_start_line = 0;
            self.document_lines.clear();
            return;
        }

        let visible_end = self.document_start_line + visible_budget;
        let near_top = cursor_line < self.document_start_line + CURSOR_SCROLL_MARGIN;
        let near_bottom = cursor_line + CURSOR_SCROLL_MARGIN >= visible_end;

        let new_start = if near_top && self.document_start_line > 0 {
            cursor_line.saturating_sub(CURSOR_SCROLL_MARGIN)
        } else if near_bottom && visible_end < line_count {
            (cursor_line + CURSOR_SCROLL_MARGIN + 1)
                .saturating_sub(visible_budget)
                .min(line_count.saturating_sub(visible_budget))
        } else {
            return;
        };

        if new_start != self.document_start_line {
            self.document_start_line = new_start;
            self.refresh_document_view();
        }
    }

    fn clamp_document_start_line(&mut self) {
        let line_count = self.document.line_count();
        self.document_start_line = self
            .document_start_line
            .min(max_document_start_line(line_count));
    }

    fn update_active_tab_dirty(&mut self) {
        let Some(path) = self.active_path.as_ref() else {
            return;
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.path == path) {
            tab.dirty = self.document.is_dirty();
        }
    }

    fn load_document(&mut self, path: Option<PathBuf>, text: String, status_message: String) {
        self.load_document_buffer(path, TextBuffer::new_saved(&text), status_message);
    }

    fn load_document_buffer(
        &mut self,
        path: Option<PathBuf>,
        buffer: TextBuffer,
        status_message: String,
    ) {
        self.active_path = path;
        self.document = buffer;
        self.marked_text_range = None;
        self.document_start_line = 0;
        self.refresh_document_metadata();
        self.status_message = status_message;
    }

    fn index_for_tree(&self, tree: &WorkspaceTree) -> VaultIndex {
        let workspace = self.workspace.clone();
        VaultIndex::from_tree_with_reader(&self.root, tree, |path| workspace.read_file(path).ok())
    }

    fn rebuild_vault_index(&mut self) {
        if let Some(tree) = self.tree.as_ref() {
            self.vault_index = self.index_for_tree(tree);
        } else {
            self.vault_index = VaultIndex::empty();
        }
    }

    fn next_note_path_in(&self, directory: &Path) -> PathBuf {
        for index in 1.. {
            let file_name = if index == 1 {
                String::from("untitled.md")
            } else {
                format!("untitled-{index}.md")
            };
            let path = directory.join(file_name);
            if !path.exists() {
                return path;
            }
        }

        unreachable!("unbounded note-name search should always return")
    }

    fn note_creation_dir(&self) -> PathBuf {
        if let Some(selected) = self.selected_path.as_ref() {
            if selected.is_dir() {
                return selected.clone();
            }

            if let Some(parent) = selected.parent() {
                return parent.to_path_buf();
            }
        }

        if let Some(active) = self.active_path.as_ref()
            && let Some(parent) = active.parent()
        {
            return parent.to_path_buf();
        }

        self.root.clone()
    }

    #[must_use]
    pub fn selected_note_path(&self) -> Option<PathBuf> {
        match self.selected_path.as_ref() {
            Some(path) if is_markdown_path(path) => Some(path.clone()),
            Some(_) => None,
            None => self.active_path.clone(),
        }
    }

    #[must_use]
    pub fn searchable_notes(&self, query: &str, limit: usize) -> Vec<NoteSearchResult> {
        self.vault_index
            .search_notes(query, limit)
            .into_iter()
            .map(|note| NoteSearchResult {
                path: note.path,
                label: note.label,
                parent_label: note.parent_label,
            })
            .collect()
    }

    pub fn open_note_path(&mut self, path: PathBuf) {
        if !is_markdown_path(&path) {
            self.status_message = String::from("Select a markdown note");
            return;
        }

        if !path.starts_with(&self.root) {
            self.status_message = String::from("Note must stay inside vault");
            return;
        }

        if !path.is_file() {
            self.status_message = format!("Missing {}", self.display_path(&path));
            self.refresh_files();
            return;
        }

        self.expand_ancestors(&path);
        self.rebuild_visible_files();
        self.open_path(path);
    }

    pub fn open_or_create_wikilink(&mut self, link: &str) {
        if let Some(path) = self.vault_index.resolve_wikilink_path(link) {
            self.open_note_path(path);
            return;
        }

        let Some(path) =
            vault_index::new_wikilink_path(&self.root, self.active_path.as_deref(), link)
        else {
            self.status_message = String::from("Invalid link");
            return;
        };

        if !path.starts_with(&self.root) {
            self.status_message = String::from("Invalid link");
            return;
        }

        let title = vault_index::wikilink_title(link).unwrap_or_else(|| String::from("Untitled"));
        let text = format!("# {title}\n\n");
        match self.workspace.write_file(&path, &text) {
            Ok(()) => {
                self.refresh_files();
                self.open_note_path(path);
            }
            Err(error) => {
                self.status_message = error.to_string();
            }
        }
    }

    pub fn open_wikilink_at_line_column(&mut self, line: usize, column: usize) -> bool {
        let Some(line_text) = self.document.line_text(line) else {
            return false;
        };
        let Some(link) = vault_index::wikilink_at_column(&line_text, column) else {
            return false;
        };

        self.open_or_create_wikilink(&link);
        true
    }

    fn open_tab_dirty(&self, path: &Path) -> bool {
        self.tabs.iter().any(|tab| tab.path == path && tab.dirty)
    }

    fn update_open_paths_after_rename(&mut self, source: &Path, target: &Path) {
        let label = self.display_path(target);
        for tab in &mut self.tabs {
            if tab.path == source {
                tab.path = target.to_path_buf();
                tab.label = label.clone();
            }
        }

        if self.active_path.as_deref() == Some(source) {
            self.active_path = Some(target.to_path_buf());
        }

        if self.selected_path.as_deref() == Some(source) {
            self.selected_path = Some(target.to_path_buf());
        }
    }

    fn close_missing_active_path(&mut self, path: PathBuf) {
        let label = self.display_path(&path);
        self.tabs.retain(|tab| tab.path != path);
        self.selected_path = None;

        if let Some(last_index) = self.tabs.len().checked_sub(1) {
            self.open_tab_buffer(last_index);
            self.status_message = format!("Closed missing {label}");
        } else {
            self.load_document(None, String::new(), format!("Closed missing {label}"));
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }

    fn toggle_folder(&mut self, path: PathBuf) {
        let label = self.display_path(&path);
        let expanded = if self.expanded_dirs.remove(&path) {
            false
        } else {
            self.expanded_dirs.insert(path);
            true
        };

        self.rebuild_visible_files();
        self.status_message = if expanded {
            format!("Expanded {label}")
        } else {
            format!("Collapsed {label}")
        };
    }

    fn expand_ancestors(&mut self, path: &Path) {
        let mut ancestor = path.parent();
        while let Some(directory) = ancestor {
            if !directory.starts_with(&self.root) {
                break;
            }

            self.expanded_dirs.insert(directory.to_path_buf());
            if directory == self.root {
                break;
            }

            ancestor = directory.parent();
        }
    }

    fn rebuild_visible_files(&mut self) {
        self.files = self
            .tree
            .as_ref()
            .map(|tree| visible_markdown_tree(tree, &self.expanded_dirs))
            .unwrap_or_default();
    }
}

fn visible_markdown_tree(tree: &WorkspaceTree, expanded_dirs: &HashSet<PathBuf>) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    let mut next_id = 0;

    for entry in &tree.entries {
        collect_visible_markdown_entries(entry, 0, &mut next_id, expanded_dirs, &mut entries);
    }

    entries
}

fn collect_visible_markdown_entries(
    entry: &WorkspaceEntry,
    depth: usize,
    next_id: &mut usize,
    expanded_dirs: &HashSet<PathBuf>,
    entries: &mut Vec<FileEntry>,
) {
    if !entry_has_markdown(entry) {
        return;
    }

    match entry.kind {
        WorkspaceEntryKind::Directory => {
            let has_children = entry.children.iter().any(entry_has_markdown);
            let expanded = expanded_dirs.contains(&entry.path);
            entries.push(FileEntry {
                id: *next_id,
                path: entry.path.clone(),
                label: entry.name.clone(),
                depth,
                kind: FileKind::Folder,
                expanded,
                has_children,
            });
            *next_id += 1;

            if expanded {
                for child in &entry.children {
                    collect_visible_markdown_entries(
                        child,
                        depth + 1,
                        next_id,
                        expanded_dirs,
                        entries,
                    );
                }
            }
        }
        WorkspaceEntryKind::File => {
            if is_markdown_path(&entry.path) {
                entries.push(FileEntry {
                    id: *next_id,
                    path: entry.path.clone(),
                    label: entry.name.clone(),
                    depth,
                    kind: FileKind::Markdown,
                    expanded: false,
                    has_children: false,
                });
                *next_id += 1;
            }
        }
    }
}

fn seed_expanded_dirs(tree: &WorkspaceTree, expanded_dirs: &mut HashSet<PathBuf>) {
    if !expanded_dirs.is_empty() {
        return;
    }

    for entry in &tree.entries {
        if matches!(entry.kind, WorkspaceEntryKind::Directory) && entry_has_markdown(entry) {
            expanded_dirs.insert(entry.path.clone());
        }
    }
}

fn first_markdown_path(tree: &WorkspaceTree) -> Option<PathBuf> {
    tree.entries.iter().find_map(first_markdown_entry_path)
}

fn first_markdown_entry_path(entry: &WorkspaceEntry) -> Option<PathBuf> {
    match entry.kind {
        WorkspaceEntryKind::File if is_markdown_path(&entry.path) => Some(entry.path.clone()),
        WorkspaceEntryKind::Directory => entry.children.iter().find_map(first_markdown_entry_path),
        WorkspaceEntryKind::File => None,
    }
}

fn max_document_start_line(line_count: usize) -> usize {
    line_count.saturating_sub(1)
}

fn entry_has_markdown(entry: &WorkspaceEntry) -> bool {
    match entry.kind {
        WorkspaceEntryKind::File => is_markdown_path(&entry.path),
        WorkspaceEntryKind::Directory => entry.children.iter().any(entry_has_markdown),
    }
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown"))
        .unwrap_or(false)
}

fn path_matches_active(path: &Path, active_path: &Path) -> bool {
    path == active_path || active_path.starts_with(path) || path.starts_with(active_path)
}

fn initial_workspace_root() -> PathBuf {
    configured_workspace_root(
        std::env::args_os().skip(1),
        std::env::var_os("OCEAN_GUI_WORKSPACE"),
    )
    .or_else(read_last_workspace)
    .unwrap_or_else(|| {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        default_workspace_root(current_dir, std::env::var_os("HOME"))
    })
}

fn configured_workspace_root<I>(args: I, env_root: Option<OsString>) -> Option<PathBuf>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == OsStr::new("--workspace") {
            if let Some(path) = non_empty_path(args.next()) {
                return Some(path);
            }
            continue;
        }

        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--workspace="))
            && !value.is_empty()
        {
            return Some(PathBuf::from(value));
        }
    }

    non_empty_path(env_root)
}

fn default_workspace_root(current_dir: PathBuf, home_root: Option<OsString>) -> PathBuf {
    if current_dir == Path::new("/")
        && let Some(home_root) = non_empty_path(home_root)
    {
        return home_root;
    }

    current_dir
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.as_os_str().is_empty())
        .map(PathBuf::from)
}

fn normalize_markdown_target(mut path: PathBuf) -> PathBuf {
    if !is_markdown_path(&path) {
        path.set_extension("md");
    }
    path
}

fn read_last_workspace() -> Option<PathBuf> {
    last_workspace_path()
        .and_then(|path| read_last_workspace_from(&path))
        .filter(|path| path.is_dir())
}

fn write_last_workspace(root: &Path) -> io::Result<()> {
    if let Some(config_path) = last_workspace_path() {
        write_last_workspace_to(&config_path, root)?;
    }
    Ok(())
}

fn last_workspace_path() -> Option<PathBuf> {
    non_empty_path(std::env::var_os("HOME"))
        .map(|home| home.join(APP_SUPPORT_DIR).join(LAST_WORKSPACE_FILE))
}

fn read_last_workspace_from(path: &Path) -> Option<PathBuf> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn write_last_workspace_to(path: &Path, root: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", root.display()))
}

fn utf16_range_in_text_to_char_range(text: &str, range: Range<usize>) -> Range<usize> {
    utf16_offset_in_text_to_char_offset(text, range.start)
        ..utf16_offset_in_text_to_char_offset(text, range.end)
}

fn utf16_offset_in_text_to_char_offset(text: &str, offset: usize) -> usize {
    let mut remaining = offset;
    for (char_offset, character) in text.chars().enumerate() {
        let len = character.len_utf16();
        if remaining < len {
            return char_offset;
        }
        remaining -= len;
        if remaining == 0 {
            return char_offset + 1;
        }
    }

    text.chars().count()
}

fn extract_outline(lines: &[String]) -> Vec<OutlineItem> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            let level = trimmed.chars().take_while(|char| *char == '#').count();
            if level == 0 || level > 6 {
                return None;
            }

            let heading = trimmed[level..].trim();
            if heading.is_empty() {
                return None;
            }

            Some(OutlineItem {
                label: heading.to_string(),
                level: level as u8,
                line_number: index + 1,
            })
        })
        .collect()
}

fn extract_links(text: &str) -> Vec<String> {
    vault_index::extract_wikilinks(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceStats;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn derives_outline_from_markdown_headings() {
        let lines = vec![
            String::from("# Title"),
            String::from("body"),
            String::from("## Child"),
        ];

        let outline = extract_outline(&lines);

        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].label, "Title");
        assert_eq!(outline[0].line_number, 1);
        assert_eq!(outline[1].level, 2);
    }

    #[test]
    fn extracts_unique_wikilinks() {
        let links = extract_links("[[Alpha]] [[Beta]] [[Alpha]]");

        assert_eq!(links, vec![String::from("Alpha"), String::from("Beta")]);
    }

    #[test]
    fn editing_marks_active_tab_dirty_and_updates_metadata() {
        let mut state = test_state();
        let path = state.root.join("alpha.md");
        state.tabs.push(EditorTab::clean(
            path.clone(),
            String::from("alpha.md"),
            String::from("# Alpha\n"),
        ));
        state.load_document(
            Some(path.clone()),
            String::from("# Alpha\n"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_end();
        state.insert_text("body");

        assert_eq!(state.document_text(), "# Alpha\nbody");
        assert_eq!(state.cursor_position(), EditorCursor { line: 1, column: 4 });
        assert_eq!(state.status.words, 3);
        assert!(state.tabs.iter().any(|tab| tab.path == path && tab.dirty));
    }

    #[test]
    fn switching_tabs_preserves_unsaved_buffer() {
        let mut state = test_state();
        let alpha = state.root.join("alpha.md");
        let beta = state.root.join("beta.md");
        state.tabs.push(EditorTab::clean(
            alpha.clone(),
            String::from("alpha.md"),
            String::from("alpha"),
        ));
        state.tabs.push(EditorTab::clean(
            beta,
            String::from("beta.md"),
            String::from("beta"),
        ));
        state.load_document(
            Some(alpha.clone()),
            String::from("alpha"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_end();
        state.insert_text(" draft");
        state.set_active_tab(1);
        assert_eq!(state.document_text(), "beta");

        state.set_active_tab(0);
        assert_eq!(state.document_text(), "alpha draft");
        assert!(state.tabs.iter().any(|tab| tab.path == alpha && tab.dirty));
    }

    #[test]
    fn cursor_placement_uses_line_and_character_column() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("one\nécho\nthree"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_line_column(1, 1);
        assert_eq!(state.cursor_position(), EditorCursor { line: 1, column: 1 });
        state.insert_text("x");
        assert_eq!(state.document_text(), "one\néxcho\nthree");

        state.move_cursor_to_line_column(99, 99);
        assert_eq!(state.cursor_position(), EditorCursor { line: 2, column: 5 });
    }

    #[test]
    fn selection_replaces_text_and_clears_after_edit() {
        let mut state = test_state();
        let path = state.root.join("alpha.md");
        state.tabs.push(EditorTab::clean(
            path.clone(),
            String::from("alpha.md"),
            String::from("alpha beta gamma"),
        ));
        state.load_document(
            Some(path.clone()),
            String::from("alpha beta gamma"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_line_column(0, 6);
        for _ in 0..4 {
            state.extend_cursor_right();
        }

        assert_eq!(state.selected_text(), Some(String::from("beta")));
        state.insert_text("BETA");

        assert_eq!(state.document_text(), "alpha BETA gamma");
        assert_eq!(state.selected_text(), None);
        assert_eq!(
            state.cursor_position(),
            EditorCursor {
                line: 0,
                column: 10
            }
        );
        assert!(state.tabs.iter().any(|tab| tab.path == path && tab.dirty));
    }

    #[test]
    fn select_all_and_cut_remove_selected_text() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("alpha\nbeta"),
            String::from("Opened alpha.md"),
        );

        state.select_all();

        assert_eq!(state.selected_text(), Some(String::from("alpha\nbeta")));
        assert_eq!(
            state.take_selected_text(),
            Some(String::from("alpha\nbeta"))
        );
        assert_eq!(state.document_text(), "");
        assert_eq!(state.cursor_position(), EditorCursor { line: 0, column: 0 });
        assert_eq!(state.selected_text(), None);
    }

    #[test]
    fn undo_redo_restores_text_cursor_selection_and_dirty_state() {
        let mut state = test_state();
        let path = state.root.join("alpha.md");
        state.tabs.push(EditorTab::clean(
            path.clone(),
            String::from("alpha.md"),
            String::from("alpha"),
        ));
        state.load_document(
            Some(path.clone()),
            String::from("alpha"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_end();
        state.insert_text(" beta");

        assert_eq!(state.document_text(), "alpha beta");
        assert!(state.tabs.iter().any(|tab| tab.path == path && tab.dirty));

        state.undo();

        assert_eq!(state.document_text(), "alpha");
        assert_eq!(state.cursor_position(), EditorCursor { line: 0, column: 5 });
        assert_eq!(state.selected_text(), None);
        assert!(state.tabs.iter().any(|tab| tab.path == path && !tab.dirty));

        state.redo();

        assert_eq!(state.document_text(), "alpha beta");
        assert_eq!(
            state.cursor_position(),
            EditorCursor {
                line: 0,
                column: 10
            }
        );
        assert!(state.tabs.iter().any(|tab| tab.path == path && tab.dirty));

        state.save_active();
        assert!(state.tabs.iter().any(|tab| tab.path == path && !tab.dirty));

        state.undo();

        assert_eq!(state.document_text(), "alpha");
        assert!(state.tabs.iter().any(|tab| tab.path == path && tab.dirty));
    }

    #[test]
    fn undo_history_stays_with_each_tab() {
        let mut state = test_state();
        let alpha = state.root.join("alpha.md");
        let beta = state.root.join("beta.md");
        state.tabs.push(EditorTab::clean(
            alpha.clone(),
            String::from("alpha.md"),
            String::from("alpha"),
        ));
        state.tabs.push(EditorTab::clean(
            beta.clone(),
            String::from("beta.md"),
            String::from("beta"),
        ));
        state.load_document(
            Some(alpha.clone()),
            String::from("alpha"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_end();
        state.insert_text(" one");
        state.set_active_tab(1);
        state.move_cursor_to_end();
        state.insert_text(" two");

        state.undo();
        assert_eq!(state.document_text(), "beta");

        state.set_active_tab(0);
        state.undo();
        assert_eq!(state.document_text(), "alpha");
    }

    #[test]
    fn large_documents_keep_typing_refresh_to_rendered_view() {
        let mut state = test_state();
        let path = state.root.join("large.md");
        let large_note = format!(
            "# Heading\n{}\n[[Backlink]]",
            "body ".repeat((LIVE_METADATA_CHAR_LIMIT / 5) + 1)
        );
        state.load_document(Some(path), large_note, String::from("Opened large.md"));

        state.move_cursor_to_end();
        state.insert_text(" tail");

        assert_eq!(state.outline.len(), 0);
        assert_eq!(state.links.len(), 0);
        assert_eq!(state.backlinks.len(), 0);
        assert_eq!(state.status.words, 0);
        assert!(state.status.lines >= 2);
        assert!(state.status.rendered_lines <= MAX_RENDERED_LINES);
    }

    #[test]
    fn cursor_movement_scrolls_small_render_window() {
        let mut state = test_state();
        let path = state.root.join("long.md");
        let text = (0..200)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.load_document(Some(path), text, String::from("Opened long.md"));

        state.move_cursor_to_line_column(150, 0);

        assert!(state.document_start_line > 0);
        assert!(state.document_start_line <= 150);
        assert!(150 < state.document_start_line + state.document_lines.len());
        assert!(state.document_lines.len() <= MAX_RENDERED_LINES);
    }

    #[test]
    fn outline_jump_moves_cursor_and_scrolls_heading_into_view() {
        let mut state = test_state();
        let path = state.root.join("long.md");
        let mut lines = (0..180)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        lines[0] = String::from("# Top");
        lines[140] = String::from("## Target");
        state.load_document(Some(path), lines.join("\n"), String::from("Opened long.md"));

        assert!(state.jump_to_outline_item(1));

        assert_eq!(
            state.cursor_position(),
            EditorCursor {
                line: 140,
                column: 0
            }
        );
        assert!(state.document_start_line <= 140);
        assert!(140 < state.document_start_line + state.document_lines.len());
        assert!(state.status_message.contains("Target"));
    }

    #[test]
    fn viewport_scrolls_without_moving_cursor() {
        let mut state = test_state();
        let path = state.root.join("long.md");
        let text = (0..200)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.load_document(Some(path), text, String::from("Opened long.md"));
        let cursor = state.cursor_position();

        assert!(state.scroll_document_by_lines(12));
        assert_eq!(state.document_start_line, 12);
        assert_eq!(state.cursor_position(), cursor);

        assert!(state.scroll_document_by_lines(-5));
        assert_eq!(state.document_start_line, 7);

        assert!(state.scroll_document_by_lines(10_000));
        assert_eq!(state.document_start_line, 199);

        assert!(state.scroll_document_by_lines(-10_000));
        assert_eq!(state.document_start_line, 0);
    }

    #[test]
    fn document_start_line_can_anchor_the_last_document_line() {
        let mut state = test_state();
        let path = state.root.join("long.md");
        let text = (0..200)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.load_document(Some(path), text, String::from("Opened long.md"));

        assert!(state.set_document_start_line(10_000));

        assert_eq!(state.document_start_line, 199);
        assert_eq!(state.document_lines, vec![String::from("line 199")]);
    }

    #[test]
    fn drag_selection_extends_to_line_column() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("alpha\nbeta"),
            String::from("Opened alpha.md"),
        );

        state.move_cursor_to_line_column(0, 1);
        state.extend_cursor_to_line_column(1, 3);

        assert_eq!(state.selected_text(), Some(String::from("lpha\nbet")));
    }

    #[test]
    fn word_selection_selects_word_without_marking_document_dirty() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("alpha beta"),
            String::from("Opened alpha.md"),
        );

        assert!(state.select_word_at_line_column(0, 7));

        assert_eq!(state.selected_text(), Some(String::from("beta")));
        assert_eq!(
            state.cursor_position(),
            EditorCursor {
                line: 0,
                column: 10
            }
        );
        assert!(!state.document.is_dirty());
    }

    #[test]
    fn gpui_utf16_input_replaces_and_marks_text() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("a🙂b"),
            String::from("Opened alpha.md"),
        );

        let mut adjusted_range = None;
        assert_eq!(
            state.text_for_utf16_range(1..3, &mut adjusted_range),
            String::from("🙂")
        );
        assert_eq!(adjusted_range, Some(1..3));

        assert!(state.replace_text_in_utf16_range(Some(1..3), "x"));
        assert_eq!(state.document_text(), "axb");

        assert!(state.replace_and_mark_text_in_utf16_range(None, "🙂", Some(0..2)));
        assert_eq!(state.document_text(), "ax🙂b");
        assert_eq!(state.marked_utf16_range(), Some(2..4));
        assert_eq!(state.selected_utf16_range(), (2..4, false));

        state.unmark_text();
        assert_eq!(state.marked_utf16_range(), None);
    }

    #[test]
    fn utf16_range_reports_editor_cursors() {
        let mut state = test_state();
        state.load_document(
            Some(state.root.join("alpha.md")),
            String::from("a🙂b\nsecond"),
            String::from("Opened alpha.md"),
        );

        assert_eq!(
            state.editor_cursors_for_utf16_range(1..3),
            (
                EditorCursor { line: 0, column: 1 },
                EditorCursor { line: 0, column: 2 },
            )
        );
        assert_eq!(
            state.editor_cursors_for_utf16_range(5..11),
            (
                EditorCursor { line: 1, column: 0 },
                EditorCursor { line: 1, column: 6 },
            )
        );
    }

    #[test]
    fn file_tree_hides_children_until_folder_expands() {
        let root = PathBuf::from("/vault");
        let docs_path = root.join("docs");
        let nested_path = docs_path.join("nested");
        let tree = WorkspaceTree {
            root,
            entries: vec![WorkspaceEntry {
                path: docs_path.clone(),
                name: String::from("docs"),
                kind: WorkspaceEntryKind::Directory,
                children: vec![
                    WorkspaceEntry {
                        path: docs_path.join("alpha.md"),
                        name: String::from("alpha.md"),
                        kind: WorkspaceEntryKind::File,
                        children: Vec::new(),
                    },
                    WorkspaceEntry {
                        path: nested_path.clone(),
                        name: String::from("nested"),
                        kind: WorkspaceEntryKind::Directory,
                        children: vec![WorkspaceEntry {
                            path: nested_path.join("beta.md"),
                            name: String::from("beta.md"),
                            kind: WorkspaceEntryKind::File,
                            children: Vec::new(),
                        }],
                    },
                ],
            }],
            max_depth: MAX_SCAN_DEPTH,
            stats: WorkspaceStats::default(),
        };
        let mut expanded_dirs = HashSet::new();

        let collapsed = visible_markdown_tree(&tree, &expanded_dirs);
        assert_eq!(labels(&collapsed), vec![String::from("docs")]);
        assert!(!collapsed[0].expanded);
        assert!(collapsed[0].has_children);

        expanded_dirs.insert(docs_path);
        let expanded = visible_markdown_tree(&tree, &expanded_dirs);
        assert_eq!(
            labels(&expanded),
            vec![
                String::from("docs"),
                String::from("alpha.md"),
                String::from("nested"),
            ]
        );
        assert!(expanded[0].expanded);
        assert_eq!(expanded[1].kind, FileKind::Markdown);
        assert_eq!(expanded[2].kind, FileKind::Folder);
        assert!(!expanded[2].expanded);
    }

    #[test]
    fn create_note_uses_selected_folder_context() {
        let root = temp_vault();
        fs::create_dir_all(root.join("notes")).expect("notes dir");
        fs::write(root.join("notes/existing.md"), "# Existing").expect("seed note");
        let mut state = ShellState::seed_with_root(root.clone());
        state.selected_path = Some(root.join("notes"));

        state.create_note();

        let created = root.join("notes/untitled.md");
        assert!(created.exists());
        assert_eq!(state.active_path, Some(created));
    }

    #[test]
    fn rename_selected_note_updates_active_tab_and_tree() {
        let root = temp_vault();
        fs::write(root.join("draft.md"), "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root.clone());
        let old_path = root.join("draft.md");
        let new_path = root.join("archive/final.md");

        state.rename_selected_to(new_path.clone());

        assert!(!old_path.exists());
        assert!(new_path.exists());
        assert_eq!(state.active_path, Some(new_path.clone()));
        assert!(state.tabs.iter().any(|tab| tab.path == new_path));
        assert!(
            state
                .files
                .iter()
                .any(|entry| entry.path == root.join("archive/final.md"))
        );
    }

    #[test]
    fn rename_selected_note_keeps_target_inside_vault() {
        let root = temp_vault();
        let outside = temp_vault().join("outside.md");
        fs::write(root.join("draft.md"), "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root.clone());

        state.rename_selected_to(outside.clone());

        assert!(root.join("draft.md").exists());
        assert!(!outside.exists());
        assert!(state.status_message.contains("inside vault"));
    }

    #[test]
    fn delete_selected_note_removes_clean_open_tab() {
        let root = temp_vault();
        fs::write(root.join("draft.md"), "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root.clone());

        state.delete_selected_note();

        assert!(!root.join("draft.md").exists());
        assert!(state.tabs.is_empty());
        assert_eq!(state.active_path, None);
    }

    #[test]
    fn delete_selected_note_blocks_dirty_open_tab() {
        let root = temp_vault();
        fs::write(root.join("draft.md"), "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root.clone());
        state.move_cursor_to_end();
        state.insert_text("\nunsaved");

        state.delete_selected_note();

        assert!(root.join("draft.md").exists());
        assert_eq!(state.active_path, Some(root.join("draft.md")));
        assert!(state.status_message.contains("Save or close"));
    }

    #[test]
    fn last_workspace_config_round_trips_plain_path() {
        let config_path = temp_vault().join("last-workspace");
        let root = temp_vault();

        write_last_workspace_to(&config_path, &root).expect("write last workspace");

        assert_eq!(read_last_workspace_from(&config_path), Some(root));
    }

    #[test]
    fn external_change_reloads_clean_active_note() {
        let root = temp_vault();
        let path = root.join("draft.md");
        fs::write(&path, "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root);

        fs::write(&path, "# Changed").expect("external write");
        state.apply_external_vault_change(&[path.clone()]);

        assert_eq!(state.document_text(), "# Changed");
        assert!(state.status_message.contains("Reloaded"));
    }

    #[test]
    fn external_change_does_not_clobber_dirty_active_note() {
        let root = temp_vault();
        let path = root.join("draft.md");
        fs::write(&path, "# Draft").expect("seed note");
        let mut state = ShellState::seed_with_root(root);
        state.move_cursor_to_end();
        state.insert_text("\nlocal");

        fs::write(&path, "# Changed").expect("external write");
        state.apply_external_vault_change(&[path.clone()]);

        assert_eq!(state.document_text(), "# Draft\nlocal");
        assert!(state.status_message.contains("External change"));
    }

    #[test]
    fn searchable_notes_match_nested_markdown_by_subsequence() {
        let root = temp_vault();
        fs::create_dir_all(root.join("archive/projects")).expect("nested dir");
        fs::write(root.join("alpha.md"), "# Alpha").expect("alpha note");
        fs::write(root.join("archive/projects/gamma-index.md"), "# Gamma").expect("gamma note");
        fs::write(root.join("archive/projects/gamma.txt"), "ignore").expect("text file");
        let state = ShellState::seed_with_root(root.clone());

        let results = state.searchable_notes("gm", 8);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].path,
            root.join("archive/projects/gamma-index.md")
        );
        assert_eq!(results[0].parent_label, "archive/projects");
    }

    #[test]
    fn active_note_metadata_includes_incoming_backlinks() {
        let root = temp_vault();
        fs::write(root.join("alpha.md"), "# Alpha").expect("alpha note");
        fs::write(root.join("daily.md"), "# Daily\n\n[[Alpha]]").expect("daily note");

        let state = ShellState::seed_with_root(root.clone());

        assert_eq!(state.active_path, Some(root.join("alpha.md")));
        assert_eq!(state.links, Vec::<String>::new());
        assert_eq!(state.backlinks.len(), 1);
        assert_eq!(state.backlinks[0].path, root.join("daily.md"));
        assert_eq!(state.backlinks[0].line_number, 3);
        assert_eq!(state.backlinks[0].snippet, "[[Alpha]]");
        assert_eq!(state.status.links, 0);
        assert_eq!(state.status.backlinks, 1);
    }

    #[test]
    fn saving_note_refreshes_backlink_index() {
        let root = temp_vault();
        let alpha = root.join("alpha.md");
        let beta = root.join("beta.md");
        fs::write(&alpha, "# Alpha").expect("alpha note");
        fs::write(&beta, "# Beta").expect("beta note");
        let mut state = ShellState::seed_with_root(root);

        state.open_note_path(beta.clone());
        state.move_cursor_to_end();
        state.insert_text("\n\n[[Alpha]]");
        state.save_active();
        state.open_note_path(alpha);

        assert_eq!(state.backlinks.len(), 1);
        assert_eq!(state.backlinks[0].path, beta);
    }

    #[test]
    fn open_note_path_opens_and_reveals_nested_note() {
        let root = temp_vault();
        let nested_dir = root.join("notes/nested");
        let path = nested_dir.join("beta.md");
        fs::create_dir_all(&nested_dir).expect("nested dir");
        fs::write(&path, "# Beta").expect("beta note");
        let mut state = ShellState::seed_with_root(root);

        state.open_note_path(path.clone());

        assert_eq!(state.active_path, Some(path.clone()));
        assert_eq!(state.selected_path, Some(path.clone()));
        assert!(state.expanded_dirs.contains(&nested_dir));
        assert_eq!(state.document_text(), "# Beta");
    }

    #[test]
    fn open_or_create_wikilink_opens_existing_note() {
        let root = temp_vault();
        fs::write(root.join("alpha.md"), "# Alpha\n\n[[Beta]]").expect("alpha note");
        fs::write(root.join("beta.md"), "# Beta").expect("beta note");
        let mut state = ShellState::seed_with_root(root.clone());

        state.open_or_create_wikilink("Beta");

        assert_eq!(state.active_path, Some(root.join("beta.md")));
        assert_eq!(state.document_text(), "# Beta");
    }

    #[test]
    fn open_or_create_wikilink_creates_missing_note_next_to_active_note() {
        let root = temp_vault();
        fs::create_dir_all(root.join("daily")).expect("daily dir");
        fs::write(root.join("daily/today.md"), "# Today\n\n[[Follow Up]]").expect("today note");
        let created = root.join("daily/Follow Up.md");
        let mut state = ShellState::seed_with_root(root.clone());
        state.open_note_path(root.join("daily/today.md"));

        state.open_or_create_wikilink("Follow Up");

        assert_eq!(state.active_path, Some(created.clone()));
        assert_eq!(
            fs::read_to_string(created).expect("created note"),
            "# Follow Up\n\n"
        );
    }

    #[test]
    fn open_wikilink_at_line_column_opens_existing_note() {
        let root = temp_vault();
        fs::write(root.join("alpha.md"), "# Alpha").expect("alpha note");
        fs::write(root.join("daily.md"), "See [[Alpha|today]].").expect("daily note");
        let mut state = ShellState::seed_with_root(root.clone());
        state.open_note_path(root.join("daily.md"));

        assert!(state.open_wikilink_at_line_column(0, "See [[".chars().count()));

        assert_eq!(state.active_path, Some(root.join("alpha.md")));
    }

    #[test]
    fn open_wikilink_at_line_column_ignores_non_link_text() {
        let root = temp_vault();
        fs::write(root.join("daily.md"), "See [[Alpha]].").expect("daily note");
        let mut state = ShellState::seed_with_root(root.clone());
        state.open_note_path(root.join("daily.md"));

        assert!(!state.open_wikilink_at_line_column(0, 0));

        assert_eq!(state.active_path, Some(root.join("daily.md")));
    }

    #[test]
    fn open_or_create_wikilink_rejects_parent_traversal() {
        let root = temp_vault();
        fs::write(root.join("alpha.md"), "# Alpha").expect("alpha note");
        let mut state = ShellState::seed_with_root(root.clone());

        state.open_or_create_wikilink("../escape");

        assert_eq!(state.active_path, Some(root.join("alpha.md")));
        assert!(state.status_message.contains("Invalid link"));
    }

    #[test]
    fn configured_workspace_prefers_cli_workspace() {
        let root = configured_workspace_root(
            vec![OsString::from("--workspace"), OsString::from("/vault/cli")],
            Some(OsString::from("/vault/env")),
        );

        assert_eq!(root, Some(PathBuf::from("/vault/cli")));
    }

    #[test]
    fn configured_workspace_accepts_equals_form() {
        let root = configured_workspace_root(
            vec![OsString::from("--workspace=/vault/inline")],
            Some(OsString::from("/vault/env")),
        );

        assert_eq!(root, Some(PathBuf::from("/vault/inline")));
    }

    #[test]
    fn configured_workspace_uses_env_without_cli_workspace() {
        let root = configured_workspace_root(
            vec![OsString::from("--ignored")],
            Some(OsString::from("/vault/env")),
        );

        assert_eq!(root, Some(PathBuf::from("/vault/env")));
    }

    #[test]
    fn app_launch_default_uses_home_when_cwd_is_root() {
        let root =
            default_workspace_root(PathBuf::from("/"), Some(OsString::from("/Users/ocean_gui")));

        assert_eq!(root, PathBuf::from("/Users/ocean_gui"));
    }

    fn labels(entries: &[FileEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.label.clone()).collect()
    }

    fn temp_vault() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ocean_gui-model-test-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temp vault");
        root
    }

    fn test_state() -> ShellState {
        let root = std::env::temp_dir();
        let workspace = Workspace::new(&root).expect("temp dir should be a readable workspace");

        ShellState {
            workspace,
            tree: None,
            vault_index: VaultIndex::empty(),
            expanded_dirs: HashSet::new(),
            root,
            active_path: None,
            selected_path: None,
            files: Vec::new(),
            tabs: Vec::new(),
            document: TextBuffer::new_saved(""),
            marked_text_range: None,
            document_start_line: 0,
            document_lines: Vec::new(),
            outline: Vec::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
            status: DocumentStatus::default(),
            status_message: String::from("Ready"),
        }
    }
}
