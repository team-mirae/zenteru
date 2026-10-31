//! The gutter's two other faces: zencode-gradient rain while a program
//! (Claude Code, vim, a build…) has the terminal, and a 24-hour clock with
//! the date at the bottom.

use std::ffi::{c_char, c_int, c_long};

use zenteru_text::palette::{self as pal, Rgb};

use crate::font::Style;
use crate::render::{Frame, Renderer, rgba};

#[repr(C)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
}

/// Local (date "yyyy-mm-dd", time "hh:mm", minute number for change detection).
pub fn local_clock() -> (String, String, i64) {
    unsafe {
        let now = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        localtime_r(&now, &mut tm);
        (
            format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
            format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
            now / 60,
        )
    }
}

/// System Settings → Accessibility → Display → Reduce motion.
pub fn reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Fall speed relative to the first version (which was too fast).
const RAIN_SPEED: f32 = 0.35;

/// Cheap deterministic hash → 0..1.
fn h(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Raindrops falling down the area (x, y, w, h) at time `t` (seconds).
/// Each drop is a streak fading in from the top, coloured by how far it has
/// fallen (pink at the top → violet at the bottom), with a brighter head.
#[allow(clippy::too_many_arguments)]
pub fn rain(f: &mut Frame, x: f32, y: f32, w: f32, hgt: f32, cell_h: f32, scale: f32, t: f32, still: bool) {
    if w <= 0.0 || hgt <= 0.0 {
        return;
    }
    if still {
        // Reduce motion: a quiet vertical gradient instead.
        f.vgradient_rect(x + w * 0.45, y, 2.0 * scale, hgt, 1.0 * scale, rgba(pal::GRADIENT_FROM, 0.45), rgba(pal::GRADIENT_TO, 0.45));
        return;
    }
    let lanes = ((w / (6.0 * scale)).floor() as u32).clamp(3, 12);
    let lane_w = w / lanes as f32;
    for lane in 0..lanes {
        for k in 0..2u32 {
            let id = lane * 7 + k * 131 + 17;
            let speed = (hgt * (0.28 + 0.55 * h(id)) + 40.0 * scale) * RAIN_SPEED; // px/s
            let len = cell_h * (1.2 + 3.5 * h(id + 3));
            let gap = hgt * (0.15 + 0.9 * h(id + 5));
            let period = (hgt + len + gap) / speed;
            let phase = h(id + 9) * period;
            let p = ((t + phase) % period) * speed - len;
            let dx = x + lane as f32 * lane_w + lane_w * (0.25 + 0.5 * h(id + 11));
            let top = p.max(0.0);
            let bottom = (p + len).min(hgt);
            if bottom <= top {
                continue;
            }
            let frac = ((p + len) / hgt).clamp(0.0, 1.0);
            let col = pal::gradient(frac);
            let alpha = 0.35 + 0.5 * h(id + 13);
            let thick = (1.5 + 1.5 * h(id + 15)) * scale;
            f.vgradient_rect(dx, y + top, thick, bottom - top, thick / 2.0, rgba(col, 0.0), rgba(col, alpha));
            if p + len <= hgt {
                // The drop's head: a little brighter, a little rounder.
                let head = col.lerp(Rgb(255, 255, 255), 0.35);
                let r = thick * 1.3;
                f.round_rect(dx + thick / 2.0 - r, y + p + len - r * 1.6, r * 2.0, r * 2.4, r, rgba(head, (alpha + 0.25).min(1.0)));
            }
        }
    }
}

/// The clock: date over a gradient hh:mm, centred in a `w`-wide strip whose
/// last row starts at `y_last`.
pub fn clock(r: &mut Renderer, f: &mut Frame, x: f32, y_last: f32, w: f32, cw: f32, ch: f32, scale: f64) {
    let (date, hm, _) = local_clock();
    let dx = x + ((w - 10.0 * cw) / 2.0).max(0.0);
    r.text(f, &date, dx, y_last - ch, rgba(pal::LABEL, 0.6), Style::Regular, 10, None, scale);
    let tx = x + ((w - 5.0 * cw) / 2.0).max(0.0);
    let g = crate::render::Grad { x0: tx, x1: tx + 5.0 * cw, to: rgba(pal::GRADIENT_TO, 1.0) };
    r.text(f, &hm, tx, y_last, rgba(pal::GRADIENT_FROM, 1.0), Style::Bold, 5, Some(g), scale);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RainStyle {
    /// Falling streaks (the default).
    Drops,
    /// A fixed matrix of blocks; only their colours change.
    Blocks,
    /// A plugin drives the block matrix (`gutter.set_frame`).
    Custom,
    Off,
}

impl RainStyle {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "drops" | "dots" | "dotted" | "default" => Self::Drops,
            "blocks" | "block" | "matrix" => Self::Blocks,
            "custom" | "plugin" => Self::Custom,
            "off" | "none" => Self::Off,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Drops => "drops",
            Self::Blocks => "blocks",
            Self::Custom => "custom",
            Self::Off => "off",
        }
    }
}

/// Geometry of the block matrix in an area: (columns, rows, block px, gap px).
pub fn matrix_dims(w: f32, hgt: f32, cell_w: f32, scale: f32) -> (u32, u32, f32, f32) {
    let b = (cell_w * 0.5).max(3.0 * scale).round();
    let gap = scale.max(1.0).round();
    let cols = ((w + gap) / (b + gap)).floor().max(0.0) as u32;
    let rows = ((hgt + gap) / (b + gap)).floor().max(0.0) as u32;
    (cols, rows, b, gap)
}

/// Draw a colour per block (row-major RGBA, `cols` × `rows`), left-aligned.
fn draw_matrix(f: &mut Frame, x: f32, y: f32, cols: u32, rows: u32, b: f32, gap: f32, color: impl Fn(u32, u32) -> Option<[f32; 4]>) {
    for r in 0..rows {
        for c in 0..cols {
            if let Some(px) = color(c, r) {
                if px[3] > 0.003 {
                    f.round_rect(x + c as f32 * (b + gap), y + r as f32 * (b + gap), b, b, b * 0.18, px);
                }
            }
        }
    }
}

/// Block rain: the matrix stays put (a faint grid), gradient trails run
/// down the columns by lighting blocks up.
#[allow(clippy::too_many_arguments)]
pub fn block_rain(f: &mut Frame, x: f32, y: f32, w: f32, hgt: f32, cell_w: f32, scale: f32, t: f32, still: bool) {
    let (cols, rows, b, gap) = matrix_dims(w, hgt, cell_w, scale);
    if cols == 0 || rows == 0 {
        return;
    }
    let step = b + gap;
    // Per column: two drops with their own speed (blocks/s), trail and phase.
    let heads: Vec<[(f32, f32); 2]> = (0..cols)
        .map(|c| {
            let mut d = [(0.0, 0.0); 2];
            for (k, slot) in d.iter_mut().enumerate() {
                let id = c * 13 + k as u32 * 977 + 5;
                let speed = (rows as f32 * (0.25 + 0.6 * h(id)) + 10.0) * RAIN_SPEED;
                let trail = 3.0 + 9.0 * h(id + 1);
                let gapb = rows as f32 * (0.2 + 1.2 * h(id + 2));
                let period = (rows as f32 + trail + gapb) / speed;
                let head = ((t + h(id + 3) * period) % period) * speed;
                *slot = if still { (-100.0, 1.0) } else { (head, trail) };
            }
            d
        })
        .collect();
    let _ = step;
    draw_matrix(f, x, y, cols, rows, b, gap, |c, r| {
        let base = pal::gradient(r as f32 / rows.max(1) as f32);
        let mut lit: f32 = 0.0;
        let mut is_head = false;
        for &(head, trail) in &heads[c as usize] {
            let d = head - r as f32;
            if (0.0..trail).contains(&d) {
                let v = 1.0 - d / trail;
                if v > lit {
                    lit = v;
                    is_head = d < 1.0;
                }
            }
        }
        let col = if is_head { base.lerp(Rgb(255, 255, 255), 0.35) } else { base };
        let a = if still { 0.12 + 0.18 * (r as f32 / rows.max(1) as f32) } else { 0.05 + 0.75 * lit };
        Some(rgba(col, a))
    });
}

/// A plugin-supplied frame (`fw` × `fh` RGBA, row-major), scaled to the matrix.
#[allow(clippy::too_many_arguments)]
pub fn custom_rain(f: &mut Frame, x: f32, y: f32, w: f32, hgt: f32, cell_w: f32, scale: f32, fw: u32, fh: u32, px: &[u8]) {
    let (cols, rows, b, gap) = matrix_dims(w, hgt, cell_w, scale);
    if cols == 0 || rows == 0 || fw == 0 || fh == 0 || px.len() < (fw * fh * 4) as usize {
        return;
    }
    draw_matrix(f, x, y, cols, rows, b, gap, |c, r| {
        let sx = (c * fw / cols).min(fw - 1);
        let sy = (r * fh / rows).min(fh - 1);
        let i = ((sy * fw + sx) * 4) as usize;
        let p = &px[i..i + 4];
        Some([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0])
    });
}
