//! `@music` and friends: focus whatever is playing, play / pause, mute,
//! volume.
//!
//! * "What is playing" comes from CoreAudio's process objects (public since
//!   macOS 14.2): every process currently sending audio to an output device.
//!   Each is walked up to the app that owns it (Chrome's audio helper →
//!   Google Chrome), music players rank above everything else (so a game's
//!   sound doesn't win), and browsers resolve to a running web-app window
//!   (e.g. the YouTube Music PWA) or a music tab.
//! * Play / pause go through MediaRemote (the system Now Playing controls).
//! * Volume and mute use the system output volume (AppleScript).

use std::ffi::{c_char, c_int, c_void};
use std::process::Command;

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};

// ---- CoreAudio ----------------------------------------------------------

#[repr(C)]
struct PropAddr {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(obj: u32, addr: *const PropAddr, qsize: u32, qdata: *const c_void, size: *mut u32) -> i32;
    fn AudioObjectGetPropertyData(obj: u32, addr: *const PropAddr, qsize: u32, qdata: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
}

const SYSTEM_OBJECT: u32 = 1;
const PROCESS_OBJECT_LIST: u32 = 0x7072_7323; // 'prs#'
const PROCESS_PID: u32 = 0x7070_6964; // 'ppid'
const PROCESS_IS_RUNNING_OUTPUT: u32 = 0x7069_726F; // 'piro'
const SCOPE_GLOBAL: u32 = 0x676C_6F62; // 'glob'

fn addr(selector: u32) -> PropAddr {
    PropAddr { selector, scope: SCOPE_GLOBAL, element: 0 }
}

/// PIDs of processes currently sending audio to an output device.
pub fn playing_pids() -> Vec<i32> {
    unsafe {
        let a = addr(PROCESS_OBJECT_LIST);
        let mut size = 0u32;
        if AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &a, 0, std::ptr::null(), &mut size) != 0 || size == 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / 4];
        if AudioObjectGetPropertyData(SYSTEM_OBJECT, &a, 0, std::ptr::null(), &mut size, ids.as_mut_ptr().cast()) != 0 {
            return Vec::new();
        }
        ids.into_iter()
            .filter_map(|id| {
                let (mut running, mut pid) = (0u32, 0i32);
                let mut s = 4u32;
                AudioObjectGetPropertyData(id, &addr(PROCESS_IS_RUNNING_OUTPUT), 0, std::ptr::null(), &mut s, (&mut running as *mut u32).cast());
                s = 4;
                AudioObjectGetPropertyData(id, &addr(PROCESS_PID), 0, std::ptr::null(), &mut s, (&mut pid as *mut i32).cast());
                (running != 0 && pid > 0).then_some(pid)
            })
            .collect()
    }
}

// ---- process → app ----------------------------------------------------

unsafe extern "C" {
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buf: *mut c_void, size: c_int) -> c_int;
}

fn parent_pid(pid: i32) -> Option<i32> {
    // struct proc_bsdinfo: pbi_ppid is the fifth u32.
    let mut buf = [0u8; 136];
    let n = unsafe { proc_pidinfo(pid, 3 /* PROC_PIDTBSDINFO */, 0, buf.as_mut_ptr().cast(), buf.len() as c_int) };
    (n as usize >= 20).then(|| i32::from_ne_bytes(buf[16..20].try_into().unwrap())).filter(|&p| p > 1)
}

fn app_for_pid(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
}

/// The regular (Dock) app that owns `pid`: itself, or the nearest ancestor.
fn owning_app(pid: i32) -> Option<Retained<NSRunningApplication>> {
    let mut p = pid;
    for _ in 0..8 {
        if let Some(app) = app_for_pid(p) {
            let id = app.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
            if app.activationPolicy() == objc2_app_kit::NSApplicationActivationPolicy::Regular && !id.contains(".helper") {
                return Some(app);
            }
        }
        p = parent_pid(p)?;
    }
    None
}

fn bundle_id(app: &NSRunningApplication) -> String {
    app.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default()
}

fn app_name(app: &NSRunningApplication) -> String {
    app.localizedName().map(|s| s.to_string()).unwrap_or_else(|| bundle_id(app))
}

const BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "company.thebrowser.Browser",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "com.apple.Safari",
    "org.mozilla.firefox",
];

const MUSIC_APPS: &[&str] = &[
    "com.apple.Music",
    "com.spotify.client",
    "com.apple.podcasts",
    "com.tidal.desktop",
    "com.deezer.deezer-desktop",
    "com.amazon.music",
    "com.vox.vox",
    "org.videolan.vlc",
    "com.colliderli.iina",
];

const MUSIC_WORDS: &[&str] = &["music", "spotify", "soundcloud", "tidal", "deezer", "radio", "podcast", "bandcamp", "pandora", "youtube"];

const MUSIC_URLS: &[&str] = &[
    "music.youtube.com",
    "open.spotify.com",
    "soundcloud.com",
    "music.apple.com",
    "tidal.com",
    "deezer.com",
    "bandcamp.com",
    "pandora.com",
    "youtube.com/watch",
];

/// Lower is better: music apps, then browsers / web apps, then the rest.
fn rank(app: &NSRunningApplication) -> u8 {
    let id = bundle_id(app);
    let name = app_name(app).to_lowercase();
    if MUSIC_APPS.contains(&id.as_str()) || MUSIC_WORDS.iter().any(|w| name.contains(w)) {
        0
    } else if BROWSERS.contains(&id.as_str()) || id.contains(".app.") {
        1
    } else {
        2
    }
}

/// Apps currently playing audio, best candidate first (zenteru excluded).
pub fn playing_apps() -> Vec<Retained<NSRunningApplication>> {
    let me = std::process::id() as i32;
    let mut apps: Vec<Retained<NSRunningApplication>> = Vec::new();
    for pid in playing_pids() {
        if pid == me {
            continue;
        }
        if let Some(app) = owning_app(pid) {
            if app.processIdentifier() != me && !apps.iter().any(|a| a.processIdentifier() == app.processIdentifier()) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|a| rank(a));
    apps
}

fn activate(app: &NSRunningApplication) -> bool {
    #[allow(deprecated)]
    let opts = NSApplicationActivationOptions::ActivateAllWindows | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
    app.activateWithOptions(opts)
}

/// A running web app (PWA) of `browser` that looks like a music player.
fn music_web_app(browser: &str) -> Option<Retained<NSRunningApplication>> {
    let prefix = format!("{browser}.app.");
    let shims: Vec<Retained<NSRunningApplication>> = NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|a| bundle_id(a).starts_with(&prefix))
        .collect();
    shims
        .iter()
        .find(|a| MUSIC_WORDS.iter().any(|w| app_name(a).to_lowercase().contains(w)))
        .cloned()
        .or_else(|| (shims.len() == 1).then(|| shims[0].clone()))
}

/// Chromium-family browsers: bring the first tab on a music site to front
/// (AppleScript; macOS asks once for permission to control the browser).
fn focus_music_tab(browser: &str) -> bool {
    let cond = MUSIC_URLS.iter().map(|u| format!("(URL of t contains \"{u}\")")).collect::<Vec<_>>().join(" or ");
    let script = format!(
        "tell application id \"{browser}\"\n\
           repeat with w in windows\n\
             set i to 0\n\
             repeat with t in tabs of w\n\
               set i to i + 1\n\
               if {cond} then\n\
                 set active tab index of w to i\n\
                 set index of w to 1\n\
                 activate\n\
                 return \"ok\"\n\
               end if\n\
             end repeat\n\
           end repeat\n\
         end tell\n\
         return \"none\""
    );
    Command::new("/usr/bin/osascript").arg("-e").arg(script).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "ok")
}

/// `@music` / `@music focus`.
pub fn focus() -> Result<String, String> {
    let apps = playing_apps();
    let Some(app) = apps.first() else {
        return Err("nothing is playing audio".into());
    };
    let id = bundle_id(app);
    if BROWSERS.contains(&id.as_str()) {
        if let Some(web) = music_web_app(&id) {
            activate(&web);
            return Ok(format!("♪ {}", app_name(&web)));
        }
        if id != "com.apple.Safari" && id != "org.mozilla.firefox" && focus_music_tab(&id) {
            return Ok(format!("♪ {} (music tab)", app_name(app)));
        }
    }
    activate(app);
    Ok(format!("♪ {}", app_name(app)))
}

// ---- MediaRemote --------------------------------------------------------

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[derive(Clone, Copy)]
pub enum MediaCommand {
    Play = 0,
    Pause = 1,
    Toggle = 2,
    Next = 4,
    Previous = 5,
}

fn media(cmd: MediaCommand) -> bool {
    unsafe {
        let h = dlopen(c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote".as_ptr(), 1);
        if h.is_null() {
            return false;
        }
        let f = dlsym(h, c"MRMediaRemoteSendCommand".as_ptr());
        if f.is_null() {
            return false;
        }
        let send: extern "C" fn(u32, *const c_void) -> bool = std::mem::transmute(f);
        send(cmd as u32, std::ptr::null())
    }
}

// ---- volume -------------------------------------------------------------

fn osascript(s: &str) -> Option<String> {
    let o = Command::new("/usr/bin/osascript").arg("-e").arg(s).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn volume_now() -> Option<u32> {
    osascript("output volume of (get volume settings)")?.parse().ok()
}

fn set_volume(v: u32) -> Result<String, String> {
    let v = v.min(100);
    osascript(&format!("set volume output volume {v} without output muted")).ok_or("could not set the volume")?;
    Ok(format!("🔊 {v}%"))
}

/// up | down | min | max | 0–100 | 0.00–1.00 (a trailing `f` is fine).
pub fn parse_volume(arg: &str, now: u32) -> Result<u32, String> {
    let a = arg.trim().to_lowercase();
    let a = a.trim_end_matches('%');
    Ok(match a {
        "up" | "+" => (now + 10).min(100),
        "down" | "-" => now.saturating_sub(10),
        "min" | "mute" => 0,
        "max" => 100,
        s if s.contains('.') => {
            let f: f32 = s.trim_end_matches('f').parse().map_err(|_| format!("bad volume {arg:?}"))?;
            if !(0.0..=1.0).contains(&f) {
                return Err("a fractional volume is 0.00–1.00".into());
            }
            (f * 100.0).round() as u32
        }
        s => {
            let n: i64 = s.parse().map_err(|_| format!("volume: up, down, min, max, 0–100 or 0.00–1.00 (got {arg:?})"))?;
            if !(0..=100).contains(&n) {
                return Err("volume is 0–100".into());
            }
            n as u32
        }
    })
}

/// Run a music command line: `focus` (default), `play`, `pause`, `toggle`,
/// `mute`, `unmute`, `volume <arg>`, `next`, `prev`.
pub fn run(line: &str) -> Result<String, String> {
    let mut it = line.split_whitespace();
    let sub = it.next().unwrap_or("focus").to_lowercase();
    let rest: Vec<&str> = it.collect();
    match sub.as_str() {
        "focus" | "" => focus(),
        "play" => media(MediaCommand::Play).then(|| "▶ play".to_string()).ok_or_else(|| "play failed".into()),
        "pause" => media(MediaCommand::Pause).then(|| "⏸ pause".to_string()).ok_or_else(|| "pause failed".into()),
        "toggle" | "playpause" => media(MediaCommand::Toggle).then(|| "⏯".to_string()).ok_or_else(|| "toggle failed".into()),
        "next" | "skip" => media(MediaCommand::Next).then(|| "⏭ next".to_string()).ok_or_else(|| "next failed".into()),
        "prev" | "previous" | "back" => media(MediaCommand::Previous).then(|| "⏮ previous".to_string()).ok_or_else(|| "previous failed".into()),
        "mute" => osascript("set volume with output muted").map(|_| "🔇 muted".to_string()).ok_or_else(|| "could not mute".into()),
        "unmute" => osascript("set volume without output muted").map(|_| format!("🔊 {}%", volume_now().unwrap_or(0))).ok_or_else(|| "could not unmute".into()),
        "volume" | "vol" => {
            let now = volume_now().unwrap_or(50);
            match rest.first() {
                None => Ok(format!("🔊 {now}%")),
                Some(arg) => set_volume(parse_volume(arg, now)?),
            }
        }
        other => Err(format!("music: unknown subcommand {other:?} (focus, play, pause, mute, unmute, volume, next, prev)")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_volume;

    #[test]
    fn volume_forms() {
        assert_eq!(parse_volume("up", 50), Ok(60));
        assert_eq!(parse_volume("up", 95), Ok(100));
        assert_eq!(parse_volume("down", 5), Ok(0));
        assert_eq!(parse_volume("min", 50), Ok(0));
        assert_eq!(parse_volume("max", 5), Ok(100));
        assert_eq!(parse_volume("37", 0), Ok(37));
        assert_eq!(parse_volume("0.25", 0), Ok(25));
        assert_eq!(parse_volume("0.5f", 0), Ok(50));
        assert_eq!(parse_volume("1.00f", 0), Ok(100));
        assert!(parse_volume("101", 0).is_err());
        assert!(parse_volume("1.5", 0).is_err());
        assert!(parse_volume("loud", 0).is_err());
    }
}
