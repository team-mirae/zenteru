//! `zenteru update [VERSION] [--force]`: install the latest release (or
//! `VERSION`) over this copy by running the same installer as
//!
//!   curl -fsSL https://raw.githubusercontent.com/team-mirae/zenteru/main/install.sh | sh
//!
//! with `ZENTERU_APP_DIR` set to wherever this zenteru.app lives. Everything
//! that can go wrong before the installer starts (no curl, no network, a
//! proxy in the way, GitHub errors, a folder that can't be written) is
//! reported in plain words; the installer reports its own steps.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "team-mirae/zenteru";
const INSTALLER: &str = "https://raw.githubusercontent.com/team-mirae/zenteru/main/install.sh";

pub fn run(args: &[String]) -> i32 {
    let mut force = false;
    let mut wanted: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--force" | "-f" => force = true,
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  zenteru update            install the latest release over this copy\n  zenteru update VERSION    install that release, e.g. v0.1.28\n  zenteru update --force    reinstall even when already up to date\n\nZENTERU_APP_DIR and ZENTERU_BIN_DIR are passed on to the installer\n({INSTALLER})."
                );
                return 0;
            }
            v if !v.starts_with('-') && wanted.is_none() && parse_version(v).is_some() => {
                wanted = Some(format!("v{}", v.trim_start_matches('v')));
            }
            other => {
                eprintln!("zenteru update: unexpected argument {other:?}; see `zenteru update --help`");
                return 2;
            }
        }
    }
    match update(wanted, force) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("zenteru update: {e}");
            1
        }
    }
}

fn update(wanted: Option<String>, force: bool) -> Result<(), String> {
    let current = zenteru_text::brand::VERSION;
    let app_dir = app_dir();
    if let Some(dir) = &app_dir {
        check_writable(dir)?;
    }

    // Which release? This is also the first thing to touch the network.
    let asked = wanted.is_some();
    let target = match wanted {
        Some(v) => {
            let url = format!("https://github.com/{REPO}/releases/tag/{v}");
            curl(&["--head", "-o", "/dev/null", &url], "looking up the release").map_err(|e| match e.http {
                Some(404) => format!("there is no release called {v}. The releases are listed at https://github.com/{REPO}/releases"),
                _ => e.message,
            })?;
            v
        }
        None => {
            let latest = format!("https://github.com/{REPO}/releases/latest");
            let out = curl(&["-L", "-o", "/dev/null", "-w", "%{url_effective}", &latest], "looking up the latest release").map_err(|e| e.message)?;
            tag_from_url(out.trim()).ok_or_else(|| format!("couldn't find a published release at {latest} (it led to {:?})", out.trim()))?
        }
    };
    if parse_version(&target) == parse_version(current) && !force {
        println!("zenteru: already up to date ({current}). `zenteru update --force` reinstalls it.");
        return Ok(());
    }
    if !asked && !force && parse_version(&target) < parse_version(current) {
        println!(
            "zenteru: this copy ({current}) is newer than the latest release ({}). `zenteru update {target}` goes back to that one.",
            target.trim_start_matches('v')
        );
        return Ok(());
    }
    println!("zenteru: updating {current} → {}", target.trim_start_matches('v'));

    let script = curl(&["-L", INSTALLER], "downloading the installer").map_err(|e| e.message)?;
    if !looks_like_installer(&script) {
        return Err(format!(
            "what came back from {INSTALLER} isn't the installer. A sign-in page (hotel or café Wi-Fi) or a proxy may have replaced it; open that address in a browser to check."
        ));
    }

    let mut sh = Command::new("/bin/sh");
    sh.arg("-s").env("ZENTERU_VERSION", &target).stdin(Stdio::piped());
    if std::env::var_os("ZENTERU_APP_DIR").is_none() {
        if let Some(dir) = &app_dir {
            sh.env("ZENTERU_APP_DIR", dir);
        }
    }
    let mut child = sh.spawn().map_err(|e| format!("couldn't start /bin/sh to run the installer: {e}"))?;
    // The installer reads nothing from stdin itself, so a failed write only
    // means it died early; its exit status says why.
    let _ = child.stdin.take().map(|mut i| i.write_all(script.as_bytes()));
    let status = child.wait().map_err(|e| format!("the installer couldn't be waited for: {e}"))?;
    if status.success() {
        return Ok(());
    }
    Err(match status.code() {
        Some(c) => format!("the installer stopped with exit status {c}; its own message is above. zenteru {current} is still installed."),
        None => format!("the installer was interrupted. zenteru {current} is still installed."),
    })
}

/// Where this copy lives (the folder holding zenteru.app), unless the user
/// chose one. None for a development build: the installer picks its default.
fn app_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ZENTERU_APP_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let bundle = crate::platform::app_bundle()?;
    (bundle.file_name()? == "zenteru.app").then(|| bundle.parent().map(Path::to_path_buf))?
}

/// The installer replaces zenteru.app inside `dir`: say so now, in words,
/// rather than after the download with a bare `mv: Permission denied`.
fn check_writable(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(()); // the installer creates it
    }
    let probe = dir.join(format!(".zenteru-update-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "can't write to {} ({e}), the folder zenteru.app is installed in. Fix the folder's permissions, or install somewhere else with ZENTERU_APP_DIR=~/Applications zenteru update.",
            dir.display()
        )),
    }
}

struct CurlError {
    message: String,
    /// The HTTP status, when the server answered with an error.
    http: Option<u32>,
}

/// Run curl (https only, quiet, failing on HTTP errors) and return its
/// stdout. `doing` finishes the sentence "failed while …".
fn curl(args: &[&str], doing: &str) -> Result<String, CurlError> {
    let out = Command::new("curl")
        .args(["-fsS", "--proto", "=https", "--tlsv1.2", "--connect-timeout", "15", "--max-time", "120"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| CurlError {
            message: if e.kind() == std::io::ErrorKind::NotFound {
                "curl was not found on your PATH. macOS ships it as /usr/bin/curl: check that /usr/bin is on PATH (echo $PATH), or install curl.".to_string()
            } else {
                format!("couldn't run curl: {e}")
            },
            http: None,
        })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let http = http_status(&stderr);
    Err(CurlError { message: curl_failure(out.status.code(), http, stderr.trim(), doing), http })
}

/// The status in curl's "The requested URL returned error: 404".
fn http_status(stderr: &str) -> Option<u32> {
    let rest = stderr.split("returned error: ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// A curl failure in plain words: what happened, and what to do about it.
fn curl_failure(code: Option<i32>, http: Option<u32>, stderr: &str, doing: &str) -> String {
    // The HTTP status first: curl's exit status for one varies (22, 56).
    let why = match (http, code) {
        (Some(403 | 429), _) => "GitHub refused the request (rate limit or blocked address). Wait a few minutes and try again.".to_string(),
        (Some(404), _) => "GitHub says that address doesn't exist.".to_string(),
        (Some(s), _) if s >= 500 => format!("GitHub is having trouble (HTTP {s}). Try again later; https://www.githubstatus.com shows its state."),
        (Some(s), _) => format!("GitHub answered with HTTP {s}."),
        (_, Some(5)) => "your proxy's name couldn't be resolved. Check the https_proxy / HTTPS_PROXY / ALL_PROXY variables.".to_string(),
        (_, Some(6)) => "github.com couldn't be found (DNS). You're probably offline: check Wi-Fi or the VPN and try again.".to_string(),
        (_, Some(7)) => "the connection to GitHub was refused or blocked. Check your network, firewall or proxy.".to_string(),
        (_, Some(28)) => "GitHub didn't answer in time. The connection is very slow or blocked; try again in a moment.".to_string(),
        (_, Some(35 | 51 | 58 | 59 | 60 | 77 | 83 | 90 | 91)) => {
            "a secure (TLS) connection to GitHub couldn't be set up. Something between you and GitHub is intercepting HTTPS (a corporate proxy, a sign-in page on public Wi-Fi), or this Mac's clock is wrong.".to_string()
        }
        (_, Some(18 | 52 | 55 | 56 | 92)) => "the connection dropped part-way. Try again.".to_string(),
        (_, Some(23)) => "curl couldn't write what it downloaded (is the disk full?).".to_string(),
        (_, Some(c)) => format!("curl stopped with exit status {c}."),
        (_, None) => "curl was interrupted.".to_string(),
    };
    let detail = if stderr.is_empty() { String::new() } else { format!("\n  {stderr}") };
    format!("failed while {doing}: {why}{detail}")
}

/// `…/releases/tag/v0.1.29` → `v0.1.29`.
fn tag_from_url(url: &str) -> Option<String> {
    let tag = url.split_once("/releases/tag/")?.1.split(['?', '#', '/']).next()?;
    parse_version(tag).map(|_| tag.to_string())
}

/// `v0.1.29` or `0.1.29` → (0, 1, 29).
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.strip_prefix('v').unwrap_or(v).split('.').map(|p| p.parse::<u32>().ok());
    match (it.next()??, it.next()??, it.next()??, it.next()) {
        (a, b, c, None) => Some((a, b, c)),
        _ => None,
    }
}

/// A shell script that is zenteru's installer, not a proxy's HTML page.
fn looks_like_installer(script: &str) -> bool {
    script.starts_with("#!/bin/sh") && script.contains("zenteru installer") && script.contains(REPO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.29"), Some((0, 1, 29)));
        assert_eq!(parse_version("0.1.29"), Some((0, 1, 29)));
        assert!(parse_version("v0.1.10") > parse_version("v0.1.9"));
        for bad in ["", "v", "latest", "0.1", "0.1.2.3", "v0.1.x", "--force"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn tag_of_the_latest_release() {
        assert_eq!(tag_from_url("https://github.com/team-mirae/zenteru/releases/tag/v0.1.29").as_deref(), Some("v0.1.29"));
        assert_eq!(tag_from_url("https://github.com/team-mirae/zenteru/releases/tag/v0.1.29?x=1").as_deref(), Some("v0.1.29"));
        // No release published: GitHub leads to the releases page instead.
        assert_eq!(tag_from_url("https://github.com/team-mirae/zenteru/releases"), None);
        assert_eq!(tag_from_url("https://example.com/login?next=/releases/tag/oops"), None);
    }

    #[test]
    fn curl_failures_say_what_to_do() {
        let m = curl_failure(Some(6), None, "curl: (6) Could not resolve host: github.com", "looking up the latest release");
        assert!(m.starts_with("failed while looking up the latest release: "));
        assert!(m.contains("offline") && m.ends_with("curl: (6) Could not resolve host: github.com"));
        assert!(curl_failure(Some(60), None, "", "x").contains("intercepting HTTPS"));
        assert!(curl_failure(Some(28), None, "", "x").contains("didn't answer in time"));
        assert!(curl_failure(Some(7), None, "", "x").contains("refused or blocked"));
        let e = "curl: (22) The requested URL returned error: 429";
        assert_eq!(http_status(e), Some(429));
        assert!(curl_failure(Some(22), http_status(e), e, "x").contains("rate limit"));
        assert!(curl_failure(Some(22), Some(503), "", "x").contains("githubstatus"));
        // Newer curls report an HTTP error as 56, not 22.
        let e = "curl: (56) The requested URL returned error: 404";
        assert!(curl_failure(Some(56), http_status(e), e, "x").contains("doesn't exist"));
        assert_eq!(http_status("curl: (6) Could not resolve host"), None);
        // No stderr: no dangling detail line.
        assert!(!curl_failure(Some(99), None, "", "x").contains('\n'));
    }

    #[test]
    fn the_installer_is_recognised() {
        let real = include_str!("../../../install.sh");
        assert!(looks_like_installer(real));
        assert!(!looks_like_installer("<html><body>Sign in to Wi-Fi</body></html>"));
        assert!(!looks_like_installer(""));
    }

    #[test]
    fn unwritable_folder_is_explained() {
        let dir = std::env::temp_dir().join(format!("zenteru-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(check_writable(&dir).is_ok());
        assert!(check_writable(&dir.join("not-there-yet")).is_ok());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let err = check_writable(&dir).unwrap_err();
        assert!(err.contains("can't write to") && err.contains("ZENTERU_APP_DIR"), "{err}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
