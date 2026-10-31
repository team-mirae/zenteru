//! Black Magic: zenteru's command line, opened by typing `@` where a
//! program isn't reading keys itself (an empty shell prompt — local or over
//! ssh — or vim outside insert / command-line mode), or with ⌘⇧P.
//!
//! Enter routes the line:
//!   1. a built-in command (the old palette commands)   → run it
//!   2. a plugin command (by `name`, handler or title)  → that plugin
//!   3. anything else → the agentic helper (e.g. hmxv), if one is
//!      registered (`rpc.register kind:"agent"`), with the whole line
//!   4. no helper → "unknown command"; the bar stays open.
//!
//! `@@` types a literal `@`. `help` and `hmxv` are reserved for hmxv.

use zenteru_term::input::{Key, Mods};
use zenteru_term::json::Json;

use crate::app::{App, Palette};

/// Command names a plugin may only claim if it says hello as the owner
/// ("" = zenteru's own built-ins: no plugin may claim them).
const RESERVED: &[(&str, &str)] = &[("help", "hmxv"), ("hmxv", "hmxv"), ("info", ""), ("version", ""), ("zenteru", ""), ("music", ""), ("play", ""), ("pause", ""), ("mute", ""), ("unmute", ""), ("volume", "")];

pub const SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish", "pwsh", "-zsh", "-bash"];
pub const REMOTES: &[&str] = &["ssh", "mosh-client", "mosh", "et", "autossh", "tsh"];
const VIMS: &[&str] = &["vim", "nvim", "vi", "view", "vimdiff", "gvim", "mvim"];

#[derive(Clone, Debug)]
pub struct Cmd {
    pub handler: String,
    pub title: String,
    /// What you type after `@`, e.g. `new-tab`, `help`.
    pub name: String,
    pub builtin: bool,
}

impl App {
    /// Every command: built-ins, then plugin palette commands.
    pub fn magic_commands(&self) -> Vec<Cmd> {
        let mut out: Vec<Cmd> = crate::app::BUILTIN_COMMANDS
            .iter()
            .map(|(h, t, n)| Cmd { handler: h.to_string(), title: t.to_string(), name: n.to_string(), builtin: true })
            .collect();
        if let Some(api) = &self.api {
            for r in api.registrations("command") {
                let title = r.params.get("title").as_str().unwrap_or(&r.handler).to_string();
                let mut name = r.params.get("name").as_str().map(str::to_lowercase).unwrap_or_else(|| r.handler.to_lowercase());
                if let Some((_, owner)) = RESERVED.iter().find(|(n, _)| *n == name) {
                    if api.conn_name(r.conn) != *owner {
                        name = r.handler.to_lowercase();
                    }
                }
                out.push(Cmd { handler: r.handler.clone(), title, name, builtin: false });
            }
        }
        out
    }

    /// Would an `@` typed now open Black Magic?
    pub fn magic_trigger(&self, sid: &str) -> bool {
        let Some(s) = self.sessions.get(sid) else { return false };
        if s.pwd.active || s.comp_pending.is_some() {
            return false;
        }
        let job = s.s.job_name().unwrap_or_default();
        let job = job.rsplit('/').next().unwrap_or("").to_string();
        let t = s.s.term.lock().unwrap();
        if t.alt_screen() {
            return VIMS.contains(&job.as_str()) && vim_not_typing(&t);
        }
        let shell_fg = s.s.is_shell_foreground() || SHELLS.contains(&job.as_str());
        let remote = REMOTES.contains(&job.as_str());
        if !shell_fg && !remote {
            return false;
        }
        // Local shell with zenteru's integration: exact.
        if shell_fg && t.prompt.phase == zenteru_term::term::PromptPhase::Editing && t.prompt.input_start.is_some() {
            let (line, cursor) = t.edit_line();
            return line.trim().is_empty() && cursor == 0;
        }
        // ssh / no integration: nothing typed since Enter, and the cursor
        // sits at the end of something that looks like a shell prompt.
        if s.line_typed {
            return false;
        }
        let Some(row) = t.screen().get(t.cursor.y) else { return false };
        let mut before = String::new();
        let mut after = String::new();
        for (x, c) in row.cells.iter().enumerate() {
            if c.is_spacer() {
                continue;
            }
            if x < t.cursor.x { c.push_text(&mut before) } else { c.push_text(&mut after) }
        }
        after.trim().is_empty() && looks_like_prompt(&before)
    }

    /// `@` (or ＠) typed: open the bar if allowed. Returns true if consumed.
    pub fn magic_at(&mut self, wid: &str, sid: &str, text: &str) -> bool {
        if (text == "@" || text == "＠") && self.magic_trigger(sid) {
            self.open_magic(wid);
            return true;
        }
        false
    }

    pub fn open_magic(&mut self, wid: &str) {
        if let Some(w) = self.win(wid) {
            w.palette = Some(Palette::default());
            w.dirty = true;
        }
    }

    /// Text typed into the bar.
    pub fn magic_text(&mut self, wid: &str, text: &str) {
        let sid = self.win(wid).and_then(|w| w.active_session()).map(String::from);
        let Some(w) = self.win(wid) else { return };
        let Some(p) = &mut w.palette else { return };
        p.preedit.clear();
        // `@@` → a literal `@` for the program underneath.
        if p.text.is_empty() && (text == "@" || text == "＠") {
            w.palette = None;
            w.dirty = true;
            if let Some(sid) = sid {
                self.send_user(&sid, text.as_bytes());
            }
            return;
        }
        p.text.push_str(text);
        p.selected = 0;
        p.moved = false;
        p.message = None;
        w.dirty = true;
    }

    /// Commands suggested for what's typed, best first.
    pub fn magic_items(&self, text: &str) -> Vec<Cmd> {
        let all = self.magic_commands();
        let tl = text.trim().to_lowercase();
        let first = tl.split_whitespace().next().unwrap_or("").to_string();
        let mut scored: Vec<(i32, Cmd)> = all
            .into_iter()
            .filter_map(|c| {
                if tl.is_empty() {
                    return Some((0, c));
                }
                let title = c.title.to_lowercase();
                if c.name == first {
                    return Some((300, c));
                }
                if c.name.starts_with(&first) {
                    return Some((200, c));
                }
                if title.contains(&tl) {
                    return Some((150 - title.find(&tl).unwrap() as i32, c));
                }
                let mut it = title.chars();
                tl.chars().all(|ch| it.any(|x| x == ch)).then_some((1, c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    /// Run a line (Enter). Returns true if the bar should close.
    pub fn run_magic(&mut self, wid: &str, line: &str, picked: Option<Cmd>) -> bool {
        let line = line.trim();
        let (tok, rest) = match line.split_once(char::is_whitespace) {
            Some((a, b)) => (a, b.trim()),
            None => (line, ""),
        };
        let tok_l = tok.to_lowercase();
        let cmd = picked.or_else(|| {
            if line.is_empty() {
                return None;
            }
            self.magic_commands().into_iter().find(|c| c.name == tok_l || c.handler.to_lowercase() == tok_l || c.title.to_lowercase() == line.to_lowercase())
        });
        if let Some(c) = cmd {
            if c.builtin {
                let b = c.handler.trim_start_matches("zenteru.").to_string();
                if matches!(b.as_str(), "music" | "music_focus" | "play" | "pause" | "mute" | "unmute" | "volume") {
                    // These take arguments: `@volume 40`, `@music pause`.
                    self.music(wid, &b, rest);
                } else {
                    self.builtin(wid, &b);
                }
            } else {
                self.invoke_command_with(wid, &c.handler, Some(rest));
            }
            return true;
        }
        if line.is_empty() {
            return true;
        }
        // Unknown: hand the whole line to an agentic helper, if any.
        let agent = self.api.as_ref().and_then(|a| {
            let regs = a.registrations("agent");
            regs.iter().max_by_key(|r| r.params.get("priority").as_i64().unwrap_or(0)).cloned()
        });
        if let Some(reg) = agent {
            let w = self.win(wid);
            let tid = w.as_ref().and_then(|w| w.tabs.get(w.active)).map(|t| t.id.clone());
            let sid = w.and_then(|w| w.active_session()).map(String::from);
            let args = Json::obj().with("text", line).with("window_id", wid).with("tab_id", tid).with("session_id", sid).with("source", "black_magic");
            self.call_handler(&reg, args);
            return true;
        }
        if let Some(w) = self.win(wid) {
            if let Some(p) = &mut w.palette {
                p.message = Some(format!("unknown command: {tok}"));
            }
            w.dirty = true;
        }
        false
    }

    /// Keys while the bar is open.
    pub fn magic_key(&mut self, wid: &str, key: &Key, mods: Mods) -> bool {
        let text = match self.win(wid).and_then(|w| w.palette.as_ref()) {
            Some(p) => p.text.clone(),
            None => return true,
        };
        let items = self.magic_items(&text);
        let Some(w) = self.win(wid) else { return true };
        let Some(p) = &mut w.palette else { return true };
        w.dirty = true;
        match key {
            Key::Escape => w.palette = None,
            Key::Up => {
                p.selected = p.selected.saturating_sub(1);
                p.moved = true;
            }
            Key::Down => {
                p.selected = (p.selected + 1).min(items.len().saturating_sub(1));
                p.moved = true;
            }
            Key::Tab => {
                // Complete the command name.
                if let Some(c) = items.get(p.selected) {
                    let rest = p.text.split_once(char::is_whitespace).map(|(_, r)| r.to_string()).unwrap_or_default();
                    p.text = format!("{} {rest}", c.name).trim_end().to_string() + " ";
                    p.moved = false;
                    p.message = None;
                }
            }
            Key::Backspace => {
                if p.text.is_empty() {
                    w.palette = None;
                } else if mods.alt || mods.cmd {
                    let n = p.text.trim_end().rfind(char::is_whitespace).map_or(0, |i| i + 1);
                    p.text.truncate(if mods.cmd { 0 } else { n });
                } else {
                    p.text.pop();
                }
                if let Some(p) = &mut w.palette {
                    p.selected = 0;
                    p.moved = false;
                    p.message = None;
                }
            }
            Key::Enter => {
                let picked = if p.moved { items.get(p.selected).cloned() } else { None };
                let line = p.text.clone();
                if self.run_magic(wid, &line, picked) {
                    if let Some(w) = self.win(wid) {
                        w.palette = None;
                        w.dirty = true;
                    }
                }
            }
            Key::Text(t) if mods.ctrl && (t == "u" || t == "U") => {
                p.text.clear();
                p.message = None;
            }
            Key::Text(t) if mods.ctrl && (t == "c" || t == "C") => w.palette = None,
            Key::Text(_) if mods.ctrl || mods.cmd => {}
            Key::Text(_) => return false,
            _ => {}
        }
        true
    }
}

/// vim (alt screen) outside insert/replace/visual typing and outside the
/// `:` / `/` command line: the bottom line has no `-- INSERT --` style mode
/// and the cursor isn't on it.
fn vim_not_typing(t: &zenteru_term::term::Terminal) -> bool {
    let last = t.rows.saturating_sub(1);
    if t.cursor.y == last {
        // On the `:` / `/` command line.
        return false;
    }
    // Insert/replace usually switch to a bar or underline cursor (neovim's
    // default guicursor, most vim configs).
    if matches!(t.modes.cursor_style, 3..=6) {
        return false;
    }
    // `showmode` (-- INSERT --) or a statusline plugin (airline, lightline:
    // " INSERT ") on the bottom two lines.
    let lines: String = (last.saturating_sub(1)..=last).filter_map(|y| t.screen().get(y)).map(|r| r.text()).collect::<Vec<_>>().join("\n");
    let typing = ["INSERT", "REPLACE", "-- (insert)", "TERMINAL", "-- SELECT"];
    !typing.iter().any(|m| lines.contains(m))
}

/// The text before the cursor ends like a shell prompt: `$ `, `% `, `# `,
/// `❯ `, `» `, `➜ `… A lone `> ` (PS2, or a TUI's own input box) and box
/// drawn input fields are not prompts.
pub fn looks_like_prompt(before: &str) -> bool {
    let t = before.trim_end();
    if t.is_empty() || t == ">" {
        return false;
    }
    if t.trim_start().starts_with(['│', '┃', '║', '╭', '╰', '|']) {
        return false;
    }
    t.ends_with(['$', '%', '#', '❯', '»', '➜', '>', 'λ'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_heuristics() {
        assert!(looks_like_prompt("me@host:~$ "));
        assert!(looks_like_prompt("[root@box ~]# "));
        assert!(looks_like_prompt("~/src ❯ "));
        assert!(looks_like_prompt("me@mac ~ % "));
        assert!(!looks_like_prompt("> "));
        assert!(!looks_like_prompt("│ > "));
        assert!(!looks_like_prompt("Password: "));
        assert!(!looks_like_prompt(""));
    }

    #[test]
    fn vim_modes() {
        let mut t = zenteru_term::term::Terminal::new(20, 5);
        t.feed(b"\x1b[?1049h\x1b[5;1H-- INSERT --\x1b[1;1H");
        assert!(!vim_not_typing(&t));
        t.feed(b"\x1b[5;1H\x1b[2K\x1b[1;1H");
        assert!(vim_not_typing(&t));
        t.feed(b"\x1b[5;1H:");
        assert!(!vim_not_typing(&t), "on the : command line");
        t.feed(b"\x1b[2K\x1b[4;1H NORMAL  file.txt\x1b[1;1H");
        assert!(vim_not_typing(&t), "airline normal");
        t.feed(b"\x1b[4;1H INSERT  file.txt\x1b[1;1H");
        assert!(!vim_not_typing(&t), "airline insert");
        t.feed(b"\x1b[4;1H\x1b[2K\x1b[5 q");
        assert!(!vim_not_typing(&t), "bar cursor = insert");
    }
}
