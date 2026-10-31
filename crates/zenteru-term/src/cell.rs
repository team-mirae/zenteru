//! Cells and rows.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub mod flags {
    pub const BOLD: u16 = 1;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 4;
    pub const CURLY_UNDERLINE: u16 = 1 << 5;
    pub const INVERSE: u16 = 1 << 6;
    pub const HIDDEN: u16 = 1 << 7;
    pub const STRIKE: u16 = 1 << 8;
    pub const BLINK: u16 = 1 << 9;
    /// First half of a two-column (zenkaku) character.
    pub const WIDE: u16 = 1 << 10;
    /// Second half of a zenkaku character; holds no text.
    pub const SPACER: u16 = 1 << 11;
    pub const ANY_UNDERLINE: u16 = UNDERLINE | DOUBLE_UNDERLINE | CURLY_UNDERLINE;
}

/// Text attributes applied to newly written cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub ul: Color,
    pub flags: u16,
    pub link: u16,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cell {
    pub c: char,
    /// Further code points of the grapheme cluster (combining marks, ZWJ sequences).
    pub extra: Option<Box<str>>,
    pub fg: Color,
    pub bg: Color,
    pub ul: Color,
    pub flags: u16,
    /// Hyperlink id (OSC 8), 0 = none.
    pub link: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(&Attrs::default())
    }
}

impl Cell {
    /// An empty cell with the background of `a` (erase uses the current bg).
    pub fn blank(a: &Attrs) -> Self {
        Self { c: ' ', extra: None, fg: a.fg, bg: a.bg, ul: Color::Default, flags: 0, link: 0 }
    }

    pub fn is_blank(&self) -> bool {
        self.c == ' ' && self.extra.is_none() && self.bg == Color::Default && self.flags & !flags::SPACER == 0
    }

    pub fn is_spacer(&self) -> bool {
        self.flags & flags::SPACER != 0
    }

    pub fn is_wide(&self) -> bool {
        self.flags & flags::WIDE != 0
    }

    /// The grapheme cluster text.
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.push_text(&mut s);
        s
    }

    pub fn push_text(&self, s: &mut String) {
        s.push(self.c);
        if let Some(e) = &self.extra {
            s.push_str(e);
        }
    }

    pub fn append(&mut self, c: char) {
        let mut e = self.extra.take().map(String::from).unwrap_or_default();
        if e.len() < 64 {
            e.push(c);
        }
        self.extra = Some(e.into_boxed_str());
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next row (soft wrap).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, a: &Attrs) -> Self {
        Self { cells: vec![Cell::blank(a); cols], wrapped: false }
    }

    /// Text of the row with zenkaku spacers skipped. Trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.cells {
            if !c.is_spacer() {
                c.push_text(&mut s);
            }
        }
        s.truncate(s.trim_end_matches(' ').len());
        s
    }

    /// Number of columns used, ignoring trailing default blanks.
    pub fn used(&self) -> usize {
        self.cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1)
    }

    pub fn resize(&mut self, cols: usize) {
        if cols < self.cells.len() {
            self.cells.truncate(cols);
            // Don't leave half a zenkaku character at the edge.
            if let Some(last) = self.cells.last_mut() {
                if last.is_wide() {
                    *last = Cell::default();
                }
            }
        } else {
            self.cells.resize(cols, Cell::default());
        }
    }
}
