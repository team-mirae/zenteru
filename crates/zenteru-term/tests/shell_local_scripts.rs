//! End-to-end: bare names of scripts in $PWD run through zenteru's
//! command-not-found handler (shell/zenteru-local.zsh). A separate test
//! binary from shell_integration.rs because both set ZDOTDIR process-wide.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zenteru_term::session::{Session, SessionConfig};
use zenteru_term::term::PromptPhase;

fn wait(s: &Session, what: &str, f: impl Fn(&Session) -> bool) {
    let t = Instant::now();
    while !f(s) {
        if t.elapsed() > Duration::from_secs(10) {
            let screen: Vec<String> = s.term.lock().unwrap().screen().iter().map(|r| r.text()).collect();
            panic!("timed out waiting for {what}; screen:\n{}", screen.join("\n"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn screen_has(s: &Session, needle: &str) -> bool {
    let t = s.term.lock().unwrap();
    (t.dropped..t.abs_end()).any(|l| t.line(l).is_some_and(|r| r.text().contains(needle)))
}

fn file(dir: &Path, name: &str, body: &str, mode: u32) {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Run `cmd`, wait for `needle` and the next prompt; return its exit status.
fn run(s: &mut Session, cmd: &str, needle: &str) -> Option<i32> {
    run_as(s, cmd, cmd, needle)
}

/// Type `typed`, which the shell runs as `ran`; as `run`.
fn run_as(s: &mut Session, typed: &str, ran: &str, needle: &str) -> Option<i32> {
    s.write(format!("{typed}\r").as_bytes());
    wait(s, needle, |s| screen_has(s, needle));
    wait(s, "prompt", |s| {
        let t = s.term.lock().unwrap();
        t.prompt.phase == PromptPhase::Editing && t.prompt.command.as_deref() == Some(ran)
    });
    s.term.lock().unwrap().prompt.exit_status
}

/// Type `typed` and Tab; the edit line must become `want`, with or without a
/// trailing space (compctl adds none). Clears the line.
fn tab(s: &mut Session, typed: &str, want: &str) {
    s.write(format!("{typed}\t").as_bytes());
    wait(s, &format!("{typed}<Tab> -> {want:?}"), |s| s.term.lock().unwrap().edit_line().0.trim_end() == want);
    s.write(&[0x15]);
    wait(s, "cleared line", |s| s.term.lock().unwrap().edit_line().0.is_empty());
}

#[test]
fn zsh_runs_local_scripts_by_bare_name() {
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../shell").canonicalize().unwrap();
    let tmp = std::env::temp_dir().join(format!("zt-local-{}", std::process::id()));
    let zdot = tmp.join("zdot");
    let work = tmp.join("work");
    std::fs::create_dir_all(&zdot).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    // A handler of the user's own, defined in .zshrc (which loads after the
    // integration): zenteru must chain to it, not replace it.
    // `open` is stubbed so nothing actually opens.
    std::fs::write(zdot.join(".zshrc"), "PS1='%# '\ncommand_not_found_handler() { print -r -- \"PREV-HANDLER:$1\"; return 127; }\nopen() { print -r -- \"OPENED:$*\"; }\n").unwrap();
    file(&work, "exec.sh", "#!/bin/sh\necho \"EXEC-RAN:$*\"\nexit 3\n", 0o755);
    file(&work, "plain.sh", "echo PLAIN-RAN\n", 0o644);
    file(&work, "bang.txt", "#!/bin/sh\necho BANG-RAN\n", 0o644);
    file(&work, "zz-notes.txt", "just notes\n", 0o644);
    std::fs::create_dir_all(work.join("zz-folder")).unwrap();
    file(&work, "echo", "#!/bin/sh\necho SHADOWED\n", 0o755);
    unsafe {
        std::env::set_var("SHELL", "/bin/zsh");
        std::env::set_var("ZDOTDIR", &zdot);
    }
    let mut s = Session::spawn(SessionConfig {
        id: "s1".into(),
        cols: 100,
        rows: 30,
        cell_px: (8, 16),
        cwd: Some(&work),
        command: None,
        shell_integration: Some(&root),
        dir: tmp.join("session"),
        api_socket: None,
        extra_env: vec![],
    })
    .unwrap();
    wait(&s, "first prompt", |s| s.term.lock().unwrap().prompt.phase == PromptPhase::Editing);

    // Executable: ./NAME with its arguments, a visible note, its exit status.
    assert_eq!(run(&mut s, "exec.sh a 'b c'", "EXEC-RAN:a b c"), Some(3));
    assert!(screen_has(&s, "zenteru: running ./exec.sh"));
    // Not executable: by extension, or by its #! line.
    assert_eq!(run(&mut s, "plain.sh", "PLAIN-RAN"), Some(0));
    assert!(screen_has(&s, "zenteru: running sh ./plain.sh"));
    assert_eq!(run(&mut s, "bang.txt", "BANG-RAN"), Some(0));
    // Nothing to run it with: opens.
    assert_eq!(run(&mut s, "zz-notes.txt", "OPENED:-- ./zz-notes.txt"), Some(0));
    assert!(screen_has(&s, "zenteru: opening ./zz-notes.txt"));
    // A folder alone on the line: cd, AUTO_CD or not (it's off here).
    assert_eq!(run_as(&mut s, "zz-folder", "cd -- zz-folder", "cd -- zz-folder"), Some(0));
    assert_eq!(run(&mut s, "print -r -- IN:${PWD:t}", "IN:zz-folder"), Some(0));
    assert_eq!(run_as(&mut s, "..", "cd -- ..", "cd -- .."), Some(0));
    assert_eq!(run_as(&mut s, "./zz-folder", "cd -- ./zz-folder", "cd -- ./zz-folder"), Some(0));
    assert_eq!(run(&mut s, "print -r -- AGAIN:${PWD:t}", "AGAIN:zz-folder"), Some(0));
    assert_eq!(run_as(&mut s, "..", "cd -- ..", "cd -- .."), Some(0));
    assert_eq!(run(&mut s, "print -r -- BACK:${PWD:t}", "BACK:work"), Some(0));
    // Not a file: falls through to the user's own handler.
    assert_eq!(run(&mut s, "nosuchthing", "PREV-HANDLER:nosuchthing"), Some(127));
    // A path that can't run as is gets its interpreter or `open --` in
    // front, visibly, instead of "permission denied".
    assert_eq!(run_as(&mut s, "./zz-notes.txt", "open -- ./zz-notes.txt", "OPENED:-- ./zz-notes.txt"), Some(0));
    assert_eq!(run_as(&mut s, "./plain.sh", "sh ./plain.sh", "PLAIN-RAN"), Some(0));
    // An executable path is left alone.
    assert_eq!(run(&mut s, "./exec.sh x", "EXEC-RAN:x"), Some(3));
    // Tab in the first word offers files too (compctl: no compinit here).
    tab(&mut s, "zz-no", "zz-notes.txt");
    tab(&mut s, "./zz-no", "./zz-notes.txt");
    // A real command always wins over a local file of the same name.
    assert_eq!(run(&mut s, "echo REAL-ECHO", "REAL-ECHO"), Some(0));
    assert!(!screen_has(&s, "SHADOWED"));

    s.write(&[0x15]);
    drop(s);

    // The same Tab with compinit set up in .zshrc.
    std::fs::write(zdot.join(".zshrc"), "PS1='%# '\nautoload -Uz compinit && compinit -u -D\n").unwrap();
    let mut s = Session::spawn(SessionConfig {
        id: "s2".into(),
        cols: 100,
        rows: 30,
        cell_px: (8, 16),
        cwd: Some(&work),
        command: None,
        shell_integration: Some(&root),
        dir: tmp.join("session2"),
        api_socket: None,
        extra_env: vec![],
    })
    .unwrap();
    wait(&s, "first prompt", |s| s.term.lock().unwrap().prompt.phase == PromptPhase::Editing);
    tab(&mut s, "zz-no", "zz-notes.txt");
    tab(&mut s, "./zz-no", "./zz-notes.txt");
    // Commands still complete.
    tab(&mut s, "ech", "echo");
    s.write(&[0x15]);
    drop(s);
    let _ = std::fs::remove_dir_all(&tmp);
}
