//! Width of a single code point in terminal cells (0, 1 or 2), using the
//! same rules as `text::grapheme_width` (including ZENTERU_AMBIGUOUS_WIDE).

use crate::text::grapheme_width;

pub fn char_width(c: char) -> usize {
    if (c as u32) < 0x7f {
        return if (c as u32) < 0x20 { 0 } else { 1 };
    }
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    // Combining marks and zero-width joiners report 0 from unicode-width.
    if is_zero_width(c) {
        return 0;
    }
    grapheme_width(s, 0).min(2)
}

fn is_zero_width(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A
        | 0x064B..=0x065F | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E
        | 0x1160..=0x11FF   // Hangul medial vowels / final consonants (conjoining jamo)
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F | 0x2028..=0x202E
        | 0x2060..=0x2064 | 0x20D0..=0x20FF | 0x302A..=0x302F | 0x3099 | 0x309A
        | 0xD7B0..=0xD7FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xFEFF
        | 0x1F3FB..=0x1F3FF | 0xE0000..=0xE0FFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('한'), 2);
        assert_eq!(char_width('日'), 2);
        assert_eq!(char_width('\u{3000}'), 2);
        assert_eq!(char_width('\u{0301}'), 0);
        assert_eq!(char_width('\u{1161}'), 0);
        assert_eq!(char_width('😀'), 2);
        assert_eq!(char_width('ｱ'), 1);
    }
}
