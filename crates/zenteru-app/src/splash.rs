//! The startup / `@info` splash: ゼ as half-block art in the zencode
//! gradient (30° clockwise from rightwards, like the logo), with zenteru,
//! its version and a few facts beside it — fastfetch style.

use std::ffi::c_void;

use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};
use objc2_core_text::{CTFont, CTLine};
use zenteru_text::palette::{self as pal, Rgb};
use zenteru_text::text::str_width;

/// Art size in cells. Half blocks give two pixels per cell vertically, and
/// a cell is about twice as tall as wide, so pixels come out square.
const ART_COLS: usize = 22;
const ART_ROWS: usize = 11;

/// ゼ rasterized at `w` × `h` pixels: coverage 0..=255, row-major, top first.
fn glyph_mask(w: usize, h: usize) -> Vec<u8> {
    let mut data = vec![0u8; w * h];
    let space = CGColorSpace::new_device_gray().expect("gray colour space");
    let ctx: Option<CFRetained<CGContext>> =
        unsafe { CGBitmapContextCreate(data.as_mut_ptr() as *mut c_void, w, h, 8, w, Some(&space), CGImageAlphaInfo::None.0) };
    let Some(ctx) = ctx else { return data };
    CGContext::set_gray_fill_color(Some(&ctx), 1.0, 1.0);
    let size = h as f64 * 0.98;
    let font = unsafe { CTFont::with_name(&CFString::from_str("HiraginoSans-W8"), size, std::ptr::null()) };
    let s = CFString::from_str("ゼ");
    let keys: [&CFString; 2] = unsafe { [objc2_core_text::kCTFontAttributeName, objc2_core_text::kCTForegroundColorFromContextAttributeName] };
    let yes = objc2_core_foundation::CFBoolean::new(true);
    let vals: [&objc2_core_foundation::CFType; 2] = [font.as_ref(), yes.as_ref()];
    let attrs = objc2_core_foundation::CFDictionary::<CFString, objc2_core_foundation::CFType>::from_slices(&keys, &vals);
    let Some(astr) = (unsafe { objc2_core_foundation::CFAttributedString::new(None, Some(&s), Some(attrs.as_opaque())) }) else { return data };
    let line = unsafe { CTLine::with_attributed_string(&astr) };
    let bounds = unsafe { line.image_bounds(Some(&ctx)) };
    // Centre the glyph's ink in the bitmap.
    let x = (w as f64 - bounds.size.width) / 2.0 - bounds.origin.x;
    let y = (h as f64 - bounds.size.height) / 2.0 - bounds.origin.y;
    CGContext::set_text_position(Some(&ctx), x, y);
    unsafe { line.draw(&ctx) };
    data
}

/// Gradient colour at pixel (x, y): along 30° clockwise from rightwards.
fn grad_at(x: f32, y: f32, w: f32, h: f32) -> Rgb {
    let (dx, dy) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
    let proj = |px: f32, py: f32| px * dx + py * dy;
    let (lo, hi) = (proj(0.0, 0.0), proj(w, h));
    pal::gradient((proj(x, y) - lo) / (hi - lo))
}

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

/// The art as `ART_ROWS` lines of ANSI text, each `ART_COLS` cells wide.
pub fn art() -> Vec<String> {
    let (w, h) = (ART_COLS, ART_ROWS * 2);
    let m = glyph_mask(w, h);
    let on = |x: usize, y: usize| m[y * w + x] >= 110;
    (0..ART_ROWS)
        .map(|r| {
            let mut s = String::new();
            for c in 0..ART_COLS {
                let (top, bottom) = (on(c, 2 * r), on(c, 2 * r + 1));
                let ct = grad_at(c as f32, (2 * r) as f32, w as f32, h as f32);
                let cb = grad_at(c as f32, (2 * r + 1) as f32, w as f32, h as f32);
                match (top, bottom) {
                    (true, true) => s.push_str(&format!("{}{}▀\x1b[49m", fg(ct), bg(cb))),
                    (true, false) => s.push_str(&format!("{}▀", fg(ct))),
                    (false, true) => s.push_str(&format!("{}▄", fg(cb))),
                    (false, false) => s.push(' '),
                }
            }
            s.push_str("\x1b[0m");
            s
        })
        .collect()
}

pub struct Info {
    pub version: String,
    pub shell: Option<String>,
    pub font: Option<String>,
    pub plugins: Vec<String>,
}

fn cmd(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The full splash as terminal output (CRLF line ends, colours reset).
pub fn render(info: &Info) -> String {
    let art = art();
    let label = |k: &str| format!("{}{k:<8}\x1b[0m", fg(pal::GRADIENT_FROM.lerp(pal::GRADIENT_TO, 0.5)));
    let muted = fg(pal::LABEL);
    let mut right: Vec<String> = Vec::new();
    // zenteru, in the gradient, letter by letter.
    let name = "zenteru";
    let mut title = String::from("\x1b[1m");
    for (i, ch) in name.chars().enumerate() {
        title.push_str(&fg(pal::gradient(i as f32 / (name.len() - 1) as f32)));
        title.push(ch);
    }
    title.push_str("\x1b[0m");
    right.push(title);
    right.push(format!("{muted}version {}\x1b[0m", info.version));
    right.push(format!("{muted}젠테루 · ゼンテール\x1b[0m"));
    right.push(format!("{}{}\x1b[0m", fg(pal::MUTED), "─".repeat(24)));
    let os = cmd("/usr/bin/sw_vers", &["-productVersion"]).map(|v| format!("macOS {v} ({})", std::env::consts::ARCH));
    if let Some(os) = os {
        right.push(format!("{}{os}", label("OS")));
    }
    if let Some(host) = cmd("/bin/hostname", &["-s"]) {
        right.push(format!("{}{host}", label("Host")));
    }
    if let Some(sh) = &info.shell {
        right.push(format!("{}{sh}", label("Shell")));
    }
    if let Some(font) = &info.font {
        right.push(format!("{}{font}", label("Font")));
    }
    if !info.plugins.is_empty() {
        right.push(format!("{}{}", label("Plugins"), info.plugins.join(", ")));
    }
    // Palette strip.
    let mut strip = String::new();
    for i in 0..8u8 {
        strip.push_str(&format!("{}   ", bg(pal::ANSI[i as usize])));
    }
    strip.push_str("\x1b[0m");
    right.push(String::new());
    right.push(strip);
    let mut strip2 = String::new();
    for i in 8..16u8 {
        strip2.push_str(&format!("{}   ", bg(pal::ANSI[i as usize])));
    }
    strip2.push_str("\x1b[0m");
    right.push(strip2);

    let rows = art.len().max(right.len());
    let pad_top = (rows - right.len()) / 2;
    let mut out = String::from("\r\n");
    for i in 0..rows {
        let a = art.get(i).cloned().unwrap_or_else(|| " ".repeat(ART_COLS));
        let r = i.checked_sub(pad_top).and_then(|j| right.get(j)).cloned().unwrap_or_default();
        out.push_str("  ");
        out.push_str(&a);
        out.push_str("   ");
        out.push_str(&r);
        out.push_str("\x1b[0m\r\n");
    }
    out.push_str("\r\n");
    let _ = str_width;
    out
}

/// Short facts for the CLI (`zenteru info`) where the app isn't running.
pub fn cli_info() -> Info {
    Info {
        version: zenteru_text::brand::VERSION.to_string(),
        shell: std::env::var("SHELL").ok().map(|s| s.rsplit('/').next().unwrap_or("").to_string()),
        font: None,
        plugins: Vec::new(),
    }
}
