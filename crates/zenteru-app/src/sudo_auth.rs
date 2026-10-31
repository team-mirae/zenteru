//! `zenteru sudo-auth`: let sudo accept Touch ID and/or a YubiKey.
//!
//! macOS reads `/etc/pam.d/sudo_local` (it survives OS updates) from
//! `/etc/pam.d/sudo`. We add
//!
//!   auth  sufficient  pam_tid.so                          (Touch ID)
//!   auth  sufficient  /opt/homebrew/lib/pam/pam_u2f.so cue  (YubiKey, FIDO2/U2F)
//!
//! Both are `sufficient`, so the password still works as a fallback.
//! Nothing is changed without showing the file and asking first; the write
//! goes through `sudo tee`, and the previous file is backed up.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const SUDO_LOCAL: &str = "/etc/pam.d/sudo_local";
const U2F_PATHS: &[&str] = &["/opt/homebrew/lib/pam/pam_u2f.so", "/usr/local/lib/pam/pam_u2f.so"];

struct Opts {
    touch_id: bool,
    yubikey: bool,
    dry_run: bool,
    yes: bool,
}

fn ask(q: &str) -> bool {
    print!("{q} [y/N] ");
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().lock().read_line(&mut s);
    matches!(s.trim(), "y" | "Y" | "yes")
}

fn u2f_module() -> Option<&'static str> {
    U2F_PATHS.iter().copied().find(|p| Path::new(p).exists())
}

fn u2f_keys_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::env::var("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| Path::new(&home).join(".config"));
    base.join("Yubico/u2f_keys")
}

/// The new file content: existing non-auth lines kept, our lines first.
pub fn plan(existing: &str, touch_id: bool, u2f: Option<&str>) -> String {
    let mut out = String::from("# sudo_local: local config file which survives system update and is included for sudo\n");
    out.push_str("# Managed in part by `zenteru sudo-auth`.\n");
    if touch_id {
        out.push_str("auth       sufficient     pam_tid.so\n");
    }
    if let Some(m) = u2f {
        out.push_str(&format!("auth       sufficient     {m} cue\n"));
    }
    for l in existing.lines() {
        let t = l.trim();
        if t.starts_with('#') || t.is_empty() || t.contains("pam_tid.so") || t.contains("pam_u2f.so") {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

pub fn run(args: &[String]) -> i32 {
    let mut o = Opts { touch_id: true, yubikey: false, dry_run: false, yes: false };
    for a in args {
        match a.as_str() {
            "--yubikey" => o.yubikey = true,
            "--yubikey-only" => {
                o.yubikey = true;
                o.touch_id = false
            }
            "--no-touchid" => o.touch_id = false,
            "--dry-run" => o.dry_run = true,
            "--yes" | "-y" => o.yes = true,
            "-h" | "--help" => {
                println!(
                    "zenteru sudo-auth [--yubikey] [--yubikey-only] [--no-touchid] [--dry-run] [--yes]\n\n\
                     Lets sudo accept Touch ID (pam_tid) and/or a YubiKey (pam_u2f) via {SUDO_LOCAL}.\n\
                     The password keeps working as a fallback. Nothing is written without confirmation."
                );
                return 0;
            }
            other => {
                eprintln!("unknown option {other}; see --help");
                return 2;
            }
        }
    }
    let existing = std::fs::read_to_string(SUDO_LOCAL).unwrap_or_default();
    println!("zenteru sudo-auth — Touch ID / YubiKey for sudo\n");
    println!("  Touch ID (pam_tid):  {}", if existing.contains("pam_tid.so") { "already on" } else { "off" });
    let u2f = u2f_module();
    println!("  YubiKey (pam_u2f):   {}", match (existing.contains("pam_u2f.so"), u2f) {
        (true, _) => "already on".to_string(),
        (false, Some(p)) => format!("available ({p})"),
        (false, None) => "module not installed (brew install pam-u2f)".to_string(),
    });

    let mut u2f_line = None;
    if o.yubikey {
        match u2f {
            Some(m) => {
                u2f_line = Some(m);
                let keys = u2f_keys_path();
                if !keys.exists() {
                    println!("\nYour YubiKey isn't registered yet ({}).", keys.display());
                    let has_cfg = Command::new("/bin/sh").args(["-c", "command -v pamu2fcfg"]).stdout(Stdio::null()).status().is_ok_and(|s| s.success());
                    if has_cfg && (o.yes || ask("Register it now with pamu2fcfg (touch the key when it blinks)?")) && !o.dry_run {
                        let _ = std::fs::create_dir_all(keys.parent().unwrap());
                        match Command::new("pamu2fcfg").output() {
                            Ok(out) if out.status.success() => {
                                let _ = std::fs::write(&keys, &out.stdout);
                                println!("Registered → {}", keys.display());
                            }
                            _ => {
                                eprintln!("pamu2fcfg failed; register later with: pamu2fcfg > {}", keys.display());
                                eprintln!("Not enabling pam_u2f without a registered key (it would just be skipped).");
                                u2f_line = None;
                            }
                        }
                    } else {
                        println!("Register later with:  mkdir -p {} && pamu2fcfg > {}", keys.parent().unwrap().display(), keys.display());
                    }
                }
            }
            None => {
                println!("\nTo use a YubiKey, first install the PAM module:\n  brew install pam-u2f\n  mkdir -p ~/.config/Yubico && pamu2fcfg > ~/.config/Yubico/u2f_keys\nthen run `zenteru sudo-auth --yubikey` again.");
            }
        }
    }
    if !o.touch_id && u2f_line.is_none() {
        println!("\nNothing to enable.");
        return 0;
    }
    let new = plan(&existing, o.touch_id, u2f_line);
    if new.trim() == existing.trim() {
        println!("\n{SUDO_LOCAL} is already set up.");
        return 0;
    }
    println!("\nProposed {SUDO_LOCAL}:\n");
    for l in new.lines() {
        println!("    {l}");
    }
    if o.dry_run {
        println!("\n(dry run — nothing written)");
        return 0;
    }
    if !o.yes && !ask("\nWrite it? sudo will ask for your password once (zenteru shows it as a password field)") {
        println!("Cancelled.");
        return 1;
    }
    if !existing.is_empty() {
        let _ = Command::new("sudo").args(["/bin/cp", SUDO_LOCAL, &format!("{SUDO_LOCAL}.zenteru-backup")]).status();
    }
    let mut child = match Command::new("sudo").args(["/usr/bin/tee", SUDO_LOCAL]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not run sudo: {e}");
            return 1;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(new.as_bytes());
    }
    match child.wait() {
        Ok(s) if s.success() => {
            println!("Done. Try `sudo -k; sudo true` — Touch ID / your YubiKey should be offered.");
            0
        }
        _ => {
            eprintln!("Writing {SUDO_LOCAL} failed; nothing changed.");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_keeps_other_lines_and_dedups() {
        let old = "# sudo_local\nauth sufficient pam_tid.so\nauth optional pam_reattach.so\n";
        let p = plan(old, true, Some("/opt/homebrew/lib/pam/pam_u2f.so"));
        assert_eq!(p.matches("pam_tid.so").count(), 1);
        assert!(p.contains("pam_u2f.so cue"));
        assert!(p.contains("pam_reattach.so"));
        let tid = p.find("pam_tid").unwrap();
        assert!(tid < p.find("pam_reattach").unwrap());
    }
}
