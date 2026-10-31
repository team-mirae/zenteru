//! VT500-series escape sequence parser (after Paul Williams' state machine),
//! with UTF-8 decoding in the ground state and colon sub-parameters for
//! SGR (`38:2::r:g:b`, `4:3`).

pub const MAX_PARAMS: usize = 32;
const MAX_OSC: usize = 1 << 20;

/// CSI parameters. Each entry is a group: a value followed by its
/// colon-separated sub-parameters.
#[derive(Default, Clone, Debug)]
pub struct Params {
    vals: [u16; MAX_PARAMS],
    /// `sub[i]` is true when `vals[i]` was introduced by ':' (a sub-parameter).
    sub: [bool; MAX_PARAMS],
    len: usize,
    cur: u32,
    has_cur: bool,
    next_is_sub: bool,
}

impl Params {
    fn clear(&mut self) {
        self.len = 0;
        self.cur = 0;
        self.has_cur = false;
        self.next_is_sub = false;
    }

    fn digit(&mut self, d: u8) {
        self.cur = (self.cur * 10 + (d - b'0') as u32).min(65535);
        self.has_cur = true;
    }

    fn push(&mut self, sub_next: bool) {
        if self.len < MAX_PARAMS {
            self.vals[self.len] = self.cur as u16;
            self.sub[self.len] = self.next_is_sub;
            self.len += 1;
        }
        self.cur = 0;
        self.has_cur = false;
        self.next_is_sub = sub_next;
    }

    fn finish(&mut self) {
        if self.has_cur || self.len > 0 || self.next_is_sub {
            self.push(false);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Groups of `[value, sub...]`.
    pub fn groups(&self) -> Vec<&[u16]> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.len {
            let mut j = i + 1;
            while j < self.len && self.sub[j] {
                j += 1;
            }
            out.push(&self.vals[i..j]);
            i = j;
        }
        out
    }

    /// The `i`-th top-level parameter, with `default` used for 0 / missing.
    pub fn get(&self, i: usize, default: u16) -> u16 {
        match self.groups().get(i).map(|g| g[0]) {
            Some(0) | None => default,
            Some(v) => v,
        }
    }

    /// Like `get` but returns 0 when given as 0.
    pub fn raw(&self, i: usize) -> Option<u16> {
        self.groups().get(i).map(|g| g[0])
    }
}

pub trait Perform {
    fn print(&mut self, c: char);
    fn execute(&mut self, b: u8);
    fn csi(&mut self, params: &Params, inter: &[u8], action: char);
    fn esc(&mut self, inter: &[u8], b: u8);
    fn osc(&mut self, data: &[u8]);
    fn dcs(&mut self, params: &Params, inter: &[u8], action: char, data: &[u8]);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Escape,
    EscInter,
    CsiEntry,
    CsiParam,
    CsiInter,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsInter,
    DcsPass,
    DcsIgnore,
    Osc,
    Sos,
}

pub struct Parser {
    state: State,
    params: Params,
    inter: Vec<u8>,
    /// CSI private marker like '?' or '>' goes in `inter` too.
    osc: Vec<u8>,
    dcs_action: char,
    dcs: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    /// ESC seen inside a string state (possible ST).
    str_esc: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::default(),
            inter: Vec::new(),
            osc: Vec::new(),
            dcs_action: '\0',
            dcs: Vec::new(),
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            str_esc: false,
        }
    }

    pub fn advance<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) {
        for &b in bytes {
            self.byte(p, b);
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.inter.clear();
    }

    fn utf8_byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8_need == 0 {
            let need = match b {
                0xC2..=0xDF => 1,
                0xE0..=0xEF => 2,
                0xF0..=0xF4 => 3,
                _ => {
                    p.print('\u{FFFD}');
                    return;
                }
            };
            self.utf8[0] = b;
            self.utf8_len = 1;
            self.utf8_need = need;
            return;
        }
        if b & 0xC0 != 0x80 {
            // Broken sequence: replace and reprocess this byte.
            self.utf8_need = 0;
            p.print('\u{FFFD}');
            self.byte(p, b);
            return;
        }
        self.utf8[self.utf8_len] = b;
        self.utf8_len += 1;
        self.utf8_need -= 1;
        if self.utf8_need == 0 {
            let c = std::str::from_utf8(&self.utf8[..self.utf8_len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{FFFD}');
            p.print(c);
        }
    }

    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        use State::*;
        // String states consume nearly everything.
        match self.state {
            Osc | DcsPass | DcsIgnore | Sos => {
                if self.str_esc {
                    self.str_esc = false;
                    if b == b'\\' {
                        self.end_string(p);
                        self.state = Ground;
                        return;
                    }
                    // ESC + something else: terminate the string, then treat as escape.
                    self.end_string(p);
                    self.state = Escape;
                    self.clear();
                    self.byte(p, b);
                    return;
                }
                match b {
                    0x1B => self.str_esc = true,
                    0x07 if self.state == Osc => {
                        self.end_string(p);
                        self.state = Ground;
                    }
                    0x18 | 0x1A => self.state = Ground,
                    // 0x9C (C1 ST) is not honoured: it is a UTF-8 continuation
                    // byte (한 is ED 95 9C), and titles are UTF-8.
                    _ => match self.state {
                        Osc if self.osc.len() < MAX_OSC => self.osc.push(b),
                        DcsPass if self.dcs.len() < MAX_OSC => self.dcs.push(b),
                        _ => {}
                    },
                }
                return;
            }
            _ => {}
        }

        if self.utf8_need > 0 {
            self.utf8_byte(p, b);
            return;
        }

        // "Anywhere" transitions.
        match b {
            0x18 | 0x1A => {
                self.state = Ground;
                return;
            }
            0x1B => {
                self.state = Escape;
                self.clear();
                return;
            }
            _ => {}
        }

        match self.state {
            Ground => match b {
                0x00..=0x1F => p.execute(b),
                0x7F => {}
                0x20..=0x7E => p.print(b as char),
                _ => self.utf8_byte(p, b),
            },
            Escape => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => {
                    self.inter.push(b);
                    self.state = EscInter;
                }
                b'[' => {
                    self.clear();
                    self.state = CsiEntry;
                }
                b']' => {
                    self.osc.clear();
                    self.state = Osc;
                }
                b'P' => {
                    self.clear();
                    self.state = DcsEntry;
                }
                b'X' | b'^' | b'_' => self.state = Sos,
                0x30..=0x7E => {
                    p.esc(&self.inter, b);
                    self.state = Ground;
                }
                _ => self.state = Ground,
            },
            EscInter => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => self.inter.push(b),
                0x30..=0x7E => {
                    p.esc(&self.inter, b);
                    self.state = Ground;
                }
                _ => self.state = Ground,
            },
            CsiEntry | CsiParam => match b {
                0x00..=0x1F => p.execute(b),
                b'0'..=b'9' => {
                    self.params.digit(b);
                    self.state = CsiParam;
                }
                b';' => {
                    self.params.push(false);
                    self.state = CsiParam;
                }
                b':' => {
                    self.params.push(true);
                    self.state = CsiParam;
                }
                b'<'..=b'?' if self.state == CsiEntry => {
                    self.inter.push(b);
                    self.state = CsiParam;
                }
                b'<'..=b'?' => self.state = CsiIgnore,
                0x20..=0x2F => {
                    self.inter.push(b);
                    self.state = CsiInter;
                }
                0x40..=0x7E => {
                    self.params.finish();
                    p.csi(&self.params, &self.inter, b as char);
                    self.state = Ground;
                }
                _ => {}
            },
            CsiInter => match b {
                0x00..=0x1F => p.execute(b),
                0x20..=0x2F => self.inter.push(b),
                0x40..=0x7E => {
                    self.params.finish();
                    p.csi(&self.params, &self.inter, b as char);
                    self.state = Ground;
                }
                _ => self.state = CsiIgnore,
            },
            CsiIgnore => {
                if (0x40..=0x7E).contains(&b) {
                    self.state = Ground;
                }
            }
            DcsEntry | DcsParam => match b {
                b'0'..=b'9' => {
                    self.params.digit(b);
                    self.state = DcsParam;
                }
                b';' => {
                    self.params.push(false);
                    self.state = DcsParam;
                }
                b':' => {
                    self.params.push(true);
                    self.state = DcsParam;
                }
                b'<'..=b'?' if self.state == DcsEntry => {
                    self.inter.push(b);
                    self.state = DcsParam;
                }
                0x20..=0x2F => {
                    self.inter.push(b);
                    self.state = DcsInter;
                }
                0x40..=0x7E => self.dcs_start(b),
                0x00..=0x1F => {}
                _ => self.state = DcsIgnore,
            },
            DcsInter => match b {
                0x20..=0x2F => self.inter.push(b),
                0x40..=0x7E => self.dcs_start(b),
                0x00..=0x1F => {}
                _ => self.state = DcsIgnore,
            },
            Osc | DcsPass | DcsIgnore | Sos => unreachable!(),
        }
    }

    fn dcs_start(&mut self, b: u8) {
        self.params.finish();
        self.dcs_action = b as char;
        self.dcs.clear();
        self.state = State::DcsPass;
    }

    fn end_string<P: Perform>(&mut self, p: &mut P) {
        match self.state {
            State::Osc => {
                let data = std::mem::take(&mut self.osc);
                p.osc(&data);
                self.osc = data;
                self.osc.clear();
            }
            State::DcsPass => {
                let data = std::mem::take(&mut self.dcs);
                p.dcs(&self.params, &self.inter, self.dcs_action, &data);
                self.dcs = data;
                self.dcs.clear();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<String>);

    impl Perform for Log {
        fn print(&mut self, c: char) {
            self.0.push(format!("p{c}"));
        }
        fn execute(&mut self, b: u8) {
            self.0.push(format!("x{b}"));
        }
        fn csi(&mut self, params: &Params, inter: &[u8], action: char) {
            self.0.push(format!(
                "csi{}{:?}{action}",
                String::from_utf8_lossy(inter),
                params.groups()
            ));
        }
        fn esc(&mut self, inter: &[u8], b: u8) {
            self.0.push(format!("esc{}{}", String::from_utf8_lossy(inter), b as char));
        }
        fn osc(&mut self, data: &[u8]) {
            self.0.push(format!("osc{}", String::from_utf8_lossy(data)));
        }
        fn dcs(&mut self, _: &Params, inter: &[u8], a: char, data: &[u8]) {
            self.0.push(format!("dcs{}{a}{}", String::from_utf8_lossy(inter), String::from_utf8_lossy(data)));
        }
    }

    fn run(bytes: &[u8]) -> Vec<String> {
        let mut l = Log::default();
        Parser::new().advance(&mut l, bytes);
        l.0
    }

    #[test]
    fn utf8_and_controls() {
        assert_eq!(run("a한\n".as_bytes()), ["pa", "p한", "x10"]);
        assert_eq!(run(b"\xff"), ["p\u{FFFD}"]);
    }

    #[test]
    fn csi_params() {
        assert_eq!(run(b"\x1b[1;2H"), ["csi[[1], [2]]H"]);
        assert_eq!(run(b"\x1b[?1049h"), ["csi?[[1049]]h"]);
        assert_eq!(run(b"\x1b[38:2::10:20:30m"), ["csi[[38, 2, 0, 10, 20, 30]]m"]);
        assert_eq!(run(b"\x1b[m"), ["csi[]m"]);
        assert_eq!(run(b"\x1b[;5H"), ["csi[[0], [5]]H"]);
        assert_eq!(run(b"\x1b[2 q"), ["csi [[2]]q"]);
    }

    #[test]
    fn strings() {
        assert_eq!(run(b"\x1b]0;title\x07x"), ["osc0;title", "px"]);
        assert_eq!(run("\x1b]2;한글\x1b\\".as_bytes()), ["osc2;한글"]);
        assert_eq!(run(b"\x1bP$qm\x1b\\"), ["dcs$qm"]);
        assert_eq!(run(b"\x1b(0"), ["esc(0"]);
    }
}
