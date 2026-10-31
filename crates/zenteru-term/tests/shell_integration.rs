//! End-to-end: a real zsh with zenteru's shell integration.

use std::path::PathBuf;
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

#[test]
fn zsh_marks_and_setenvvar() {
    if !std::path::Path::new("/bin/zsh").exists() {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../shell").canonicalize().unwrap();
    let tmp = std::env::temp_dir().join(format!("zt-si-{}", std::process::id()));
    let zdot = tmp.join("zdot");
    std::fs::create_dir_all(&zdot).unwrap();
    std::fs::write(zdot.join(".zshrc"), "PS1='%# '\n").unwrap();
    unsafe {
        std::env::set_var("SHELL", "/bin/zsh");
        std::env::set_var("ZDOTDIR", &zdot);
    }
    let mut s = Session::spawn(SessionConfig {
        id: "s1".into(),
        cols: 80,
        rows: 24,
        cell_px: (8, 16),
        cwd: Some(&tmp),
        command: None,
        shell_integration: Some(&root),
        dir: tmp.join("session"),
        api_socket: None,
        extra_env: vec![],
    })
    .unwrap();
    let editing = |s: &Session| s.term.lock().unwrap().prompt.phase == PromptPhase::Editing;
    wait(&s, "first prompt", editing);
    assert!(s.term.lock().unwrap().prompt.cwd.as_deref().is_some_and(|c| c.contains("zt-si-")));

    s.env_set("ZT_SECRET", "s3cr3t value").unwrap();
    s.write(b"echo \"[${ZT_SECRET-unset}]\"\r");
    wait(&s, "user command output", |s| screen_has(s, "[s3cr3t value]"));
    wait(&s, "prompt after command", |s| {
        let t = s.term.lock().unwrap();
        t.prompt.phase == PromptPhase::Editing && t.prompt.exit_status == Some(0)
    });
    assert_eq!(s.term.lock().unwrap().prompt.command.as_deref(), Some("echo \"[${ZT_SECRET-unset}]\""));

    // Sent by a plugin: the variable is not applied.
    s.write_as_plugin(b"echo \"<${ZT_SECRET-unset}>\"\r");
    wait(&s, "plugin command output", |s| screen_has(s, "<unset>"));
    wait(&s, "prompt", editing);

    // Not left in the shell's own environment between commands: a
    // command that inspects the parent shell's env via ps would be overkill;
    // check via a plugin-sent command (which skips the hook).
    s.write_as_plugin(b"typeset -p ZT_SECRET 2>/dev/null || echo NOT-IN-SHELL\r");
    wait(&s, "not in shell env", |s| screen_has(s, "NOT-IN-SHELL"));

    // Edit line tracking from the OSC 133;B mark.
    wait(&s, "prompt", editing);
    s.write("echo 한국어를".as_bytes());
    wait(&s, "typed", |s| s.term.lock().unwrap().edit_line().0 == "echo 한국어를");
    assert_eq!(s.term.lock().unwrap().edit_line().1, 9);
    s.write(&[0x15]);
    drop(s);
    let _ = std::fs::remove_dir_all(&tmp);
}
