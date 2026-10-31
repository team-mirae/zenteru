//! Terminal state: the primary and alternate screens, scrollback, cursor,
//! modes, and the escape-sequence semantics (xterm-compatible subset).
//!
//! Zenkaku: a two-column character occupies a `WIDE` cell followed by a
//! `SPACER` cell. Overwriting either half clears the other, and a wide
//! character that does not fit at the right margin wraps (leaving a
//! padding spacer that reflow knows to drop).

use std::collections::VecDeque;

use zenteru_text::cwidth::char_width;

use crate::base64;
use crate::cell::{flags, Attrs, Cell, Color, Row};
use crate::parser::{Params, Parser, Perform};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseMode {
    #[default]
    Off,
    /// 9 / 1000: presses (and releases for 1000).
    Click,
    /// 1002: + motion while a button is held.
    Drag,
    /// 1003: + all motion.
    Motion,
}

#[derive(Clone, Debug)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub insert: bool,
    pub newline: bool,
    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub reverse_video: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub alt_scroll: bool,
    pub sync_output: bool,
    /// DECSCUSR: 0/1 blinking block, 2 block, 3/4 underline, 5/6 bar.
    pub cursor_style: u8,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            origin: false,
            insert: false,
            newline: false,
            cursor_visible: true,
            cursor_blink: true,
            reverse_video: false,
            bracketed_paste: false,
            focus_events: false,
            mouse: MouseMode::Off,
            mouse_sgr: false,
            alt_scroll: true,
            sync_output: false,
            cursor_style: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Charset {
    #[default]
    Ascii,
    DecGraphics,
}

#[derive(Clone, Debug, Default)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
    pub attrs: Attrs,
    pub pending_wrap: bool,
}

#[derive(Clone, Debug, Default)]
struct Saved {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    gl: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PromptPhase {
    #[default]
    Unknown,
    /// Prompt drawn, user editing the command line (after OSC 133;B).
    Editing,
    /// Command running (after OSC 133;C).
    Running,
    /// Command finished (after OSC 133;D).
    Finished,
}

#[derive(Clone, Debug, Default)]
pub struct PromptState {
    pub phase: PromptPhase,
    /// Where the command line input starts (absolute line, column).
    pub input_start: Option<(u64, usize)>,
    pub command: Option<String>,
    pub exit_status: Option<i32>,
    pub cwd: Option<String>,
    /// OSC 633;E arrived for the command about to run.
    command_fresh: bool,
    /// Absolute lines where prompts started, for ⌘↑ / ⌘↓.
    pub marks: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Title(String),
    Bell,
    Clipboard(String),
    Cwd(String),
    Prompt(PromptPhase),
    UserVar(String, String),
    Notify(String, String),
    /// Decoded JSON payload of `OSC 1337 ; ZenteruCompletions=…`.
    Completions(String),
}

pub struct Terminal {
    pub cols: usize,
    pub rows: usize,
    primary: Vec<Row>,
    alt: Vec<Row>,
    alt_active: bool,
    pub scrollback: VecDeque<Row>,
    pub scrollback_limit: usize,
    /// Lines dropped off the top of scrollback, ever. Absolute line `n` is
    /// `scrollback[n - dropped]` or the screen below it.
    pub dropped: u64,
    pub cursor: Cursor,
    saved: [Saved; 2],
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    pub modes: Modes,
    charsets: [Charset; 2],
    gl: usize,
    pub title: String,
    title_stack: Vec<String>,
    responses: Vec<u8>,
    events: Vec<Event>,
    /// Rows scrolled back from the bottom in the view.
    pub display_offset: usize,
    links: Vec<String>,
    parser: Option<Parser>,
    last_char: Option<char>,
    rng: u64,
    pub prompt: PromptState,
    /// OSC 4 / 10 / 11 / 12 overrides.
    pub palette: [Option<(u8, u8, u8)>; 256],
    pub fg_override: Option<(u8, u8, u8)>,
    pub bg_override: Option<(u8, u8, u8)>,
    pub cursor_color: Option<(u8, u8, u8)>,
    /// Cell size in pixels, for `CSI 14 t` / `16 t`.
    pub cell_px: (u16, u16),
    pub user_vars: Vec<(String, String)>,
    /// Bumped on every change that affects rendering.
    pub generation: u64,
    /// Absolute line where the screen started after the last `clear`
    /// (ED 2 / ED 3). Resizing never pulls lines above it back on screen.
    pub screen_floor: Option<u64>,
}

const DEFAULT_SCROLLBACK: usize = 10_000;

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let a = Attrs::default();
        Self {
            cols,
            rows,
            primary: (0..rows).map(|_| Row::new(cols, &a)).collect(),
            alt: (0..rows).map(|_| Row::new(cols, &a)).collect(),
            alt_active: false,
            scrollback: VecDeque::new(),
            scrollback_limit: DEFAULT_SCROLLBACK,
            dropped: 0,
            cursor: Cursor::default(),
            saved: Default::default(),
            top: 0,
            bottom: rows - 1,
            tabs: (0..cols).map(|i| i % 8 == 0).collect(),
            modes: Modes::default(),
            charsets: Default::default(),
            gl: 0,
            title: String::new(),
            title_stack: Vec::new(),
            responses: Vec::new(),
            events: Vec::new(),
            display_offset: 0,
            links: Vec::new(),
            parser: Some(Parser::new()),
            last_char: None,
            rng: crate::lineref::seed(),
            prompt: PromptState::default(),
            palette: [None; 256],
            fg_override: None,
            bg_override: None,
            cursor_color: None,
            cell_px: (8, 16),
            user_vars: Vec::new(),
            generation: 0,
            screen_floor: None,
        }
    }

    // ---- public API ------------------------------------------------------

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut p = self.parser.take().unwrap_or_default();
        p.advance(self, bytes);
        self.parser = Some(p);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn alt_screen(&self) -> bool {
        self.alt_active
    }

    pub fn screen(&self) -> &[Row] {
        if self.alt_active { &self.alt } else { &self.primary }
    }

    fn grid(&mut self) -> &mut Vec<Row> {
        if self.alt_active { &mut self.alt } else { &mut self.primary }
    }

    pub fn link(&self, id: u16) -> Option<&str> {
        id.checked_sub(1).and_then(|i| self.links.get(i as usize)).map(String::as_str)
    }

    /// Absolute index of the first screen row.
    pub fn screen_base(&self) -> u64 {
        self.dropped + self.scrollback.len() as u64
    }

    /// Total lines (scrollback + screen), as absolute end.
    pub fn abs_end(&self) -> u64 {
        self.screen_base() + self.rows as u64
    }

    /// The row at absolute line `abs`, if still held.
    pub fn line(&self, abs: u64) -> Option<&Row> {
        let rel = abs.checked_sub(self.dropped)? as usize;
        if rel < self.scrollback.len() {
            self.scrollback.get(rel)
        } else {
            self.screen().get(rel - self.scrollback.len())
        }
    }

    /// The row shown at view row `y`, accounting for `display_offset`.
    pub fn view_row(&self, y: usize) -> Option<&Row> {
        let base = self.screen_base() - self.display_offset as u64;
        self.line(base + y as u64)
    }

    /// Absolute line shown at view row `y`.
    pub fn view_abs(&self, y: usize) -> u64 {
        self.screen_base() - self.display_offset as u64 + y as u64
    }

    pub fn scroll_view(&mut self, delta: isize) {
        let max = if self.alt_active { 0 } else { self.scrollback.len() };
        let off = (self.display_offset as isize + delta).clamp(0, max as isize);
        if off as usize != self.display_offset {
            self.display_offset = off as usize;
            self.generation += 1;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.display_offset != 0 {
            self.display_offset = 0;
            self.generation += 1;
        }
    }

    /// Text between two absolute positions (inclusive start, exclusive end
    /// column), joining soft-wrapped rows without newlines.
    pub fn text_range(&self, start: (u64, usize), end: (u64, usize)) -> String {
        let mut out = String::new();
        if end < start {
            return out;
        }
        for abs in start.0..=end.0 {
            let Some(row) = self.line(abs) else { continue };
            let from = if abs == start.0 { start.1 } else { 0 };
            let to = if abs == end.0 { end.1.min(row.cells.len()) } else { row.cells.len() };
            let mut s = String::new();
            for c in row.cells.get(from..to).unwrap_or(&[]) {
                if !c.is_spacer() {
                    c.push_text(&mut s);
                }
            }
            if abs != end.0 && !row.wrapped {
                s.truncate(s.trim_end_matches(' ').len());
                out.push_str(&s);
                out.push('\n');
            } else {
                out.push_str(&s);
            }
        }
        out
    }

    /// Absolute position of the cursor.
    pub fn cursor_abs(&self) -> (u64, usize) {
        (self.screen_base() + self.cursor.y as u64, self.cursor.x)
    }

    /// The command line being edited: text from the OSC 133;B mark (or the
    /// start of the cursor's logical line) to the end of that logical line,
    /// plus the cursor's grapheme index in it.
    pub fn edit_line(&self) -> (String, usize) {
        let (cy, cx) = self.cursor_abs();
        let start = match self.prompt.input_start {
            Some(s) if self.prompt.phase == PromptPhase::Editing && s <= (cy, cx) => s,
            _ => {
                let mut l = cy;
                while l > self.dropped && self.line(l - 1).is_some_and(|r| r.wrapped) {
                    l -= 1;
                }
                (l, 0)
            }
        };
        let mut end_line = cy;
        while self.line(end_line).is_some_and(|r| r.wrapped) {
            end_line += 1;
        }
        let before = self.text_range(start, (cy, cx));
        let mut all = self.text_range(start, (end_line, self.cols));
        all.truncate(all.trim_end_matches(' ').len());
        let cursor = zenteru_text::text::grapheme_count(&before);
        (all, cursor)
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        // Primary screen reflows; the alternate screen is cropped / padded.
        let (pcx, pcy) = if self.alt_active {
            (self.saved[0].cursor.x, self.saved[0].cursor.y)
        } else {
            (self.cursor.x, self.cursor.y)
        };
        let mut lines: Vec<Row> = self.scrollback.drain(..).collect();
        let sb_len = lines.len();
        lines.append(&mut self.primary);
        let dropped_before = self.dropped;
        let floor_row = self.screen_floor.and_then(|f| f.checked_sub(dropped_before)).map(|f| f as usize).filter(|&f| f <= sb_len + pcy);
        let (mut new_lines, (nx, ny), new_floor) = reflow(lines, (pcx, sb_len + pcy), cols, floor_row);
        // Split into scrollback + screen with the cursor on screen.
        let total = new_lines.len();
        let mut start = total.saturating_sub(rows);
        // Don't pull lines from before a `clear` back onto the screen.
        if let Some(fl) = new_floor {
            start = start.max(fl);
        }
        if ny < start {
            start = ny;
        }
        new_lines.truncate((start + rows).max(ny + 1).min(total));
        let screen: Vec<Row> = new_lines.split_off(start);
        self.scrollback = new_lines.into();
        self.primary = screen;
        while self.primary.len() < rows {
            self.primary.push(Row::new(cols, &Attrs::default()));
        }
        for r in &mut self.primary {
            r.resize(cols);
        }
        self.screen_floor = new_floor.map(|fl| dropped_before + fl as u64).filter(|_| floor_row.is_some());
        let new_cursor = (nx.min(cols - 1), ny - start);
        if self.alt_active {
            self.saved[0].cursor.x = new_cursor.0;
            self.saved[0].cursor.y = new_cursor.1.min(rows - 1);
            self.alt.truncate(rows);
            while self.alt.len() < rows {
                self.alt.push(Row::new(cols, &Attrs::default()));
            }
            for r in &mut self.alt {
                r.resize(cols);
                r.wrapped = false;
            }
            self.cursor.x = self.cursor.x.min(cols - 1);
            self.cursor.y = self.cursor.y.min(rows - 1);
        } else {
            self.cursor.x = new_cursor.0;
            self.cursor.y = new_cursor.1.min(rows - 1);
            // Keep the alt screen sized too.
            self.alt = (0..rows).map(|_| Row::new(cols, &Attrs::default())).collect();
        }
        self.cursor.pending_wrap = false;
        // Prompt marks on reflowed lines are approximate; drop the input start.
        self.prompt.input_start = None;
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.tabs = (0..cols).map(|i| i % 8 == 0).collect();
        self.trim_scrollback();
        self.display_offset = self.display_offset.min(self.scrollback.len());
        self.generation += 1;
    }

    // ---- internals -------------------------------------------------------


    fn trim_scrollback(&mut self) {
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
            self.dropped += 1;
        }
    }

    fn blank_row(&self) -> Row {
        let a = Attrs { bg: self.cursor.attrs.bg, ..Attrs::default() };
        Row::new(self.cols, &a)
    }

    fn erase_attrs(&self) -> Attrs {
        Attrs { bg: self.cursor.attrs.bg, ..Attrs::default() }
    }

    /// Scroll lines `top..=bottom` up by `n`.
    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let to_history = !self.alt_active && top == 0;
        for _ in 0..n {
            let blank = self.blank_row();
            let row = self.grid().remove(top);
            self.grid().insert(bottom, blank);
            if to_history {
                self.scrollback.push_back(row);
                if self.display_offset > 0 {
                    // Keep the viewed content in place while scrolled back.
                    self.display_offset += 1;
                }
            }
        }
        if to_history {
            let before = self.scrollback.len();
            self.trim_scrollback();
            let trimmed = before - self.scrollback.len();
            self.display_offset = self.display_offset.min(self.scrollback.len());
            let _ = trimmed;
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        for _ in 0..n {
            let blank = self.blank_row();
            self.grid().remove(bottom);
            self.grid().insert(top, blank);
        }
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.y + 1 < self.rows {
            self.cursor.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.top {
            self.scroll_down(1);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Clear the other half of any zenkaku character overlapping `x`.
    fn unsplit(&mut self, y: usize, x: usize) {
        let a = self.erase_attrs();
        let cols = self.cols;
        let row = &mut self.grid()[y];
        if x >= cols {
            return;
        }
        if row.cells[x].is_spacer() && x > 0 && row.cells[x - 1].is_wide() {
            row.cells[x - 1] = Cell::blank(&a);
        }
        if row.cells[x].is_wide() && x + 1 < cols {
            row.cells[x + 1] = Cell::blank(&a);
        }
    }

    fn print_char(&mut self, c: char) {
        let c = if self.charsets[self.gl] == Charset::DecGraphics { dec_graphics(c) } else { c };
        let w = char_width(c);
        if w == 0 {
            self.combine(c);
            return;
        }
        let cols = self.cols;
        if self.cursor.pending_wrap && self.modes.autowrap {
            let y = self.cursor.y;
            self.grid()[y].wrapped = true;
            self.cursor.x = 0;
            self.linefeed();
        }
        self.cursor.pending_wrap = false;
        if w == 2 && self.cursor.x == cols - 1 {
            if self.modes.autowrap {
                let (x, y) = (self.cursor.x, self.cursor.y);
                self.unsplit(y, x);
                let mut pad = Cell::blank(&self.erase_attrs());
                pad.flags = flags::SPACER;
                self.grid()[y].cells[x] = pad;
                self.grid()[y].wrapped = true;
                self.cursor.x = 0;
                self.linefeed();
            } else {
                return;
            }
        }
        let (x, y) = (self.cursor.x, self.cursor.y);
        if self.modes.insert {
            let a = self.erase_attrs();
            let row = &mut self.grid()[y];
            for _ in 0..w {
                row.cells.insert(x, Cell::blank(&a));
                row.cells.pop();
            }
            if row.cells.last().is_some_and(Cell::is_wide) {
                *row.cells.last_mut().unwrap() = Cell::blank(&a);
            }
        }
        self.unsplit(y, x);
        if w == 2 {
            self.unsplit(y, x + 1);
        }
        let at = self.cursor.attrs;
        let mut cell = Cell { c, extra: None, fg: at.fg, bg: at.bg, ul: at.ul, flags: at.flags, link: at.link };
        if w == 2 {
            cell.flags |= flags::WIDE;
            let mut sp = cell.clone();
            sp.c = ' ';
            sp.flags = (sp.flags & !flags::WIDE) | flags::SPACER;
            self.grid()[y].cells[x + 1] = sp;
        }
        self.grid()[y].cells[x] = cell;
        if self.grid()[y].id == 0 {
            let id = crate::lineref::uuid7(&mut self.rng);
            self.grid()[y].id = id;
        }
        self.last_char = Some(c);
        if x + w >= cols {
            self.cursor.x = cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.x = x + w;
        }
    }

    /// Attach a zero-width code point to the previous cell.
    fn combine(&mut self, c: char) {
        let (mut x, y) = (self.cursor.x, self.cursor.y);
        if !self.cursor.pending_wrap {
            if x == 0 {
                return;
            }
            x -= 1;
        }
        let row = &mut self.grid()[y];
        if row.cells[x].is_spacer() && x > 0 {
            x -= 1;
        }
        row.cells[x].append(c);
    }

    fn set_x(&mut self, x: usize) {
        self.cursor.x = x.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn set_y(&mut self, y: usize) {
        let (lo, hi) = if self.modes.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.cursor.y = (y + if self.modes.origin { self.top } else { 0 }).clamp(lo, hi);
        self.cursor.pending_wrap = false;
    }

    fn move_rel(&mut self, dy: isize) {
        let (lo, hi) = if self.cursor.y >= self.top && self.cursor.y <= self.bottom {
            (self.top, self.bottom)
        } else {
            (0, self.rows - 1)
        };
        self.cursor.y = (self.cursor.y as isize + dy).clamp(lo as isize, hi as isize) as usize;
        self.cursor.pending_wrap = false;
    }

    fn erase_cells(&mut self, y: usize, from: usize, to: usize) {
        let a = self.erase_attrs();
        let to = to.min(self.cols);
        if from >= to {
            return;
        }
        self.unsplit(y, from);
        if to > 0 {
            self.unsplit(y, to - 1);
        }
        for c in &mut self.grid()[y].cells[from..to] {
            *c = Cell::blank(&a);
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        match mode {
            0 => {
                self.erase_cells(y, x, self.cols);
                self.grid()[y].wrapped = false;
                for r in y + 1..self.rows {
                    let b = self.blank_row();
                    self.grid()[r] = b;
                }
            }
            1 => {
                for r in 0..y {
                    let b = self.blank_row();
                    self.grid()[r] = b;
                }
                self.erase_cells(y, 0, x + 1);
            }
            2 => {
                // Like most terminals, push a non-empty primary screen into
                // scrollback instead of losing it (`clear`).
                if !self.alt_active {
                    let used = self.primary.iter().rposition(|r| r.used() > 0);
                    if let Some(last) = used {
                        let mut rows = self.primary[..=last].to_vec();
                        // What was on screen ends here: don't let reflow join
                        // it to whatever is printed after the clear.
                        if let Some(r) = rows.last_mut() {
                            r.wrapped = false;
                        }
                        for r in rows {
                            self.scrollback.push_back(r);
                        }
                        self.trim_scrollback();
                    }
                    self.screen_floor = Some(self.screen_base());
                }
                for r in 0..self.rows {
                    let b = self.blank_row();
                    self.grid()[r] = b;
                }
            }
            3 => {
                self.dropped += self.scrollback.len() as u64;
                self.scrollback.clear();
                if !self.alt_active {
                    self.screen_floor = Some(self.screen_base());
                }
                self.display_offset = 0;
                self.prompt.marks.clear();
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        match mode {
            0 => {
                self.erase_cells(y, x, self.cols);
                self.grid()[y].wrapped = false;
            }
            1 => self.erase_cells(y, 0, x + 1),
            2 => {
                self.erase_cells(y, 0, self.cols);
                self.grid()[y].wrapped = false;
            }
            _ => {}
        }
    }

    fn insert_lines(&mut self, n: usize) {
        let y = self.cursor.y;
        if y < self.top || y > self.bottom {
            return;
        }
        let n = n.min(self.bottom - y + 1);
        for _ in 0..n {
            let b = self.blank_row();
            let bottom = self.bottom;
            self.grid().remove(bottom);
            self.grid().insert(y, b);
        }
        self.cursor.x = 0;
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        let y = self.cursor.y;
        if y < self.top || y > self.bottom {
            return;
        }
        let n = n.min(self.bottom - y + 1);
        for _ in 0..n {
            let b = self.blank_row();
            let bottom = self.bottom;
            self.grid().remove(y);
            self.grid().insert(bottom, b);
        }
        self.cursor.x = 0;
        self.cursor.pending_wrap = false;
    }

    fn insert_chars(&mut self, n: usize) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let a = self.erase_attrs();
        self.unsplit(y, x);
        let cols = self.cols;
        let row = &mut self.grid()[y];
        let n = n.min(cols - x);
        for _ in 0..n {
            row.cells.insert(x, Cell::blank(&a));
        }
        row.cells.truncate(cols);
        if row.cells[cols - 1].is_wide() {
            row.cells[cols - 1] = Cell::blank(&a);
        }
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let a = self.erase_attrs();
        self.unsplit(y, x);
        let cols = self.cols;
        let n = n.min(cols - x);
        if x + n < cols {
            self.unsplit(y, x + n);
        }
        let row = &mut self.grid()[y];
        row.cells.drain(x..x + n);
        row.cells.resize(cols, Cell::blank(&a));
        self.cursor.pending_wrap = false;
    }

    fn save_cursor(&mut self) {
        let i = self.alt_active as usize;
        self.saved[i] = Saved { cursor: self.cursor.clone(), origin: self.modes.origin, charsets: self.charsets, gl: self.gl };
    }

    fn restore_cursor(&mut self) {
        let i = self.alt_active as usize;
        let s = self.saved[i].clone();
        self.cursor = s.cursor;
        self.cursor.x = self.cursor.x.min(self.cols - 1);
        self.cursor.y = self.cursor.y.min(self.rows - 1);
        self.modes.origin = s.origin;
        self.charsets = s.charsets;
        self.gl = s.gl;
    }

    fn enter_alt(&mut self, save: bool, clear: bool) {
        if self.alt_active {
            return;
        }
        if save {
            self.save_cursor();
        }
        self.alt_active = true;
        self.display_offset = 0;
        if clear {
            for r in 0..self.rows {
                let b = Row::new(self.cols, &Attrs::default());
                self.alt[r] = b;
            }
        }
    }

    fn leave_alt(&mut self, restore: bool) {
        if !self.alt_active {
            return;
        }
        self.alt_active = false;
        if restore {
            self.restore_cursor();
        }
    }

    fn reset(&mut self) {
        let (c, r, px) = (self.cols, self.rows, self.cell_px);
        let sb = std::mem::take(&mut self.scrollback);
        let dropped = self.dropped;
        *self = Terminal::new(c, r);
        self.scrollback = sb;
        self.dropped = dropped;
        self.cell_px = px;
    }

    fn respond(&mut self, s: &str) {
        self.responses.extend_from_slice(s.as_bytes());
    }

    fn set_mode(&mut self, private: bool, mode: u16, on: bool) {
        if !private {
            match mode {
                4 => self.modes.insert = on,
                20 => self.modes.newline = on,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = on,
            5 => self.modes.reverse_video = on,
            6 => {
                self.modes.origin = on;
                self.set_x(0);
                self.set_y(0);
            }
            7 => self.modes.autowrap = on,
            12 => self.modes.cursor_blink = on,
            25 => self.modes.cursor_visible = on,
            9 | 1000 => self.modes.mouse = if on { MouseMode::Click } else { MouseMode::Off },
            1002 => self.modes.mouse = if on { MouseMode::Drag } else { MouseMode::Off },
            1003 => self.modes.mouse = if on { MouseMode::Motion } else { MouseMode::Off },
            1004 => self.modes.focus_events = on,
            1006 => self.modes.mouse_sgr = on,
            1007 => self.modes.alt_scroll = on,
            47 | 1047 => {
                if on {
                    self.enter_alt(false, mode == 1047)
                } else {
                    if mode == 1047 {
                        for r in 0..self.rows {
                            self.alt[r] = Row::new(self.cols, &Attrs::default());
                        }
                    }
                    self.leave_alt(false)
                }
            }
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => {
                if on {
                    self.enter_alt(true, true)
                } else {
                    self.leave_alt(true)
                }
            }
            66 => self.modes.app_keypad = on,
            2004 => self.modes.bracketed_paste = on,
            2026 => self.modes.sync_output = on,
            _ => {}
        }
    }

    fn mode_state(&self, private: bool, mode: u16) -> u8 {
        let b = |v: bool| if v { 1 } else { 2 };
        if !private {
            return match mode {
                4 => b(self.modes.insert),
                20 => b(self.modes.newline),
                _ => 0,
            };
        }
        match mode {
            1 => b(self.modes.app_cursor),
            5 => b(self.modes.reverse_video),
            6 => b(self.modes.origin),
            7 => b(self.modes.autowrap),
            12 => b(self.modes.cursor_blink),
            25 => b(self.modes.cursor_visible),
            1000 => b(self.modes.mouse == MouseMode::Click),
            1002 => b(self.modes.mouse == MouseMode::Drag),
            1003 => b(self.modes.mouse == MouseMode::Motion),
            1004 => b(self.modes.focus_events),
            1006 => b(self.modes.mouse_sgr),
            1049 | 1047 | 47 => b(self.alt_active),
            2004 => b(self.modes.bracketed_paste),
            2026 => b(self.modes.sync_output),
            _ => 0,
        }
    }

    fn sgr(&mut self, params: &Params) {
        let groups = params.groups();
        if groups.is_empty() {
            self.cursor.attrs = Attrs { link: self.cursor.attrs.link, ..Attrs::default() };
            return;
        }
        let a = &mut self.cursor.attrs;
        let mut i = 0;
        while i < groups.len() {
            let g = groups[i];
            match g[0] {
                0 => *a = Attrs { link: a.link, ..Attrs::default() },
                1 => a.flags |= flags::BOLD,
                2 => a.flags |= flags::DIM,
                3 => a.flags |= flags::ITALIC,
                4 => {
                    a.flags &= !flags::ANY_UNDERLINE;
                    match g.get(1).copied() {
                        Some(0) => {}
                        Some(2) => a.flags |= flags::DOUBLE_UNDERLINE,
                        Some(3) => a.flags |= flags::CURLY_UNDERLINE,
                        _ => a.flags |= flags::UNDERLINE,
                    }
                }
                5 | 6 => a.flags |= flags::BLINK,
                7 => a.flags |= flags::INVERSE,
                8 => a.flags |= flags::HIDDEN,
                9 => a.flags |= flags::STRIKE,
                21 => {
                    a.flags &= !flags::ANY_UNDERLINE;
                    a.flags |= flags::DOUBLE_UNDERLINE
                }
                22 => a.flags &= !(flags::BOLD | flags::DIM),
                23 => a.flags &= !flags::ITALIC,
                24 => a.flags &= !flags::ANY_UNDERLINE,
                25 => a.flags &= !flags::BLINK,
                27 => a.flags &= !flags::INVERSE,
                28 => a.flags &= !flags::HIDDEN,
                29 => a.flags &= !flags::STRIKE,
                n @ 30..=37 => a.fg = Color::Indexed((n - 30) as u8),
                39 => a.fg = Color::Default,
                n @ 40..=47 => a.bg = Color::Indexed((n - 40) as u8),
                49 => a.bg = Color::Default,
                59 => a.ul = Color::Default,
                n @ 90..=97 => a.fg = Color::Indexed((n - 90 + 8) as u8),
                n @ 100..=107 => a.bg = Color::Indexed((n - 100 + 8) as u8),
                n @ (38 | 48 | 58) => {
                    let (color, used) = if g.len() > 1 {
                        (ext_color(&g[1..], true), 0)
                    } else {
                        let rest: Vec<u16> = groups[i + 1..].iter().map(|g| g[0]).collect();
                        let c = ext_color(&rest, false);
                        let used = match rest.first() {
                            Some(2) => 4,
                            Some(5) => 2,
                            _ => rest.len(),
                        };
                        (c, used)
                    };
                    i += used;
                    if let Some(c) = color {
                        match n {
                            38 => a.fg = c,
                            48 => a.bg = c,
                            _ => a.ul = c,
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        let s = String::from_utf8_lossy(data);
        let (num, rest) = s.split_once(';').unwrap_or((&s, ""));
        match num {
            "0" | "2" => {
                self.title = rest.chars().take(256).collect();
                self.events.push(Event::Title(self.title.clone()));
            }
            "1" => {}
            "4" => {
                let parts: Vec<&str> = rest.split(';').collect();
                for pair in parts.chunks(2) {
                    let (Some(i), Some(spec)) = (pair.first().and_then(|i| i.parse::<u8>().ok()), pair.get(1)) else { continue };
                    if *spec == "?" {
                        let c = self.palette[i as usize].unwrap_or_else(|| {
                            let p = zenteru_text::palette::ansi256(i);
                            (p.0, p.1, p.2)
                        });
                        let r = format!("\x1b]4;{i};{}\x1b\\", rgb_spec(c));
                        self.respond(&r);
                    } else if let Some(c) = parse_color(spec) {
                        self.palette[i as usize] = Some(c);
                    }
                }
            }
            "7" => {
                // file://host/path, percent-encoded.
                let path = rest.strip_prefix("file://").map(|r| r.find('/').map_or("", |i| &r[i..])).unwrap_or(rest);
                let path = percent_decode(path);
                self.prompt.cwd = Some(path.clone());
                self.events.push(Event::Cwd(path));
            }
            "8" => {
                // OSC 8 ; params ; uri
                let uri = rest.split_once(';').map_or("", |(_, u)| u);
                if uri.is_empty() {
                    self.cursor.attrs.link = 0;
                } else {
                    let id = match self.links.iter().position(|l| l == uri) {
                        Some(i) => i + 1,
                        None => {
                            if self.links.len() >= u16::MAX as usize - 1 {
                                self.links.clear();
                            }
                            self.links.push(uri.to_string());
                            self.links.len()
                        }
                    };
                    self.cursor.attrs.link = id as u16;
                }
            }
            "9" => self.events.push(Event::Notify(String::new(), rest.to_string())),
            "777" => {
                let mut it = rest.splitn(3, ';');
                if it.next() == Some("notify") {
                    let t = it.next().unwrap_or("").to_string();
                    let b = it.next().unwrap_or("").to_string();
                    self.events.push(Event::Notify(t, b));
                }
            }
            "10" | "11" | "12" => {
                let which = num.parse::<u8>().unwrap();
                if rest == "?" {
                    let c = match which {
                        10 => self.fg_override.unwrap_or(rgb3(zenteru_text::palette::FG)),
                        11 => self.bg_override.unwrap_or(rgb3(zenteru_text::palette::BG)),
                        _ => self.cursor_color.unwrap_or(rgb3(zenteru_text::palette::CURSOR)),
                    };
                    let r = format!("\x1b]{which};{}\x1b\\", rgb_spec(c));
                    self.respond(&r);
                } else if let Some(c) = parse_color(rest) {
                    match which {
                        10 => self.fg_override = Some(c),
                        11 => self.bg_override = Some(c),
                        _ => self.cursor_color = Some(c),
                    }
                }
            }
            "104" => {
                if rest.is_empty() {
                    self.palette = [None; 256];
                } else {
                    for i in rest.split(';').filter_map(|i| i.parse::<u8>().ok()) {
                        self.palette[i as usize] = None;
                    }
                }
            }
            "110" => self.fg_override = None,
            "111" => self.bg_override = None,
            "112" => self.cursor_color = None,
            "52" => {
                // Set only; reading the clipboard from the pty is refused.
                if let Some((_, b64)) = rest.split_once(';') {
                    if b64 != "?" {
                        if let Some(bytes) = base64::decode(b64) {
                            self.events.push(Event::Clipboard(String::from_utf8_lossy(&bytes).into_owned()));
                        }
                    }
                }
            }
            "133" => self.shell_mark(rest),
            "633" => {
                // VS Code shell integration: E;<commandline>
                if let Some(cmd) = rest.strip_prefix("E;") {
                    let cmd = cmd.split(';').next().unwrap_or("");
                    self.prompt.command = Some(unescape_633(cmd));
                    self.prompt.command_fresh = true;
                } else {
                    self.shell_mark(rest);
                }
            }
            "1337" => {
                if let Some(v) = rest.strip_prefix("SetUserVar=") {
                    if let Some((name, b64)) = v.split_once('=') {
                        let value = base64::decode(b64).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                        self.user_vars.retain(|(n, _)| n != name);
                        self.user_vars.push((name.to_string(), value.clone()));
                        self.events.push(Event::UserVar(name.to_string(), value));
                    }
                } else if let Some(p) = rest.strip_prefix("CurrentDir=") {
                    self.prompt.cwd = Some(p.to_string());
                    self.events.push(Event::Cwd(p.to_string()));
                } else if let Some(b64) = rest.strip_prefix("ZenteruCompletions=") {
                    if let Some(bytes) = base64::decode(b64) {
                        self.events.push(Event::Completions(String::from_utf8_lossy(&bytes).into_owned()));
                    }
                }
            }
            _ => {}
        }
    }

    fn shell_mark(&mut self, rest: &str) {
        let mut parts = rest.split(';');
        let kind = parts.next().unwrap_or("");
        let base = self.screen_base();
        match kind {
            "A" => {
                let abs = base + self.cursor.y as u64;
                if self.prompt.marks.last() != Some(&abs) {
                    self.prompt.marks.push(abs);
                    if self.prompt.marks.len() > 5000 {
                        self.prompt.marks.remove(0);
                    }
                }
                self.prompt.phase = PromptPhase::Unknown;
                self.prompt.input_start = None;
                self.prompt.command_fresh = false;
            }
            "B" => {
                self.prompt.phase = PromptPhase::Editing;
                self.prompt.input_start = Some(self.cursor_abs());
                self.events.push(Event::Prompt(PromptPhase::Editing));
            }
            "C" => {
                self.prompt.exit_status = None;
                if !self.prompt.command_fresh {
                    self.prompt.command = None;
                    if let Some(start) = self.prompt.input_start {
                        // The cursor has moved past the command line; take
                        // the logical line(s) from the input start.
                        let mut end = start.0;
                        while self.line(end).is_some_and(|r| r.wrapped) {
                            end += 1;
                        }
                        let t = self.text_range(start, (end, self.cols));
                        self.prompt.command = Some(t.trim_end().to_string());
                    }
                }
                self.prompt.phase = PromptPhase::Running;
                self.events.push(Event::Prompt(PromptPhase::Running));
            }
            "D" => {
                self.prompt.exit_status = parts.next().and_then(|s| s.parse().ok());
                self.prompt.phase = PromptPhase::Finished;
                self.events.push(Event::Prompt(PromptPhase::Finished));
            }
            _ => {}
        }
    }

    /// Jump the view to the previous (`dir < 0`) or next prompt mark.
    pub fn jump_to_prompt(&mut self, dir: i32) {
        let top = self.view_abs(0);
        let target = if dir < 0 {
            self.prompt.marks.iter().rev().find(|&&m| m < top).copied()
        } else {
            self.prompt.marks.iter().find(|&&m| m > top).copied()
        };
        if let Some(m) = target {
            let base = self.screen_base();
            let off = base.saturating_sub(m).min(self.scrollback.len() as u64);
            self.display_offset = off as usize;
            self.generation += 1;
        } else if dir > 0 {
            self.scroll_to_bottom();
        }
    }
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, b: u8) {
        match b {
            0x07 => self.events.push(Event::Bell),
            0x08 => {
                if self.cursor.pending_wrap {
                    self.cursor.pending_wrap = false;
                } else if self.cursor.x > 0 {
                    self.cursor.x -= 1;
                }
            }
            0x09 => {
                let mut x = self.cursor.x + 1;
                while x < self.cols - 1 && !self.tabs[x] {
                    x += 1;
                }
                self.set_x(x.min(self.cols - 1));
            }
            0x0A..=0x0C => {
                self.linefeed();
                if self.modes.newline {
                    self.cursor.x = 0;
                }
            }
            0x0D => self.set_x(0),
            0x0E => self.gl = 1,
            0x0F => self.gl = 0,
            _ => {}
        }
    }

    fn csi(&mut self, p: &Params, inter: &[u8], action: char) {
        let private = inter.first() == Some(&b'?');
        let n = |i| p.get(i, 1) as usize;
        match (inter, action) {
            ([], '@') => self.insert_chars(n(0)),
            ([], 'A') => self.move_rel(-(n(0) as isize)),
            ([], 'B') | ([], 'e') => self.move_rel(n(0) as isize),
            ([], 'C') | ([], 'a') => {
                let x = self.cursor.x + n(0);
                self.set_x(x)
            }
            ([], 'D') => {
                let x = self.cursor.x.saturating_sub(n(0));
                self.set_x(x)
            }
            ([], 'E') => {
                self.move_rel(n(0) as isize);
                self.set_x(0)
            }
            ([], 'F') => {
                self.move_rel(-(n(0) as isize));
                self.set_x(0)
            }
            ([], 'G') | ([], '`') => self.set_x(n(0) - 1),
            ([], 'H') | ([], 'f') => {
                self.set_y(n(0) - 1);
                self.set_x(n(1) - 1)
            }
            ([], 'I') => {
                for _ in 0..n(0) {
                    self.execute(0x09);
                }
            }
            ([], 'J') | ([b'?'], 'J') => self.erase_display(p.raw(0).unwrap_or(0)),
            ([], 'K') | ([b'?'], 'K') => self.erase_line(p.raw(0).unwrap_or(0)),
            ([], 'L') => self.insert_lines(n(0)),
            ([], 'M') => self.delete_lines(n(0)),
            ([], 'P') => self.delete_chars(n(0)),
            ([], 'S') => self.scroll_up(n(0)),
            ([], 'T') => self.scroll_down(n(0)),
            ([], 'X') => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                self.erase_cells(y, x, x + n(0));
                self.cursor.pending_wrap = false;
            }
            ([], 'Z') => {
                for _ in 0..n(0) {
                    let mut x = self.cursor.x;
                    while x > 0 {
                        x -= 1;
                        if self.tabs[x] {
                            break;
                        }
                    }
                    self.set_x(x);
                }
            }
            ([], 'b') => {
                if let Some(c) = self.last_char {
                    for _ in 0..n(0).min(65535) {
                        self.print_char(c);
                    }
                }
            }
            ([], 'c') if p.raw(0).unwrap_or(0) == 0 => self.respond("\x1b[?62;22c"),
            ([b'>'], 'c') => self.respond("\x1b[>1;10;0c"),
            ([b'='], 'c') => self.respond("\x1bP!|7A454E54\x1b\\"),
            ([], 'd') => self.set_y(n(0) - 1),
            ([], 'g') => match p.raw(0).unwrap_or(0) {
                0 => {
                    let x = self.cursor.x;
                    self.tabs[x] = false
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            ([], 'h') | ([b'?'], 'h') => {
                for g in p.groups() {
                    self.set_mode(private, g[0], true);
                }
            }
            ([], 'l') | ([b'?'], 'l') => {
                for g in p.groups() {
                    self.set_mode(private, g[0], false);
                }
            }
            ([], 'm') => self.sgr(p),
            ([], 'n') => match p.raw(0).unwrap_or(0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let y = if self.modes.origin { self.cursor.y - self.top } else { self.cursor.y };
                    let r = format!("\x1b[{};{}R", y + 1, self.cursor.x + 1);
                    self.respond(&r)
                }
                _ => {}
            },
            ([b'?'], 'n') if p.raw(0) == Some(6) => {
                let r = format!("\x1b[?{};{}R", self.cursor.y + 1, self.cursor.x + 1);
                self.respond(&r)
            }
            ([b'!'], 'p') => {
                self.modes = Modes::default();
                self.cursor.attrs = Attrs::default();
                self.top = 0;
                self.bottom = self.rows - 1;
                self.charsets = Default::default();
                self.gl = 0;
            }
            ([b'$'], 'p') | ([b'?', b'$'], 'p') => {
                let m = p.raw(0).unwrap_or(0);
                let v = self.mode_state(private, m);
                let r = format!("\x1b[{}{m};{v}$y", if private { "?" } else { "" });
                self.respond(&r)
            }
            ([b' '], 'q') => self.modes.cursor_style = p.raw(0).unwrap_or(0) as u8,
            ([b'>'], 'q') => {
                let r = format!("\x1bP>|zenteru {}\x1b\\", env!("CARGO_PKG_VERSION"));
                self.respond(&r)
            }
            ([], 'r') => {
                let top = p.get(0, 1) as usize - 1;
                let bottom = (p.get(1, self.rows as u16) as usize).min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.set_x(0);
                    self.set_y(0);
                }
            }
            ([], 's') => self.save_cursor(),
            ([], 'u') => self.restore_cursor(),
            ([], 't') => match p.raw(0).unwrap_or(0) {
                14 => {
                    let r = format!(
                        "\x1b[4;{};{}t",
                        self.rows * self.cell_px.1 as usize,
                        self.cols * self.cell_px.0 as usize
                    );
                    self.respond(&r)
                }
                16 => {
                    let r = format!("\x1b[6;{};{}t", self.cell_px.1, self.cell_px.0);
                    self.respond(&r)
                }
                18 => {
                    let r = format!("\x1b[8;{};{}t", self.rows, self.cols);
                    self.respond(&r)
                }
                22 => {
                    if self.title_stack.len() < 32 {
                        self.title_stack.push(self.title.clone());
                    }
                }
                23 => {
                    if let Some(t) = self.title_stack.pop() {
                        self.title = t.clone();
                        self.events.push(Event::Title(t));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn esc(&mut self, inter: &[u8], b: u8) {
        match (inter, b) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.linefeed();
                self.cursor.x = 0;
            }
            ([], b'H') => {
                let x = self.cursor.x;
                self.tabs[x] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([b'('], c) => self.charsets[0] = if c == b'0' { Charset::DecGraphics } else { Charset::Ascii },
            ([b')'], c) => self.charsets[1] = if c == b'0' { Charset::DecGraphics } else { Charset::Ascii },
            ([b'#'], b'8') => {
                for y in 0..self.rows {
                    for x in 0..self.cols {
                        self.grid()[y].cells[x] = Cell { c: 'E', ..Cell::default() };
                    }
                }
            }
            _ => {}
        }
    }

    fn osc(&mut self, data: &[u8]) {
        self.osc_dispatch(data);
    }

    fn dcs(&mut self, _p: &Params, inter: &[u8], action: char, data: &[u8]) {
        match (inter, action) {
            ([b'$'], 'q') => {
                let r = match data {
                    b"m" => "\x1bP1$r0m\x1b\\".to_string(),
                    b" q" => format!("\x1bP1$r{} q\x1b\\", self.modes.cursor_style),
                    b"r" => format!("\x1bP1$r{};{}r\x1b\\", self.top + 1, self.bottom + 1),
                    _ => "\x1bP0$r\x1b\\".to_string(),
                };
                self.respond(&r);
            }
            ([b'+'], 'q') => {
                // XTGETTCAP: answer the few that matter, refuse the rest.
                let mut out = String::new();
                for name in data.split(|&b| b == b';') {
                    let n = hex_decode(name).unwrap_or_default();
                    let v = match n.as_str() {
                        "TN" | "name" => Some("xterm-256color".to_string()),
                        "Co" | "colors" => Some("256".to_string()),
                        "RGB" => Some("8".to_string()),
                        _ => None,
                    };
                    match v {
                        Some(v) => out.push_str(&format!(
                            "\x1bP1+r{}={}\x1b\\",
                            String::from_utf8_lossy(name),
                            hex_encode(&v)
                        )),
                        None => out.push_str(&format!("\x1bP0+r{}\x1b\\", String::from_utf8_lossy(name))),
                    }
                }
                self.respond(&out);
            }
            _ => {}
        }
    }
}

fn ext_color(v: &[u16], colon: bool) -> Option<Color> {
    match v.first()? {
        5 => v.get(1).map(|&i| Color::Indexed(i.min(255) as u8)),
        2 => {
            // Colon form may carry a colour-space id: 2:cs:r:g:b (or 2::r:g:b).
            let rgb = if colon && v.len() >= 5 { &v[2..5] } else { v.get(1..4)? };
            Some(Color::Rgb(rgb[0].min(255) as u8, rgb[1].min(255) as u8, rgb[2].min(255) as u8))
        }
        _ => None,
    }
}

fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        c => c,
    }
}

fn rgb3(c: zenteru_text::palette::Rgb) -> (u8, u8, u8) {
    (c.0, c.1, c.2)
}

fn rgb_spec((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// `rgb:rr/gg/bb` (1–4 hex digits each) or `#rrggbb`.
fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    if let Some(h) = s.strip_prefix('#') {
        if h.len() == 6 {
            let v = u32::from_str_radix(h, 16).ok()?;
            return Some(((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return None;
    }
    let body = s.strip_prefix("rgb:")?;
    let mut it = body.split('/').map(|c| {
        let v = u32::from_str_radix(c, 16).ok()?;
        let max = (1u32 << (4 * c.len().clamp(1, 4))) - 1;
        Some((v * 255 / max) as u8)
    });
    Some((it.next()??, it.next()??, it.next()??))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unescape_633(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some('\\') => {
                    it.next();
                    out.push('\\');
                }
                Some('x') => {
                    it.next();
                    let h: String = it.by_ref().take(2).collect();
                    if let Ok(v) = u8::from_str_radix(&h, 16) {
                        out.push(v as char);
                    }
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn hex_decode(b: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(b).ok()?;
    let bytes: Option<Vec<u8>> = (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

fn hex_encode(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02X}")).collect()
}

/// Rewrap `lines` (scrollback + screen) to `cols`. `cursor` is (x, line
/// index). Returns the new lines and the cursor's new (x, line index).
/// `floor` is a row index whose logical line must stay at or below the top
/// of the screen; its new row index is returned alongside.
fn reflow(lines: Vec<Row>, cursor: (usize, usize), cols: usize, floor: Option<usize>) -> (Vec<Row>, (usize, usize), Option<usize>) {
    // Gather logical lines.
    let mut logical: Vec<(Vec<Cell>, bool, Vec<u128>, u128)> = Vec::new(); // (cells, holds cursor, per-cell stamps, first row's stamp)
    let mut cur: Vec<Cell> = Vec::new();
    let mut cur_st: Vec<u128> = Vec::new();
    let mut cur_first: Option<u128> = None;
    let mut cur_has = false;
    let mut cursor_off = 0usize;
    let mut cursor_line = 0usize;
    let mut floor_logical: Option<usize> = None;
    for (i, row) in lines.into_iter().enumerate() {
        let is_cursor_row = i == cursor.1;
        if Some(i) == floor {
            floor_logical = Some(logical.len());
        }
        let mut cells = row.cells;
        // Drop a wrap padding spacer (a spacer not preceded by a wide cell).
        if row.wrapped {
            if let Some(last) = cells.last() {
                let n = cells.len();
                if last.is_spacer() && !(n >= 2 && cells[n - 2].is_wide()) {
                    cells.pop();
                }
            }
        } else {
            let mut used = cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
            if is_cursor_row {
                used = used.max(cursor.0);
            }
            cells.truncate(used);
        }
        if is_cursor_row {
            cursor_off = cur.len() + cursor.0;
            cur_has = true;
        }
        cur_st.extend(std::iter::repeat_n(row.id, cells.len()));
        cur_first.get_or_insert(row.id);
        cur.extend(cells);
        if !row.wrapped {
            if cur_has {
                cursor_line = logical.len();
            }
            logical.push((std::mem::take(&mut cur), cur_has, std::mem::take(&mut cur_st), cur_first.take().unwrap_or(0)));
            cur_has = false;
        }
    }
    if !cur.is_empty() || cur_has {
        if cur_has {
            cursor_line = logical.len();
        }
        logical.push((cur, cur_has, cur_st, cur_first.unwrap_or(0)));
    }
    // Drop blank lines after the cursor's line.
    while logical.len() > cursor_line + 1 && logical.last().is_some_and(|(c, ..)| c.iter().all(Cell::is_blank)) {
        logical.pop();
    }

    let mut out: Vec<Row> = Vec::new();
    let mut new_cursor = (0, 0);
    let mut new_floor = None;
    for (li, (cells, has, stamps, first_stamp)) in logical.into_iter().enumerate() {
        let first = out.len();
        if Some(li) == floor_logical {
            new_floor = Some(first);
        }
        let mut row: Vec<Cell> = Vec::with_capacity(cols);
        // A rewrapped row lands when its first cell's source row did.
        let mut row_stamp = first_stamp;
        let mut offsets: Vec<(usize, usize, usize)> = Vec::new(); // (logical idx, out row, col)
        for (i, c) in cells.into_iter().enumerate() {
            let w = if c.is_wide() { 2 } else { 1 };
            if c.is_spacer() && row.last().is_some_and(Cell::is_wide) {
                offsets.push((i, out.len(), row.len()));
                row.push(c);
                continue;
            }
            if row.len() + w > cols {
                if row.len() < cols {
                    let mut pad = Cell::default();
                    pad.flags = flags::SPACER;
                    row.push(pad);
                }
                out.push(Row { cells: std::mem::take(&mut row), wrapped: true, id: row_stamp });
                row_stamp = stamps.get(i).copied().unwrap_or(0);
            }
            offsets.push((i, out.len(), row.len()));
            row.push(c);
        }
        out.push(Row { cells: row, wrapped: false, id: row_stamp });
        if has {
            new_cursor = match offsets.iter().find(|(i, _, _)| *i == cursor_off) {
                Some(&(_, r, c)) => (c, r),
                None => {
                    // Past the end of the content.
                    let len: usize = out[first..].iter().map(|r| r.cells.len()).sum::<usize>();
                    let extra = cursor_off.saturating_sub(offsets.len());
                    let _ = len;
                    let last = out.len() - 1;
                    let x = out[last].cells.len() + extra;
                    if x >= cols {
                        let more = x / cols;
                        for _ in 0..more {
                            out.last_mut().unwrap().wrapped = true;
                            out.push(Row { cells: Vec::new(), wrapped: false, id: 0 });
                        }
                        (x % cols, out.len() - 1)
                    } else {
                        (x, last)
                    }
                }
            };
        }
    }
    if out.is_empty() {
        out.push(Row { cells: Vec::new(), wrapped: false, id: 0 });
    }
    for r in &mut out {
        r.resize(cols);
    }
    (out, new_cursor, new_floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: usize, rows: usize, input: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows);
        t.feed(input.as_bytes());
        t
    }

    fn lines(t: &Terminal) -> Vec<String> {
        t.screen().iter().map(Row::text).collect()
    }

    #[test]
    fn zenkaku_takes_two_cells() {
        let t = term(10, 2, "a한b");
        assert_eq!(t.cursor.x, 4);
        assert!(t.screen()[0].cells[1].is_wide());
        assert!(t.screen()[0].cells[2].is_spacer());
        assert_eq!(lines(&t)[0], "a한b");
    }

    #[test]
    fn wide_char_wraps_at_margin() {
        let t = term(5, 3, "abcd日本");
        assert_eq!(lines(&t), ["abcd", "日本", ""]);
        assert!(t.screen()[0].wrapped);
        assert_eq!(t.cursor.x, 4);
    }

    #[test]
    fn overwriting_half_clears_other_half() {
        let t = term(10, 1, "日本\x1b[2Gx");
        assert_eq!(lines(&t)[0], " x本");
    }

    #[test]
    fn combining_and_jamo_attach() {
        let t = term(10, 1, "e\u{301}\u{1112}\u{1161}\u{11AB}");
        assert_eq!(lines(&t)[0], "e\u{301}\u{1112}\u{1161}\u{11AB}");
        assert_eq!(t.cursor.x, 3);
    }

    #[test]
    fn scrollback_and_erase() {
        let t = term(10, 2, "1\r\n2\r\n3\r\n4");
        assert_eq!(t.scrollback.len(), 2);
        assert_eq!(lines(&t), ["3", "4"]);
        let mut t = t;
        t.feed(b"\x1b[2J");
        assert_eq!(t.scrollback.len(), 4);
    }

    #[test]
    fn alt_screen_restores() {
        let mut t = term(10, 2, "hi");
        t.feed(b"\x1b[?1049hfull\x1b[?1049l");
        assert_eq!(lines(&t)[0], "hi");
        assert_eq!(t.cursor.x, 2);
    }

    #[test]
    fn sgr_colors() {
        let t = term(10, 1, "\x1b[1;38;2;1;2;3;48:5:200mX");
        let c = &t.screen()[0].cells[0];
        assert_eq!(c.fg, Color::Rgb(1, 2, 3));
        assert_eq!(c.bg, Color::Indexed(200));
        assert!(c.flags & flags::BOLD != 0);
    }

    #[test]
    fn responses() {
        let mut t = term(10, 5, "\x1b[3;4H\x1b[6n\x1b[c");
        assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[?62;22c");
    }

    #[test]
    fn reflow_rewraps_zenkaku() {
        let mut t = term(6, 3, "日本語です");
        assert_eq!(lines(&t), ["日本語", "です", ""]);
        t.resize(10, 3);
        assert_eq!(lines(&t)[0], "日本語です");
        // The cursor was just past the last character: it moves to the next row.
        assert_eq!((t.cursor.x, t.cursor.y), (0, 1));
        t.resize(5, 3);
        assert_eq!(lines(&t), ["日本", "語で", "す"]);
    }

    #[test]
    fn resize_does_not_undo_clear() {
        let mut t = term(10, 4, "one\r\ntwo\r\nthree\r\nfour");
        // `clear` (ncurses sends E3 too): home, erase display, erase scrollback.
        t.feed(b"\x1b[H\x1b[2J\x1b[3J$ ");
        assert_eq!(lines(&t)[0], "$");
        t.resize(10, 8);
        assert_eq!(lines(&t)[0], "$", "taller: nothing pulled back");
        t.resize(5, 8);
        t.resize(20, 3);
        assert_eq!(lines(&t)[0], "$");
        // The exact bytes macOS `clear` sends: E3 first, then home + ED 2.
        let mut t = term(10, 4, "one\r\ntwo\r\nthree\r\nfour");
        t.feed(b"\x1b[3J\x1b[H\x1b[2J$ ");
        t.resize(12, 9);
        assert_eq!(lines(&t)[0], "$");
        // Without E3 the old screen stays in scrollback, still off screen.
        let mut t = term(10, 4, "one\r\ntwo\r\nthree\r\nfour");
        t.feed(b"\x1b[H\x1b[2J$ ");
        t.resize(10, 8);
        assert_eq!(lines(&t)[0], "$");
        assert!(t.scrollback.iter().any(|r| r.text() == "four"));
        // New output after the clear scrolls normally and can come back.
        t.feed(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        t.resize(10, 4);
        t.resize(10, 6);
        assert_eq!(lines(&t)[5], "8");
        assert_eq!(lines(&t)[0], "3", "lines after the clear may be pulled back");
    }

    #[test]
    fn line_ids_survive_reflow() {
        let mut t = term(10, 4, "abcdefghijklmno\r\nxy\r\n$ ");
        let before: Vec<u128> = (0..4).map(|l| t.line(l).unwrap().id).collect();
        assert!(t.screen()[0].wrapped);
        t.resize(20, 4);
        // The wrapped pair joins into one row that keeps the first row's id.
        assert_eq!(t.screen()[0].id, before[0]);
        assert_eq!(t.screen()[1].id, before[2]);
        t.resize(5, 6);
        assert_eq!(t.screen()[0].id, before[0]);
        assert_ne!(t.screen()[1].id, 0);
    }

    #[test]
    fn reflow_keeps_cursor_on_prompt() {
        let mut t = term(20, 5, "line1\r\n$ ");
        t.resize(10, 5);
        assert_eq!(t.cursor.y, 1);
        assert_eq!(t.cursor.x, 2);
        t.resize(10, 2);
        assert_eq!(lines(&t), ["line1", "$"]);
        assert_eq!((t.cursor.x, t.cursor.y), (2, 1));
    }

    #[test]
    fn shell_integration_marks() {
        let mut t = term(20, 3, "\x1b]133;A\x07$ \x1b]133;B\x07ls -la");
        assert_eq!(t.prompt.phase, PromptPhase::Editing);
        assert_eq!(t.edit_line(), ("ls -la".to_string(), 6));
        t.feed(b"\x1b[3D");
        assert_eq!(t.edit_line().1, 3);
        t.feed(b"\x1b[3C\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        assert_eq!(t.prompt.command.as_deref(), Some("ls -la"));
        assert_eq!(t.prompt.exit_status, Some(0));
    }

    #[test]
    fn osc_title_and_uservar() {
        let mut t = term(10, 1, "\x1b]2;제목\x07\x1b]1337;SetUserVar=foo=YmFy\x07");
        let ev = t.take_events();
        assert!(ev.contains(&Event::Title("제목".into())));
        assert!(ev.contains(&Event::UserVar("foo".into(), "bar".into())));
    }

    #[test]
    fn dec_graphics_charset() {
        let t = term(10, 1, "\x1b(0lqk\x1b(B");
        assert_eq!(lines(&t)[0], "┌─┐");
    }
}
