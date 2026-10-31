//! Password safety heuristics.
//!
//! 1. A prompt that asks for a secret ("Password:", "비밀번호", "パスワード")
//!    while the tty still echoes is a field that will show what you type.
//! 2. Secrets typed into real password fields (echo off) are remembered
//!    only as salted 64-bit fingerprints, in memory, so a later line typed
//!    into a normal field can be recognised as "that was your password"
//!    without ever storing the password.

const PROMPT_WORDS: &[&str] = &[
    "password",
    "passphrase",
    "passcode",
    "pin:",
    "secret",
    "token:",
    "api key",
    "otp",
    "비밀번호",
    "암호",
    "패스워드",
    "パスワード",
    "暗証番号",
    "パスフレーズ",
];

/// Does `line` (the text before the cursor) look like it asks for a secret?
pub fn prompt_asks_for_secret(line: &str) -> bool {
    let l = line.trim_end().to_lowercase();
    if l.is_empty() || l.len() > 200 {
        return false;
    }
    let tail: String = l.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
    PROMPT_WORDS.iter().any(|w| tail.contains(w))
        && (tail.ends_with(':') || tail.ends_with('：') || tail.ends_with('?') || tail.ends_with('>'))
}

/// FNV-1a over salt + text. Not cryptographic; only used to compare in
/// memory against a per-process random salt.
pub fn fingerprint(salt: u64, s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ salt;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Remembers fingerprints of recent password-field entries.
pub struct SecretMemory {
    salt: u64,
    prints: Vec<u64>,
}

impl SecretMemory {
    pub fn new(salt: u64) -> Self {
        Self { salt, prints: Vec::new() }
    }

    pub fn remember(&mut self, secret: &str) {
        if secret.chars().count() < 4 {
            return;
        }
        let p = fingerprint(self.salt, secret);
        if !self.prints.contains(&p) {
            self.prints.push(p);
            if self.prints.len() > 16 {
                self.prints.remove(0);
            }
        }
    }

    /// Does `line` contain a remembered secret as a whitespace-separated word
    /// or as the whole line?
    pub fn matches(&self, line: &str) -> bool {
        if self.prints.is_empty() {
            return false;
        }
        let whole = line.trim();
        std::iter::once(whole)
            .chain(whole.split_whitespace())
            .any(|w| self.prints.contains(&fingerprint(self.salt, w)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_prompts() {
        assert!(prompt_asks_for_secret("[sudo] password for me:"));
        assert!(prompt_asks_for_secret("Enter passphrase for key '/x/id_ed25519': "));
        assert!(prompt_asks_for_secret("비밀번호:"));
        assert!(prompt_asks_for_secret("パスワード："));
        assert!(!prompt_asks_for_secret("~/src/password-manager %"));
        assert!(!prompt_asks_for_secret("$ "));
    }

    #[test]
    fn remembers_without_storing() {
        let mut m = SecretMemory::new(42);
        m.remember("hunter22");
        assert!(m.matches("hunter22"));
        assert!(m.matches("  echo hunter22 "));
        assert!(!m.matches("hunter2"));
    }
}
