//! ⌘-click / ⌃-click on terminal text. A link opens in the default browser,
//! a folder in a new tab, a script in a new tab after a countdown you can
//! cancel, and any other file in its default app.
//!
//! What's under the pointer: an OSC 8 hyperlink if the cell has one, else
//! the word there (soft-wrapped rows joined), with surrounding quotes and
//! punctuation trimmed. A word counts when it's a URL or names something
//! that exists, relative to the shell's folder. `path:12:3` (compiler
//! output) finds `path`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zenteru_term::lineref::{self, LineRef};
use zenteru_term::term::Terminal;

#[derive(Debug, PartialEq)]
pub enum Target {
    Url(String),
    Dir(PathBuf),
    Script(PathBuf),
    File(PathBuf),
}

/// The target under absolute line `abs`, column `col`.
pub fn target_at(t: &Terminal, abs: u64, col: usize, cwd: Option<&Path>) -> Option<Target> {
    if let Some(uri) = link_at(t, abs, col) {
        return classify(&uri, cwd);
    }
    classify(&word_at(t, abs, col)?, cwd)
}

fn link_at(t: &Terminal, abs: u64, col: usize) -> Option<String> {
    let row = t.line(abs)?;
    let mut c = col.min(row.cells.len().checked_sub(1)?);
    if c > 0 && row.cells[c].is_spacer() {
        c -= 1;
    }
    let id = row.cells[c].link;
    (id != 0).then(|| t.link(id).map(String::from)).flatten()
}

fn is_delim(s: &str) -> bool {
    let Some(c) = s.chars().next() else { return true };
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '\u{2500}'..='\u{259F}')
}

/// The word around the click, over soft-wrapped rows, trimmed.
fn word_at(t: &Terminal, abs: u64, col: usize) -> Option<String> {
    let mut start = abs;
    while start > t.dropped && t.line(start - 1).is_some_and(|r| r.wrapped) {
        start -= 1;
    }
    let mut cells: Vec<String> = Vec::new();
    let mut at = None;
    let mut l = start;
    while let Some(row) = t.line(l) {
        for (i, c) in row.cells.iter().enumerate() {
            if c.is_spacer() {
                if l == abs && i == col {
                    at = cells.len().checked_sub(1);
                }
                continue;
            }
            if l == abs && i == col {
                at = Some(cells.len());
            }
            let mut s = String::new();
            c.push_text(&mut s);
            cells.push(s);
        }
        if !row.wrapped {
            break;
        }
        l += 1;
    }
    let at = at?;
    if is_delim(&cells[at]) {
        return None;
    }
    let a = (0..at).rev().take_while(|&i| !is_delim(&cells[i])).last().unwrap_or(at);
    let b = (at..cells.len()).take_while(|&i| !is_delim(&cells[i])).last().unwrap_or(at);
    let word = trim_word(&cells[a..=b].concat());
    (!word.is_empty()).then_some(word)
}

/// Drop wrapping brackets and trailing sentence punctuation; a closing
/// bracket stays when the word opened it (`wiki/Foo_(bar)`).
pub fn trim_word(w: &str) -> String {
    let mut s = w.trim_start_matches(['(', '[', '{']);
    loop {
        let before = s.len();
        s = s.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            if s.ends_with(close) && s.matches(open).count() < s.matches(close).count() {
                s = &s[..s.len() - 1];
            }
        }
        if s.len() == before {
            break;
        }
    }
    // A URL inside a word (`href=https://…`, `<https://…>`).
    if let Some(i) = s.find("://") {
        let head = &s[..i];
        let from = head.rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))).map_or(0, |j| j + 1);
        if from < i {
            return s[from..].to_string();
        }
    }
    s.to_string()
}

fn has_scheme(s: &str) -> bool {
    let Some(i) = s.find("://") else { return false };
    let scheme = &s[..i];
    i + 3 < s.len()
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// What a word (or link URI) is, if anything.
pub fn classify(raw: &str, cwd: Option<&Path>) -> Option<Target> {
    if let Some(rest) = raw.strip_prefix("file://") {
        // file://host/path: the host is ours (or empty).
        let path = &rest[rest.find('/')?..];
        return path_target(Path::new(&percent_decode(path)));
    }
    if has_scheme(raw) || (raw.starts_with("mailto:") && raw.contains('@')) {
        return Some(Target::Url(raw.to_string()));
    }
    if raw.starts_with("www.") && raw[4..].contains('.') {
        return Some(Target::Url(format!("https://{raw}")));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut tries = vec![raw.to_string()];
    // file.rs:12:3, file.rs:12, file.rs:12:3:
    let mut s = raw.trim_end_matches(':');
    for _ in 0..2 {
        match s.rsplit_once(':') {
            Some((head, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                s = head;
                tries.push(s.to_string());
            }
            _ => break,
        }
    }
    // git diff's a/… and b/…
    for p in [raw.strip_prefix("a/"), raw.strip_prefix("b/")].into_iter().flatten() {
        tries.push(p.to_string());
    }
    for t in tries {
        let p = if t == "~" {
            home.clone()?
        } else if let Some(r) = t.strip_prefix("~/") {
            home.as_ref()?.join(r)
        } else if t.starts_with('/') {
            PathBuf::from(&t)
        } else {
            cwd?.join(&t)
        };
        if let Some(target) = path_target(&p) {
            return Some(target);
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn path_target(p: &Path) -> Option<Target> {
    let meta = std::fs::metadata(p).ok()?;
    let p = std::fs::canonicalize(p).ok()?;
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if meta.is_dir() {
        // Bundles (Foo.app, Bar.pages) open like files.
        let bundle = matches!(ext.as_deref(), Some("app" | "bundle" | "pages" | "numbers" | "key" | "rtfd" | "xcodeproj" | "xcworkspace" | "playground"));
        return Some(if bundle { Target::File(p) } else { Target::Dir(p) });
    }
    Some(if is_script(&p, &meta, ext.as_deref()) { Target::Script(p) } else { Target::File(p) })
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "ksh", "dash", "fish", "csh", "tcsh"];

/// A shell script by extension or `#!`, or any executable with a `#!` or
/// Mach-O header (so a program never launches in another app unasked).
fn is_script(p: &Path, meta: &std::fs::Metadata, ext: Option<&str>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if ext.is_some_and(|e| SHELLS.contains(&e) || e == "command" || e == "tool") {
        return true;
    }
    let mut head = [0u8; 256];
    let n = std::fs::File::open(p).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
    let head = &head[..n];
    let exec = meta.permissions().mode() & 0o111 != 0;
    if let Some(line) = head.strip_prefix(b"#!") {
        let line = String::from_utf8_lossy(line.split(|&b| b == b'\n').next().unwrap_or(&[])).into_owned();
        let mut words = line.split_whitespace();
        let mut prog = words.next().unwrap_or("").rsplit('/').next().unwrap_or("");
        if prog == "env" {
            prog = words.find(|w| !w.starts_with('-')).unwrap_or("");
        }
        return exec || SHELLS.contains(&prog);
    }
    let macho = head.len() >= 4 && matches!(head[..4], [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]);
    exec && macho
}

// ---- the countdown before a clicked script runs ---------------------------

pub const COUNTDOWN: Duration = Duration::from_secs(3);

/// A script waiting to run in a fresh tab. The notice is printed before
/// the shell's first prompt and rewritten in place each second, so the
/// shell's line editor is never disturbed.
pub struct Countdown {
    /// The row the notice starts on (`Row::id`; survives scrolling and reflow).
    pub row: u128,
    /// What gets typed into the shell when time is up.
    pub line: String,
    pub started: Instant,
    pub shown: u64,
}

pub fn countdown_text(secs: u64) -> String {
    let unit = if secs == 1 { "second" } else { "seconds" };
    format!("Executing shell script in {secs} {unit}. Press any key or abort to cancel.")
}

pub const CANCELLED: &str = "Cancelled: the shell script was not executed.";
pub const EXECUTING: &str = "Executing shell script.";

/// Print the notice (dim) and return the id of the row it starts on.
pub fn print_notice(t: &mut Terminal) -> u128 {
    let (line, _) = t.cursor_abs();
    t.feed(format!("\x1b[2m{}\x1b[0m\r\n", countdown_text(COUNTDOWN.as_secs())).as_bytes());
    t.line(line).map_or(0, |r| r.id)
}

/// Rewrite the notice's text in place, blanking what's left of the old one.
pub fn rewrite_notice(t: &mut Terminal, row: u128, text: &str) {
    let Some(&abs) = lineref::resolve(t, &LineRef::Id(row)).first() else { return };
    let width = countdown_text(COUNTDOWN.as_secs()).chars().count().max(CANCELLED.len());
    let mut chars = text.chars().chain(std::iter::repeat(' ')).take(width);
    let mut l = abs;
    loop {
        let Some(r) = t.line_mut(l) else { return };
        let wrapped = r.wrapped;
        for cell in r.cells.iter_mut() {
            let Some(c) = chars.next() else { return };
            cell.c = c;
            cell.extra = None;
        }
        if !wrapped {
            return;
        }
        l += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn term(text: &str, cols: usize) -> Terminal {
        let mut t = Terminal::new(cols, 5);
        t.feed(text.as_bytes());
        t
    }

    #[test]
    fn trims_words() {
        assert_eq!(trim_word("(https://example.com/a)."), "https://example.com/a");
        assert_eq!(trim_word("https://en.wikipedia.org/wiki/Foo_(bar),"), "https://en.wikipedia.org/wiki/Foo_(bar)");
        assert_eq!(trim_word("href=https://x.io/y"), "https://x.io/y");
        assert_eq!(trim_word("src/main.rs:12:3:"), "src/main.rs:12:3");
    }

    #[test]
    fn urls() {
        assert_eq!(classify("https://example.com", None), Some(Target::Url("https://example.com".into())));
        assert_eq!(classify("www.example.com", None), Some(Target::Url("https://www.example.com".into())));
        assert_eq!(classify("mailto:a@b.c", None), Some(Target::Url("mailto:a@b.c".into())));
        assert_eq!(classify("://nope", None), None);
        assert_eq!(classify("nothing-here-at-all", Some(Path::new("/"))), None);
    }

    #[test]
    fn words_and_links_on_screen() {
        let t = term("see https://example.com/x, ok", 80);
        assert_eq!(word_at(&t, 0, 8).as_deref(), Some("https://example.com/x"));
        assert_eq!(word_at(&t, 0, 3), None);
        // Soft-wrapped across rows.
        let t = term("go https://example.com/long/path now", 20);
        assert_eq!(word_at(&t, 1, 2).as_deref(), Some("https://example.com/long/path"));
        // OSC 8: the link's URI, not its text.
        let t = term("\x1b]8;;https://example.com/\x07click\x1b]8;;\x07 here", 80);
        assert_eq!(target_at(&t, 0, 2, None), Some(Target::Url("https://example.com/".into())));
        assert_eq!(target_at(&t, 0, 7, None), None);
    }

    #[test]
    fn paths() {
        let d = std::env::temp_dir().join(format!("zt-click-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let d = std::fs::canonicalize(&d).unwrap();
        std::fs::write(d.join("notes.txt"), "hi").unwrap();
        std::fs::write(d.join("run.sh"), "echo hi").unwrap();
        std::fs::write(d.join("tool"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::write(d.join("lib.py"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::set_permissions(d.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = Some(d.as_path());
        assert_eq!(classify("sub", c), Some(Target::Dir(d.join("sub"))));
        assert_eq!(classify("./sub/", c), Some(Target::Dir(d.join("sub"))));
        assert_eq!(classify("notes.txt:3:1", c), Some(Target::File(d.join("notes.txt"))));
        assert_eq!(classify("b/notes.txt", c), Some(Target::File(d.join("notes.txt"))));
        assert_eq!(classify("run.sh", c), Some(Target::Script(d.join("run.sh"))));
        assert_eq!(classify("tool", c), Some(Target::Script(d.join("tool"))));
        assert_eq!(classify("lib.py", c), Some(Target::File(d.join("lib.py"))));
        let url = format!("file://localhost{}", d.join("notes.txt").display()).replace("notes", "%6Eotes");
        assert_eq!(classify(&url, None), Some(Target::File(d.join("notes.txt"))));
        assert_eq!(classify("missing.txt", c), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn notice_rewrites_in_place() {
        let mut t = Terminal::new(40, 6);
        let id = print_notice(&mut t);
        t.feed(b"$ ");
        rewrite_notice(&mut t, id, &countdown_text(1));
        let both = |t: &Terminal| t.line(0).unwrap().text() + &t.line(1).unwrap().text();
        assert_eq!(both(&t), countdown_text(1));
        rewrite_notice(&mut t, id, CANCELLED);
        assert_eq!(both(&t), CANCELLED);
        assert_eq!(t.line(2).unwrap().text(), "$");
    }
}
