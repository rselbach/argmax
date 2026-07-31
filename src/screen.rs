//! Bounded virtual-terminal observation for overlay placement.
//!
//! The observer consumes only child-terminal output. It retains a bounded cell
//! grid and the small set of terminal modes needed to decide whether an inline
//! overlay can be drawn without disturbing the wrapped shell.

use std::error::Error;
use std::fmt;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Params, Parser, Perform};

/// Maximum accepted terminal dimension in either direction.
pub const MAX_TERMINAL_DIMENSION: u16 = 4_096;
/// Maximum cells retained by one primary or alternate screen.
pub const MAX_SCREEN_CELLS: usize = 262_144;
/// Maximum bytes retained for one base glyph and its combining marks.
pub const MAX_CELL_TEXT_BYTES: usize = 64;
/// Maximum bytes accepted inside one OSC control string.
pub const MAX_CONTROL_STRING_BYTES: usize = 4_096;
/// Maximum incomplete UTF-8 prefix retained between observations.
pub const MAX_UTF8_CARRY_BYTES: usize = 3;

/// Validated terminal dimensions in display cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    columns: u16,
    rows: u16,
}

impl TerminalSize {
    /// Validates non-zero, bounded terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenError::InvalidSize`] when either dimension is zero, a
    /// dimension exceeds [`MAX_TERMINAL_DIMENSION`], or the retained grid would
    /// exceed [`MAX_SCREEN_CELLS`].
    pub fn new(columns: u16, rows: u16) -> Result<Self, ScreenError> {
        let cells = usize::from(columns).saturating_mul(usize::from(rows));
        if columns == 0
            || rows == 0
            || columns > MAX_TERMINAL_DIMENSION
            || rows > MAX_TERMINAL_DIMENSION
            || cells > MAX_SCREEN_CELLS
        {
            return Err(ScreenError::InvalidSize { columns, rows });
        }
        Ok(Self { columns, rows })
    }

    /// Terminal columns.
    #[must_use]
    pub const fn columns(self) -> u16 {
        self.columns
    }

    /// Terminal rows.
    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }

    const fn cell_count(self) -> usize {
        self.columns as usize * self.rows as usize
    }
}

/// Zero-based terminal cursor position.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CursorPosition {
    /// Zero-based row.
    pub row: u16,
    /// Zero-based display-cell column.
    pub column: u16,
}

impl CursorPosition {
    /// Constructs a cursor position without assuming terminal dimensions.
    #[must_use]
    pub const fn new(row: u16, column: u16) -> Self {
        Self { row, column }
    }
}

/// Active terminal screen buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenBuffer {
    /// Normal shell screen with scrollback.
    Primary,
    /// Alternate screen commonly used by full-screen applications.
    Alternate,
}

/// Inclusive zero-based scrolling region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollRegion {
    /// First row in the region.
    pub top: u16,
    /// Last row in the region.
    pub bottom: u16,
}

impl ScrollRegion {
    const fn full(size: TerminalSize) -> Self {
        Self {
            top: 0,
            bottom: size.rows - 1,
        }
    }

    /// Whether this region covers the entire terminal.
    #[must_use]
    pub const fn is_full(self, size: TerminalSize) -> bool {
        self.top == 0 && self.bottom == size.rows - 1
    }
}

/// Immutable placement and safety state used by the overlay renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// These are independent terminal and cell-grid facts, not alternate states.
#[allow(clippy::struct_excessive_bools)]
pub struct ScreenSnapshot {
    size: TerminalSize,
    cursor: CursorPosition,
    saved_cursor: CursorPosition,
    scroll_region: ScrollRegion,
    blank_cells_to_right: u16,
    rows_below_clear: bool,
    buffer: ScreenBuffer,
    wrapping: bool,
    wrap_pending: bool,
    cursor_visible: bool,
    origin_mode: bool,
    insert_mode: bool,
    synchronized: bool,
}

impl ScreenSnapshot {
    /// Current terminal dimensions.
    #[must_use]
    pub const fn size(self) -> TerminalSize {
        self.size
    }

    /// Current terminal cursor.
    #[must_use]
    pub const fn cursor(self) -> CursorPosition {
        self.cursor
    }

    /// Most recently saved terminal cursor.
    #[must_use]
    pub const fn saved_cursor(self) -> CursorPosition {
        self.saved_cursor
    }

    /// Current scrolling region.
    #[must_use]
    pub const fn scroll_region(self) -> ScrollRegion {
        self.scroll_region
    }

    /// Consecutive untouched cells from the cursor to the next occupied cell.
    #[must_use]
    pub const fn blank_cells_to_right(self) -> u16 {
        self.blank_cells_to_right
    }

    /// Whether every row geometrically below the cursor is untouched.
    #[must_use]
    pub const fn rows_below_clear(self) -> bool {
        self.rows_below_clear
    }

    /// Active primary or alternate buffer.
    #[must_use]
    pub const fn buffer(self) -> ScreenBuffer {
        self.buffer
    }

    /// Whether automatic line wrapping is enabled.
    #[must_use]
    pub const fn wrapping(self) -> bool {
        self.wrapping
    }

    /// Whether the next printable character would trigger delayed wrapping.
    #[must_use]
    pub const fn wrap_pending(self) -> bool {
        self.wrap_pending
    }

    /// Whether the terminal cursor is visible.
    #[must_use]
    pub const fn cursor_visible(self) -> bool {
        self.cursor_visible
    }

    /// Whether cursor rows are relative to the scrolling region.
    #[must_use]
    pub const fn origin_mode(self) -> bool {
        self.origin_mode
    }

    /// Whether printable characters insert rather than replace cells.
    #[must_use]
    pub const fn insert_mode(self) -> bool {
        self.insert_mode
    }

    /// Whether all observed output since the last reset is understood.
    #[must_use]
    pub const fn synchronized(self) -> bool {
        self.synchronized
    }

    /// Whether an inline shell overlay may be drawn at this snapshot.
    #[must_use]
    pub const fn overlay_safe(self) -> bool {
        self.synchronized
            && self.cursor_visible
            && matches!(self.buffer, ScreenBuffer::Primary)
            && self.scroll_region.is_full(self.size)
            && !self.origin_mode
            && !self.insert_mode
            && !self.wrap_pending
    }
}

/// Summary of one output or resize observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenObservation {
    /// Number of raw bytes consumed.
    pub consumed_bytes: usize,
    /// Any child output requires clearing a previously owned overlay first.
    pub clear_overlay: bool,
    /// Safety state after the observation.
    pub overlay_safe: bool,
}

/// Invalid screen operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenError {
    /// Terminal dimensions were zero or exceeded a hard bound.
    InvalidSize {
        /// Rejected columns.
        columns: u16,
        /// Rejected rows.
        rows: u16,
    },
    /// A synchronization cursor was outside the current dimensions.
    InvalidCursor(CursorPosition),
}

impl fmt::Display for ScreenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize { columns, rows } => {
                write!(formatter, "terminal size {columns}x{rows} is unsupported")
            }
            Self::InvalidCursor(cursor) => write!(
                formatter,
                "terminal cursor {},{} is outside the current screen",
                cursor.row, cursor.column
            ),
        }
    }
}

impl Error for ScreenError {}

#[derive(Clone, Default, Eq, PartialEq)]
struct Cell {
    text: String,
    width: u8,
    continuation: bool,
}

impl Cell {
    fn blank(&mut self) {
        self.text.clear();
        self.width = 0;
        self.continuation = false;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SavedCursor {
    position: CursorPosition,
    wrap_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GraphemePending {
    #[default]
    None,
    Boundary,
    Width,
}

fn scalar_tail_pending(character: char) -> bool {
    character == '\u{200D}'
        || matches!(character, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
        || matches!(character, '\u{1F3FB}'..='\u{1F3FF}')
}

fn grapheme_tail_pending(text: &str) -> GraphemePending {
    let Some(last) = text.chars().next_back() else {
        return GraphemePending::None;
    };
    if scalar_tail_pending(last) {
        return GraphemePending::Boundary;
    }
    if matches!(last, '\u{1F1E6}'..='\u{1F1FF}')
        && text
            .chars()
            .rev()
            .take_while(|character| matches!(character, '\u{1F1E6}'..='\u{1F1FF}'))
            .count()
            % 2
            == 1
    {
        return GraphemePending::Boundary;
    }
    GraphemePending::None
}

#[derive(Clone, Eq, PartialEq)]
struct Surface {
    size: TerminalSize,
    cells: Vec<Cell>,
    cursor: CursorPosition,
    saved: SavedCursor,
    scroll_region: ScrollRegion,
    wrap_pending: bool,
    grapheme_pending: GraphemePending,
}

impl fmt::Debug for Surface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Surface")
            .field("size", &self.size)
            .field("cursor", &self.cursor)
            .field("saved", &self.saved)
            .field("scroll_region", &self.scroll_region)
            .field("wrap_pending", &self.wrap_pending)
            .field("grapheme_pending", &self.grapheme_pending)
            .field("nonempty_cells", &self.nonempty_cells())
            .finish_non_exhaustive()
    }
}

impl Surface {
    fn new(size: TerminalSize) -> Self {
        Self {
            size,
            cells: vec![Cell::default(); size.cell_count()],
            cursor: CursorPosition::default(),
            saved: SavedCursor::default(),
            scroll_region: ScrollRegion::full(size),
            wrap_pending: false,
            grapheme_pending: GraphemePending::None,
        }
    }

    fn nonempty_cells(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| !cell.text.is_empty() || cell.continuation)
            .count()
    }

    fn index(&self, row: u16, column: u16) -> usize {
        usize::from(row) * usize::from(self.size.columns) + usize::from(column)
    }

    fn cell(&self, row: u16, column: u16) -> &Cell {
        &self.cells[self.index(row, column)]
    }

    fn cell_mut(&mut self, row: u16, column: u16) -> &mut Cell {
        let index = self.index(row, column);
        &mut self.cells[index]
    }

    fn clear_cell_and_wide_pair(&mut self, row: u16, column: u16) {
        let cell = self.cell(row, column).clone();
        if cell.continuation && column > 0 {
            self.cell_mut(row, column - 1).blank();
        } else if cell.width == 2 && column + 1 < self.size.columns {
            self.cell_mut(row, column + 1).blank();
        }
        self.cell_mut(row, column).blank();
    }

    fn clear_range(&mut self, row: u16, start: u16, end: u16) {
        let end = end.min(self.size.columns);
        for column in start.min(end)..end {
            self.clear_cell_and_wide_pair(row, column);
        }
    }

    fn clear_rows(&mut self, start: u16, end: u16) {
        let end = end.min(self.size.rows);
        for row in start.min(end)..end {
            self.clear_range(row, 0, self.size.columns);
        }
    }

    fn reset(&mut self) {
        for cell in &mut self.cells {
            cell.blank();
        }
        self.cursor = CursorPosition::default();
        self.saved = SavedCursor::default();
        self.scroll_region = ScrollRegion::full(self.size);
        self.wrap_pending = false;
        self.grapheme_pending = GraphemePending::None;
    }

    fn resize(&mut self, size: TerminalSize) {
        let old_size = self.size;
        let old_cells = std::mem::take(&mut self.cells);
        let mut cells = vec![Cell::default(); size.cell_count()];
        let rows = old_size.rows.min(size.rows);
        let columns = old_size.columns.min(size.columns);
        for row in 0..rows {
            for column in 0..columns {
                let old_index =
                    usize::from(row) * usize::from(old_size.columns) + usize::from(column);
                let new_index = usize::from(row) * usize::from(size.columns) + usize::from(column);
                cells[new_index] = old_cells[old_index].clone();
            }
        }
        self.size = size;
        self.cells = cells;
        self.cursor.row = self.cursor.row.min(size.rows - 1);
        self.cursor.column = self.cursor.column.min(size.columns - 1);
        self.saved.position.row = self.saved.position.row.min(size.rows - 1);
        self.saved.position.column = self.saved.position.column.min(size.columns - 1);
        self.scroll_region = ScrollRegion::full(size);
        self.wrap_pending = false;
        for row in 0..size.rows {
            self.repair_row(row);
        }
    }

    fn repair_row(&mut self, row: u16) {
        for column in 0..self.size.columns {
            let continuation = self.cell(row, column).continuation;
            if continuation && (column == 0 || self.cell(row, column - 1).width != 2) {
                self.cell_mut(row, column).blank();
            }
            if self.cell(row, column).width == 2
                && (column + 1 >= self.size.columns || !self.cell(row, column + 1).continuation)
            {
                self.cell_mut(row, column).blank();
            }
        }
    }

    fn save_cursor(&mut self) {
        self.saved = SavedCursor {
            position: self.cursor,
            wrap_pending: self.wrap_pending,
        };
    }

    fn restore_cursor(&mut self) {
        self.cursor = self.saved.position;
        self.wrap_pending = self.saved.wrap_pending;
    }

    fn cancel_wrap(&mut self) {
        self.wrap_pending = false;
    }

    fn finish_grapheme(&mut self) -> bool {
        matches!(
            std::mem::take(&mut self.grapheme_pending),
            GraphemePending::Width
        )
    }

    fn carriage_return(&mut self) {
        self.cursor.column = 0;
        self.cancel_wrap();
    }

    fn backspace(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
        self.cancel_wrap();
    }

    fn linefeed(&mut self) {
        self.cancel_wrap();
        if self.cursor.row == self.scroll_region.bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.size.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cancel_wrap();
        if self.cursor.row == self.scroll_region.top {
            self.scroll_down(1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let top = usize::from(self.scroll_region.top);
        let bottom = usize::from(self.scroll_region.bottom);
        let columns = usize::from(self.size.columns);
        let rows = bottom - top + 1;
        let count = count.min(rows);
        let range = top * columns..(bottom + 1) * columns;
        self.cells[range.clone()].rotate_left(count * columns);
        for cell in &mut self.cells[range.end - count * columns..range.end] {
            cell.blank();
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let top = usize::from(self.scroll_region.top);
        let bottom = usize::from(self.scroll_region.bottom);
        let columns = usize::from(self.size.columns);
        let rows = bottom - top + 1;
        let count = count.min(rows);
        let range = top * columns..(bottom + 1) * columns;
        self.cells[range.clone()].rotate_right(count * columns);
        for cell in &mut self.cells[range.start..range.start + count * columns] {
            cell.blank();
        }
    }

    fn insert_lines(&mut self, count: usize) {
        if self.cursor.row < self.scroll_region.top || self.cursor.row > self.scroll_region.bottom {
            return;
        }
        let old_top = self.scroll_region.top;
        self.scroll_region.top = self.cursor.row;
        self.scroll_down(count);
        self.scroll_region.top = old_top;
    }

    fn delete_lines(&mut self, count: usize) {
        if self.cursor.row < self.scroll_region.top || self.cursor.row > self.scroll_region.bottom {
            return;
        }
        let old_top = self.scroll_region.top;
        self.scroll_region.top = self.cursor.row;
        self.scroll_up(count);
        self.scroll_region.top = old_top;
    }

    fn move_relative(&mut self, rows: i32, columns: i32) {
        let row = i32::from(self.cursor.row)
            .saturating_add(rows)
            .clamp(0, i32::from(self.size.rows - 1));
        let column = i32::from(self.cursor.column)
            .saturating_add(columns)
            .clamp(0, i32::from(self.size.columns - 1));
        self.cursor = CursorPosition {
            row: u16::try_from(row).unwrap_or(0),
            column: u16::try_from(column).unwrap_or(0),
        };
        self.cancel_wrap();
    }

    fn goto(&mut self, row: u16, column: u16, origin_mode: bool) {
        let row = if origin_mode {
            self.scroll_region
                .top
                .saturating_add(row)
                .min(self.scroll_region.bottom)
        } else {
            row.min(self.size.rows - 1)
        };
        self.cursor = CursorPosition {
            row,
            column: column.min(self.size.columns - 1),
        };
        self.cancel_wrap();
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) -> bool {
        if top >= bottom || bottom >= self.size.rows {
            return false;
        }
        self.scroll_region = ScrollRegion { top, bottom };
        self.cursor = CursorPosition::default();
        self.cancel_wrap();
        true
    }

    fn insert_chars(&mut self, count: usize) {
        let row = self.cursor.row;
        let start = usize::from(self.cursor.column);
        let columns = usize::from(self.size.columns);
        let count = count.min(columns - start);
        let row_start = usize::from(row) * columns;
        self.cells[row_start + start..row_start + columns].rotate_right(count);
        for cell in &mut self.cells[row_start + start..row_start + start + count] {
            cell.blank();
        }
        self.repair_row(row);
    }

    fn delete_chars(&mut self, count: usize) {
        let row = self.cursor.row;
        let start = usize::from(self.cursor.column);
        let columns = usize::from(self.size.columns);
        let count = count.min(columns - start);
        let row_start = usize::from(row) * columns;
        self.cells[row_start + start..row_start + columns].rotate_left(count);
        for cell in &mut self.cells[row_start + columns - count..row_start + columns] {
            cell.blank();
        }
        self.repair_row(row);
    }

    fn previous_base_position(&self) -> Option<CursorPosition> {
        let mut column = self.cursor.column;
        if !self.wrap_pending {
            if column == 0 {
                return None;
            }
            column -= 1;
        }
        if self.cell(self.cursor.row, column).continuation && column > 0 {
            column -= 1;
        }
        let cell = self.cell(self.cursor.row, column);
        (!cell.text.is_empty()).then_some(CursorPosition {
            row: self.cursor.row,
            column,
        })
    }

    fn extend_previous_grapheme(&mut self, character: char, wrapping: bool) -> Option<bool> {
        let position = self.previous_base_position()?;
        let cell = self.cell(position.row, position.column);
        let mut text = cell.text.clone();
        text.push(character);
        if UnicodeSegmentation::graphemes(text.as_str(), true).count() != 1 {
            return None;
        }
        if text.len() > MAX_CELL_TEXT_BYTES {
            return Some(false);
        }

        let width = UnicodeWidthStr::width(text.as_str());
        if !(1..=2).contains(&width)
            || usize::from(position.column).saturating_add(width) > usize::from(self.size.columns)
        {
            self.cell_mut(position.row, position.column).text = text;
            self.grapheme_pending = GraphemePending::Width;
            return Some(true);
        }
        let old_width = usize::from(cell.width);
        match (old_width, width) {
            (1, 2) => {
                self.clear_cell_and_wide_pair(position.row, position.column + 1);
                self.cell_mut(position.row, position.column + 1)
                    .continuation = true;
            }
            (2, 1) => self.cell_mut(position.row, position.column + 1).blank(),
            (1 | 2, 1 | 2) => {}
            _ => return Some(false),
        }

        let pending = grapheme_tail_pending(text.as_str());
        let cell = self.cell_mut(position.row, position.column);
        cell.text = text;
        cell.width = u8::try_from(width).unwrap_or(1);
        self.grapheme_pending = pending;
        let next = usize::from(position.column) + width;
        if next >= usize::from(self.size.columns) {
            self.cursor.column = self.size.columns - 1;
            self.wrap_pending = wrapping;
        } else {
            self.cursor.column = u16::try_from(next).unwrap_or(self.size.columns - 1);
            self.wrap_pending = false;
        }
        Some(true)
    }

    fn print(&mut self, character: char, wrapping: bool, insert_mode: bool) -> bool {
        if let Some(extended) = self.extend_previous_grapheme(character, wrapping) {
            return extended;
        }
        let previous_grapheme_unresolved = self.finish_grapheme();
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 {
            return !previous_grapheme_unresolved;
        }
        let width = width.min(2);
        if self.wrap_pending && wrapping {
            self.cursor.column = 0;
            self.linefeed();
        }
        self.wrap_pending = false;

        let remaining = usize::from(self.size.columns - self.cursor.column);
        if width > remaining && wrapping {
            self.cursor.column = 0;
            self.linefeed();
        }
        let remaining = usize::from(self.size.columns - self.cursor.column);
        if width > remaining {
            return false;
        }
        if insert_mode {
            self.insert_chars(width);
        }

        let row = self.cursor.row;
        let column = self.cursor.column;
        let pending =
            if scalar_tail_pending(character) || matches!(character, '\u{1F1E6}'..='\u{1F1FF}') {
                GraphemePending::Boundary
            } else {
                GraphemePending::None
            };
        for offset in 0..u16::try_from(width).unwrap_or(1) {
            self.clear_cell_and_wide_pair(row, column + offset);
        }
        let cell = self.cell_mut(row, column);
        cell.text.push(character);
        cell.width = u8::try_from(width).unwrap_or(1);
        self.grapheme_pending = pending;
        if width == 2 {
            let continuation = self.cell_mut(row, column + 1);
            continuation.continuation = true;
        }

        let next = usize::from(column) + width;
        if next >= usize::from(self.size.columns) {
            self.cursor.column = self.size.columns - 1;
            self.wrap_pending = wrapping;
        } else {
            self.cursor.column = u16::try_from(next).unwrap_or(self.size.columns - 1);
        }
        !previous_grapheme_unresolved
    }

    fn row_text(&self, row: u16) -> String {
        let mut text = String::new();
        for column in 0..self.size.columns {
            let cell = self.cell(row, column);
            if cell.continuation {
                continue;
            }
            if cell.text.is_empty() {
                text.push(' ');
            } else {
                text.push_str(&cell.text);
            }
        }
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    fn blank_cells_to_right(&self) -> u16 {
        let row = self.cursor.row;
        (self.cursor.column..self.size.columns)
            .take_while(|column| {
                let cell = self.cell(row, *column);
                cell.text.is_empty() && !cell.continuation
            })
            .count()
            .try_into()
            .unwrap_or(self.size.columns)
    }

    fn rows_below_clear(&self) -> bool {
        (self.cursor.row + 1..self.size.rows).all(|row| {
            (0..self.size.columns).all(|column| {
                let cell = self.cell(row, column);
                cell.text.is_empty() && !cell.continuation
            })
        })
    }
}

#[derive(Debug)]
// These modes are independent terminal protocol facts, not alternate states.
#[allow(clippy::struct_excessive_bools)]
struct TerminalMachine {
    primary: Surface,
    alternate: Surface,
    active: ScreenBuffer,
    wrapping: bool,
    cursor_visible: bool,
    origin_mode: bool,
    insert_mode: bool,
    newline_mode: bool,
    synchronized_output: bool,
    synchronized: bool,
    tab_stops: Vec<bool>,
    last_printed: Option<char>,
}

impl TerminalMachine {
    fn new(size: TerminalSize) -> Self {
        Self {
            primary: Surface::new(size),
            alternate: Surface::new(size),
            active: ScreenBuffer::Primary,
            wrapping: true,
            cursor_visible: true,
            origin_mode: false,
            insert_mode: false,
            newline_mode: false,
            synchronized_output: false,
            synchronized: true,
            tab_stops: default_tab_stops(size.columns),
            last_printed: None,
        }
    }

    fn surface(&self) -> &Surface {
        match self.active {
            ScreenBuffer::Primary => &self.primary,
            ScreenBuffer::Alternate => &self.alternate,
        }
    }

    fn surface_mut(&mut self) -> &mut Surface {
        match self.active {
            ScreenBuffer::Primary => &mut self.primary,
            ScreenBuffer::Alternate => &mut self.alternate,
        }
    }

    fn mark_unsafe(&mut self) {
        self.synchronized = false;
    }

    fn finish_grapheme(&mut self) {
        if self.surface_mut().finish_grapheme() {
            self.mark_unsafe();
        }
    }

    fn reset(&mut self) {
        let size = self.primary.size;
        self.primary.reset();
        self.alternate.reset();
        self.active = ScreenBuffer::Primary;
        self.wrapping = true;
        self.cursor_visible = true;
        self.origin_mode = false;
        self.insert_mode = false;
        self.newline_mode = false;
        self.synchronized_output = false;
        self.synchronized = true;
        self.tab_stops = default_tab_stops(size.columns);
        self.last_printed = None;
    }

    fn resize(&mut self, size: TerminalSize) {
        let unresolved_grapheme = matches!(self.primary.grapheme_pending, GraphemePending::Width)
            || matches!(self.alternate.grapheme_pending, GraphemePending::Width);
        self.primary.resize(size);
        self.alternate.resize(size);
        let old = std::mem::replace(&mut self.tab_stops, default_tab_stops(size.columns));
        for (index, stop) in old.into_iter().enumerate().take(self.tab_stops.len()) {
            self.tab_stops[index] = stop;
        }
        if unresolved_grapheme {
            self.mark_unsafe();
        }
    }

    fn switch_alternate(&mut self, clear: bool) {
        self.active = ScreenBuffer::Alternate;
        if clear {
            self.alternate.reset();
        }
        self.origin_mode = false;
    }

    fn switch_primary(&mut self) {
        self.active = ScreenBuffer::Primary;
        self.origin_mode = false;
    }

    fn save_cursor(&mut self) {
        self.surface_mut().save_cursor();
    }

    fn restore_cursor(&mut self) {
        self.surface_mut().restore_cursor();
    }

    fn tab(&mut self, count: usize, backwards: bool) {
        let columns = self.surface().size.columns;
        let mut column = self.surface().cursor.column;
        for _ in 0..count {
            if backwards {
                let candidate = (0..column)
                    .rev()
                    .find(|index| self.tab_stops[usize::from(*index)]);
                column = candidate.unwrap_or(0);
            } else {
                let candidate =
                    (column + 1..columns).find(|index| self.tab_stops[usize::from(*index)]);
                column = candidate.unwrap_or(columns - 1);
            }
        }
        let surface = self.surface_mut();
        surface.cursor.column = column;
        surface.cancel_wrap();
    }

    fn set_private_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 | 5 | 12 | 45 | 1000 | 1002 | 1003 | 1004 | 1005 | 1006 | 1015 | 2004 => {}
            6 => {
                self.origin_mode = enabled;
                let top = if enabled {
                    self.surface().scroll_region.top
                } else {
                    0
                };
                self.surface_mut().goto(top, 0, false);
            }
            7 => {
                self.wrapping = enabled;
                self.surface_mut().cancel_wrap();
            }
            25 => self.cursor_visible = enabled,
            47 | 1047 => {
                if enabled {
                    self.switch_alternate(mode == 1047);
                } else {
                    self.switch_primary();
                }
            }
            1048 => {
                if enabled {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if enabled {
                    self.primary.save_cursor();
                    self.switch_alternate(true);
                } else {
                    self.switch_primary();
                    self.primary.restore_cursor();
                }
            }
            2026 => self.synchronized_output = enabled,
            _ => self.mark_unsafe(),
        }
    }

    fn set_ansi_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            4 => self.insert_mode = enabled,
            20 => self.newline_mode = enabled,
            _ => self.mark_unsafe(),
        }
    }

    fn param(params: &Params, index: usize, default: u16) -> u16 {
        params
            .iter()
            .nth(index)
            .and_then(|parameter| parameter.first())
            .copied()
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    fn clear_screen(&mut self, mode: u16) {
        let cursor = self.surface().cursor;
        let size = self.surface().size;
        match mode {
            0 => {
                self.surface_mut()
                    .clear_range(cursor.row, cursor.column, size.columns);
                self.surface_mut().clear_rows(cursor.row + 1, size.rows);
            }
            1 => {
                self.surface_mut().clear_rows(0, cursor.row);
                self.surface_mut()
                    .clear_range(cursor.row, 0, cursor.column + 1);
            }
            2 => self.surface_mut().clear_rows(0, size.rows),
            3 => {}
            _ => self.mark_unsafe(),
        }
    }

    fn clear_line(&mut self, mode: u16) {
        let cursor = self.surface().cursor;
        let columns = self.surface().size.columns;
        match mode {
            0 => self
                .surface_mut()
                .clear_range(cursor.row, cursor.column, columns),
            1 => self
                .surface_mut()
                .clear_range(cursor.row, 0, cursor.column + 1),
            2 => self.surface_mut().clear_range(cursor.row, 0, columns),
            _ => self.mark_unsafe(),
        }
    }
}

impl Perform for TerminalMachine {
    fn print(&mut self, character: char) {
        if matches!(character, '\u{80}'..='\u{9F}') {
            let byte = u8::try_from(u32::from(character)).unwrap_or(0x9F);
            self.execute(byte);
            return;
        }
        let wrapping = self.wrapping;
        let insert_mode = self.insert_mode;
        if !self.surface_mut().print(character, wrapping, insert_mode) {
            self.mark_unsafe();
        }
        self.last_printed = Some(character);
    }

    fn execute(&mut self, byte: u8) {
        self.finish_grapheme();
        match byte {
            0x00 | 0x07 | 0x0E | 0x0F | 0x18 | 0x1A | 0x7F => {}
            0x08 => self.surface_mut().backspace(),
            0x09 => self.tab(1, false),
            0x0A..=0x0C => {
                self.surface_mut().linefeed();
                if self.newline_mode {
                    self.surface_mut().carriage_return();
                }
            }
            0x0D => self.surface_mut().carriage_return(),
            // Raw C1 motions (IND, NEL, HTS, RI) reach this path only through
            // invalid UTF-8 or a UTF-8-encoded C1 scalar, and real terminals
            // disagree on whether those move the cursor or print a
            // replacement glyph. Trusting them would let child output steer
            // overlay writes onto coordinates the terminal never moved to,
            // so they suppress the overlay like every other C1. The reliable
            // ESC-encoded forms stay modeled in esc_dispatch.
            _ => self.mark_unsafe(),
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        self.finish_grapheme();
        self.mark_unsafe();
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {
        self.finish_grapheme();
    }

    #[allow(clippy::too_many_lines)]
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.finish_grapheme();
        if ignore {
            self.mark_unsafe();
            return;
        }
        let count = usize::from(Self::param(params, 0, 1));
        match (action, intermediates) {
            ('A', []) => self
                .surface_mut()
                .move_relative(-i32::try_from(count).unwrap_or(i32::MAX), 0),
            ('B' | 'e', []) => self
                .surface_mut()
                .move_relative(i32::try_from(count).unwrap_or(i32::MAX), 0),
            ('C' | 'a', []) => self
                .surface_mut()
                .move_relative(0, i32::try_from(count).unwrap_or(i32::MAX)),
            ('D', []) => self
                .surface_mut()
                .move_relative(0, -i32::try_from(count).unwrap_or(i32::MAX)),
            ('E', []) => {
                self.surface_mut()
                    .move_relative(i32::try_from(count).unwrap_or(i32::MAX), 0);
                self.surface_mut().carriage_return();
            }
            ('F', []) => {
                self.surface_mut()
                    .move_relative(-i32::try_from(count).unwrap_or(i32::MAX), 0);
                self.surface_mut().carriage_return();
            }
            ('G' | '`', []) => {
                let column = Self::param(params, 0, 1).saturating_sub(1);
                let row = self.surface().cursor.row;
                self.surface_mut().goto(row, column, false);
            }
            ('d', []) => {
                let row = Self::param(params, 0, 1).saturating_sub(1);
                let column = self.surface().cursor.column;
                let origin_mode = self.origin_mode;
                self.surface_mut().goto(row, column, origin_mode);
            }
            ('H' | 'f', []) => {
                let row = Self::param(params, 0, 1).saturating_sub(1);
                let column = Self::param(params, 1, 1).saturating_sub(1);
                let origin_mode = self.origin_mode;
                self.surface_mut().goto(row, column, origin_mode);
            }
            ('I', []) => self.tab(count, false),
            ('Z', []) => self.tab(count, true),
            ('J', []) => self.clear_screen(Self::param(params, 0, 0)),
            ('K', []) => self.clear_line(Self::param(params, 0, 0)),
            ('@', []) => self.surface_mut().insert_chars(count),
            ('P', []) => self.surface_mut().delete_chars(count),
            ('X', []) => {
                let cursor = self.surface().cursor;
                let end = cursor
                    .column
                    .saturating_add(u16::try_from(count).unwrap_or(u16::MAX));
                self.surface_mut()
                    .clear_range(cursor.row, cursor.column, end);
            }
            ('L', []) => self.surface_mut().insert_lines(count),
            ('M', []) => self.surface_mut().delete_lines(count),
            ('S', []) => self.surface_mut().scroll_up(count),
            ('T', []) => self.surface_mut().scroll_down(count),
            ('b', []) => {
                if let Some(character) = self.last_printed {
                    for _ in 0..count {
                        self.print(character);
                    }
                }
            }
            ('g', []) => match Self::param(params, 0, 0) {
                0 => {
                    let column = usize::from(self.surface().cursor.column);
                    self.tab_stops[column] = false;
                }
                3 => self.tab_stops.fill(false),
                _ => self.mark_unsafe(),
            },
            ('h', []) => {
                for parameter in params {
                    if let Some(mode) = parameter.first() {
                        self.set_ansi_mode(*mode, true);
                    }
                }
            }
            ('l', []) => {
                for parameter in params {
                    if let Some(mode) = parameter.first() {
                        self.set_ansi_mode(*mode, false);
                    }
                }
            }
            ('h', [b'?']) => {
                for parameter in params {
                    if let Some(mode) = parameter.first() {
                        self.set_private_mode(*mode, true);
                    }
                }
            }
            ('l', [b'?']) => {
                for parameter in params {
                    if let Some(mode) = parameter.first() {
                        self.set_private_mode(*mode, false);
                    }
                }
            }
            ('m' | 'c' | 'n', []) | ('q', [b' ']) => {}
            ('r', []) => {
                let rows = self.surface().size.rows;
                let top = Self::param(params, 0, 1).saturating_sub(1);
                let bottom = Self::param(params, 1, rows);
                if bottom == 0 || !self.surface_mut().set_scroll_region(top, bottom - 1) {
                    self.mark_unsafe();
                } else if self.origin_mode {
                    self.surface_mut().goto(0, 0, true);
                }
            }
            ('s', []) if params.is_empty() => self.save_cursor(),
            ('u', []) if params.is_empty() => self.restore_cursor(),
            ('p', [b'!']) => {
                self.origin_mode = false;
                self.insert_mode = false;
                self.newline_mode = false;
                self.wrapping = true;
                self.surface_mut().cancel_wrap();
            }
            _ => self.mark_unsafe(),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.finish_grapheme();
        if ignore {
            self.mark_unsafe();
            return;
        }
        match (byte, intermediates) {
            (b'7', []) => self.save_cursor(),
            (b'8', []) => self.restore_cursor(),
            (b'D', []) => self.surface_mut().linefeed(),
            (b'E', []) => {
                self.surface_mut().linefeed();
                self.surface_mut().carriage_return();
            }
            (b'H', []) => {
                let column = usize::from(self.surface().cursor.column);
                self.tab_stops[column] = true;
            }
            (b'M', []) => self.surface_mut().reverse_index(),
            (b'c', []) => self.reset(),
            (b'=' | b'>' | b'\\', [])
            | (b'B' | b'0', [b'(' | b')' | b'*' | b'+'])
            | (b'@' | b'G', [b'%']) => {}
            _ => self.mark_unsafe(),
        }
    }
}

fn default_tab_stops(columns: u16) -> Vec<bool> {
    (0..columns)
        .map(|column| column != 0 && column % 8 == 0)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuardState {
    Ground,
    Escape,
    Csi,
    Osc(usize),
    OscEscape(usize),
    String,
    StringEscape,
    DiscardOsc,
    DiscardOscEscape,
}

impl GuardState {
    const fn complete(self) -> bool {
        matches!(self, Self::Ground)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuardEvent {
    None,
    Unsupported,
    Overflow,
    ResumeAfterDiscard,
}

fn guard_step(state: &mut GuardState, byte: u8) -> GuardEvent {
    use GuardState::{
        Csi, DiscardOsc, DiscardOscEscape, Escape, Ground, Osc, OscEscape, String as StringState,
        StringEscape,
    };
    match *state {
        Ground => {
            if byte == 0x1B {
                *state = Escape;
            }
        }
        Escape => match byte {
            b'[' => *state = Csi,
            b']' => *state = Osc(0),
            b'P' | b'X' | b'^' | b'_' => {
                *state = StringState;
                return GuardEvent::Unsupported;
            }
            0x18 | 0x1A | 0x30..=0x7E => *state = Ground,
            _ => {}
        },
        Csi => match byte {
            0x18 | 0x1A | 0x40..=0x7E => *state = Ground,
            0x1B => *state = Escape,
            _ => {}
        },
        Osc(bytes) => match byte {
            0x07 | 0x18 | 0x1A => *state = Ground,
            0x1B => *state = OscEscape(bytes),
            _ => {
                let bytes = bytes.saturating_add(1);
                if bytes > MAX_CONTROL_STRING_BYTES {
                    *state = DiscardOsc;
                    return GuardEvent::Overflow;
                }
                *state = Osc(bytes);
            }
        },
        OscEscape(bytes) => {
            if byte == b'\\' {
                *state = Ground;
            } else {
                *state = Escape;
                let _ = guard_step(state, byte);
                if matches!(*state, Osc(0)) && bytes > MAX_CONTROL_STRING_BYTES {
                    *state = DiscardOsc;
                    return GuardEvent::Overflow;
                }
            }
        }
        StringState => match byte {
            0x18 | 0x1A => *state = Ground,
            0x1B => *state = StringEscape,
            _ => {}
        },
        StringEscape => {
            if byte == b'\\' {
                *state = Ground;
            } else {
                *state = StringState;
            }
        }
        DiscardOsc => match byte {
            0x07 | 0x18 | 0x1A => {
                *state = Ground;
                return GuardEvent::ResumeAfterDiscard;
            }
            0x1B => *state = DiscardOscEscape,
            _ => {}
        },
        DiscardOscEscape => {
            if byte == b'\\' {
                *state = Ground;
                return GuardEvent::ResumeAfterDiscard;
            }
            *state = DiscardOsc;
        }
    }
    GuardEvent::None
}

#[derive(Clone, Copy, Default)]
struct Utf8Carry {
    bytes: [u8; 4],
    len: u8,
    expected: u8,
}

impl fmt::Debug for Utf8Carry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Utf8Carry")
            .field("retained_bytes", &self.len)
            .finish_non_exhaustive()
    }
}

impl Utf8Carry {
    const fn is_empty(self) -> bool {
        self.len == 0
    }

    const fn len(self) -> usize {
        self.len as usize
    }

    fn clear(&mut self) {
        self.len = 0;
        self.expected = 0;
    }

    fn normalize(&mut self, input: &[u8], mut guard: GuardState) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len().saturating_add(self.len()));
        for byte in input.iter().copied() {
            if matches!(guard, GuardState::Ground) {
                self.push_ground(byte, &mut output);
            } else {
                debug_assert!(self.is_empty());
                output.push(byte);
            }
            let _ = guard_step(&mut guard, byte);
        }
        debug_assert!(self.len() <= MAX_UTF8_CARRY_BYTES);
        output
    }

    fn start(&mut self, byte: u8, expected: u8) {
        self.bytes[0] = byte;
        self.len = 1;
        self.expected = expected;
    }

    fn push_ground(&mut self, byte: u8, output: &mut Vec<u8>) {
        if self.is_empty() {
            match byte {
                0x00..=0x9F => output.push(byte),
                0xC2..=0xDF => self.start(byte, 2),
                0xE0..=0xEF => self.start(byte, 3),
                0xF0..=0xF4 => self.start(byte, 4),
                _ => output.extend_from_slice("�".as_bytes()),
            }
            return;
        }

        self.bytes[self.len()] = byte;
        self.len += 1;
        let sequence = &self.bytes[..self.len()];
        match std::str::from_utf8(sequence) {
            Ok(_) => {
                output.extend_from_slice(sequence);
                self.clear();
            }
            Err(error) => {
                let Some(invalid_len) = error.error_len() else {
                    debug_assert!(self.len < self.expected);
                    return;
                };
                let retained = self.len().saturating_sub(invalid_len);
                let mut remainder = [0; 3];
                remainder[..retained]
                    .copy_from_slice(&self.bytes[invalid_len..invalid_len + retained]);
                output.extend_from_slice("�".as_bytes());
                self.clear();
                for byte in remainder[..retained].iter().copied() {
                    self.push_ground(byte, output);
                }
            }
        }
    }
}

/// Bounded VTE observer for child output and trusted renderer transactions.
pub struct ScreenObserver {
    parser: Parser,
    machine: TerminalMachine,
    guard: GuardState,
    utf8_carry: Utf8Carry,
}

impl fmt::Debug for ScreenObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScreenObserver")
            .field("snapshot", &self.snapshot())
            .field("guard", &self.guard)
            .field("utf8_carry", &self.utf8_carry)
            .field("primary", &self.machine.primary)
            .field("alternate", &self.machine.alternate)
            .finish_non_exhaustive()
    }
}

impl ScreenObserver {
    /// Creates a synchronized, blank primary terminal screen.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self {
            parser: Parser::new(),
            machine: TerminalMachine::new(size),
            guard: GuardState::Ground,
            utf8_carry: Utf8Carry::default(),
        }
    }

    /// Suppresses absolute overlay placement until a trusted cursor boundary
    /// or terminal reset establishes screen coordinates.
    pub fn desynchronize(&mut self) {
        self.machine.mark_unsafe();
    }

    /// Consumes output while retaining only a bounded incomplete UTF-8 prefix.
    #[must_use]
    pub fn observe(&mut self, bytes: &[u8]) -> ScreenObservation {
        let normalized = self.utf8_carry.normalize(bytes, self.guard);
        self.feed(&normalized);
        let overlay_safe = self.snapshot().overlay_safe();
        ScreenObservation {
            consumed_bytes: bytes.len(),
            clear_overlay: !bytes.is_empty(),
            overlay_safe,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        let mut feed_start = (!matches!(
            self.guard,
            GuardState::DiscardOsc | GuardState::DiscardOscEscape
        ))
        .then_some(0);
        for (index, byte) in bytes.iter().copied().enumerate() {
            match guard_step(&mut self.guard, byte) {
                GuardEvent::None => {}
                GuardEvent::Unsupported => self.machine.mark_unsafe(),
                GuardEvent::Overflow => {
                    if let Some(start) = feed_start {
                        self.parser.advance(&mut self.machine, &bytes[start..index]);
                    }
                    self.parser = Parser::new();
                    self.machine.mark_unsafe();
                    feed_start = None;
                }
                GuardEvent::ResumeAfterDiscard => feed_start = Some(index + 1),
            }
        }
        if let Some(start) = feed_start {
            self.parser.advance(&mut self.machine, &bytes[start..]);
        }
    }

    /// Applies a validated resize while preserving the intersecting cell grid.
    #[must_use]
    pub fn resize(&mut self, size: TerminalSize) -> ScreenObservation {
        self.machine.resize(size);
        ScreenObservation {
            consumed_bytes: 0,
            clear_overlay: true,
            overlay_safe: self.snapshot().overlay_safe(),
        }
    }

    /// Establishes a known primary-shell cursor boundary after an adapter sync.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenError::InvalidCursor`] without changing state when the
    /// cursor is outside the current terminal dimensions.
    pub fn synchronize(&mut self, cursor: CursorPosition) -> Result<(), ScreenError> {
        let size = self.machine.primary.size;
        if cursor.row >= size.rows || cursor.column >= size.columns {
            return Err(ScreenError::InvalidCursor(cursor));
        }
        self.parser = Parser::new();
        self.guard = GuardState::Ground;
        self.utf8_carry.clear();
        self.machine.active = ScreenBuffer::Primary;
        // A trusted snapshot re-establishes placement, so state inherited from
        // whatever ran before it is cleared along with the parser. A child that
        // exited while a synchronized update was open would otherwise suppress
        // the overlay until the terminal was reset, and a partially printed
        // grapheme would keep suppressing it with nothing left to complete it.
        self.machine.synchronized_output = false;
        self.machine.primary.grapheme_pending = GraphemePending::None;
        let cursor_changed = self.machine.primary.cursor != cursor;
        self.machine.primary.cursor = cursor;
        if cursor_changed {
            self.machine.primary.cancel_wrap();
        }
        self.machine.synchronized = true;
        Ok(())
    }

    /// Current placement and terminal-mode snapshot.
    #[must_use]
    pub fn snapshot(&self) -> ScreenSnapshot {
        let surface = self.machine.surface();
        ScreenSnapshot {
            size: surface.size,
            cursor: surface.cursor,
            saved_cursor: surface.saved.position,
            scroll_region: surface.scroll_region,
            blank_cells_to_right: surface.blank_cells_to_right(),
            rows_below_clear: surface.rows_below_clear(),
            buffer: self.machine.active,
            wrapping: self.machine.wrapping,
            wrap_pending: surface.wrap_pending,
            cursor_visible: self.machine.cursor_visible,
            origin_mode: self.machine.origin_mode,
            insert_mode: self.machine.insert_mode,
            synchronized: self.machine.synchronized
                && !self.machine.synchronized_output
                && self.guard.complete()
                && self.utf8_carry.is_empty()
                && matches!(surface.grapheme_pending, GraphemePending::None),
        }
    }

    /// Visible text for one row, omitting trailing blank cells.
    ///
    /// Shell content is sensitive and is deliberately excluded from `Debug`.
    #[must_use]
    pub fn row_text(&self, row: u16) -> Option<String> {
        (row < self.machine.surface().size.rows).then(|| self.machine.surface().row_text(row))
    }

    /// Display width of the visible text in one row.
    #[must_use]
    pub fn row_width(&self, row: u16) -> Option<usize> {
        self.row_text(row)
            .map(|text| UnicodeWidthStr::width(text.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(columns: u16, rows: u16) -> TerminalSize {
        TerminalSize::new(columns, rows).unwrap()
    }

    fn assert_same_screen(actual: &ScreenObserver, want: &ScreenObserver, context: &str) {
        assert_eq!(actual.snapshot(), want.snapshot(), "{context} snapshot");
        for row in 0..want.snapshot().size().rows() {
            assert_eq!(
                actual.row_text(row),
                want.row_text(row),
                "{context} row {row}"
            );
        }
    }

    #[test]
    fn unknown_startup_cursor_suppresses_overlay_until_synchronized() {
        let mut screen = ScreenObserver::new(size(80, 24));
        screen.desynchronize();
        assert!(!screen.snapshot().overlay_safe());

        screen.synchronize(CursorPosition::new(6, 32)).unwrap();
        assert!(screen.snapshot().overlay_safe());
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(6, 32));
    }

    fn direct_vte(bytes: &[u8], terminal_size: TerminalSize) -> ScreenObserver {
        let mut observer = ScreenObserver::new(terminal_size);
        for byte in bytes.iter().copied() {
            assert_eq!(guard_step(&mut observer.guard, byte), GuardEvent::None);
        }
        observer.parser.advance(&mut observer.machine, bytes);
        observer
    }

    fn assert_vte_partition_invariant(
        label: &str,
        bytes: &[u8],
        terminal_size: TerminalSize,
    ) -> ScreenObserver {
        let oracle = direct_vte(bytes, terminal_size);
        let mut whole = ScreenObserver::new(terminal_size);
        let _ = whole.observe(bytes);
        assert_same_screen(&whole, &oracle, &format!("{label} whole"));

        for split in 0..=bytes.len() {
            let mut partitioned = ScreenObserver::new(terminal_size);
            let _ = partitioned.observe(&bytes[..split]);
            if partitioned.snapshot().overlay_safe() {
                let prefix_oracle = direct_vte(&bytes[..split], terminal_size);
                assert_same_screen(
                    &partitioned,
                    &prefix_oracle,
                    &format!("{label} safe prefix {split}"),
                );
            }
            let _ = partitioned.observe(&bytes[split..]);
            assert_same_screen(&partitioned, &oracle, &format!("{label} split {split}"));
        }

        let mut bytewise = ScreenObserver::new(terminal_size);
        for (index, byte) in bytes.iter().enumerate() {
            let _ = bytewise.observe(std::slice::from_ref(byte));
            if bytewise.snapshot().overlay_safe() {
                let prefix_oracle = direct_vte(&bytes[..=index], terminal_size);
                assert_same_screen(
                    &bytewise,
                    &prefix_oracle,
                    &format!("{label} bytewise safe prefix {}", index + 1),
                );
            }
        }
        assert_same_screen(&bytewise, &oracle, &format!("{label} bytewise"));
        whole
    }

    #[test]
    fn tracks_controls_wide_combining_wrap_and_chunks() {
        let bytes = "ab\t界e\u{301}\rZ\nnext\x08!".as_bytes();
        let mut whole = ScreenObserver::new(size(12, 4));
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..4)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();

        for split in 0..=bytes.len() {
            let mut chunked = ScreenObserver::new(size(12, 4));
            let _ = chunked.observe(&bytes[..split]);
            let _ = chunked.observe(&bytes[split..]);
            assert_eq!(chunked.snapshot(), want, "split {split}");
            assert_eq!(
                (0..4)
                    .map(|row| chunked.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "split {split}"
            );
        }
        assert_eq!(want.cursor, CursorPosition::new(1, 5));
        assert_eq!(want_rows[0], "Zb      界e\u{301}");
        assert_eq!(want_rows[1], " nex!");
    }

    #[test]
    fn escape_and_utf8_sequences_are_chunk_partition_invariant() {
        let bytes =
            "\x1b[2J\x1b[2;3H界e\u{301}\x1b7\x1b]0;Greendale\x1b\\\x1b[4;10HX\x1b8!".as_bytes();
        let mut whole = ScreenObserver::new(size(16, 5));
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..5)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();

        for split in 0..=bytes.len() {
            let mut chunked = ScreenObserver::new(size(16, 5));
            let _ = chunked.observe(&bytes[..split]);
            let _ = chunked.observe(&bytes[split..]);
            assert_eq!(chunked.snapshot(), want, "split {split}");
            assert_eq!(
                (0..5)
                    .map(|row| chunked.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "split {split}"
            );
        }

        let mut bytewise = ScreenObserver::new(size(16, 5));
        for byte in bytes {
            let _ = bytewise.observe(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise.snapshot(), want);
        assert_eq!(bytewise.row_text(1).unwrap(), "  界e\u{301}!");
        assert_eq!(bytewise.row_text(3).unwrap(), "         X");
        assert!(bytewise.snapshot().overlay_safe());
    }

    #[test]
    fn incomplete_utf8_suppresses_overlay_until_the_character_completes() {
        let bytes = "界".as_bytes();
        let mut screen = ScreenObserver::new(size(10, 3));
        let _ = screen.observe(&bytes[..1]);
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(&bytes[1..2]);
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(&bytes[2..]);
        assert!(screen.snapshot().overlay_safe());
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(0, 2));
    }

    #[test]
    fn utf8_and_combining_are_identical_across_every_partition() {
        let bytes = "éa\u{301}".as_bytes();
        let mut whole = ScreenObserver::new(size(10, 3));
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..3)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();

        for boundaries in 0..(1_u32 << (bytes.len() - 1)) {
            let mut partitioned = ScreenObserver::new(size(10, 3));
            let mut start = 0;
            for end in 1..bytes.len() {
                if boundaries & (1 << (end - 1)) != 0 {
                    let _ = partitioned.observe(&bytes[start..end]);
                    assert!(partitioned.utf8_carry.len() <= MAX_UTF8_CARRY_BYTES);
                    start = end;
                }
            }
            let _ = partitioned.observe(&bytes[start..]);
            assert_eq!(partitioned.snapshot(), want, "boundaries {boundaries:b}");
            assert_eq!(
                (0..3)
                    .map(|row| partitioned.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "boundaries {boundaries:b}"
            );
        }
        assert_eq!(want.cursor(), CursorPosition::new(0, 2));
        assert_eq!(want_rows[0], "éa\u{301}");
    }

    #[test]
    fn invalid_utf8_is_bounded_and_ordered_with_terminal_resets() {
        let mut incomplete = ScreenObserver::new(size(10, 3));
        for byte in [0xF0, 0x9F, 0x92] {
            let _ = incomplete.observe(&[byte]);
            assert!(incomplete.utf8_carry.len() <= MAX_UTF8_CARRY_BYTES);
            assert!(!incomplete.snapshot().overlay_safe());
        }
        let _ = incomplete.observe(b"\x1bcA");
        assert!(incomplete.snapshot().overlay_safe());
        assert_eq!(incomplete.row_text(0).as_deref(), Some("A"));

        let mut invalid_after_reset = ScreenObserver::new(size(10, 3));
        let _ = invalid_after_reset.observe(b"\x1bc\xFF");
        assert!(invalid_after_reset.snapshot().overlay_safe());
        assert_eq!(invalid_after_reset.row_text(0).as_deref(), Some("�"));
        assert_eq!(
            invalid_after_reset.snapshot().cursor(),
            CursorPosition::new(0, 1)
        );

        let mut reset_after_invalid = ScreenObserver::new(size(10, 3));
        let _ = reset_after_invalid.observe(b"\xFF\x1bcA");
        assert!(reset_after_invalid.snapshot().overlay_safe());
        assert_eq!(reset_after_invalid.row_text(0).as_deref(), Some("A"));

        let malformed = b"\xF0(\x8C(\x1bcA";
        let mut whole = ScreenObserver::new(size(10, 3));
        let _ = whole.observe(malformed);
        for split in 0..=malformed.len() {
            let mut partitioned = ScreenObserver::new(size(10, 3));
            let _ = partitioned.observe(&malformed[..split]);
            let _ = partitioned.observe(&malformed[split..]);
            assert_eq!(partitioned.snapshot(), whole.snapshot(), "split {split}");
            assert_eq!(partitioned.row_text(0), whole.row_text(0), "split {split}");
        }
    }

    #[test]
    fn interrupted_utf8_matches_vte_before_controls_and_resets() {
        let terminal_size = size(12, 4);
        let bell = assert_vte_partition_invariant("bell", b"\xC3\x07X", terminal_size);
        assert_eq!(bell.row_text(0).as_deref(), Some("�X"));
        assert_eq!(bell.snapshot().cursor(), CursorPosition::new(0, 2));
        assert!(bell.snapshot().overlay_safe());

        let linefeed = assert_vte_partition_invariant("linefeed", b"\xC3\nX", terminal_size);
        assert_eq!(linefeed.row_text(0).as_deref(), Some("�"));
        assert_eq!(linefeed.row_text(1).as_deref(), Some(" X"));
        assert_eq!(linefeed.snapshot().cursor(), CursorPosition::new(1, 2));
        assert!(linefeed.snapshot().overlay_safe());

        let csi = assert_vte_partition_invariant("escape CSI", b"\xC3\x1b[2CX", terminal_size);
        assert_eq!(csi.row_text(0).as_deref(), Some("�  X"));
        assert_eq!(csi.snapshot().cursor(), CursorPosition::new(0, 4));
        assert!(csi.snapshot().overlay_safe());

        let c1 = assert_vte_partition_invariant("C1 NEL", b"\xE0\x85X", terminal_size);
        assert_eq!(c1.row_text(0).as_deref(), Some("�X"));
        assert_eq!(c1.snapshot().cursor(), CursorPosition::new(0, 2));
        assert!(!c1.snapshot().overlay_safe());

        let osc =
            assert_vte_partition_invariant("OSC", b"\xC3\x1b]0;Greendale\x07X", terminal_size);
        assert_eq!(osc.row_text(0).as_deref(), Some("�X"));
        assert_eq!(osc.snapshot().cursor(), CursorPosition::new(0, 2));
        assert!(osc.snapshot().overlay_safe());

        let reset = assert_vte_partition_invariant("reset", b"\xF0\x9F\x92\x1bcX", terminal_size);
        assert_eq!(reset.row_text(0).as_deref(), Some("X"));
        assert_eq!(reset.snapshot().cursor(), CursorPosition::new(0, 1));
        assert!(reset.snapshot().overlay_safe());
    }

    #[test]
    fn utf8_c1_controls_are_chunk_partition_invariant() {
        let mut whole_nel = ScreenObserver::new(size(20, 5));
        let _ = whole_nel.observe(&[0xC2, 0x85]);
        let mut split_nel = ScreenObserver::new(size(20, 5));
        let _ = split_nel.observe(&[0xC2]);
        let _ = split_nel.observe(&[0x85]);
        assert_eq!(split_nel.snapshot(), whole_nel.snapshot());
        assert_eq!(whole_nel.snapshot().cursor(), CursorPosition::new(0, 0));
        assert!(!whole_nel.snapshot().overlay_safe());

        let mut whole_osc = ScreenObserver::new(size(20, 5));
        let _ = whole_osc.observe(&[0xC2, 0x9D]);
        let mut split_osc = ScreenObserver::new(size(20, 5));
        let _ = split_osc.observe(&[0xC2]);
        let _ = split_osc.observe(&[0x9D]);
        assert_eq!(split_osc.snapshot(), whole_osc.snapshot());
        assert!(!whole_osc.snapshot().overlay_safe());
    }

    #[test]
    fn emoji_graphemes_keep_cursor_and_row_width_consistent() {
        for (grapheme, pending_boundary) in [
            ("👨‍❤️‍💋‍👨", false),
            ("👩‍💻", false),
            ("👋🏽", true),
            ("❤️", true),
            ("1️⃣", false),
            ("🇨🇦", false),
            ("a\u{301}", false),
        ] {
            let bytes = grapheme.as_bytes();
            let mut screen = ScreenObserver::new(size(20, 3));
            let _ = screen.observe(bytes);
            let width = u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap();
            assert_eq!(screen.snapshot().cursor().column, width, "{grapheme}");
            assert_eq!(screen.row_width(0), Some(usize::from(width)), "{grapheme}");
            assert_eq!(screen.row_text(0).as_deref(), Some(grapheme), "{grapheme}");
            assert_eq!(
                screen.snapshot().overlay_safe(),
                !pending_boundary,
                "{grapheme}"
            );

            for split in 0..=bytes.len() {
                let mut partitioned = ScreenObserver::new(size(20, 3));
                let _ = partitioned.observe(&bytes[..split]);
                let _ = partitioned.observe(&bytes[split..]);
                assert_eq!(
                    partitioned.snapshot(),
                    screen.snapshot(),
                    "{grapheme} split {split}"
                );
                assert_eq!(partitioned.row_text(0).as_deref(), Some(grapheme));
            }

            let mut bytewise = ScreenObserver::new(size(20, 3));
            for byte in bytes {
                let _ = bytewise.observe(std::slice::from_ref(byte));
            }
            assert_eq!(
                bytewise.snapshot(),
                screen.snapshot(),
                "{grapheme} bytewise"
            );
            assert_eq!(bytewise.row_text(0).as_deref(), Some(grapheme));

            if pending_boundary {
                let _ = screen.observe(b"\x07");
                assert!(screen.snapshot().overlay_safe(), "{grapheme} boundary");
                assert_eq!(screen.row_text(0).as_deref(), Some(grapheme));
            }
        }
    }

    #[test]
    fn incomplete_grapheme_tails_suppress_until_a_proven_boundary() {
        let mut joiner = ScreenObserver::new(size(20, 3));
        let _ = joiner.observe("👨‍".as_bytes());
        assert_eq!(
            joiner.machine.surface().grapheme_pending,
            GraphemePending::Boundary
        );
        assert!(!joiner.snapshot().overlay_safe());
        let _ = joiner.observe("💻".as_bytes());
        assert_eq!(joiner.row_text(0).as_deref(), Some("👨‍💻"));
        assert_eq!(joiner.snapshot().cursor(), CursorPosition::new(0, 2));
        assert!(joiner.snapshot().overlay_safe());

        let mut resized = ScreenObserver::new(size(20, 3));
        let _ = resized.observe("👨‍".as_bytes());
        let _ = resized.resize(size(24, 4));
        assert!(!resized.snapshot().overlay_safe());
        let _ = resized.observe("💻".as_bytes());
        assert_eq!(resized.row_text(0).as_deref(), Some("👨‍💻"));
        assert!(resized.snapshot().overlay_safe());

        let mut variation = ScreenObserver::new(size(20, 3));
        let _ = variation.observe("❤️".as_bytes());
        assert!(!variation.snapshot().overlay_safe());
        let _ = variation.observe(b"\x1b[0m");
        assert_eq!(variation.row_text(0).as_deref(), Some("❤️"));
        assert!(variation.snapshot().overlay_safe());

        let mut modifier = ScreenObserver::new(size(20, 3));
        let _ = modifier.observe("👋🏽".as_bytes());
        assert!(!modifier.snapshot().overlay_safe());
        let _ = modifier.observe(b"\x1bE");
        assert_eq!(modifier.row_text(0).as_deref(), Some("👋🏽"));
        assert_eq!(modifier.snapshot().cursor(), CursorPosition::new(1, 0));
        assert!(modifier.snapshot().overlay_safe());

        let mut regional_indicator = ScreenObserver::new(size(20, 3));
        let _ = regional_indicator.observe("🇨".as_bytes());
        assert!(!regional_indicator.snapshot().overlay_safe());
        let _ = regional_indicator.observe("🇦".as_bytes());
        assert_eq!(regional_indicator.row_text(0).as_deref(), Some("🇨🇦"));
        assert_eq!(
            regional_indicator.snapshot().cursor(),
            CursorPosition::new(0, 2)
        );
        assert!(regional_indicator.snapshot().overlay_safe());

        let mut osc = ScreenObserver::new(size(20, 3));
        let _ = osc.observe("👨‍\x1b]0;Greendale\x07".as_bytes());
        assert_eq!(osc.row_text(0).as_deref(), Some("👨‍"));
        assert!(osc.snapshot().overlay_safe());

        let mut reset = ScreenObserver::new(size(20, 3));
        let _ = reset.observe("👨‍\x1bc".as_bytes());
        assert_eq!(reset.row_text(0).as_deref(), Some(""));
        assert!(reset.snapshot().overlay_safe());
    }

    #[test]
    fn provisional_zwj_width_recovers_when_the_cluster_completes() {
        let mut screen = ScreenObserver::new(size(20, 3));
        let _ = screen.observe("👨‍❤".as_bytes());
        assert!(screen.machine.synchronized);
        assert_eq!(
            screen.machine.surface().grapheme_pending,
            GraphemePending::Width
        );
        assert!(!screen.snapshot().overlay_safe());
        assert_eq!(screen.row_text(0).as_deref(), Some("👨‍❤"));

        let _ = screen.observe("️‍💋‍👨".as_bytes());
        assert_eq!(
            screen.machine.surface().grapheme_pending,
            GraphemePending::None
        );
        assert!(screen.snapshot().overlay_safe());
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(0, 2));
        assert_eq!(screen.row_text(0).as_deref(), Some("👨‍❤️‍💋‍👨"));
    }

    #[test]
    fn valid_unicode_and_controls_match_random_chunk_partitions() {
        let bytes = "éa\u{301} 👨‍❤️‍💋‍👨\x1b[2;3H1️⃣ 🇨🇦".as_bytes();
        let mut whole = ScreenObserver::new(size(30, 4));
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..4)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();
        let mut seed = 0x4752_4545_4E44_414C_u64;
        for partition in 0..512 {
            let mut chunked = ScreenObserver::new(size(30, 4));
            let mut start = 0;
            while start < bytes.len() {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let chunk = usize::try_from((seed >> 32) % 7 + 1).unwrap();
                let end = start.saturating_add(chunk).min(bytes.len());
                let _ = chunked.observe(&bytes[start..end]);
                start = end;
            }
            assert_eq!(chunked.snapshot(), want, "partition {partition}");
            assert_eq!(
                (0..4)
                    .map(|row| chunked.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "partition {partition}"
            );
        }
    }

    #[test]
    fn editing_modes_partial_regions_and_delayed_wrap_are_overlay_unsafe() {
        let mut origin = ScreenObserver::new(size(20, 6));
        let _ = origin.observe(b"\x1b[3;6r\x1b[?6h");
        assert!(origin.snapshot().origin_mode());
        assert!(!origin.snapshot().overlay_safe());
        let _ = origin.observe(b"\x1b[?6l\x1b[r");
        assert!(origin.snapshot().overlay_safe());

        let mut insert = ScreenObserver::new(size(20, 6));
        let _ = insert.observe(b"\x1b[4h");
        assert!(insert.snapshot().insert_mode());
        assert!(!insert.snapshot().overlay_safe());
        let _ = insert.observe(b"\x1b[4l");
        assert!(insert.snapshot().overlay_safe());

        let mut pending = ScreenObserver::new(size(5, 3));
        let _ = pending.observe(b"12345");
        assert!(pending.snapshot().wrap_pending());
        assert!(!pending.snapshot().overlay_safe());
    }

    #[test]
    fn same_cursor_synchronization_preserves_delayed_wrap() {
        let mut screen = ScreenObserver::new(size(8, 3));
        let _ = screen.observe(b"12345678");
        let cursor = screen.snapshot().cursor();
        assert_eq!(cursor, CursorPosition::new(0, 7));
        assert!(screen.snapshot().wrap_pending());

        screen.synchronize(cursor).unwrap();
        assert!(screen.snapshot().wrap_pending());
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"X");
        assert_eq!(screen.row_text(0).as_deref(), Some("12345678"));
        assert_eq!(screen.row_text(1).as_deref(), Some("X"));
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(1, 1));
    }

    #[test]
    fn prompt_ansi_osc_saved_cursor_and_right_prompt_are_observed() {
        let mut screen = ScreenObserver::new(size(30, 5));
        let _ = screen
            .observe(b"\x1b[31mgreendale>\x1b[0m \x1b]0;coursework\x07git\x1b7\x1b[1;25HRP\x1b8");
        assert!(screen.snapshot().overlay_safe());
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(0, 14));
        assert_eq!(screen.snapshot().blank_cells_to_right(), 10);
        assert!(screen.snapshot().rows_below_clear());
        assert_eq!(screen.row_text(0).unwrap(), "greendale> git          RP");
    }

    #[test]
    fn snapshot_reports_only_proven_clear_overlay_space() {
        let mut screen = ScreenObserver::new(size(30, 5));
        let _ = screen.observe(b"> git\x1b7\x1b[1;20HRP\x1b[3;1Hold output\x1b8");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.cursor(), CursorPosition::new(0, 5));
        assert_eq!(snapshot.blank_cells_to_right(), 14);
        assert!(!snapshot.rows_below_clear());
    }

    #[test]
    fn scroll_region_and_resize_remain_bounded() {
        let mut screen = ScreenObserver::new(size(8, 4));
        let _ = screen.observe(b"one\r\ntwo\r\nthree\r\nfour");
        let _ = screen.observe(b"\x1b[2;4r\x1b[4;1H\n");
        assert_eq!(screen.row_text(0).unwrap(), "one");
        assert_eq!(screen.row_text(1).unwrap(), "three");
        assert_eq!(screen.row_text(2).unwrap(), "four");

        let observation = screen.resize(size(5, 3));
        assert!(observation.clear_overlay);
        assert_eq!(screen.snapshot().size(), size(5, 3));
        assert!(screen.row_width(2).unwrap() <= 5);

        let mut origin = ScreenObserver::new(size(8, 5));
        let _ = origin.observe(b"\x1b[?6h\x1b[2;4r");
        assert_eq!(origin.snapshot().cursor(), CursorPosition::new(1, 0));
    }

    #[test]
    fn alternate_screen_and_hidden_cursor_suppress_overlay() {
        let mut screen = ScreenObserver::new(size(20, 5));
        let _ = screen.observe(b"prompt> ");
        assert!(screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1b[?1049hTUI\x1b[?25l");
        assert_eq!(screen.snapshot().buffer(), ScreenBuffer::Alternate);
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1b[?25h\x1b[?1049l");
        assert_eq!(screen.snapshot().buffer(), ScreenBuffer::Primary);
        assert!(screen.snapshot().overlay_safe());
        assert_eq!(screen.row_text(0).unwrap(), "prompt>");

        let _ = screen.observe(b"\x1b[?2026hbatched");
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1b[?2026l");
        assert!(screen.snapshot().overlay_safe());
    }

    #[test]
    fn unsupported_incomplete_and_oversized_sequences_fail_safe_until_reset() {
        let mut screen = ScreenObserver::new(size(20, 5));
        let _ = screen.observe(b"\x1b[");
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"999z");
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1bc");
        assert!(screen.snapshot().overlay_safe());

        let _ = screen.observe(b"\x1b[8;80;120t");
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1bc");
        assert!(screen.snapshot().overlay_safe());

        let _ = screen.observe(b"\x1b_private\x1b\\");
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x1bc");
        assert!(screen.snapshot().overlay_safe());

        let mut huge = Vec::with_capacity(MAX_CONTROL_STRING_BYTES + 32);
        huge.extend_from_slice(b"\x1b]0;");
        huge.resize(MAX_CONTROL_STRING_BYTES + 24, b'x');
        let _ = screen.observe(&huge);
        assert!(!screen.snapshot().overlay_safe());
        let _ = screen.observe(b"\x07\x1bc");
        assert!(screen.snapshot().overlay_safe());

        let combining = format!("a{}", "\u{301}".repeat(MAX_CELL_TEXT_BYTES));
        let _ = screen.observe(combining.as_bytes());
        assert!(!screen.snapshot().overlay_safe());
        assert!(screen.machine.surface().cell(0, 0).text.len() <= MAX_CELL_TEXT_BYTES);
        let _ = screen.observe(b"\x1bc");
        assert!(screen.snapshot().overlay_safe());
    }

    #[test]
    fn synchronizing_clears_state_left_open_by_a_departed_child() {
        let size = TerminalSize::new(80, 24).unwrap();
        let mut screen = ScreenObserver::new(size);

        let _ = screen.observe(b"\x1b[?2026h");
        assert!(!screen.snapshot().overlay_safe());

        screen.synchronize(CursorPosition::new(0, 0)).unwrap();

        assert!(
            screen.snapshot().overlay_safe(),
            "a synchronized update left open by a child kept suppressing the overlay"
        );
    }

    #[test]
    fn invalid_sizes_and_sync_positions_are_rejected() {
        assert!(TerminalSize::new(0, 24).is_err());
        assert!(TerminalSize::new(MAX_TERMINAL_DIMENSION, MAX_TERMINAL_DIMENSION).is_err());
        let mut screen = ScreenObserver::new(size(10, 3));
        assert_eq!(
            screen.synchronize(CursorPosition::new(3, 0)),
            Err(ScreenError::InvalidCursor(CursorPosition::new(3, 0)))
        );
    }

    #[test]
    fn debug_never_includes_screen_contents() {
        let mut screen = ScreenObserver::new(size(20, 5));
        let _ = screen.observe(b"hunter2");
        let debug = format!("{screen:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("nonempty_cells"));
    }
}
