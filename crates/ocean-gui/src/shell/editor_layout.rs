use std::ops::Range;

use gpui::{Bounds, FontWeight, LineFragment, Pixels, Point, Window, font, point, px, size};

use super::editor_buffer::EditorCursor;
use super::theme;

pub(crate) const EDITOR_TEXT_TOP_PADDING_PX: f32 = 24.0;
pub(crate) const EDITOR_TEXT_GUTTER_WIDTH_PX: f32 = 44.0;
pub(crate) const EDITOR_TEXT_RIGHT_PADDING_PX: f32 = 18.0;
pub(crate) const EDITOR_LINE_HEIGHT_PX: f32 = 26.0;
pub(crate) const EDITOR_FALLBACK_WRAP_WIDTH_PX: f32 = 720.0;

#[derive(Clone, Debug)]
pub(crate) struct EditorRenderLine {
    pub(crate) document_line_index: usize,
    pub(crate) text: String,
    pub(crate) selected_columns: Option<Range<usize>>,
    pub(crate) style: EditorLineStyle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditorVisualLine {
    pub(crate) document_line_index: usize,
    pub(crate) source_columns: Range<usize>,
    pub(crate) text: String,
    pub(crate) selected_columns: Option<Range<usize>>,
    pub(crate) is_line_end: bool,
    pub(crate) style: EditorLineStyle,
}

impl EditorVisualLine {
    pub(crate) fn contains_cursor(&self, cursor: EditorCursor) -> bool {
        self.document_line_index == cursor.line && self.contains_source_column(cursor.column)
    }

    pub(crate) fn contains_source_column(&self, column: usize) -> bool {
        (self.source_columns.start <= column && column < self.source_columns.end)
            || (self.is_line_end && column == self.source_columns.end)
    }

    pub(crate) fn relative_column_for_source_column(&self, column: usize) -> usize {
        column
            .saturating_sub(self.source_columns.start)
            .min(self.source_columns.end - self.source_columns.start)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EditorVisualLayout {
    pub(crate) lines: Vec<EditorVisualLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditorVisualAnchor {
    pub(crate) document_line_index: usize,
    pub(crate) local_visual_row: usize,
}

impl EditorVisualLayout {
    pub(crate) fn from_render_lines(
        render_lines: &[EditorRenderLine],
        wrap_width: Pixels,
        window: &Window,
    ) -> Self {
        Self::from_render_lines_with_boundaries(render_lines, |line| {
            measured_wrap_boundaries(line, wrap_width, window)
        })
    }

    fn from_render_lines_with_boundaries(
        render_lines: &[EditorRenderLine],
        mut boundary_provider: impl FnMut(&EditorRenderLine) -> Vec<usize>,
    ) -> Self {
        let mut lines = Vec::new();

        for line in render_lines {
            if line.text.is_empty() {
                lines.push(EditorVisualLine {
                    document_line_index: line.document_line_index,
                    source_columns: 0..0,
                    text: String::new(),
                    selected_columns: None,
                    is_line_end: true,
                    style: line.style,
                });
                continue;
            }

            let mut segment_start_byte = 0;
            let mut boundaries = boundary_provider(line);
            boundaries.push(line.text.len());

            for end_byte in boundaries {
                let start_byte =
                    floor_char_boundary(&line.text, segment_start_byte.min(line.text.len()));
                let end_byte = if end_byte >= line.text.len() {
                    line.text.len()
                } else {
                    snap_wrap_boundary_to_word_start(
                        &line.text,
                        segment_start_byte,
                        floor_char_boundary(&line.text, end_byte.min(line.text.len())),
                    )
                };
                if end_byte <= start_byte {
                    continue;
                }

                let source_columns = char_column_for_byte_index(&line.text, start_byte)
                    ..char_column_for_byte_index(&line.text, end_byte);
                let text = line.text[start_byte..end_byte].to_string();
                let selected_columns =
                    selected_columns_for_visual_segment(line, source_columns.clone());

                lines.push(EditorVisualLine {
                    document_line_index: line.document_line_index,
                    source_columns,
                    text,
                    selected_columns,
                    is_line_end: end_byte == line.text.len(),
                    style: line.style,
                });
                segment_start_byte = end_byte;
            }
        }

        Self { lines }
    }

    pub(crate) fn visual_line_at_row(&self, row: usize) -> Option<&EditorVisualLine> {
        self.lines.get(row)
    }

    pub(crate) fn visual_line_for_cursor(
        &self,
        cursor: EditorCursor,
    ) -> Option<(usize, &EditorVisualLine)> {
        self.lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains_cursor(cursor))
    }

    pub(crate) fn cursor_after_visual_delta(
        &self,
        cursor: EditorCursor,
        row_delta: isize,
    ) -> Option<EditorCursor> {
        let (source_row, source_line) = self.visual_line_for_cursor(cursor)?;
        let target_row = apply_row_delta(source_row, row_delta)?;
        let target_line = self.lines.get(target_row)?;
        let visual_column = source_line.relative_column_for_source_column(cursor.column);
        let target_column = target_line.source_columns.start
            + visual_column.min(target_line.source_columns.end - target_line.source_columns.start);

        Some(EditorCursor {
            line: target_line.document_line_index,
            column: target_column,
        })
    }

    pub(crate) fn visual_row_start_for_cursor(&self, cursor: EditorCursor) -> Option<EditorCursor> {
        let (_, line) = self.visual_line_for_cursor(cursor)?;

        Some(EditorCursor {
            line: line.document_line_index,
            column: line.source_columns.start,
        })
    }

    pub(crate) fn visual_row_end_for_cursor(&self, cursor: EditorCursor) -> Option<EditorCursor> {
        let (_, line) = self.visual_line_for_cursor(cursor)?;

        Some(EditorCursor {
            line: line.document_line_index,
            column: line.source_columns.end,
        })
    }

    pub(crate) fn visible_lines_from(
        &self,
        start_row: usize,
        capacity: usize,
    ) -> &[EditorVisualLine] {
        let start = start_row.min(self.lines.len());
        let end = start.saturating_add(capacity).min(self.lines.len());
        &self.lines[start..end]
    }

    pub(crate) fn max_scroll_row(&self, visible_capacity: usize) -> usize {
        self.lines.len().saturating_sub(visible_capacity.max(1))
    }

    pub(crate) fn clamp_scroll_row(&self, scroll_row: usize, visible_capacity: usize) -> usize {
        scroll_row.min(self.max_scroll_row(visible_capacity))
    }

    pub(crate) fn anchor_for_visual_row(&self, row: usize) -> Option<EditorVisualAnchor> {
        let line = self.lines.get(row)?;
        let local_visual_row = self
            .lines
            .iter()
            .take(row)
            .rev()
            .take_while(|candidate| candidate.document_line_index == line.document_line_index)
            .count();

        Some(EditorVisualAnchor {
            document_line_index: line.document_line_index,
            local_visual_row,
        })
    }

    pub(crate) fn last_visual_row_for_document_line(
        &self,
        document_line_index: usize,
    ) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.document_line_index == document_line_index)
            .map(|(row, _)| row)
    }

    pub(crate) fn scroll_row_to_reveal_row(
        &self,
        target_row: usize,
        current_scroll_row: usize,
        visible_capacity: usize,
        margin: usize,
    ) -> usize {
        if self.lines.is_empty() {
            return 0;
        }

        let visible_capacity = visible_capacity.max(1);
        let current_scroll_row = self.clamp_scroll_row(current_scroll_row, visible_capacity);
        let target_row = target_row.min(self.lines.len().saturating_sub(1));
        let visible_end = current_scroll_row.saturating_add(visible_capacity);

        let next_scroll_row = if target_row < current_scroll_row.saturating_add(margin) {
            target_row.saturating_sub(margin)
        } else if target_row.saturating_add(margin) >= visible_end {
            target_row
                .saturating_add(margin)
                .saturating_add(1)
                .saturating_sub(visible_capacity)
        } else {
            current_scroll_row
        };

        self.clamp_scroll_row(next_scroll_row, visible_capacity)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EditorLineStyle {
    Body,
    Heading,
}

impl EditorLineStyle {
    pub(crate) fn for_text(text: &str) -> Self {
        if text.starts_with('#') {
            Self::Heading
        } else {
            Self::Body
        }
    }

    pub(crate) fn font_family(self) -> &'static str {
        match self {
            Self::Body => theme::MONO_FONT,
            Self::Heading => theme::SERIF_FONT,
        }
    }

    pub(crate) fn font_size(self) -> Pixels {
        match self {
            Self::Body => px(14.0),
            Self::Heading => px(20.0),
        }
    }

    pub(crate) fn font_weight(self) -> FontWeight {
        match self {
            Self::Body => FontWeight::NORMAL,
            Self::Heading => FontWeight::BOLD,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorViewport {
    pub(crate) surface_bounds: Bounds<Pixels>,
    pub(crate) text_bounds: Bounds<Pixels>,
    pub(crate) gutter_x: Pixels,
}

impl EditorViewport {
    pub(crate) fn from_surface(surface_bounds: Bounds<Pixels>) -> Self {
        let text_origin = point(
            surface_bounds.left() + px(EDITOR_TEXT_TOP_PADDING_PX + EDITOR_TEXT_GUTTER_WIDTH_PX),
            surface_bounds.top() + px(EDITOR_TEXT_TOP_PADDING_PX),
        );
        let text_width =
            (surface_bounds.right() - text_origin.x - px(EDITOR_TEXT_RIGHT_PADDING_PX))
                .max(px(0.0));
        let text_height =
            (surface_bounds.bottom() - text_origin.y - px(EDITOR_TEXT_TOP_PADDING_PX)).max(px(0.0));

        Self {
            surface_bounds,
            text_bounds: Bounds::new(text_origin, size(text_width, text_height)),
            gutter_x: surface_bounds.left() + px(16.0),
        }
    }

    pub(crate) fn text_origin(&self) -> Point<Pixels> {
        self.text_bounds.origin
    }

    pub(crate) fn line_y(&self, visible_row: usize) -> Pixels {
        self.text_origin().y + px(visible_row as f32 * EDITOR_LINE_HEIGHT_PX)
    }

    pub(crate) fn row_for_point(&self, point: Point<Pixels>) -> usize {
        let y = (point.y - self.text_origin().y).max(px(0.0));
        let row = (y / px(EDITOR_LINE_HEIGHT_PX)).floor() as usize;
        row.min(self.visible_row_capacity().saturating_sub(1))
    }

    pub(crate) fn x_in_text(&self, point: Point<Pixels>) -> Pixels {
        (point.x - self.text_origin().x).max(px(0.0))
    }

    pub(crate) fn clamp_to_text(&self, position: Point<Pixels>) -> Point<Pixels> {
        point(
            self.clamp_text_x(position.x),
            position
                .y
                .max(self.text_bounds.top())
                .min(self.text_bounds.bottom()),
        )
    }

    pub(crate) fn clamp_text_x(&self, x: Pixels) -> Pixels {
        x.max(self.text_bounds.left()).min(self.text_bounds.right())
    }

    pub(crate) fn wrap_width(&self) -> Pixels {
        (self.text_bounds.right() - self.text_bounds.left()).max(px(1.0))
    }

    pub(crate) fn visible_row_capacity(&self) -> usize {
        let text_height = self.text_bounds.bottom() - self.text_bounds.top();
        ((text_height / px(EDITOR_LINE_HEIGHT_PX)).ceil() as usize).max(1)
    }
}

pub(crate) fn byte_offset_for_char_column(text: &str, column: usize) -> usize {
    if column == 0 {
        return 0;
    }

    text.char_indices()
        .nth(column)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

pub(crate) fn char_column_for_byte_index(text: &str, byte_index: usize) -> usize {
    let byte_index = floor_char_boundary(text, byte_index.min(text.len()));
    text[..byte_index].chars().count()
}

fn measured_wrap_boundaries(
    line: &EditorRenderLine,
    wrap_width: Pixels,
    window: &Window,
) -> Vec<usize> {
    let mut wrapper_font = font(line.style.font_family().to_string());
    wrapper_font.weight = line.style.font_weight();

    let mut line_wrapper = window
        .text_system()
        .line_wrapper(wrapper_font, line.style.font_size());
    let fragments = [LineFragment::text(line.text.as_str())];
    let mut boundaries = Vec::new();
    let mut last_boundary = 0;

    for boundary in line_wrapper.wrap_line(&fragments, wrap_width) {
        let boundary = floor_char_boundary(&line.text, boundary.ix.min(line.text.len()));
        if boundary > last_boundary && boundary < line.text.len() {
            boundaries.push(boundary);
            last_boundary = boundary;
        }
    }

    boundaries
}

fn selected_columns_for_visual_segment(
    line: &EditorRenderLine,
    source_columns: Range<usize>,
) -> Option<Range<usize>> {
    line.selected_columns
        .as_ref()
        .and_then(|selection| intersect_ranges(selection.clone(), source_columns.clone()))
        .map(|selection| {
            selection.start - source_columns.start..selection.end - source_columns.start
        })
}

fn snap_wrap_boundary_to_word_start(
    text: &str,
    segment_start_byte: usize,
    boundary_byte: usize,
) -> usize {
    let segment_start_byte = floor_char_boundary(text, segment_start_byte.min(text.len()));
    let boundary_byte = floor_char_boundary(text, boundary_byte.min(text.len()));

    if boundary_byte <= segment_start_byte || !splits_editor_word_at(text, boundary_byte) {
        return boundary_byte;
    }

    let mut candidate = boundary_byte;
    while candidate > segment_start_byte {
        let Some((previous_index, previous_character)) =
            text[..candidate].char_indices().next_back()
        else {
            break;
        };
        if !is_editor_word_character(previous_character) {
            break;
        }
        candidate = previous_index;
    }

    if candidate > segment_start_byte {
        candidate
    } else {
        boundary_byte
    }
}

fn splits_editor_word_at(text: &str, boundary_byte: usize) -> bool {
    if boundary_byte == 0 || boundary_byte >= text.len() {
        return false;
    }

    let previous = text[..boundary_byte].chars().next_back();
    let next = text[boundary_byte..].chars().next();

    previous.is_some_and(is_editor_word_character) && next.is_some_and(is_editor_word_character)
}

fn is_editor_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '\u{00C0}'..='\u{024F}')
        || matches!(character, '\u{0400}'..='\u{04FF}')
        || matches!(
            character,
            '-' | '_' | '.' | '\'' | '$' | '%' | '@' | '#' | '^' | '~' | ',' | '=' | ':' | '⋯'
        )
}

fn intersect_ranges(left: Range<usize>, right: Range<usize>) -> Option<Range<usize>> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);

    (start < end).then_some(start..end)
}

fn apply_row_delta(row: usize, delta: isize) -> Option<usize> {
    if delta.is_negative() {
        row.checked_sub(delta.unsigned_abs())
    } else {
        Some(row.saturating_add(delta as usize))
    }
}

fn floor_char_boundary(text: &str, mut byte_index: usize) -> usize {
    while !text.is_char_boundary(byte_index) {
        byte_index -= 1;
    }

    byte_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_columns_convert_through_utf8_boundaries() {
        let text = "a🙂b";

        assert_eq!(byte_offset_for_char_column(text, 0), 0);
        assert_eq!(byte_offset_for_char_column(text, 1), 1);
        assert_eq!(byte_offset_for_char_column(text, 2), 5);
        assert_eq!(byte_offset_for_char_column(text, 99), text.len());
        assert_eq!(char_column_for_byte_index(text, 0), 0);
        assert_eq!(char_column_for_byte_index(text, 1), 1);
        assert_eq!(char_column_for_byte_index(text, 3), 1);
        assert_eq!(char_column_for_byte_index(text, 5), 2);
        assert_eq!(char_column_for_byte_index(text, text.len()), 3);
    }

    #[test]
    fn editor_viewport_owns_text_and_gutter_geometry() {
        let surface = Bounds::new(point(px(10.0), px(20.0)), size(px(500.0), px(300.0)));
        let viewport = EditorViewport::from_surface(surface);

        assert_eq!(viewport.surface_bounds, surface);
        assert_eq!(viewport.gutter_x, px(26.0));
        assert_eq!(viewport.text_origin(), point(px(78.0), px(44.0)));
        assert_eq!(viewport.text_bounds.right(), px(492.0));
        assert_eq!(viewport.text_bounds.bottom(), px(296.0));
    }

    #[test]
    fn editor_viewport_clamps_hit_testing_to_text_surface() {
        let surface = Bounds::new(point(px(0.0), px(0.0)), size(px(300.0), px(200.0)));
        let viewport = EditorViewport::from_surface(surface);
        let clamped = viewport.clamp_to_text(point(px(500.0), px(-50.0)));

        assert_eq!(clamped.x, viewport.text_bounds.right());
        assert_eq!(clamped.y, viewport.text_bounds.top());
        assert_eq!(viewport.row_for_point(viewport.text_origin()), 0);
        assert_eq!(
            viewport.row_for_point(point(
                viewport.text_origin().x,
                viewport.text_origin().y + px(52.0)
            )),
            2
        );
    }

    #[test]
    fn editor_viewport_reports_visible_visual_row_capacity() {
        let surface = Bounds::new(point(px(0.0), px(0.0)), size(px(400.0), px(200.0)));
        let viewport = EditorViewport::from_surface(surface);

        assert_eq!(viewport.visible_row_capacity(), 6);
    }

    #[test]
    fn editor_viewport_clamps_bottom_edge_to_last_visible_row() {
        let surface = Bounds::new(point(px(0.0), px(0.0)), size(px(400.0), px(204.0)));
        let viewport = EditorViewport::from_surface(surface);

        assert_eq!(viewport.visible_row_capacity(), 6);
        assert_eq!(
            viewport.row_for_point(point(
                viewport.text_origin().x,
                viewport.text_bounds.bottom()
            )),
            5
        );
    }

    #[test]
    fn visual_layout_wraps_long_lines_by_source_columns() {
        let layout = test_layout(&[test_render_line(8, "abcdefghi", None)], vec![vec![4, 8]]);

        assert_eq!(layout.lines.len(), 3);
        assert_eq!(layout.lines[0].document_line_index, 8);
        assert_eq!(layout.lines[0].source_columns, 0..4);
        assert_eq!(layout.lines[0].text, "abcd");
        assert_eq!(layout.lines[1].source_columns, 4..8);
        assert_eq!(layout.lines[1].text, "efgh");
        assert_eq!(layout.lines[2].source_columns, 8..9);
        assert_eq!(layout.lines[2].text, "i");
        assert!(layout.lines[2].is_line_end);
    }

    #[test]
    fn visual_layout_maps_selection_into_segment_columns() {
        let layout = test_layout(
            &[test_render_line(3, "abcdefgh", Some(2..7))],
            vec![vec![4]],
        );

        assert_eq!(layout.lines[0].selected_columns, Some(2..4));
        assert_eq!(layout.lines[1].selected_columns, Some(0..3));
    }

    #[test]
    fn visual_layout_preserves_words_when_wrap_boundary_is_before_word() {
        let layout = test_layout(
            &[test_render_line(0, "Ocean GUI terminal workflows", None)],
            vec![vec!["Ocean GUI ".len()]],
        );

        assert_eq!(layout.lines[0].text, "Ocean GUI ");
        assert_eq!(layout.lines[1].text, "terminal workflows");
        assert!(!layout.lines.iter().any(|line| line.text == "termi"));
    }

    #[test]
    fn visual_layout_snaps_mid_word_boundary_to_word_start() {
        let layout = test_layout(
            &[test_render_line(0, "Ocean GUI terminal workflows", None)],
            vec![vec!["Ocean GUI termi".len()]],
        );

        assert_eq!(layout.lines[0].text, "Ocean GUI ");
        assert_eq!(layout.lines[1].text, "terminal workflows");
    }

    #[test]
    fn visual_layout_finds_cursor_row_at_wrap_boundaries() {
        let layout = test_layout(&[test_render_line(1, "abcdefgh", None)], vec![vec![4]]);

        let first = layout.visual_line_for_cursor(EditorCursor { line: 1, column: 3 });
        let boundary = layout.visual_line_for_cursor(EditorCursor { line: 1, column: 4 });
        let end = layout.visual_line_for_cursor(EditorCursor { line: 1, column: 8 });

        assert_eq!(first.map(|(row, _)| row), Some(0));
        assert_eq!(boundary.map(|(row, _)| row), Some(1));
        assert_eq!(end.map(|(row, _)| row), Some(1));
    }

    #[test]
    fn visual_layout_moves_cursor_inside_wrapped_physical_line() {
        let layout = test_layout(&[test_render_line(1, "abcdefgh", None)], vec![vec![4]]);

        assert_eq!(
            layout.cursor_after_visual_delta(EditorCursor { line: 1, column: 1 }, 1),
            Some(EditorCursor { line: 1, column: 5 })
        );
        assert_eq!(
            layout.cursor_after_visual_delta(EditorCursor { line: 1, column: 4 }, -1),
            Some(EditorCursor { line: 1, column: 0 })
        );
    }

    #[test]
    fn visual_layout_clamps_cursor_when_target_visual_row_is_shorter() {
        let layout = test_layout(
            &[
                test_render_line(0, "abcdefgh", None),
                test_render_line(1, "xy", None),
            ],
            vec![vec![4], vec![]],
        );

        assert_eq!(
            layout.cursor_after_visual_delta(EditorCursor { line: 0, column: 7 }, 1),
            Some(EditorCursor { line: 1, column: 2 })
        );
    }

    #[test]
    fn visual_layout_reports_current_visual_row_start_and_end() {
        let layout = test_layout(
            &[test_render_line(1, "abcdefghijkl", None)],
            vec![vec![4, 8]],
        );

        assert_eq!(
            layout.visual_row_start_for_cursor(EditorCursor { line: 1, column: 6 }),
            Some(EditorCursor { line: 1, column: 4 })
        );
        assert_eq!(
            layout.visual_row_end_for_cursor(EditorCursor { line: 1, column: 6 }),
            Some(EditorCursor { line: 1, column: 8 })
        );
        assert_eq!(
            layout.visual_row_start_for_cursor(EditorCursor {
                line: 1,
                column: 12
            }),
            Some(EditorCursor { line: 1, column: 8 })
        );
        assert_eq!(
            layout.visual_row_end_for_cursor(EditorCursor {
                line: 1,
                column: 12
            }),
            Some(EditorCursor {
                line: 1,
                column: 12
            })
        );
    }

    #[test]
    fn visual_layout_slices_visible_lines_from_scroll_offset() {
        let layout = test_layout(
            &[
                test_render_line(0, "zero", None),
                test_render_line(1, "one", None),
                test_render_line(2, "two", None),
                test_render_line(3, "three", None),
            ],
            vec![vec![], vec![], vec![], vec![]],
        );

        let visible = layout.visible_lines_from(1, 2);

        assert_eq!(
            visible
                .iter()
                .map(|line| line.document_line_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(layout.visible_lines_from(99, 2).is_empty());
    }

    #[test]
    fn visual_layout_clamps_scroll_row_to_full_viewport_window() {
        let layout = test_layout(
            &[
                test_render_line(0, "zero", None),
                test_render_line(1, "one", None),
                test_render_line(2, "two", None),
                test_render_line(3, "three", None),
                test_render_line(4, "four", None),
            ],
            vec![vec![], vec![], vec![], vec![], vec![]],
        );

        assert_eq!(layout.max_scroll_row(2), 3);
        assert_eq!(layout.clamp_scroll_row(99, 2), 3);
        assert_eq!(layout.clamp_scroll_row(4, 20), 0);
    }

    #[test]
    fn visual_layout_maps_visual_rows_back_to_document_anchors() {
        let layout = test_layout(
            &[
                test_render_line(7, "abcdefghijkl", None),
                test_render_line(8, "next", None),
            ],
            vec![vec![4, 8], vec![]],
        );

        assert_eq!(
            layout.anchor_for_visual_row(1),
            Some(EditorVisualAnchor {
                document_line_index: 7,
                local_visual_row: 1
            })
        );
        assert_eq!(
            layout.anchor_for_visual_row(3),
            Some(EditorVisualAnchor {
                document_line_index: 8,
                local_visual_row: 0
            })
        );
        assert_eq!(layout.anchor_for_visual_row(99), None);
    }

    #[test]
    fn visual_layout_computes_scroll_row_to_reveal_cursor_row() {
        let layout = test_layout(
            &[
                test_render_line(0, "zero", None),
                test_render_line(1, "one", None),
                test_render_line(2, "two", None),
                test_render_line(3, "three", None),
                test_render_line(4, "four", None),
                test_render_line(5, "five", None),
                test_render_line(6, "six", None),
                test_render_line(7, "seven", None),
            ],
            vec![
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
            ],
        );

        assert_eq!(layout.scroll_row_to_reveal_row(6, 0, 4, 1), 4);
        assert_eq!(layout.scroll_row_to_reveal_row(0, 4, 4, 1), 0);
        assert_eq!(layout.scroll_row_to_reveal_row(3, 1, 4, 1), 1);
    }

    fn test_render_line(
        document_line_index: usize,
        text: &str,
        selected_columns: Option<Range<usize>>,
    ) -> EditorRenderLine {
        EditorRenderLine {
            document_line_index,
            text: text.to_string(),
            selected_columns,
            style: EditorLineStyle::for_text(text),
        }
    }

    fn test_layout(
        render_lines: &[EditorRenderLine],
        boundaries_by_line: Vec<Vec<usize>>,
    ) -> EditorVisualLayout {
        let mut boundaries_by_line = boundaries_by_line.into_iter();
        EditorVisualLayout::from_render_lines_with_boundaries(render_lines, |_| {
            boundaries_by_line.next().unwrap_or_default()
        })
    }
}
