//! Text-field editing for the side panel's input box: a caret plus an
//! optional selection anchor (both char indices), with the usual macOS
//! behaviour — ⇧ extends, typing replaces the selection, ⌘A / ⌘C / ⌘X / ⌘V,
//! click / drag / double-click (letterblock) / triple-click.

use zenteru_text::text::grapheme_width;

use crate::app::{Panel, byte_at};

impl Panel {
    pub fn len_chars(&self) -> usize {
        self.input.chars().count()
    }

    /// The selection as an ordered char range, if non-empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.input.chars().skip(a).take(b - a).collect())
    }

    /// Delete the selection; returns whether there was one.
    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let (ba, bb) = (byte_at(&self.input, a), byte_at(&self.input, b));
        self.input.replace_range(ba..bb, "");
        self.cursor = a;
        self.anchor = None;
        true
    }

    /// Type / paste `text`, replacing the selection.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let bi = byte_at(&self.input, self.cursor);
        self.input.insert_str(bi, text);
        self.cursor += text.chars().count();
    }

    /// Move the caret; with `extend` the selection grows from where it was.
    pub fn move_to(&mut self, to: usize, extend: bool) {
        let to = to.min(self.len_chars());
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = to;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len_chars();
    }

    /// Select the letterblock (zencode rules) around char `at`.
    pub fn select_block(&mut self, at: usize) {
        let gs = zenteru_text::seg::graphemes(&self.input);
        // grapheme index ↔ char index
        let mut g_of_char = Vec::new();
        let mut char_of_g = Vec::with_capacity(gs.len() + 1);
        let mut ci = 0;
        for (gi, g) in gs.iter().enumerate() {
            char_of_g.push(ci);
            for _ in g.chars() {
                g_of_char.push(gi);
            }
            ci += g.chars().count();
        }
        char_of_g.push(ci);
        let gi = g_of_char.get(at.min(ci.saturating_sub(1))).copied().unwrap_or(0);
        let r = zenteru_text::letterblock::block_at(&self.input, gi);
        self.anchor = Some(char_of_g[r.start.min(gs.len())]);
        self.cursor = char_of_g[r.end.min(gs.len())];
    }
}

/// Hard-wrap `text` to `width` cells without dropping any characters, so
/// screen positions map back to char indices. Returns (first char index,
/// line text) per line; always at least one line.
pub fn layout(text: &str, width: usize) -> Vec<(usize, String)> {
    let width = width.max(2);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut start = 0usize;
    let mut col = 0usize;
    let mut ci = 0usize;
    for g in zenteru_text::seg::graphemes(text) {
        let w = grapheme_width(g, col).max(1);
        if col + w > width && !line.is_empty() {
            out.push((start, std::mem::take(&mut line)));
            start = ci;
            col = 0;
        }
        line.push_str(g);
        col += w;
        ci += g.chars().count();
    }
    out.push((start, line));
    out
}

/// Char index for a point `col_cells` cells into laid-out line `line`.
pub fn char_at(lines: &[(usize, String)], line: usize, col_cells: f32) -> usize {
    let Some((start, text)) = lines.get(line) else {
        return lines.last().map_or(0, |(s, t)| s + t.chars().count());
    };
    let mut col = 0.0f32;
    let mut ci = *start;
    for g in zenteru_text::seg::graphemes(text) {
        let w = grapheme_width(g, col as usize).max(1) as f32;
        // Past the middle of a character → after it.
        if col_cells < col + w / 2.0 {
            return ci;
        }
        col += w;
        ci += g.chars().count();
    }
    ci
}

/// (line, cells from line start) of char index `at` in a layout.
pub fn position_of(lines: &[(usize, String)], at: usize) -> (usize, usize) {
    let mut idx = lines.len() - 1;
    for (i, (s, _)) in lines.iter().enumerate() {
        if *s <= at {
            idx = i;
        } else {
            break;
        }
    }
    let (s, t) = &lines[idx];
    let prefix: String = t.chars().take(at - s).collect();
    (idx, zenteru_text::text::str_width(&prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_every_char_and_maps_back() {
        let l = layout("echo 한국어를 hello", 8);
        let joined: String = l.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, "echo 한국어를 hello");
        assert_eq!(l[0], (0, "echo 한".to_string()));
        assert_eq!(char_at(&l, 0, 0.2), 0);
        assert_eq!(char_at(&l, 0, 5.4), 5, "left half of 한");
        assert_eq!(char_at(&l, 0, 6.6), 6, "right half of 한");
        assert_eq!(position_of(&l, 6), (1, 0));
    }
}
