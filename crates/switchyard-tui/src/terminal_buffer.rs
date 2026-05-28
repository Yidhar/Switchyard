//! ANSI-aware terminal screen buffer for provider mirror views.
//!
//! Goals:
//! - preserve a scrollback tail
//! - apply carriage-return redraw semantics
//! - handle a focused subset of CSI cursor / erase / styling commands
//! - expose both plain-text lines and styled `ratatui::text::Line` output

use std::collections::VecDeque;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use vte::{Params, Parser, Perform};

const DEFAULT_ROWS: usize = 40;
const DEFAULT_COLS: usize = 120;
const DEFAULT_SCROLLBACK: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalCell {
    ch: char,
    style: Style,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

impl TerminalCell {
    fn blank() -> Self {
        Self::default()
    }

    fn is_blank(&self) -> bool {
        self.ch == ' '
    }
}

pub struct TerminalScreenBuffer {
    parser: Parser,
    state: ScreenState,
}

impl TerminalScreenBuffer {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            parser: Parser::new(),
            state: ScreenState::new(rows, cols),
        }
    }

    pub fn apply_text(&mut self, text: &str) {
        for byte in text.as_bytes() {
            self.parser.advance(&mut self.state, *byte);
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.state.resize(rows, cols);
    }

    pub fn visible_lines(&self, max_lines: usize) -> Vec<String> {
        self.state.visible_lines(max_lines)
    }

    pub fn rendered_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        self.state.rendered_lines(max_lines)
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }
}

impl Default for TerminalScreenBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_ROWS, DEFAULT_COLS)
    }
}

struct ScreenState {
    rows: usize,
    cols: usize,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: Option<(usize, usize)>,
    current_style: Style,
    screen: Vec<Vec<TerminalCell>>,
    scrollback: VecDeque<Vec<TerminalCell>>,
    scrollback_limit: usize,
}

impl ScreenState {
    fn new(rows: usize, cols: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            rows,
            cols,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: None,
            current_style: Style::default(),
            screen: vec![blank_row(cols); rows],
            scrollback: VecDeque::new(),
            scrollback_limit: DEFAULT_SCROLLBACK,
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }

        let mut new_screen = vec![blank_row(cols); rows];
        for (dst_row, src_row) in new_screen.iter_mut().zip(self.screen.iter()) {
            for (dst_cell, src_cell) in dst_row.iter_mut().zip(src_row.iter()) {
                *dst_cell = *src_cell;
            }
        }

        self.rows = rows;
        self.cols = cols;
        self.screen = new_screen;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
    }

    fn visible_lines(&self, max_lines: usize) -> Vec<String> {
        if self.is_empty() {
            return Vec::new();
        }

        let mut lines: Vec<String> = self
            .scrollback
            .iter()
            .map(|row| row_to_string(row))
            .collect();
        let mut screen_lines = self
            .screen
            .iter()
            .map(|row| row_to_string(row))
            .collect::<Vec<_>>();

        let last_non_empty = screen_lines
            .iter()
            .rposition(|line| !line.is_empty())
            .unwrap_or(self.cursor_row.min(screen_lines.len().saturating_sub(1)));
        screen_lines.truncate(last_non_empty.saturating_add(1));
        lines.extend(screen_lines);

        if max_lines == 0 || lines.len() <= max_lines {
            lines
        } else {
            lines[lines.len() - max_lines..].to_vec()
        }
    }

    fn rendered_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        if self.is_empty() {
            return Vec::new();
        }

        let mut lines: Vec<Line<'static>> =
            self.scrollback.iter().map(|row| row_to_line(row)).collect();
        let mut screen_rows = self
            .screen
            .iter()
            .map(|row| row_to_line(row))
            .collect::<Vec<_>>();
        let screen_text = self
            .screen
            .iter()
            .map(|row| row_to_string(row))
            .collect::<Vec<_>>();

        let last_non_empty = screen_text
            .iter()
            .rposition(|line| !line.is_empty())
            .unwrap_or(self.cursor_row.min(screen_rows.len().saturating_sub(1)));
        screen_rows.truncate(last_non_empty.saturating_add(1));
        lines.extend(screen_rows);

        if max_lines == 0 || lines.len() <= max_lines {
            lines
        } else {
            lines[lines.len() - max_lines..].to_vec()
        }
    }

    fn is_empty(&self) -> bool {
        self.scrollback.is_empty() && self.screen.iter().all(|row| row_is_empty(row))
    }

    fn newline(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor_row += 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    fn tab(&mut self) {
        let next_tab = ((self.cursor_col / 8) + 1) * 8;
        self.cursor_col = next_tab.min(self.cols.saturating_sub(1));
    }

    fn put_char(&mut self, c: char) {
        if self.cursor_row >= self.rows {
            self.cursor_row = self.rows.saturating_sub(1);
        }
        if self.cursor_col >= self.cols {
            self.newline();
            self.cursor_col = 0;
        }

        if self.cursor_row < self.rows && self.cursor_col < self.cols {
            self.screen[self.cursor_row][self.cursor_col] = TerminalCell {
                ch: c,
                style: self.current_style,
            };
        }

        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.newline();
            self.cursor_col = 0;
        }
    }

    fn scroll_up(&mut self) {
        if let Some(first) = self.screen.first() {
            self.push_scrollback(first.clone());
        }
        self.screen.remove(0);
        self.screen.push(blank_row(self.cols));
        self.cursor_row = self.rows.saturating_sub(1);
    }

    fn push_scrollback(&mut self, row: Vec<TerminalCell>) {
        if self.scrollback.len() >= self.scrollback_limit {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
    }

    fn clear_screen(&mut self) {
        for row in &mut self.screen {
            clear_row(row);
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn clear_line_from_cursor(&mut self) {
        if let Some(row) = self.screen.get_mut(self.cursor_row) {
            for cell in row.iter_mut().skip(self.cursor_col) {
                *cell = TerminalCell::blank();
            }
        }
    }

    fn clear_line_to_cursor(&mut self) {
        if let Some(row) = self.screen.get_mut(self.cursor_row) {
            for cell in row.iter_mut().take(self.cursor_col.saturating_add(1)) {
                *cell = TerminalCell::blank();
            }
        }
    }

    fn clear_entire_line(&mut self) {
        if let Some(row) = self.screen.get_mut(self.cursor_row) {
            clear_row(row);
        }
    }

    fn clear_from_cursor_to_end_of_screen(&mut self) {
        self.clear_line_from_cursor();
        for row in self
            .screen
            .iter_mut()
            .skip(self.cursor_row.saturating_add(1))
        {
            clear_row(row);
        }
    }

    fn clear_from_start_to_cursor(&mut self) {
        for row in self.screen.iter_mut().take(self.cursor_row) {
            clear_row(row);
        }
        self.clear_line_to_cursor();
    }

    fn set_cursor_position(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows.saturating_sub(1));
        self.cursor_col = col.min(self.cols.saturating_sub(1));
    }

    fn apply_sgr(&mut self, params: &Params) {
        let flattened = params
            .iter()
            .map(|values| values.first().copied().unwrap_or(0))
            .collect::<Vec<_>>();

        if flattened.is_empty() {
            self.current_style = Style::default();
            return;
        }

        let mut index = 0usize;
        while index < flattened.len() {
            let code = flattened[index];
            match code {
                0 => self.current_style = Style::default(),
                1 => self.current_style = self.current_style.add_modifier(Modifier::BOLD),
                2 => self.current_style = self.current_style.add_modifier(Modifier::DIM),
                3 => self.current_style = self.current_style.add_modifier(Modifier::ITALIC),
                4 => self.current_style = self.current_style.add_modifier(Modifier::UNDERLINED),
                5 => self.current_style = self.current_style.add_modifier(Modifier::SLOW_BLINK),
                6 => self.current_style = self.current_style.add_modifier(Modifier::RAPID_BLINK),
                7 => self.current_style = self.current_style.add_modifier(Modifier::REVERSED),
                8 => self.current_style = self.current_style.add_modifier(Modifier::HIDDEN),
                9 => self.current_style = self.current_style.add_modifier(Modifier::CROSSED_OUT),
                21 | 22 => {
                    self.current_style = self
                        .current_style
                        .remove_modifier(Modifier::BOLD | Modifier::DIM)
                }
                23 => self.current_style = self.current_style.remove_modifier(Modifier::ITALIC),
                24 => self.current_style = self.current_style.remove_modifier(Modifier::UNDERLINED),
                25 => {
                    self.current_style = self
                        .current_style
                        .remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK)
                }
                27 => self.current_style = self.current_style.remove_modifier(Modifier::REVERSED),
                28 => self.current_style = self.current_style.remove_modifier(Modifier::HIDDEN),
                29 => {
                    self.current_style = self.current_style.remove_modifier(Modifier::CROSSED_OUT)
                }
                30..=37 => self.current_style.fg = Some(basic_ansi_color(code - 30, false)),
                39 => self.current_style.fg = None,
                40..=47 => self.current_style.bg = Some(basic_ansi_color(code - 40, false)),
                49 => self.current_style.bg = None,
                90..=97 => self.current_style.fg = Some(basic_ansi_color(code - 90, true)),
                100..=107 => self.current_style.bg = Some(basic_ansi_color(code - 100, true)),
                38 => {
                    if let Some((color, consumed)) = parse_extended_color(&flattened, index + 1) {
                        self.current_style.fg = Some(color);
                        index += consumed;
                    }
                }
                48 => {
                    if let Some((color, consumed)) = parse_extended_color(&flattened, index + 1) {
                        self.current_style.bg = Some(color);
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn param(params: &Params, index: usize, default: u16) -> u16 {
        params
            .iter()
            .nth(index)
            .and_then(|values| values.first().copied())
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }
}

impl Perform for ScreenState {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                self.carriage_return();
                self.newline();
            }
            b'\r' => self.carriage_return(),
            0x08 => self.backspace(),
            b'\t' => self.tab(),
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}

    fn esc_dispatch(&mut self, _: &[u8], _: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = Some((self.cursor_row, self.cursor_col)),
            b'8' => {
                if let Some((row, col)) = self.saved_cursor {
                    self.set_cursor_position(row, col);
                }
            }
            b'c' => {
                self.current_style = Style::default();
                self.clear_screen();
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _: bool, action: char) {
        let question_mode = intermediates == [b'?'];
        match action {
            'A' => {
                let amount = usize::from(Self::param(params, 0, 1));
                self.cursor_row = self.cursor_row.saturating_sub(amount);
            }
            'B' => {
                let amount = usize::from(Self::param(params, 0, 1));
                self.cursor_row = (self.cursor_row + amount).min(self.rows.saturating_sub(1));
            }
            'C' => {
                let amount = usize::from(Self::param(params, 0, 1));
                self.cursor_col = (self.cursor_col + amount).min(self.cols.saturating_sub(1));
            }
            'D' => {
                let amount = usize::from(Self::param(params, 0, 1));
                self.cursor_col = self.cursor_col.saturating_sub(amount);
            }
            'G' => {
                let col = usize::from(Self::param(params, 0, 1).saturating_sub(1));
                self.cursor_col = col.min(self.cols.saturating_sub(1));
            }
            'H' | 'f' => {
                let row = usize::from(Self::param(params, 0, 1).saturating_sub(1));
                let col = usize::from(Self::param(params, 1, 1).saturating_sub(1));
                self.set_cursor_position(row, col);
            }
            'J' => match Self::param(params, 0, 0) {
                0 => self.clear_from_cursor_to_end_of_screen(),
                1 => self.clear_from_start_to_cursor(),
                2 => self.clear_screen(),
                _ => {}
            },
            'K' => match Self::param(params, 0, 0) {
                0 => self.clear_line_from_cursor(),
                1 => self.clear_line_to_cursor(),
                2 => self.clear_entire_line(),
                _ => {}
            },
            's' => self.saved_cursor = Some((self.cursor_row, self.cursor_col)),
            'u' => {
                if let Some((row, col)) = self.saved_cursor {
                    self.set_cursor_position(row, col);
                }
            }
            'm' => self.apply_sgr(params),
            'h' | 'l' if question_mode && Self::param(params, 0, 0) == 1049 => {
                // Handle alt-screen enter/leave conservatively by clearing.
                self.clear_screen();
            }
            _ => {}
        }
    }
}

fn blank_row(cols: usize) -> Vec<TerminalCell> {
    vec![TerminalCell::blank(); cols.max(1)]
}

fn clear_row(row: &mut [TerminalCell]) {
    for cell in row.iter_mut() {
        *cell = TerminalCell::blank();
    }
}

fn row_is_empty(row: &[TerminalCell]) -> bool {
    row.iter().all(TerminalCell::is_blank)
}

fn row_to_string(row: &[TerminalCell]) -> String {
    let mut text = row.iter().map(|cell| cell.ch).collect::<String>();
    while text.ends_with(' ') {
        text.pop();
    }
    text
}

fn row_to_line(row: &[TerminalCell]) -> Line<'static> {
    let trimmed_len = row
        .iter()
        .rposition(|cell| !cell.is_blank())
        .map(|index| index + 1)
        .unwrap_or(0);

    if trimmed_len == 0 {
        return Line::from(String::new());
    }

    let mut spans = Vec::new();
    let mut current_style = row[0].style;
    let mut current_text = String::new();

    for cell in row.iter().take(trimmed_len) {
        if cell.style == current_style {
            current_text.push(cell.ch);
        } else {
            spans.push(Span::styled(
                std::mem::take(&mut current_text),
                current_style,
            ));
            current_style = cell.style;
            current_text.push(cell.ch);
        }
    }

    if !current_text.is_empty() {
        spans.push(Span::styled(current_text, current_style));
    }

    Line::from(spans)
}

fn basic_ansi_color(index: u16, bright: bool) -> Color {
    match (bright, index) {
        (false, 0) => Color::Black,
        (false, 1) => Color::Red,
        (false, 2) => Color::Green,
        (false, 3) => Color::Yellow,
        (false, 4) => Color::Blue,
        (false, 5) => Color::Magenta,
        (false, 6) => Color::Cyan,
        (false, 7) => Color::Gray,
        (true, 0) => Color::DarkGray,
        (true, 1) => Color::LightRed,
        (true, 2) => Color::LightGreen,
        (true, 3) => Color::LightYellow,
        (true, 4) => Color::LightBlue,
        (true, 5) => Color::LightMagenta,
        (true, 6) => Color::LightCyan,
        (true, 7) => Color::White,
        _ => Color::Reset,
    }
}

fn parse_extended_color(params: &[u16], start: usize) -> Option<(Color, usize)> {
    let mode = *params.get(start)?;
    match mode {
        2 => {
            let red = u8::try_from(*params.get(start + 1)?).ok()?;
            let green = u8::try_from(*params.get(start + 2)?).ok()?;
            let blue = u8::try_from(*params.get(start + 3)?).ok()?;
            Some((Color::Rgb(red, green, blue), 4))
        }
        5 => {
            let index = u8::try_from(*params.get(start + 1)?).ok()?;
            Some((Color::Indexed(index), 2))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carriage_return_redraw_replaces_same_line_content() {
        let mut screen = TerminalScreenBuffer::new(10, 40);
        screen.apply_text("Progress 1%\rProgress 42%\r");

        assert_eq!(screen.visible_lines(10), vec!["Progress 42%".to_string()]);
    }

    #[test]
    fn ansi_erase_line_clears_previous_content_before_redraw() {
        let mut screen = TerminalScreenBuffer::new(10, 40);
        screen.apply_text("Downloading 10%\r\x1b[2KDownloading 90%\r");

        assert_eq!(
            screen.visible_lines(10),
            vec!["Downloading 90%".to_string()]
        );
    }

    #[test]
    fn cursor_positioning_updates_existing_screen_cells() {
        let mut screen = TerminalScreenBuffer::new(5, 20);
        screen.apply_text("hello\nworld");
        screen.apply_text("\x1b[1;1HHEY");

        let lines = screen.visible_lines(5);
        assert_eq!(lines[0], "HEYlo");
        assert_eq!(lines[1], "world");
    }

    #[test]
    fn sgr_colors_and_modifiers_render_to_styled_lines() {
        let mut screen = TerminalScreenBuffer::new(5, 40);
        screen.apply_text("\x1b[1;31mERR\x1b[0m ok");

        let lines = screen.rendered_lines(5);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "ERR");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[0].spans[1].content, " ok");
        assert_eq!(lines[0].spans[1].style, Style::default());
    }

    #[test]
    fn extended_rgb_color_is_preserved_in_screen_render() {
        let mut screen = TerminalScreenBuffer::new(5, 40);
        screen.apply_text("\x1b[38;2;12;34;56mhi");

        let lines = screen.rendered_lines(5);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(12, 34, 56)));
    }
}
