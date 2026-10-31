//! macOS glue that isn't windowing: pasteboard, the current keyboard input
//! source, notifications, resource paths and Python discovery.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;

use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF};
use objc2_core_foundation::CFString;
use objc2_foundation::{NSArray, NSString, NSURL};

use zenteru_term::base64;
use zenteru_term::json::Json;

pub fn pasteboard() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}

pub fn clipboard_text() -> Option<String> {
    let pb = pasteboard();
    unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string())
}

pub fn set_clipboard_text(s: &str) {
    let pb = pasteboard();
    pb.clearContents();
    unsafe { pb.setString_forType(&NSString::from_str(s), NSPasteboardTypeString) };
}

/// Files and image data on a pasteboard, as drop-handler items
/// (`{kind:"file", path}` / `{kind:"data", uti, name, base64}`).
pub fn pasteboard_items(pb: &NSPasteboard) -> Vec<Json> {
    let mut items = Vec::new();
    if let Some(list) = pb.pasteboardItems() {
        for item in list.iter() {
            if let Some(url) = unsafe { item.stringForType(NSPasteboardTypeFileURL) } {
                if let Some(u) = NSURL::URLWithString(&url) {
                    if let Some(p) = u.path() {
                        items.push(Json::obj().with("kind", "file").with("path", p.to_string()));
                        continue;
                    }
                }
            }
            for (ty, uti, ext) in [
                (unsafe { NSPasteboardTypePNG }, "public.png", "png"),
                (unsafe { NSPasteboardTypeTIFF }, "public.tiff", "tiff"),
            ] {
                if let Some(data) = item.dataForType(ty) {
                    let bytes = data.to_vec();
                    items.push(
                        Json::obj()
                            .with("kind", "data")
                            .with("uti", uti)
                            .with("name", format!("clipboard.{ext}"))
                            .with("base64", base64::encode(&bytes)),
                    );
                    break;
                }
            }
        }
    }
    items
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(src: *const c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyInputSourceID: *const c_void;
}

unsafe extern "C" {
    fn CFRelease(p: *const c_void);
}

/// e.g. `com.apple.keylayout.ABC`, `me.sjun.inputmethod.JISKorean.2Set`.
pub fn input_source_id() -> String {
    unsafe {
        let src = TISCopyCurrentKeyboardInputSource();
        if src.is_null() {
            return String::new();
        }
        let v = TISGetInputSourceProperty(src, kTISPropertyInputSourceID);
        let s = if v.is_null() { String::new() } else { (*(v as *const CFString)).to_string() };
        CFRelease(src);
        s
    }
}

pub fn notify(title: &str, body: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(if title.is_empty() { "zenteru" } else { title }));
    let _ = Command::new("/usr/bin/osascript").arg("-e").arg(script).spawn();
}

/// Where `plugins/`, `python/` and `shell/` live: the app bundle's
/// Resources, or the source tree when run with `cargo run` (VS Code,
/// RustRover, …). `ZENTERU_RESOURCES` overrides.
pub fn resources_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ZENTERU_RESOURCES") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(res) = exe.parent().and_then(Path::parent).map(|c| c.join("Resources")) {
            if res.join("plugins").is_dir() {
                return res;
            }
        }
    }
    // crates/zenteru-app → workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap_or_else(|_| PathBuf::from("."))
}

/// Per-user runtime dir: `$TMPDIR/zenteru-$UID` (0700), holding the API
/// socket and cookie.
pub fn runtime_dir() -> PathBuf {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    std::env::temp_dir().join(format!("zenteru-{}", unsafe { getuid() }))
}

pub fn support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/zenteru")
}

pub fn log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Logs/zenteru")
}

/// A Python 3.11+ for plugins. GUI apps get a minimal PATH, so look in the
/// usual places before trusting `python3` on PATH.
pub fn find_python() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("ZENTERU_PYTHON") {
        cands.push(p.into());
    }
    for p in ["/opt/homebrew/bin/python3", "/usr/local/bin/python3"] {
        cands.push(p.into());
    }
    if let Ok(path) = std::env::var("PATH") {
        cands.extend(path.split(':').map(|d| Path::new(d).join("python3")));
    }
    cands.push("/usr/bin/python3".into());
    cands.into_iter().find(|p| {
        p.exists()
            && Command::new(p)
                .args(["-c", "import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

pub fn ns_array_strings(a: &NSArray<NSString>) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}
