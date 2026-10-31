//! Pseudo-terminal spawning for macOS (and other BSD-likes), with the few
//! libc / libproc functions declared by hand so the crate needs no deps.

use std::ffi::{CString, c_char, c_int, c_void};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::Path;

#[repr(C)]
#[derive(Default)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// macOS `struct termios` (tcflag_t and speed_t are unsigned long).
#[repr(C)]
struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 20],
    c_ispeed: u64,
    c_ospeed: u64,
}

const ECHO: u64 = 0x0000_0008;
const ICANON: u64 = 0x0000_0100;
const TIOCSWINSZ: u64 = 0x8008_7467;
const WNOHANG: c_int = 1;
const SIGHUP: c_int = 1;
const SIGKILL: c_int = 9;
const PROC_PIDVNODEPATHINFO: c_int = 9;
const VNODE_INFO_SIZE: usize = 152;
const MAXPATHLEN: usize = 1024;

unsafe extern "C" {
    fn forkpty(amaster: *mut c_int, name: *mut c_char, termp: *const c_void, winp: *const Winsize) -> c_int;
    fn ioctl(fd: c_int, req: u64, ...) -> c_int;
    fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
    fn tcgetpgrp(fd: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
    fn _exit(code: c_int) -> !;
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buf: *mut c_void, size: c_int) -> c_int;
    fn proc_name(pid: c_int, buf: *mut c_void, size: u32) -> c_int;
    fn ptsname(fd: c_int) -> *const c_char;
}

pub struct SpawnOptions<'a> {
    pub program: &'a str,
    pub args: Vec<String>,
    /// Pass argv[0] as `-name` (login shell).
    pub login: bool,
    pub cwd: Option<&'a Path>,
    /// Added to / overriding the inherited environment. `None` value removes.
    pub env: Vec<(String, Option<String>)>,
    pub cols: u16,
    pub rows: u16,
    pub px: (u16, u16),
}

pub struct Pty {
    master: File,
    pub pid: i32,
    pub tty: String,
}

impl Pty {
    pub fn spawn(o: &SpawnOptions) -> io::Result<Pty> {
        let cstr = |s: &str| CString::new(s).map_err(|_| io::Error::other("NUL in string"));
        let prog = cstr(o.program)?;
        let base = Path::new(o.program).file_name().and_then(|s| s.to_str()).unwrap_or(o.program);
        let argv0 = if o.login { format!("-{base}") } else { base.to_string() };
        let mut argv_c = vec![cstr(&argv0)?];
        for a in &o.args {
            argv_c.push(cstr(a)?);
        }
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        for (k, v) in &o.env {
            env.retain(|(ek, _)| ek != k);
            if let Some(v) = v {
                env.push((k.clone(), v.clone()));
            }
        }
        let env_c: Vec<CString> = env.iter().filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok()).collect();
        let cwd_c = o.cwd.and_then(|p| p.to_str()).and_then(|p| CString::new(p).ok());

        let mut argv_p: Vec<*const c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
        argv_p.push(std::ptr::null());
        let mut env_p: Vec<*const c_char> = env_c.iter().map(|c| c.as_ptr()).collect();
        env_p.push(std::ptr::null());

        let ws = Winsize { ws_row: o.rows, ws_col: o.cols, ws_xpixel: o.px.0, ws_ypixel: o.px.1 };
        let mut master: c_int = -1;
        // SAFETY: everything the child touches was allocated before fork;
        // the child only calls async-signal-safe functions.
        let pid = unsafe { forkpty(&mut master, std::ptr::null_mut(), std::ptr::null(), &ws) };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                if let Some(c) = &cwd_c {
                    chdir(c.as_ptr());
                }
                execve(prog.as_ptr(), argv_p.as_ptr(), env_p.as_ptr());
                _exit(127);
            }
        }
        let tty = unsafe {
            let p = ptsname(master);
            if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
        };
        // SAFETY: forkpty gave us ownership of `master`.
        let master = unsafe { File::from_raw_fd(master) };
        set_cloexec(master.as_raw_fd());
        Ok(Pty { master, pid, tty })
    }

    pub fn reader(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    pub fn writer(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    pub fn resize(&self, cols: u16, rows: u16, px: (u16, u16)) {
        let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: px.0, ws_ypixel: px.1 };
        unsafe {
            ioctl(self.master.as_raw_fd(), TIOCSWINSZ, &ws as *const Winsize);
        }
    }

    /// Echo off with canonical input on: a password prompt (sudo, ssh,
    /// `read -s`, getpass).
    pub fn password_mode(&self) -> bool {
        let mut t: Termios = unsafe { std::mem::zeroed() };
        if unsafe { tcgetattr(self.master.as_raw_fd(), &mut t) } != 0 {
            return false;
        }
        t.c_lflag & ECHO == 0 && t.c_lflag & ICANON != 0
    }

    /// Whether the tty echoes input at all.
    pub fn echo_on(&self) -> bool {
        let mut t: Termios = unsafe { std::mem::zeroed() };
        unsafe { tcgetattr(self.master.as_raw_fd(), &mut t) == 0 && t.c_lflag & ECHO != 0 }
    }

    /// Foreground process group leader of the tty.
    pub fn foreground_pid(&self) -> Option<i32> {
        let p = unsafe { tcgetpgrp(self.master.as_raw_fd()) };
        (p > 0).then_some(p)
    }

    /// Exit status if the child has exited.
    pub fn try_wait(&self) -> Option<i32> {
        let mut st: c_int = 0;
        let r = unsafe { waitpid(self.pid, &mut st, WNOHANG) };
        (r == self.pid).then_some(st)
    }

    pub fn hangup(&self) {
        unsafe {
            kill(self.pid, SIGHUP);
        }
    }

    pub fn kill(&self) {
        unsafe {
            kill(self.pid, SIGKILL);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.hangup();
        let mut st = 0;
        unsafe {
            waitpid(self.pid, &mut st, WNOHANG);
        }
    }
}

fn set_cloexec(fd: RawFd) {
    unsafe extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }
    const F_SETFD: c_int = 2;
    const FD_CLOEXEC: c_int = 1;
    unsafe {
        fcntl(fd, F_SETFD, FD_CLOEXEC);
    }
}

/// Current working directory of a process.
pub fn process_cwd(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; 2 * (VNODE_INFO_SIZE + MAXPATHLEN)];
    let n = unsafe { proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, buf.as_mut_ptr().cast(), buf.len() as c_int) };
    if n <= 0 {
        return None;
    }
    let path = &buf[VNODE_INFO_SIZE..VNODE_INFO_SIZE + MAXPATHLEN];
    let end = path.iter().position(|&b| b == 0)?;
    String::from_utf8(path[..end].to_vec()).ok().filter(|s| !s.is_empty())
}

/// Short name of a process (e.g. "vim").
pub fn process_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// The user's login shell (from `SHELL`, falling back to zsh).
pub fn default_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| Path::new(s).exists()).unwrap_or_else(|| "/bin/zsh".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn spawn_echo_and_password_mode() {
        let pty = Pty::spawn(&SpawnOptions {
            program: "/bin/sh",
            args: vec!["-c".into(), "printf 'hi 한글'; stty -echo; sleep 0.3".into()],
            login: false,
            cwd: Some(Path::new("/tmp")),
            env: vec![("TERM".into(), Some("xterm-256color".into()))],
            cols: 80,
            rows: 24,
            px: (0, 0),
        })
        .unwrap();
        let mut r = pty.reader().unwrap();
        let mut buf = [0u8; 64];
        let mut got = Vec::new();
        while !String::from_utf8_lossy(&got).contains("한글") {
            let n = r.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(!pty.echo_on());
        assert!(pty.process_cwd_ok());
        let mut w = pty.writer().unwrap();
        let _ = w.write_all(b"\n");
    }

    impl Pty {
        fn process_cwd_ok(&self) -> bool {
            process_cwd(self.pid).is_some_and(|p| p.ends_with("tmp"))
        }
    }
}
