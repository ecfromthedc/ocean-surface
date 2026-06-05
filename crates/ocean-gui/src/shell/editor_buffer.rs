use std::ops::Range;

use ropey::Rope;

pub const UNDO_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditorCursor {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CursorState {
    cursor: usize,
    selection_anchor: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditDelta {
    range: Range<usize>,
    inserted: String,
    deleted: String,
    before: CursorState,
    after: CursorState,
    before_marker: u64,
    after_marker: u64,
}

#[derive(Clone, Debug)]
pub struct TextBuffer {
    rope: Rope,
    cursor: usize,
    selection_anchor: Option<usize>,
    undo_stack: Vec<EditDelta>,
    redo_stack: Vec<EditDelta>,
    state_marker: u64,
    saved_marker: u64,
    next_marker: u64,
}

impl TextBuffer {
    #[must_use]
    pub fn new_saved(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            cursor: 0,
            selection_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            state_marker: 0,
            saved_marker: 0,
            next_marker: 1,
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.state_marker != self.saved_marker
    }

    pub fn mark_saved(&mut self) {
        self.saved_marker = self.state_marker;
    }

    pub fn mark_dirty(&mut self) {
        if self.is_dirty() {
            return;
        }

        self.state_marker = self.next_marker;
        self.next_marker += 1;
    }

    #[must_use]
    pub fn cursor_char_offset(&self) -> usize {
        self.cursor.min(self.len_chars())
    }

    #[must_use]
    pub fn cursor_utf16_offset(&self) -> usize {
        self.char_to_utf16_offset(self.cursor_char_offset())
    }

    #[must_use]
    pub fn utf16_selection_range(&self) -> (Range<usize>, bool) {
        let Some(range) = self.selection_range() else {
            let cursor = self.cursor_utf16_offset();
            return (cursor..cursor, false);
        };
        let reversed = self
            .selection_anchor
            .map(|anchor| anchor > self.cursor_char_offset())
            .unwrap_or(false);

        (self.char_range_to_utf16(&range), reversed)
    }

    #[must_use]
    pub fn char_range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.char_to_utf16_offset(range.start)..self.char_to_utf16_offset(range.end)
    }

    #[must_use]
    pub fn utf16_range_to_char(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_to_char_offset(range.start)..self.utf16_to_char_offset(range.end)
    }

    #[must_use]
    pub fn text_for_utf16_range(
        &self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
    ) -> String {
        let char_range = self.utf16_range_to_char(&range);
        adjusted_range.replace(self.char_range_to_utf16(&char_range));
        self.rope.slice(char_range).to_string()
    }

    pub fn replace_range(&mut self, range: Range<usize>, text: &str) -> Option<Range<usize>> {
        let start = range.start.min(self.len_chars());
        let inserted_len = text.chars().count();
        self.apply_edit(range, text)
            .then_some(start..start + inserted_len)
    }

    pub fn set_selection_range(&mut self, range: Range<usize>, reversed: bool) {
        let start = range.start.min(self.len_chars());
        let end = range.end.min(self.len_chars());
        let range = start.min(end)..start.max(end);

        if range.start == range.end {
            self.cursor = range.start;
            self.selection_anchor = None;
        } else if reversed {
            self.cursor = range.start;
            self.selection_anchor = Some(range.end);
        } else {
            self.cursor = range.end;
            self.selection_anchor = Some(range.start);
        }
    }

    #[must_use]
    pub fn line_column_for_char_offset(&self, offset: usize) -> EditorCursor {
        let offset = offset.min(self.len_chars());
        let line = self.rope.char_to_line(offset);
        let line_start = self.rope.line_to_char(line);

        EditorCursor {
            line,
            column: offset - line_start,
        }
    }

    pub fn insert_text(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }

        if let Some(range) = self.selection_range() {
            self.apply_edit(range, text)
        } else {
            self.apply_edit(self.cursor..self.cursor, text)
        }
    }

    pub fn delete_backward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }

        if self.cursor == 0 {
            return false;
        }

        self.apply_edit(self.cursor - 1..self.cursor, "")
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }

        if self.cursor >= self.len_chars() {
            return false;
        }

        self.apply_edit(self.cursor..self.cursor + 1, "")
    }

    pub fn undo(&mut self) -> bool {
        let Some(delta) = self.undo_stack.pop() else {
            return false;
        };

        let inserted_end = delta.range.start + delta.inserted.chars().count();
        self.rope.remove(delta.range.start..inserted_end);
        self.rope.insert(delta.range.start, &delta.deleted);
        self.cursor = delta.before.cursor.min(self.len_chars());
        self.selection_anchor = delta
            .before
            .selection_anchor
            .map(|offset| offset.min(self.len_chars()));
        self.state_marker = delta.before_marker;
        self.redo_stack.push(delta);
        self.normalize_selection();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(delta) = self.redo_stack.pop() else {
            return false;
        };

        let deleted_end = delta.range.start + delta.deleted.chars().count();
        self.rope.remove(delta.range.start..deleted_end);
        self.rope.insert(delta.range.start, &delta.inserted);
        self.cursor = delta.after.cursor.min(self.len_chars());
        self.selection_anchor = delta
            .after
            .selection_anchor
            .map(|offset| offset.min(self.len_chars()));
        self.state_marker = delta.after_marker;
        self.undo_stack.push(delta);
        self.normalize_selection();
        true
    }

    pub fn move_left(&mut self) {
        let offset = self
            .selection_range()
            .map(|range| range.start)
            .unwrap_or_else(|| self.cursor.saturating_sub(1));
        self.move_to_offset(offset, false);
    }

    pub fn move_right(&mut self) {
        let offset = self
            .selection_range()
            .map(|range| range.end)
            .unwrap_or_else(|| (self.cursor + 1).min(self.len_chars()));
        self.move_to_offset(offset, false);
    }

    pub fn move_up(&mut self, extend_selection: bool) {
        self.move_vertical(-1, extend_selection);
    }

    pub fn move_down(&mut self, extend_selection: bool) {
        self.move_vertical(1, extend_selection);
    }

    pub fn move_to_start(&mut self) {
        self.move_to_offset(0, false);
    }

    pub fn move_to_end(&mut self) {
        self.move_to_offset(self.len_chars(), false);
    }

    pub fn move_to_line_column(&mut self, line: usize, column: usize) {
        self.move_to_offset(self.offset_for_line_column(line, column), false);
    }

    pub fn extend_to_line_column(&mut self, line: usize, column: usize) {
        self.move_to_offset(self.offset_for_line_column(line, column), true);
    }

    #[must_use]
    pub fn utf16_offset_for_line_column(&self, line: usize, column: usize) -> usize {
        self.char_to_utf16_offset(self.offset_for_line_column(line, column))
    }

    pub fn extend_left(&mut self) {
        self.move_to_offset(self.cursor.saturating_sub(1), true);
    }

    pub fn extend_right(&mut self) {
        self.move_to_offset((self.cursor + 1).min(self.len_chars()), true);
    }

    pub fn extend_up(&mut self) {
        self.move_vertical(-1, true);
    }

    pub fn extend_down(&mut self) {
        self.move_vertical(1, true);
    }

    pub fn select_all(&mut self) {
        if self.is_empty() {
            self.cursor = 0;
            self.selection_anchor = None;
        } else {
            self.selection_anchor = Some(0);
            self.cursor = self.len_chars();
        }
    }

    pub fn select_word_at_line_column(&mut self, line: usize, column: usize) -> bool {
        let offset = self.offset_for_line_column(line, column);
        let Some(range) = self.word_range_at_offset(offset) else {
            self.move_to_offset(offset, false);
            return false;
        };

        self.set_selection_range(range, false);
        true
    }

    #[must_use]
    pub fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.selection_anchor?.min(self.len_chars());
        let cursor = self.cursor.min(self.len_chars());
        if anchor == cursor {
            return None;
        }

        Some(anchor.min(cursor)..anchor.max(cursor))
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        self.selection_range()
            .map(|range| self.rope.slice(range).to_string())
    }

    pub fn take_selected_text(&mut self) -> Option<String> {
        let range = self.selection_range()?;
        let selected = self.rope.slice(range.clone()).to_string();
        self.apply_edit(range, "");
        Some(selected)
    }

    #[must_use]
    pub fn selected_columns_for_line(&self, line: usize) -> Option<Range<usize>> {
        let selection = self.selection_range()?;
        let line_start = self.line_to_char(line)?;
        let line_end = line_start + self.line_len_without_break(line);
        let start = selection.start.max(line_start);
        let end = selection.end.min(line_end);

        if start >= end {
            return None;
        }

        Some(start - line_start..end - line_start)
    }

    #[must_use]
    pub fn cursor_position(&self) -> EditorCursor {
        self.line_column_for_char_offset(self.cursor_char_offset())
    }

    #[must_use]
    pub fn rendered_lines_from(&self, start_line: usize, limit: usize) -> Vec<String> {
        let start_line = start_line.min(self.line_count());
        let end_line = (start_line + limit).min(self.line_count());

        (start_line..end_line)
            .map(|line| self.line_without_break(line))
            .collect()
    }

    #[must_use]
    pub fn all_lines(&self) -> Vec<String> {
        (0..self.line_count())
            .map(|line| self.line_without_break(line))
            .collect()
    }

    #[must_use]
    pub fn line_text(&self, line: usize) -> Option<String> {
        (line < self.line_count()).then(|| self.line_without_break(line))
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    #[must_use]
    pub fn word_count(&self) -> usize {
        self.rope.chunks().flat_map(str::split_whitespace).count()
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };

        self.apply_edit(range, "")
    }

    fn apply_edit(&mut self, range: Range<usize>, inserted: &str) -> bool {
        let start = range.start.min(self.len_chars());
        let end = range.end.min(self.len_chars());
        if start == end && inserted.is_empty() {
            return false;
        }

        let range = start..end;
        let deleted = self.rope.slice(range.clone()).to_string();
        if deleted == inserted {
            return false;
        }

        let before = self.cursor_state();
        let before_marker = self.state_marker;
        self.rope.remove(range.clone());
        self.rope.insert(range.start, inserted);
        self.cursor = range.start + inserted.chars().count();
        self.selection_anchor = None;
        let after = self.cursor_state();
        let after_marker = self.next_marker;
        self.next_marker += 1;
        self.state_marker = after_marker;

        push_limited(
            &mut self.undo_stack,
            EditDelta {
                range,
                inserted: inserted.to_string(),
                deleted,
                before,
                after,
                before_marker,
                after_marker,
            },
        );
        self.redo_stack.clear();
        true
    }

    fn cursor_state(&self) -> CursorState {
        CursorState {
            cursor: self.cursor,
            selection_anchor: self.selection_anchor,
        }
    }

    fn move_vertical(&mut self, delta: isize, extend_selection: bool) {
        let cursor = self.cursor_position();
        let target_line = match delta {
            -1 if cursor.line > 0 => cursor.line - 1,
            1 if cursor.line + 1 < self.line_count() => cursor.line + 1,
            _ => return,
        };

        let offset = self.offset_for_line_column(target_line, cursor.column);
        self.move_to_offset(offset, extend_selection);
    }

    fn move_to_offset(&mut self, offset: usize, extend_selection: bool) {
        if extend_selection {
            self.selection_anchor.get_or_insert(self.cursor);
        } else {
            self.selection_anchor = None;
        }

        self.cursor = offset.min(self.len_chars());
        self.normalize_selection();
    }

    fn normalize_selection(&mut self) {
        if self.selection_anchor == Some(self.cursor) {
            self.selection_anchor = None;
        }
    }

    fn offset_for_line_column(&self, line: usize, column: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        line_start + column.min(self.line_len_without_break(line))
    }

    fn word_range_at_offset(&self, offset: usize) -> Option<Range<usize>> {
        let len = self.len_chars();
        if len == 0 {
            return None;
        }

        let offset = offset.min(len);
        let seed = if offset < len && is_editor_word_character(self.rope.char(offset)) {
            offset
        } else if offset == len
            && offset > 0
            && is_editor_word_character(self.rope.char(offset - 1))
        {
            offset - 1
        } else {
            return None;
        };

        let start = self.word_start_before(seed);
        let end = self.word_end_after(seed);
        (start < end).then_some(start..end)
    }

    fn word_start_before(&self, seed: usize) -> usize {
        let mut start = seed.min(self.len_chars());
        while start > 0 && is_editor_word_character(self.rope.char(start - 1)) {
            start -= 1;
        }
        start
    }

    fn word_end_after(&self, seed: usize) -> usize {
        let mut end = seed.min(self.len_chars()).saturating_add(1);
        while end < self.len_chars() && is_editor_word_character(self.rope.char(end)) {
            end += 1;
        }
        end
    }

    fn line_to_char(&self, line: usize) -> Option<usize> {
        (line < self.line_count()).then(|| self.rope.line_to_char(line))
    }

    fn line_len_without_break(&self, line: usize) -> usize {
        let line = self.rope.line(line);
        let mut len = line.len_chars();
        while len > 0 {
            let character = line.char(len - 1);
            if character == '\n' || character == '\r' {
                len -= 1;
            } else {
                break;
            }
        }
        len
    }

    fn line_without_break(&self, line_index: usize) -> String {
        let line = self.rope.line(line_index);
        let end = self.line_len_without_break(line_index);
        line.slice(0..end).to_string()
    }

    fn char_to_utf16_offset(&self, char_offset: usize) -> usize {
        let mut remaining_chars = char_offset.min(self.len_chars());
        let mut utf16_offset = 0;

        for chunk in self.rope.chunks() {
            let chunk_chars = chunk.chars().count();
            if remaining_chars >= chunk_chars {
                utf16_offset += chunk.chars().map(char::len_utf16).sum::<usize>();
                remaining_chars -= chunk_chars;
            } else {
                utf16_offset += chunk
                    .chars()
                    .take(remaining_chars)
                    .map(char::len_utf16)
                    .sum::<usize>();
                break;
            }
        }

        utf16_offset
    }

    fn utf16_to_char_offset(&self, utf16_offset: usize) -> usize {
        let mut remaining_utf16 = utf16_offset;
        let mut char_offset = 0;

        for chunk in self.rope.chunks() {
            for character in chunk.chars() {
                let character_len = character.len_utf16();
                if remaining_utf16 < character_len {
                    return char_offset;
                }

                remaining_utf16 -= character_len;
                char_offset += 1;

                if remaining_utf16 == 0 {
                    return char_offset;
                }
            }
        }

        self.len_chars()
    }
}

fn is_editor_word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '\'')
}

fn push_limited(stack: &mut Vec<EditDelta>, delta: EditDelta) {
    if stack.len() == UNDO_HISTORY_LIMIT {
        stack.remove(0);
    }
    stack.push(delta);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_large_text_without_full_snapshot_undo() {
        let mut buffer = TextBuffer::new_saved("alpha\nbeta\ngamma");
        buffer.move_to_line_column(1, 4);

        assert!(buffer.insert_text("!"));
        assert_eq!(buffer.text(), "alpha\nbeta!\ngamma");
        assert!(buffer.is_dirty());
        assert_eq!(
            buffer.cursor_position(),
            EditorCursor { line: 1, column: 5 }
        );

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "alpha\nbeta\ngamma");
        assert!(!buffer.is_dirty());

        assert!(buffer.redo());
        assert_eq!(buffer.text(), "alpha\nbeta!\ngamma");
        assert!(buffer.is_dirty());
    }

    #[test]
    fn selection_uses_character_offsets() {
        let mut buffer = TextBuffer::new_saved("écho beta");
        buffer.move_to_line_column(0, 0);
        for _ in 0..4 {
            buffer.extend_right();
        }

        assert_eq!(buffer.selected_text(), Some(String::from("écho")));
        assert_eq!(buffer.selected_columns_for_line(0), Some(0..4));
        assert!(buffer.insert_text("echo"));
        assert_eq!(buffer.text(), "echo beta");
    }

    #[test]
    fn renders_line_window_without_prefix_scan_allocation() {
        let buffer = TextBuffer::new_saved("zero\none\ntwo\nthree\nfour");

        assert_eq!(
            buffer.rendered_lines_from(2, 2),
            vec![String::from("two"), String::from("three")]
        );
    }

    #[test]
    fn line_text_returns_line_without_break() {
        let buffer = TextBuffer::new_saved("alpha\nbeta");

        assert_eq!(buffer.line_text(1), Some(String::from("beta")));
        assert_eq!(buffer.line_text(99), None);
    }

    #[test]
    fn utf16_ranges_round_trip_surrogate_pairs() {
        let mut buffer = TextBuffer::new_saved("a🙂b");

        assert_eq!(buffer.char_range_to_utf16(&(1..2)), 1..3);
        assert_eq!(buffer.utf16_range_to_char(&(1..3)), 1..2);

        let char_range = buffer.utf16_range_to_char(&(1..3));
        assert_eq!(buffer.replace_range(char_range, "x"), Some(1..2));
        assert_eq!(buffer.text(), "axb");
    }

    #[test]
    fn line_column_to_utf16_offset_uses_rope_without_probe_clone() {
        let buffer = TextBuffer::new_saved("a🙂b\nc🙂d");

        assert_eq!(buffer.utf16_offset_for_line_column(0, 2), 3);
        assert_eq!(buffer.utf16_offset_for_line_column(1, 2), 8);
        assert_eq!(buffer.utf16_offset_for_line_column(99, 99), 9);
    }

    #[test]
    fn extend_to_line_column_selects_across_lines() {
        let mut buffer = TextBuffer::new_saved("alpha\nbeta\ngamma");

        buffer.move_to_line_column(0, 2);
        buffer.extend_to_line_column(1, 2);

        assert_eq!(buffer.selected_text(), Some(String::from("pha\nbe")));
    }

    #[test]
    fn select_word_at_line_column_uses_unicode_character_offsets() {
        let mut buffer = TextBuffer::new_saved("alpha béta gamma");

        assert!(buffer.select_word_at_line_column(0, 7));

        assert_eq!(buffer.selected_text(), Some(String::from("béta")));
        assert_eq!(
            buffer.cursor_position(),
            EditorCursor {
                line: 0,
                column: 10
            }
        );
    }

    #[test]
    fn select_word_at_line_column_clears_selection_on_separator() {
        let mut buffer = TextBuffer::new_saved("alpha beta");
        assert!(buffer.select_word_at_line_column(0, 1));

        assert!(!buffer.select_word_at_line_column(0, 5));

        assert_eq!(buffer.selected_text(), None);
        assert_eq!(
            buffer.cursor_position(),
            EditorCursor { line: 0, column: 5 }
        );
    }
}
