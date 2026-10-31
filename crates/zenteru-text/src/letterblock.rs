//! Vendored from team-mirae/zencode `src/letterblock.rs` @ 815657f. Keep in sync.

//! Letterblocks (KEYBINDS.md "Letterblock deletion"): blocks of text with a
//! distinct meaning. They are the unit for ⌃/⌥ + ←→ movement and
//! ⌃/⌥ + ⌫⌦ deletion.
//!
//! - English and other Latin text: a word.
//! - Japanese: a run of one script, so あなたのギターになりたい！ is
//!   あなたの | ギター | になりたい | ！.
//! - Korean: a space-delimited run with a trailing particle (조사) or
//!   ending (어미) split off: 한국어를 is 한국어 | 를, 먹었습니다 is
//!   먹 | 었습니다.
//! - Runs of punctuation are blocks of their own; whitespace only
//!   separates blocks.
//!
//! The Korean split uses a short suffix list, not a dictionary. Two rules
//! keep nouns that merely end like a particle intact: a particle that only
//! follows a consonant (을) or a vowel (를) must agree with the syllable
//! before it, so 가을 and 사과 stay whole; and a one-syllable particle that
//! follows anything (도, 의) needs at least two syllables in front of it,
//! so 포도 stays whole. A noun like 고양이 still splits as 고양 | 이. A run
//! made only of particles (에서는, left over after deleting 학교) stays
//! whole.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Space,
    Latin,
    Hangul,
    Han,
    Hiragana,
    Katakana,
    Punct,
}

fn class(g: &str) -> Class {
    let Some(c) = g.chars().next() else {
        return Class::Space;
    };
    match c {
        c if c.is_whitespace() => Class::Space,
        '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' => {
            Class::Hangul
        }
        '\u{3040}'..='\u{309F}' => Class::Hiragana,
        // ー (U+30FC) lives in this block, so カード stays one block.
        '\u{30A0}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9F}' => {
            Class::Katakana
        }
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}' | '々' => {
            Class::Han
        }
        '_' => Class::Latin,
        c if c.is_alphanumeric() => Class::Latin,
        _ => Class::Punct,
    }
}

/// What the syllable before a Korean suffix must end with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Any,
    Consonant,
    Vowel,
    /// A vowel or ㄹ, as for 로 (집으로 but 서울로, 학교로).
    VowelOrRieul,
}

use After::{Any, Consonant, Vowel, VowelOrRieul};

/// Common particles (조사) and endings (어미). The longest one that fits
/// wins, so 에서는 beats 는.
#[rustfmt::skip]
const SUFFIXES: &[(&str, After)] = &[
    // particles that only follow a consonant / a vowel
    ("은", Consonant), ("을", Consonant), ("이", Consonant), ("과", Consonant),
    ("이랑", Consonant), ("이나", Consonant), ("이라도", Consonant), ("이야", Consonant),
    ("이며", Consonant), ("이든지", Consonant), ("과는", Consonant), ("이라고", Consonant),
    ("이라는", Consonant), ("으로", Consonant), ("으로서", Consonant), ("으로써", Consonant),
    ("으로부터", Consonant), ("으로는", Consonant),
    ("는", Vowel), ("를", Vowel), ("가", Vowel), ("와", Vowel), ("랑", Vowel),
    ("나", Vowel), ("라도", Vowel), ("야", Vowel), ("며", Vowel), ("든지", Vowel),
    ("와는", Vowel), ("라고", Vowel), ("라는", Vowel),
    ("로", VowelOrRieul), ("로서", VowelOrRieul), ("로써", VowelOrRieul),
    ("로부터", VowelOrRieul), ("로는", VowelOrRieul),
    // particles that follow anything
    ("의", Any), ("에", Any), ("께", Any), ("도", Any), ("만", Any), ("뿐", Any),
    ("에서", Any), ("에게", Any), ("에게서", Any), ("한테", Any), ("한테서", Any),
    ("께서", Any), ("까지", Any), ("부터", Any), ("마저", Any), ("조차", Any),
    ("보다", Any), ("처럼", Any), ("같이", Any), ("만큼", Any), ("밖에", Any),
    ("대로", Any), ("에는", Any), ("에서는", Any), ("에게는", Any), ("에도", Any),
    ("에서도", Any), ("까지는", Any), ("까지도", Any), ("부터는", Any), ("만은", Any),
    // endings, including 하다 / 되다 / 이다 forms that attach to nouns
    ("습니다", Any), ("었습니다", Any), ("았습니다", Any), ("였습니다", Any),
    ("했습니다", Any), ("겠습니다", Any), ("습니까", Any), ("입니다", Any),
    ("입니까", Any), ("합니다", Any), ("합니까", Any), ("됩니다", Any),
    ("이에요", Any), ("예요", Any), ("이었다", Any), ("였다", Any), ("었다", Any),
    ("았다", Any), ("했다", Any), ("겠다", Any), ("한다", Any), ("하다", Any),
    ("하는", Any), ("하고", Any), ("해서", Any), ("하면", Any), ("해요", Any),
    ("했어요", Any), ("었어요", Any), ("았어요", Any), ("겠어요", Any), ("어요", Any),
    ("아요", Any), ("하던", Any), ("었던", Any), ("았던", Any), ("된다", Any),
    ("되다", Any), ("되는", Any), ("됐다", Any), ("돼요", Any), ("지만", Any),
    ("는데", Any), ("은데", Any), ("니까", Any), ("으니까", Any), ("어서", Any),
    ("아서", Any), ("면서", Any), ("으면서", Any), ("으면", Any), ("려고", Any),
    ("으려고", Any), ("도록", Any), ("세요", Any), ("으세요", Any), ("십시오", Any),
];

/// Final-consonant index of a precomposed syllable (0 = none, 8 = ㄹ).
fn final_consonant(c: char) -> Option<u32> {
    let s = (c as u32).checked_sub(0xAC00).filter(|&s| s < 11_172)?;
    Some(s % 28)
}

/// Where to split a run of Hangul syllables into stem and suffix, as the
/// stem's length in syllables, or `None` to keep it whole.
fn korean_split(run: &[char]) -> Option<usize> {
    if SUFFIXES
        .iter()
        .any(|(s, _)| s.chars().eq(run.iter().copied()))
    {
        return None;
    }
    SUFFIXES
        .iter()
        .filter_map(|&(suffix, after)| {
            let n = suffix.chars().count();
            let stem = run.len().checked_sub(n).filter(|&s| s > 0)?;
            if !run[stem..].iter().copied().eq(suffix.chars()) {
                return None;
            }
            if after == Any && n == 1 && stem < 2 {
                return None;
            }
            // A run of nothing but particles (에서는) stays whole.
            if SUFFIXES
                .iter()
                .any(|(s, _)| s.chars().eq(run[..stem].iter().copied()))
            {
                return None;
            }
            let fin = final_consonant(run[stem - 1]);
            let fits = match after {
                Any => true,
                Consonant => fin.is_some_and(|f| f != 0),
                Vowel => fin == Some(0),
                VowelOrRieul => matches!(fin, Some(0 | 8)),
            };
            fits.then_some((stem, n))
        })
        .max_by_key(|&(_, n)| n)
        .map(|(stem, _)| stem)
}

/// The letterblocks of a line, as grapheme ranges, in order.
pub fn blocks(s: &str) -> Vec<Range<usize>> {
    let gs: Vec<&str> = s.graphemes(true).collect();
    let classes: Vec<Class> = gs.iter().map(|g| class(g)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < gs.len() {
        let c = classes[i];
        let mut j = i + 1;
        while j < gs.len() && classes[j] == c {
            j += 1;
        }
        if c == Class::Hangul {
            // Precomposed syllables are one char per grapheme.
            let run: Vec<char> = gs[i..j].iter().filter_map(|g| g.chars().next()).collect();
            match korean_split(&run).filter(|_| run.len() == j - i) {
                Some(stem) => {
                    out.push(i..i + stem);
                    out.push(i + stem..j);
                }
                None => out.push(i..j),
            }
        } else if c != Class::Space {
            out.push(i..j);
        }
        i = j;
    }
    out
}

/// Grapheme index at the end of the letterblock under or after `gidx`.
pub fn right(s: &str, gidx: usize) -> usize {
    blocks(s)
        .into_iter()
        .find(|b| b.end > gidx)
        .map_or_else(|| s.graphemes(true).count(), |b| b.end)
}

/// Grapheme index at the start of the letterblock under or before `gidx`.
pub fn left(s: &str, gidx: usize) -> usize {
    blocks(s)
        .into_iter()
        .rev()
        .find(|b| b.start < gidx)
        .map_or(0, |b| b.start)
}

/// The letterblock under `gidx`, for double-click. Between blocks this is
/// the run of whitespace there; past the end of the line it is the last
/// block.
pub fn block_at(s: &str, gidx: usize) -> Range<usize> {
    let len = s.graphemes(true).count();
    let gidx = gidx.min(len.saturating_sub(1));
    let bs = blocks(s);
    if let Some(b) = bs.iter().find(|b| b.contains(&gidx)) {
        return b.clone();
    }
    let start = bs.iter().rev().find(|b| b.end <= gidx).map_or(0, |b| b.end);
    let end = bs.iter().find(|b| b.start > gidx).map_or(len, |b| b.start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The blocks of `s` as strings.
    fn split(s: &str) -> Vec<String> {
        let gs: Vec<&str> = s.graphemes(true).collect();
        blocks(s).into_iter().map(|r| gs[r].concat()).collect()
    }

    #[test]
    fn english_words_and_punctuation() {
        assert_eq!(split("hello, world_1"), ["hello", ",", "world_1"]);
    }

    #[test]
    fn japanese_splits_at_script_borders() {
        assert_eq!(
            split("あなたのギターになりたい！"),
            ["あなたの", "ギター", "になりたい", "！"]
        );
        assert_eq!(split("日本語のテキスト"), ["日本語", "の", "テキスト"]);
    }

    #[test]
    fn korean_splits_particles_and_endings() {
        assert_eq!(split("한국어를 배웁니다"), ["한국어", "를", "배웁니다"]);
        assert_eq!(split("먹었습니다"), ["먹", "었습니다"]);
        assert_eq!(split("학교에서는"), ["학교", "에서는"]);
        assert_eq!(split("서울로 집으로"), ["서울", "로", "집", "으로"]);
        assert_eq!(split("학생입니다"), ["학생", "입니다"]);
        assert_eq!(split("포도를"), ["포도", "를"]);
    }

    #[test]
    fn korean_particle_chains_stay_whole() {
        assert_eq!(split("에서는 한국어"), ["에서는", "한국어"]);
        assert_eq!(split("으로부터"), ["으로부터"]);
    }

    #[test]
    fn korean_nouns_that_look_like_particles_stay_whole() {
        assert_eq!(split("가을 사과 포도"), ["가을", "사과", "포도"]);
        assert_eq!(
            split("아마존웹서비시즈유한책임회사"),
            ["아마존웹서비시즈유한책임회사"]
        );
    }

    #[test]
    fn block_under_a_position() {
        let s = "학교에서는  日本語の";
        assert_eq!(block_at(s, 0), 0..2);
        assert_eq!(block_at(s, 3), 2..5);
        assert_eq!(block_at(s, 5), 5..7);
        assert_eq!(block_at(s, 8), 7..10);
        assert_eq!(block_at(s, 99), 10..11);
        assert_eq!(block_at("", 0), 0..0);
    }

    #[test]
    fn movement_skips_spaces() {
        let s = "hello  한국어를, world";
        assert_eq!(right(s, 0), 5);
        assert_eq!(right(s, 5), 10);
        assert_eq!(right(s, 10), 11);
        assert_eq!(right(s, 11), 12);
        assert_eq!(left(s, 18), 13);
        assert_eq!(left(s, 11), 10);
        assert_eq!(left(s, 10), 7);
        assert_eq!(left(s, 7), 0);
        assert_eq!(right("end  ", 3), 5);
    }
}
