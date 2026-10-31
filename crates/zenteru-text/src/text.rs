//! Vendored from team-mirae/zencode `src/text.rs` @ 815657f (env var renamed to ZENTERU_AMBIGUOUS_WIDE). Keep in sync.

//! Display-width helpers. Everything on screen is measured in terminal
//! columns, and zenkaku (full-width) characters such as 한글 and 日本語 take
//! two columns each. The cursor always moves by grapheme cluster, never by
//! byte or `char`, so combining marks and emoji sequences stay intact.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

static TAB_WIDTH: AtomicUsize = AtomicUsize::new(4);

/// Columns between tab stops; `-T` / `--tabsize` changes it at startup.
pub fn tab_width() -> usize {
    TAB_WIDTH.load(Ordering::Relaxed)
}

pub fn set_tab_width(n: usize) {
    TAB_WIDTH.store(n.max(1), Ordering::Relaxed);
}

/// Some East Asian terminal setups render "ambiguous width" characters
/// (·, ─, →, ①, …) as two columns. Set `ZENTERU_AMBIGUOUS_WIDE=1` to match.
fn ambiguous_wide() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("ZENTERU_AMBIGUOUS_WIDE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    })
}

pub fn is_control(g: &str) -> bool {
    g != "\t" && g.chars().any(char::is_control)
}

/// Width of a grapheme that starts at display column `col`.
/// `col` only matters for tabs, which expand to the next tab stop.
pub fn grapheme_width(g: &str, col: usize) -> usize {
    if g == "\t" {
        let tab = tab_width();
        return tab - col % tab;
    }
    if is_control(g) {
        return 1;
    }
    if ambiguous_wide() {
        g.width_cjk()
    } else {
        g.width()
    }
}

/// Total display width of `s`, assuming it starts at column 0.
pub fn str_width(s: &str) -> usize {
    let mut col = 0;
    for g in s.graphemes(true) {
        col += grapheme_width(g, col);
    }
    col
}

pub fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Byte offset of the `gidx`-th grapheme (or `s.len()` past the end).
pub fn byte_index(s: &str, gidx: usize) -> usize {
    s.grapheme_indices(true)
        .nth(gidx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Display column at which the `gidx`-th grapheme starts.
pub fn display_col(s: &str, gidx: usize) -> usize {
    let mut col = 0;
    for g in s.graphemes(true).take(gidx) {
        col += grapheme_width(g, col);
    }
    col
}

/// Width of the grapheme at `gidx`, or 1 when the cursor sits past the end.
pub fn width_at(s: &str, gidx: usize) -> usize {
    let col = display_col(s, gidx);
    s.graphemes(true)
        .nth(gidx)
        .map(|g| grapheme_width(g, col).max(1))
        .unwrap_or(1)
}

/// Grapheme index whose cell covers display column `target`; used to keep
/// the cursor in roughly the same column when moving between lines that
/// mix half-width and full-width text.
pub fn gidx_for_col(s: &str, target: usize) -> usize {
    let mut col = 0;
    for (i, g) in s.graphemes(true).enumerate() {
        let w = grapheme_width(g, col);
        if col + w > target {
            return i;
        }
        col += w;
    }
    grapheme_count(s)
}

/// Truncate `s` so it fits within `max` columns, never splitting a
/// full-width character in half.
pub fn truncate(s: &str, max: usize) -> (&str, usize) {
    let mut col = 0;
    for (i, g) in s.grapheme_indices(true) {
        let w = grapheme_width(g, col);
        if col + w > max {
            return (&s[..i], col);
        }
        col += w;
    }
    (s, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zenkaku_is_two_columns() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("한글"), 4);
        assert_eq!(str_width("日本語"), 6);
        assert_eq!(str_width("a한b日"), 6);
    }

    #[test]
    fn hangul_jamo_compose_into_one_grapheme() {
        // 한 written as decomposed jamo (ᄒ + ᅡ + ᆫ) is still one cell pair.
        let decomposed = "\u{1112}\u{1161}\u{11AB}";
        assert_eq!(grapheme_count(decomposed), 1);
        assert_eq!(str_width(decomposed), 2);
    }

    #[test]
    fn column_mapping_round_trips() {
        let s = "ab한글cd";
        assert_eq!(display_col(s, 2), 2);
        assert_eq!(display_col(s, 3), 4);
        assert_eq!(display_col(s, 4), 6);
        // Column 3 is the right half of 한, so it maps to 한 itself.
        assert_eq!(gidx_for_col(s, 3), 2);
        assert_eq!(gidx_for_col(s, 100), 6);
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(str_width("\t"), tab_width());
        assert_eq!(str_width("a\t"), tab_width());
        assert_eq!(str_width("한\tx"), tab_width() + 1);
    }

    #[test]
    fn truncate_never_splits_wide_chars() {
        assert_eq!(truncate("日本語", 5), ("日本", 4));
        assert_eq!(truncate("abc", 10), ("abc", 3));
    }
}
