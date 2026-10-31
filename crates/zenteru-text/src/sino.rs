//! Which regional glyph set to draw Sino characters (漢字 / 한자 / 汉字)
//! with. The same code point looks different per region (直, 骨, 写), so
//! the font is picked by preference, not by code point.
//!
//! `SINO_DISPLAY` selects it: `JA` (default, 新字体), `KO`, or `SG`
//! (simplified). Anything else falls back to `JA`. Non-JA is best effort.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SinoDisplay {
    Ja,
    Ko,
    Sg,
}

impl SinoDisplay {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "KO" => Self::Ko,
            "SG" => Self::Sg,
            _ => Self::Ja,
        }
    }

    /// From `SINO_DISPLAY`, read once.
    pub fn current() -> Self {
        static V: OnceLock<SinoDisplay> = OnceLock::new();
        *V.get_or_init(|| {
            std::env::var("SINO_DISPLAY")
                .map(|v| Self::parse(&v))
                .unwrap_or(Self::Ja)
        })
    }

    /// BCP 47 tag, for asking the system font cascade for regional glyphs.
    pub fn language_tag(self) -> &'static str {
        match self {
            Self::Ja => "ja-JP",
            Self::Ko => "ko-KR",
            Self::Sg => "zh-Hans",
        }
    }
}

/// Script buckets that need a dedicated CJK font.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Script {
    Other,
    Hangul,
    Kana,
    Han,
}

pub fn script(c: char) -> Script {
    match c {
        '\u{AC00}'..='\u{D7A3}'
        | '\u{1100}'..='\u{11FF}'
        | '\u{3130}'..='\u{318F}'
        | '\u{A960}'..='\u{A97F}'
        | '\u{D7B0}'..='\u{D7FF}'
        | '\u{FFA0}'..='\u{FFDC}' => Script::Hangul,
        '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9F}' => {
            Script::Kana
        }
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{3134F}'
        | '\u{3000}'..='\u{303F}'
        | '\u{FF00}'..='\u{FF65}'
        | '\u{2E80}'..='\u{2FDF}' => Script::Han,
        _ => Script::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_to_ja() {
        assert_eq!(SinoDisplay::parse("ko"), SinoDisplay::Ko);
        assert_eq!(SinoDisplay::parse("SG"), SinoDisplay::Sg);
        assert_eq!(SinoDisplay::parse("TW"), SinoDisplay::Ja);
        assert_eq!(SinoDisplay::parse(""), SinoDisplay::Ja);
    }

    #[test]
    fn scripts() {
        assert_eq!(script('한'), Script::Hangul);
        assert_eq!(script('あ'), Script::Kana);
        assert_eq!(script('直'), Script::Han);
        assert_eq!(script('a'), Script::Other);
    }
}
