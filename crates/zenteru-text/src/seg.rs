//! Grapheme segmentation re-exported for crates that don't depend on
//! unicode-segmentation directly.

use unicode_segmentation::UnicodeSegmentation;

pub fn graphemes(s: &str) -> Vec<&str> {
    s.graphemes(true).collect()
}
