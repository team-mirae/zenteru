//! Keyboard and mouse encoding, plus "natural text editing": ⌥←/⌥→ and
//! ⌥⌫ move or delete by zencode letterblocks (so 한국어를 is 한국어 | 를 and
//! あなたのギター is あなたの | ギター) by sending the right number of
//! arrow / backspace keys, which every line editor understands.

use zenteru_text::letterblock;

use crate::term::{Modes, MouseMode};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
}

impl Mods {
    /// xterm modifier parameter (1 + bits), or 1 for none.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    /// Text produced by the key (after IME / layout), e.g. "a", "한".
    Text(String),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

/// Encode a key for the pty. `ctrl_char` is the unmodified character of
/// the key, used for Ctrl combinations (Ctrl+C → 0x03).
pub fn encode_key(key: &Key, mods: Mods, modes: &Modes) -> Vec<u8> {
    let m = mods.param();
    let csi_mod = |final_: char, num: Option<u8>| -> Vec<u8> {
        match (num, m) {
            (None, 1) => {
                if modes.app_cursor && matches!(final_, 'A' | 'B' | 'C' | 'D' | 'H' | 'F') {
                    format!("\x1bO{final_}").into_bytes()
                } else {
                    format!("\x1b[{final_}").into_bytes()
                }
            }
            (None, m) => format!("\x1b[1;{m}{final_}").into_bytes(),
            (Some(n), 1) => format!("\x1b[{n}{final_}").into_bytes(),
            (Some(n), m) => format!("\x1b[{n};{m}{final_}").into_bytes(),
        }
    };
    let alt_prefix = |mut v: Vec<u8>| {
        if mods.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    match key {
        Key::Text(t) => {
            if mods.ctrl {
                if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                    if let Some(b) = ctrl_byte(c) {
                        return alt_prefix(vec![b]);
                    }
                }
            }
            alt_prefix(t.as_bytes().to_vec())
        }
        Key::Enter => alt_prefix(if modes.newline { b"\r\n".to_vec() } else { b"\r".to_vec() }),
        Key::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                alt_prefix(b"\t".to_vec())
            }
        }
        Key::Backspace => alt_prefix(if mods.ctrl { vec![0x08] } else { vec![0x7f] }),
        Key::Escape => alt_prefix(vec![0x1b]),
        Key::Up => csi_mod('A', None),
        Key::Down => csi_mod('B', None),
        Key::Right => csi_mod('C', None),
        Key::Left => csi_mod('D', None),
        Key::Home => csi_mod('H', None),
        Key::End => csi_mod('F', None),
        Key::PageUp => csi_mod('~', Some(5)),
        Key::PageDown => csi_mod('~', Some(6)),
        Key::Insert => csi_mod('~', Some(2)),
        Key::Delete => csi_mod('~', Some(3)),
        Key::F(n) => match (n, m) {
            (1..=4, 1) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
            (1..=4, m) => format!("\x1b[1;{m}{}", (b'P' + n - 1) as char).into_bytes(),
            _ => {
                let code = match n {
                    5 => 15,
                    6 => 17,
                    7 => 18,
                    8 => 19,
                    9 => 20,
                    10 => 21,
                    11 => 23,
                    12 => 24,
                    13 => 25,
                    14 => 26,
                    15 => 28,
                    16 => 29,
                    17 => 31,
                    18 => 32,
                    19 => 33,
                    20 => 34,
                    _ => return Vec::new(),
                };
                csi_mod('~', Some(code))
            }
        },
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Natural-editing actions bound to ⌥ / ⌘ + arrows and delete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditAction {
    WordLeft,
    WordRight,
    DeleteWordBack,
    DeleteWordForward,
    LineStart,
    LineEnd,
    DeleteToLineStart,
    DeleteToLineEnd,
}

/// Bytes for an edit action. `line`/`cursor` are the command line and the
/// cursor's grapheme index in it (see `Terminal::edit_line`). In the
/// alternate screen (vim, less, …) the classic meta sequences are sent so
/// full-screen programs keep their own bindings.
pub fn encode_edit(action: EditAction, line: &str, cursor: usize, alt_screen: bool) -> Vec<u8> {
    use EditAction::*;
    if alt_screen {
        return match action {
            WordLeft => b"\x1bb".to_vec(),
            WordRight => b"\x1bf".to_vec(),
            DeleteWordBack => b"\x1b\x7f".to_vec(),
            DeleteWordForward => b"\x1bd".to_vec(),
            LineStart => vec![0x01],
            LineEnd => vec![0x05],
            DeleteToLineStart => vec![0x15],
            DeleteToLineEnd => vec![0x0b],
        };
    }
    let repeat = |s: &[u8], n: usize| s.repeat(n);
    match action {
        WordLeft => repeat(b"\x1b[D", cursor - letterblock::left(line, cursor).min(cursor)),
        WordRight => repeat(b"\x1b[C", letterblock::right(line, cursor).saturating_sub(cursor)),
        DeleteWordBack => repeat(b"\x7f", cursor - letterblock::left(line, cursor).min(cursor)),
        DeleteWordForward => repeat(b"\x1b[3~", letterblock::right(line, cursor).saturating_sub(cursor)),
        LineStart => vec![0x01],
        LineEnd => vec![0x05],
        DeleteToLineStart => vec![0x15],
        DeleteToLineEnd => vec![0x0b],
    }
}

/// Keys that strip whitespace around a command line, from what's on screen:
/// `line`/`cursor` as for `encode_edit`. Emacs-style keys (⌃A, ⌃E, ⌃D,
/// Backspace) that bash, zsh and most line editors share. Spaces typed at
/// the end of the line leave no trace on screen, so trailing ones are only
/// seen up to the cursor; with the cursor inside the text they stay.
pub fn trim_keys(line: &str, cursor: usize) -> Vec<u8> {
    use zenteru_text::text::{byte_index, grapheme_count};
    let g: Vec<&str> = (0..grapheme_count(line)).map(|i| &line[byte_index(line, i)..byte_index(line, i + 1)]).collect();
    let space = |s: &&&str| s.chars().all(char::is_whitespace);
    let lead = g.iter().take_while(space).count();
    if lead == g.len() {
        // Nothing but whitespace: clear the line (⌃E then ⌃U, which kills
        // the whole line in zsh and back to the start in bash).
        return if cursor == 0 && g.is_empty() { Vec::new() } else { vec![0x05, 0x15] };
    }
    let trail = g.iter().rev().take_while(space).count();
    let text_end = g.len() - trail;
    let mut out = Vec::new();
    if cursor >= text_end {
        // At the end: delete back to the text, which leaves the cursor there.
        out.extend(b"\x7f".repeat(cursor - text_end));
        let to = text_end - lead;
        out.push(0x01);
        out.extend(std::iter::repeat_n(0x04, lead));
        out.extend(b"\x1b[C".repeat(to));
    } else {
        // Inside the text: only the leading run, then back to the same character.
        out.push(0x01);
        out.extend(std::iter::repeat_n(0x04, lead));
        out.extend(b"\x1b[C".repeat(cursor.saturating_sub(lead)));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Motion with no button held.
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Encode a mouse event at cell (`col`, `row`) (0-based), or `None` when
/// the program hasn't asked for this kind of event.
pub fn encode_mouse(btn: MouseButton, action: MouseAction, col: usize, row: usize, mods: Mods, modes: &Modes) -> Option<Vec<u8>> {
    match (modes.mouse, action) {
        (MouseMode::Off, _) => return None,
        (MouseMode::Click, MouseAction::Motion) => return None,
        (MouseMode::Drag, MouseAction::Motion) if btn == MouseButton::None => return None,
        _ => {}
    }
    let mut code: u32 = match btn {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    code += 4 * mods.shift as u32 + 8 * mods.alt as u32 + 16 * mods.ctrl as u32;
    if modes.mouse_sgr {
        let fin = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{fin}", col + 1, row + 1).into_bytes());
    }
    if action == MouseAction::Release {
        code = 3 + (code & !3);
    }
    let enc = |v: usize| (v + 33).min(255) as u8;
    Some(vec![0x1b, b'[', b'M', (code + 32).min(255) as u8, enc(col), enc(row)])
}

/// Wrap pasted text for the pty: bracketed when the program asked for it,
/// with any embedded end-marker removed so a paste can't end itself.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        let clean = normalized.replace("\x1b[201~", "");
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(clean.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        normalized.into_bytes()
    }
}

/// Quote a path for a POSIX shell, leaving it bare when that's safe.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "/._-+,:@%".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let m = Modes::default();
        assert_eq!(encode_key(&Key::Text("c".into()), Mods { ctrl: true, ..Default::default() }, &m), [3]);
        assert_eq!(encode_key(&Key::Up, Mods::default(), &m), b"\x1b[A");
        assert_eq!(encode_key(&Key::Up, Mods { shift: true, ..Default::default() }, &m), b"\x1b[1;2A");
        let app = Modes { app_cursor: true, ..Modes::default() };
        assert_eq!(encode_key(&Key::Left, Mods::default(), &app), b"\x1bOD");
        assert_eq!(encode_key(&Key::F(5), Mods::default(), &m), b"\x1b[15~");
        assert_eq!(encode_key(&Key::Text("x".into()), Mods { alt: true, ..Default::default() }, &m), b"\x1bx");
    }

    #[test]
    fn natural_editing_uses_letterblocks() {
        // @trim from the screen: leading run always, trailing up to the cursor.
    assert_eq!(trim_keys("  ls -l", 9), [b"\x7f".repeat(2), vec![0x01, 0x04, 0x04], b"\x1b[C".repeat(5)].concat());
    assert_eq!(trim_keys("  ls", 3), [vec![0x01, 0x04, 0x04], b"\x1b[C".repeat(1)].concat());
    assert_eq!(trim_keys("　한국어", 4), [vec![0x01, 0x04], b"\x1b[C".repeat(3)].concat());
    assert_eq!(trim_keys("ls", 2), [vec![0x01], b"\x1b[C".repeat(2)].concat());
    assert_eq!(trim_keys("", 3), vec![0x05, 0x15]);
    assert!(trim_keys("", 0).is_empty());

    // "echo 한국어를" with the cursor at the end: ⌥⌫ removes 를 only.
        let line = "echo 한국어를";
        let end = 9;
        assert_eq!(encode_edit(EditAction::DeleteWordBack, line, end, false), b"\x7f");
        assert_eq!(encode_edit(EditAction::WordLeft, line, 8, false), b"\x1b[D\x1b[D\x1b[D");
        assert_eq!(encode_edit(EditAction::WordRight, "あなたのギター", 0, false), b"\x1b[C".repeat(4));
        assert_eq!(encode_edit(EditAction::WordLeft, line, end, true), b"\x1bb");
    }

    #[test]
    fn mouse() {
        let m = Modes { mouse: MouseMode::Drag, mouse_sgr: true, ..Modes::default() };
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Press, 4, 2, Mods::default(), &m).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert!(encode_mouse(MouseButton::None, MouseAction::Motion, 0, 0, Mods::default(), &m).is_none());
    }

    #[test]
    fn paste_is_bracketed_and_sanitized() {
        let m = Modes { bracketed_paste: true, ..Modes::default() };
        assert_eq!(encode_paste("a\nb\x1b[201~", &m), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(shell_quote("/tmp/a b's.png"), "'/tmp/a b'\\''s.png'");
        assert_eq!(shell_quote("/tmp/shot.png"), "/tmp/shot.png");
    }
}
