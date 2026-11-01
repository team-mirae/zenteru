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
    s.write(format!("{cmd}\r").as_bytes());
    wait(s, needle, |s| screen_has(s, needle));
    wait(s, "prompt", |s| {
        let t = s.term.lock().unwrap();
        t.prompt.phase == PromptPhase::Editing && t.prompt.command.as_deref() == Some(cmd)
    });
    s.term.lock().unwrap().prompt.exit_status
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
    std::fs::write(zdot.join(".zshrc"), "PS1='%# '\ncommand_not_found_handler() { print -r -- \"PREV-HANDLER:$1\"; return 127; }\n").unwrap();
    file(&work, "exec.sh", "#!/bin/sh\necho \"EXEC-RAN:$*\"\nexit 3\n", 0o755);
    file(&work, "plain.sh", "echo PLAIN-RAN\n", 0o644);
    file(&work, "bang.txt", "#!/bin/sh\necho BANG-RAN\n", 0o644);
    file(&work, "notes.txt", "just notes\n", 0o644);
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
    // Nothing to run it with: falls through to the user's own handler.
    assert_eq!(run(&mut s, "notes.txt", "PREV-HANDLER:notes.txt"), Some(127));
    assert_eq!(run(&mut s, "nosuchthing", "PREV-HANDLER:nosuchthing"), Some(127));
    // A real command always wins over a local file of the same name.
    assert_eq!(run(&mut s, "echo REAL-ECHO", "REAL-ECHO"), Some(0));
    assert!(!screen_has(&s, "SHADOWED"));

    s.write(&[0x15]);
    drop(s);
    let _ = std::fs::remove_dir_all(&tmp);
}
