//! A session: one pty + one terminal, with a reader thread feeding the
//! parser and a writer thread draining input, so a slow program can never
//! block the UI.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use crate::input::shell_quote;
use crate::pty::{self, Pty, SpawnOptions};
use crate::term::Terminal;

pub struct SessionConfig<'a> {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
    pub cell_px: (u16, u16),
    pub cwd: Option<&'a Path>,
    /// Run this via `$SHELL -c` instead of an interactive login shell.
    pub command: Option<&'a str>,
    /// Root of the shipped `shell/` integration scripts, if available.
    pub shell_integration: Option<&'a Path>,
    /// Per-session private directory (created 0700, removed on drop).
    pub dir: PathBuf,
    pub api_socket: Option<&'a Path>,
    pub extra_env: Vec<(String, Option<String>)>,
}

pub struct Session {
    pub id: String,
    pub term: Arc<Mutex<Terminal>>,
    pty: Pty,
    tx: Sender<Vec<u8>>,
    pub dir: PathBuf,
    pub exited: Arc<AtomicBool>,
    pub dirty: Arc<AtomicBool>,
    pub shell: String,
    env_names: Vec<String>,
    pub history: Vec<String>,
}

impl Session {
    pub fn spawn(c: SessionConfig) -> io::Result<Session> {
        fs::create_dir_all(&c.dir)?;
        fs::set_permissions(&c.dir, fs::Permissions::from_mode(0o700))?;
        let shell = pty::default_shell();
        let shell_name = Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();

        let mut env: Vec<(String, Option<String>)> = vec![
            ("TERM".into(), Some("xterm-256color".into())),
            ("COLORTERM".into(), Some("truecolor".into())),
            ("TERM_PROGRAM".into(), Some("zenteru".into())),
            ("TERM_PROGRAM_VERSION".into(), Some(env!("CARGO_PKG_VERSION").into())),
            ("ZENTERU_SESSION_ID".into(), Some(c.id.clone())),
            ("ZENTERU_SESSION_DIR".into(), Some(c.dir.to_string_lossy().into())),
            // Never leak the plugin cookie into shells.
            ("ZENTERU_COOKIE".into(), None),
        ];
        if let Some(s) = c.api_socket {
            env.push(("ZENTERU_API_SOCKET".into(), Some(s.to_string_lossy().into())));
        }
        if std::env::var("LANG").map_or(true, |l| !l.to_ascii_uppercase().contains("UTF")) {
            env.push(("LANG".into(), Some("en_US.UTF-8".into())));
        }
        let mut args = Vec::new();
        let mut login = true;
        let mut program = shell.clone();
        if let Some(cmd) = c.command {
            login = false;
            args = vec!["-l".into(), "-c".into(), cmd.to_string()];
        } else if let Some(si) = c.shell_integration {
            env.push(("ZENTERU_SHELL_DIR".into(), Some(si.to_string_lossy().into())));
            match shell_name.as_str() {
                "zsh" => {
                    env.push(("ZENTERU_ORIG_ZDOTDIR".into(), std::env::var("ZDOTDIR").ok()));
                    env.push(("ZDOTDIR".into(), Some(si.join("zdotdir").to_string_lossy().into())));
                }
                "bash" => {
                    login = false;
                    args = vec!["--rcfile".into(), si.join("bashrc").to_string_lossy().into(), "-i".into()];
                }
                _ => {}
            }
        }
        if !Path::new(&program).exists() {
            program = "/bin/zsh".into();
        }
        env.extend(c.extra_env);
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        let cwd = c.cwd.map(Path::to_path_buf).or(home);
        let p = Pty::spawn(&SpawnOptions {
            program: &program,
            args,
            login,
            cwd: cwd.as_deref(),
            env,
            cols: c.cols,
            rows: c.rows,
            px: (c.cols * c.cell_px.0, c.rows * c.cell_px.1),
        })?;

        let mut t = Terminal::new(c.cols as usize, c.rows as usize);
        t.cell_px = c.cell_px;
        let term = Arc::new(Mutex::new(t));
        let exited = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(true));

        let (tx, rx) = channel::<Vec<u8>>();
        let mut w = p.writer()?;
        std::thread::Builder::new().name(format!("pty-w-{}", c.id)).spawn(move || {
            for buf in rx {
                if w.write_all(&buf).is_err() {
                    break;
                }
            }
        })?;

        let mut r: File = p.reader()?;
        let (term2, exited2, dirty2, tx2) = (term.clone(), exited.clone(), dirty.clone(), tx.clone());
        std::thread::Builder::new().name(format!("pty-r-{}", c.id)).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let resp = {
                            let mut t = term2.lock().unwrap();
                            t.feed(&buf[..n]);
                            t.take_responses()
                        };
                        if !resp.is_empty() {
                            let _ = tx2.send(resp);
                        }
                        dirty2.store(true, Ordering::Release);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            exited2.store(true, Ordering::Release);
            dirty2.store(true, Ordering::Release);
        })?;

        Ok(Session { id: c.id, term, pty: p, tx, dir: c.dir, exited, dirty, shell: shell_name, env_names: Vec::new(), history: Vec::new() })
    }

    /// Bytes typed by the user.
    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.tx.send(bytes.to_vec());
        }
    }

    /// Bytes sent by a plugin: setenvvar variables are skipped for the
    /// next command (see docs/PROTOCOL.md "setenvvar store").
    pub fn write_as_plugin(&self, bytes: &[u8]) {
        if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            let _ = File::create(self.dir.join("skip-env-once"));
        }
        self.write(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16, px: (u16, u16)) {
        {
            let mut t = self.term.lock().unwrap();
            t.cell_px = px;
            t.resize(cols as usize, rows as usize);
        }
        self.pty.resize(cols, rows, (cols * px.0, rows * px.1));
        self.dirty.store(true, Ordering::Release);
    }

    /// Hang up the shell (the tab is closing).
    pub fn hangup(&self) {
        self.pty.hangup();
    }

    pub fn pid(&self) -> i32 {
        self.pty.pid
    }

    pub fn tty(&self) -> &str {
        &self.pty.tty
    }

    pub fn password_mode(&self) -> bool {
        !self.exited.load(Ordering::Acquire) && self.pty.password_mode()
    }

    pub fn echo_on(&self) -> bool {
        self.pty.echo_on()
    }

    /// Working directory: OSC 7 if reported, else the foreground process's.
    pub fn cwd(&self) -> Option<String> {
        if let Some(c) = self.term.lock().unwrap().prompt.cwd.clone() {
            return Some(c);
        }
        self.pty.foreground_pid().and_then(pty::process_cwd).or_else(|| pty::process_cwd(self.pty.pid))
    }

    /// Name of the foreground job (e.g. "vim"), if not the shell itself.
    pub fn job_name(&self) -> Option<String> {
        let fg = self.pty.foreground_pid()?;
        pty::process_name(fg)
    }

    pub fn is_shell_foreground(&self) -> bool {
        self.pty.foreground_pid().is_none_or(|p| p == self.pty.pid)
    }

    // ---- setenvvar store ----

    pub fn env_set(&mut self, name: &str, value: &str) -> io::Result<()> {
        if !valid_env_name(name) {
            return Err(io::Error::other("invalid variable name"));
        }
        self.env_names.retain(|n| n != name);
        self.env_names.push(name.to_string());
        let mut vals = self.read_env_values();
        vals.retain(|(n, _)| n != name);
        vals.push((name.to_string(), value.to_string()));
        self.write_env(&vals)
    }

    pub fn env_unset(&mut self, name: &str) -> io::Result<()> {
        self.env_names.retain(|n| n != name);
        let mut vals = self.read_env_values();
        vals.retain(|(n, _)| n != name);
        self.write_env(&vals)?;
        // Keep the name listed once so the shell hook unsets it if exported.
        Ok(())
    }

    pub fn env_names(&self) -> &[String] {
        &self.env_names
    }

    fn read_env_values(&self) -> Vec<(String, String)> {
        // Values live only in the 0600 file; parse our own format back.
        let Ok(s) = fs::read_to_string(self.dir.join("env.values")) else { return Vec::new() };
        s.lines()
            .filter_map(|l| {
                let (n, v) = l.split_once('=')?;
                let v = crate::base64::decode(v)?;
                Some((n.to_string(), String::from_utf8(v).ok()?))
            })
            .collect()
    }

    fn write_env(&self, vals: &[(String, String)]) -> io::Result<()> {
        let mut sh = String::from("# zenteru setenvvar — sourced before each user command\n");
        let mut raw = String::new();
        for (n, v) in vals {
            sh.push_str(&format!("export {n}={}\n", shell_quote(v)));
            raw.push_str(&format!("{n}={}\n", crate::base64::encode(v.as_bytes())));
        }
        let names: String = vals.iter().map(|(n, _)| format!("{n}\n")).collect();
        write_private(&self.dir.join("env"), &sh)?;
        write_private(&self.dir.join("env.values"), &raw)?;
        write_private(&self.dir.join("env.names"), &names)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_private(p: &Path, s: &str) -> io::Result<()> {
    let tmp = p.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        f.write_all(s.as_bytes())?;
    }
    fs::rename(tmp, p)
}

pub fn valid_env_name(n: &str) -> bool {
    let mut it = n.chars();
    it.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_validated() {
        assert!(valid_env_name("API_KEY"));
        assert!(!valid_env_name("1X"));
        assert!(!valid_env_name("A-B"));
        assert!(!valid_env_name("A;rm"));
    }

    #[test]
    fn spawn_runs_command_and_env_store() {
        let dir = std::env::temp_dir().join(format!("zt-sess-{}", std::process::id()));
        let mut s = Session::spawn(SessionConfig {
            id: "s1".into(),
            cols: 40,
            rows: 5,
            cell_px: (8, 16),
            cwd: None,
            command: Some("printf '한글 OK'; sleep 1"),
            shell_integration: None,
            dir: dir.clone(),
            api_socket: None,
            extra_env: vec![],
        })
        .unwrap();
        for _ in 0..200 {
            if s.term.lock().unwrap().screen()[0].text().contains("OK") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(s.term.lock().unwrap().screen()[0].text(), "한글 OK");
        s.env_set("TOKEN", "a b'c").unwrap();
        let sh = fs::read_to_string(dir.join("env")).unwrap();
        assert!(sh.contains("export TOKEN='a b'\\''c'"));
        assert_eq!(fs::metadata(dir.join("env")).unwrap().permissions().mode() & 0o777, 0o600);
        s.env_unset("TOKEN").unwrap();
        assert!(!fs::read_to_string(dir.join("env")).unwrap().contains("TOKEN"));
        drop(s);
        assert!(!dir.exists());
    }
}
