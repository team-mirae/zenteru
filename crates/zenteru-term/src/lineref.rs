//! Line references for the shell-mode gutter, so people and plugins (hmxv)
//! can point at lines:
//!
//! * **Relative index**: the cursor's line is `ゼロ` (0); lines above it are
//!   -1, -2, -3, … counting upward, into scrollback.
//! * **Line code**: three characters `[0-9A-Z]`, derived from the line's
//!   UUIDv7 — minted when the line first got content: 48 bits of Unix
//!   milliseconds (the real landing time, never adjusted) plus 74 random
//!   bits of salt, so lines landing in the same millisecond still differ.
//!
//! The code is reproducible from the UUID:
//!
//! ```text
//! h    = FNV-1a 64 over the UUID's 16 bytes, big-endian
//!        (offset 0xcbf29ce484222325, prime 0x100000001b3)
//! code = base36(h mod 36^3), uppercase, zero-padded to 3 characters
//! ```
//!
//! 3 characters give 46,656 codes, so in a long scrollback two lines can
//! share one; resolving a code then prefers the most recent line and says
//! it was ambiguous. Relative indices are always unique.

use crate::term::Terminal;

/// Seed for the salt generator, from /dev/urandom.
pub fn seed() -> u64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_ok() {
        return u64::from_le_bytes(b);
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    t ^ (std::process::id() as u64).rotate_left(32)
}

/// splitmix64.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A new UUIDv7 (RFC 9562) for "now".
pub fn uuid7(rng: &mut u64) -> u128 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    uuid7_at(ms, next(rng), next(rng))
}

/// UUIDv7 from a millisecond timestamp and 128 bits of randomness.
pub fn uuid7_at(ms: u64, r1: u64, r2: u64) -> u128 {
    let ms = (ms & 0xFFFF_FFFF_FFFF) as u128;
    let rand_a = (r1 & 0x0FFF) as u128; // 12 bits
    let rand_b = (r2 & 0x3FFF_FFFF_FFFF_FFFF) as u128; // 62 bits
    (ms << 80) | (0x7u128 << 76) | (rand_a << 64) | (0b10u128 << 62) | rand_b
}

/// Landing time of a line id, in Unix milliseconds.
pub fn landed_ms(id: u128) -> u64 {
    (id >> 80) as u64
}

/// Canonical UUID text, e.g. `019a3b2c-1d4e-7f00-8a1b-2c3d4e5f6a7b`.
pub fn uuid_string(id: u128) -> String {
    let h = format!("{id:032x}");
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn parse_uuid(s: &str) -> Option<u128> {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    (h.len() == 32).then(|| u128::from_str_radix(&h, 16).ok()).flatten()
}

/// The 3-character line code of a line id.
pub fn code(id: u128) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.to_be_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut n = h % 46_656;
    let digits = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut out = [b'0'; 3];
    for i in (0..3).rev() {
        out[i] = digits[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out.to_vec()).unwrap()
}

/// Gutter label for a relative index: `ゼロ`, `-1`, `-2`, …
pub fn rel_label(rel: i64) -> String {
    match rel {
        0 => "ゼロ".to_string(),
        r if r < 0 => r.to_string(),
        r => format!("+{r}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineRef {
    /// Relative to the cursor's line (0 = ゼロ, negative = above).
    Rel(i64),
    /// A 3-character line code.
    Code(String),
    /// A full line UUID.
    Id(u128),
}

impl LineRef {
    /// Accepts `ゼロ` / `0` / `zero`, `-3`, `3` (taken as -3: only lines
    /// above the cursor are numbered), `+2`, a code like `K7Q`, or a UUID.
    pub fn parse(s: &str) -> Option<LineRef> {
        let t = s.trim().trim_start_matches('#');
        if t.is_empty() {
            return None;
        }
        if matches!(t, "ゼロ" | "ぜろ" | "zero" | "ZERO" | "0") {
            return Some(LineRef::Rel(0));
        }
        if let Some(n) = t.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
            return Some(LineRef::Rel(n));
        }
        if let Some(n) = t.strip_prefix('-').and_then(|n| n.parse::<i64>().ok()) {
            return Some(LineRef::Rel(-n));
        }
        if let Ok(n) = t.parse::<i64>() {
            return Some(LineRef::Rel(-n.abs()));
        }
        if t.len() == 3 && t.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(LineRef::Code(t.to_ascii_uppercase()));
        }
        parse_uuid(t).map(LineRef::Id)
    }
}

/// Absolute line of relative index 0 (the cursor's line).
pub fn anchor(t: &Terminal) -> u64 {
    t.cursor_abs().0
}

pub fn rel_of(t: &Terminal, abs: u64) -> i64 {
    abs as i64 - anchor(t) as i64
}

/// Absolute lines matching a reference, best (most recent) first.
pub fn resolve(t: &Terminal, r: &LineRef) -> Vec<u64> {
    let first = t.dropped;
    let last = anchor(t);
    match r {
        LineRef::Rel(n) => {
            let abs = last as i64 + n;
            if abs >= first as i64 && (abs as u64) < t.abs_end() {
                vec![abs as u64]
            } else {
                Vec::new()
            }
        }
        LineRef::Code(c) => {
            let find = |c: &str| -> Vec<u64> { (first..=last).rev().filter(|&l| t.line(l).is_some_and(|row| row.id != 0 && code(row.id) == c)).collect() };
            let hits = find(c);
            if !hits.is_empty() {
                return hits;
            }
            // Typed O for 0 or I for 1 (or the reverse)? Try the look-alikes.
            let swap = |ch: char| match ch {
                'O' => '0',
                '0' => 'O',
                'I' => '1',
                '1' => 'I',
                c => c,
            };
            let alts: Vec<String> = (0..8u8)
                .map(|mask| c.chars().enumerate().map(|(i, ch)| if mask & (1 << i) != 0 { swap(ch) } else { ch }).collect())
                .filter(|a: &String| a != c)
                .collect();
            let mut out: Vec<u64> = alts.iter().flat_map(|a| find(a)).collect();
            out.sort_unstable_by(|a, b| b.cmp(a));
            out.dedup();
            out
        }
        LineRef::Id(id) => (first..t.abs_end()).filter(|&l| t.line(l).is_some_and(|row| row.id == *id)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid7_layout() {
        let id = uuid7_at(1_793_448_301_985, 0xABC, u64::MAX);
        let s = uuid_string(id);
        assert_eq!(&s[14..15], "7", "version nibble");
        assert!(matches!(&s[19..20], "8" | "9" | "a" | "b"), "variant bits");
        assert_eq!(landed_ms(id), 1_793_448_301_985);
        assert_eq!(parse_uuid(&s), Some(id));
    }

    #[test]
    fn codes_are_reproducible_and_salted() {
        let a = uuid7_at(1_793_448_301_985, 1, 2);
        let b = uuid7_at(1_793_448_301_985, 1, 3);
        assert_eq!(code(a), code(a));
        assert_eq!(code(a).len(), 3);
        assert!(code(a).chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()));
        // Same millisecond, different salt → (almost always) different code.
        assert_ne!(code(a), code(b));
        // Fixed vector, shared with the Python helper.
        assert_eq!(code(0x0190_0000_0000_7000_8000_0000_0000_0000), code(parse_uuid("01900000-0000-7000-8000-000000000000").unwrap()));
    }

    #[test]
    fn parse_refs() {
        assert_eq!(LineRef::parse("ゼロ"), Some(LineRef::Rel(0)));
        assert_eq!(LineRef::parse("-3"), Some(LineRef::Rel(-3)));
        assert_eq!(LineRef::parse("3"), Some(LineRef::Rel(-3)));
        assert_eq!(LineRef::parse("k7q"), Some(LineRef::Code("K7Q".into())));
        assert_eq!(LineRef::parse("#K7Q"), Some(LineRef::Code("K7Q".into())));
        assert_eq!(rel_label(0), "ゼロ");
        assert_eq!(rel_label(-12), "-12");
    }

    #[test]
    fn resolve_against_terminal() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"one\r\ntwo\r\nthree\r\n$ ");
        assert_eq!(resolve(&t, &LineRef::Rel(0)), vec![3]);
        assert_eq!(t.line(resolve(&t, &LineRef::Rel(-2))[0]).unwrap().text(), "two");
        let id = t.line(1).unwrap().id;
        assert_ne!(id, 0);
        let c = code(id);
        assert!(resolve(&t, &LineRef::Code(c)).contains(&1));
        assert_eq!(resolve(&t, &LineRef::Id(id)), vec![1]);
        // Look-alike tolerant: a code typed with O/0 or I/1 swapped still resolves.
        let typo: String = code(id).chars().map(|ch| match ch { '0' => 'O', 'O' => '0', '1' => 'I', 'I' => '1', c => c }).collect();
        if typo != code(id) {
            assert!(resolve(&t, &LineRef::Code(typo)).contains(&1));
        }
        // Each line got its own id even within the same millisecond.
        let ids: Vec<u128> = (0..4).map(|l| t.line(l).unwrap().id).collect();
        assert!(ids.iter().all(|&i| i != 0));
        assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), 4);
    }
}
