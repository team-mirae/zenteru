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
    /// A contribution-graph style grid whose squares brighten and fade.
    Heatmap,
    /// A plugin drives the block matrix (`gutter.set_frame`).
    Custom,
    Off,
}

impl RainStyle {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "drops" | "dots" | "dotted" | "default" => Self::Drops,
            "blocks" | "block" | "matrix" => Self::Blocks,
            "heatmap" | "heat" | "activity" | "contributions" => Self::Heatmap,
            "custom" | "plugin" => Self::Custom,
            "off" | "none" => Self::Off,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Drops => "drops",
            Self::Blocks => "blocks",
            Self::Heatmap => "heatmap",
            Self::Custom => "custom",
            Self::Off => "off",
        }
    }

    /// Milliseconds between frames, in the key window or another one. The
    /// heatmap's slow fades need fewer frames than falling rain.
    pub fn frame_ms(self, key: bool) -> u128 {
        match (self, key) {
            (Self::Heatmap, true) => 50,
            (Self::Heatmap, false) => 150,
            (_, true) => 33,
            (_, false) => 100,
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

/// Columns of the heatmap: seven, like the weekdays of a contribution graph.
pub const HEAT_COLS: u32 = 7;

/// Opacity of the five activity levels, from none to most.
const HEAT_ALPHA: [f32; 5] = [0.09, 0.30, 0.52, 0.76, 1.0];

/// Geometry of the heatmap in an area: (rows, square px, gap px, left inset px).
pub fn heatmap_dims(w: f32, hgt: f32, scale: f32) -> (u32, f32, f32, f32) {
    let gap = (2.0 * scale).round().max(1.0);
    let b = ((w + gap) / HEAT_COLS as f32 - gap).floor();
    if b < 2.0 || hgt < b {
        return (0, 0.0, gap, 0.0);
    }
    let rows = ((hgt + gap) / (b + gap)).floor() as u32;
    let used = HEAT_COLS as f32 * (b + gap) - gap;
    (rows, b, gap, ((w - used) / 2.0).floor().max(0.0))
}

/// Smooth value noise in 0..1 over a 3-D lattice.
fn noise(x: f32, y: f32, z: f32) -> f32 {
    let (xi, yi, zi) = (x.floor(), y.floor(), z.floor());
    let ease = |t: f32| t * t * (3.0 - 2.0 * t);
    let (u, v, w) = (ease(x - xi), ease(y - yi), ease(z - zi));
    let (xi, yi, zi) = (xi as i32, yi as i32, zi as i32);
    let at = |dx: i32, dy: i32, dz: i32| {
        h(((xi + dx) as u32).wrapping_mul(0x8DA6_B343) ^ ((yi + dy) as u32).wrapping_mul(0xD816_3841) ^ ((zi + dz) as u32).wrapping_mul(0xCB1A_B31F))
    };
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    mix(
        mix(mix(at(0, 0, 0), at(1, 0, 0), u), mix(at(0, 1, 0), at(1, 1, 0), u), v),
        mix(mix(at(0, 0, 1), at(1, 0, 1), u), mix(at(0, 1, 1), at(1, 1, 1), u), v),
        w,
    )
}

/// Activity of the square at (column, row) at time `t` (seconds): a level
/// in 0..=4 that rests on the whole levels and eases from one to the next.
pub fn heat_level(c: u32, r: u32, t: f32) -> f32 {
    let (x, y) = (c as f32, r as f32);
    // Busy and quiet stretches a few squares across that drift slowly, and
    // a quicker shimmer from square to square.
    let broad = noise(x * 0.42, y * 0.30, t * 0.11);
    let fine = noise(x * 1.7 + 31.0, y * 1.3 + 7.0, t * 0.37 + 13.0);
    let v = 0.58 * broad + 0.42 * fine;
    // Like a real contribution graph, most squares are empty or pale.
    let level = ((v - 0.40) / 0.36).clamp(0.0, 1.0) * 4.0;
    let (whole, part) = (level.floor(), level.fract());
    let e = ((part - 0.35) / 0.30).clamp(0.0, 1.0);
    whole + e * e * (3.0 - 2.0 * e)
}

/// Where on the pink → violet gradient the square at (column, row) sits:
/// pink towards the top and violet towards the bottom, like the rain, with
/// slow patches that wander between the two.
fn heat_hue(c: u32, r: u32, rows: u32, t: f32) -> f32 {
    let down = (r as f32 + 0.5) / rows.max(1) as f32;
    let wander = noise(c as f32 * 0.25 + 90.0, r as f32 * 0.10 + 40.0, t * 0.08 + 50.0);
    (0.55 * down + 0.45 * ((wander - 0.5) * 1.8 + 0.5)).clamp(0.0, 1.0)
}

/// Heatmap: a grid of rounded squares in the style of a GitHub contribution
/// graph, seven across. Each square's activity level rises and falls on its
/// own, coloured along the zencode gradient; busier squares are more solid.
#[allow(clippy::too_many_arguments)]
pub fn heatmap(f: &mut Frame, x: f32, y: f32, w: f32, hgt: f32, scale: f32, t: f32, still: bool) {
    let (rows, b, gap, inset) = heatmap_dims(w, hgt, scale);
    // Reduce motion: one frame of it, standing still.
    let t = if still { 0.0 } else { t };
    for r in 0..rows {
        for c in 0..HEAT_COLS {
            let level = heat_level(c, r, t);
            let lo = level.floor() as usize;
            let hi = (lo + 1).min(HEAT_ALPHA.len() - 1);
            let alpha = HEAT_ALPHA[lo] + (HEAT_ALPHA[hi] - HEAT_ALPHA[lo]) * (level - lo as f32);
            let col = pal::gradient(heat_hue(c, r, rows, t));
            f.round_rect(x + inset + c as f32 * (b + gap), y + r as f32 * (b + gap), b, b, (b * 0.22).max(1.0), rgba(col, alpha));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heatmap_fits_seven_squares_across() {
        // An 11-cell gutter at 1× (8 px cells) and at 2×.
        for (w, scale) in [(82.0, 1.0), (166.0, 2.0)] {
            let (rows, b, gap, inset) = heatmap_dims(w, 400.0, scale);
            assert!(rows > 10 && b >= 8.0 * scale, "{rows} rows of {b} px");
            assert!(inset + HEAT_COLS as f32 * (b + gap) - gap <= w);
            assert!(rows as f32 * (b + gap) - gap <= 400.0);
        }
        // Too narrow or too short: nothing, rather than slivers.
        assert_eq!(heatmap_dims(10.0, 400.0, 1.0).0, 0);
        assert_eq!(heatmap_dims(82.0, 4.0, 1.0).0, 0);
    }

    #[test]
    fn heat_levels_look_like_a_contribution_graph() {
        let mut count = [0usize; 5];
        let mut moved = 0;
        for r in 0..60 {
            for c in 0..HEAT_COLS {
                let (a, b) = (heat_level(c, r, 3.0), heat_level(c, r, 3.05));
                assert!((0.0..=4.0).contains(&a));
                // One frame later a square has barely changed: it fades, it doesn't flicker.
                assert!((a - b).abs() < 0.5, "({c},{r}) jumped from {a} to {b}");
                count[a.round() as usize] += 1;
                if (heat_level(c, r, 30.0) - a).abs() > 0.5 {
                    moved += 1;
                }
            }
        }
        let total: usize = count.iter().sum();
        // Every level shows up, quiet squares outnumber busy ones, and it isn't frozen.
        assert!(count.iter().all(|&n| n > 0), "{count:?}");
        assert!(count[0] + count[1] > count[3] + count[4], "{count:?}");
        assert!(count[0] < total * 3 / 4, "{count:?}");
        assert!(moved > total / 4, "only {moved} of {total} squares changed in 27 s");
    }

    #[test]
    fn heatmap_draws_gradient_squares_and_stands_still_on_request() {
        let draw = |t: f32, still: bool| {
            let mut f = Frame::new([0.0; 4]);
            heatmap(&mut f, 6.0, 40.0, 82.0, 300.0, 1.0, t, still);
            f.insts
        };
        let a = draw(1.0, false);
        let (rows, b, ..) = heatmap_dims(82.0, 300.0, 1.0);
        assert_eq!(a.len(), (rows * HEAT_COLS) as usize);
        assert!(a.iter().all(|i| i.rect[2] == b && i.rect[3] == b && i.radius > 0.0));
        assert!(a.iter().all(|i| i.rect[0] >= 6.0 && i.rect[0] + b <= 88.0 && i.rect[1] >= 40.0 && i.rect[1] + b <= 340.0));
        // Pink at the top, violet at the bottom (red falls, blue rises), on average.
        let avg = |rows: &[crate::render::Inst], ch: usize| rows.iter().map(|i| i.color[ch]).sum::<f32>() / rows.len() as f32;
        let (top, bottom) = (&a[..a.len() / 4], &a[a.len() * 3 / 4..]);
        assert!(avg(top, 0) > avg(bottom, 0) && avg(top, 2) < avg(bottom, 2));
        // It moves over time, except with Reduce Motion.
        let differs = |x: &[crate::render::Inst], y: &[crate::render::Inst]| x.iter().zip(y).any(|(p, q)| p.color != q.color);
        assert!(differs(&a, &draw(9.0, false)));
        assert!(!differs(&draw(1.0, true), &draw(9.0, true)));
    }

    #[test]
    fn rain_style_names_round_trip() {
        for s in [RainStyle::Drops, RainStyle::Blocks, RainStyle::Heatmap, RainStyle::Custom, RainStyle::Off] {
            assert_eq!(RainStyle::parse(s.name()), Some(s));
        }
        assert_eq!(RainStyle::parse("nope"), None);
    }
}
