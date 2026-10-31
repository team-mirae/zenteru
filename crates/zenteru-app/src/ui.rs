//! Drawing: tab bar, terminal grid, IME preedit, password overlays,
//! completion menu, side panel and command palette — all through one
//! Metal frame, in the zencode palette.

use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Instant;

use zenteru_term::cell::{Color, flags};
use zenteru_text::palette::{self as pal, Rgb};
use zenteru_text::text::{grapheme_width, str_width};

use crate::app::{App, Focus, Hits, Layout, TRAFFIC_LIGHTS_PT};
use crate::font::Style;
use crate::render::{Frame, Grad, Rgba, rgba};

fn c(rgb: Rgb) -> Rgba {
    rgba(rgb, 1.0)
}

fn grad_to() -> Rgba {
    c(pal::GRADIENT_TO)
}


fn started() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

/// Touch ID already configured for sudo?
fn sudo_touch_id() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::fs::read_to_string("/etc/pam.d/sudo_local").is_ok_and(|s| s.lines().any(|l| !l.trim_start().starts_with('#') && (l.contains("pam_tid") || l.contains("pam_u2f")))))
}

fn resolve(color: Color, default: Rgb, t_palette: &[Option<(u8, u8, u8)>; 256], bold_bright: bool) -> Rgb {
    match color {
        Color::Default => default,
        Color::Indexed(mut i) => {
            if bold_bright && i < 8 {
                i += 8;
            }
            match t_palette[i as usize] {
                Some((r, g, b)) => Rgb(r, g, b),
                None => pal::ansi256(i),
            }
        }
        Color::Rgb(r, g, b) => Rgb(r, g, b),
    }
}

/// Wrap `text` into lines of at most `width` cells (zenkaku aware),
/// preferring to break at spaces.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let gs = zenteru_text::seg::graphemes(para);
        let mut line = String::new();
        let mut col = 0;
        let mut last_space: Option<(usize, usize)> = None; // (byte idx in line, col after)
        for g in gs {
            let w = grapheme_width(g, col).max(if g == "\t" { 1 } else { 0 });
            let g = if g == "\t" { " " } else { g };
            if col + w > width && g == " " {
                // Break right here; the space itself is dropped.
                out.push(std::mem::take(&mut line));
                col = 0;
                last_space = None;
                continue;
            }
            if col + w > width {
                if let Some((bi, _)) = last_space.filter(|(bi, _)| *bi > 0) {
                    let rest = line[bi + 1..].to_string();
                    line.truncate(bi);
                    out.push(std::mem::take(&mut line));
                    line = rest;
                    col = str_width(&line);
                } else {
                    out.push(std::mem::take(&mut line));
                    col = 0;
                }
                last_space = None;
            }
            if g == " " {
                last_space = Some((line.len(), col + 1));
            }
            line.push_str(g);
            col += w;
        }
        out.push(line);
    }
    out
}

fn elide(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_string();
    }
    let (t, _) = zenteru_text::text::truncate(s, max.saturating_sub(1));
    format!("{t}…")
}

pub fn draw(app: &mut App, wid: &str) {
    let Some(wi) = app.win_index(wid) else { return };
    let magic_items = app.windows[wi].palette.as_ref().map(|p| app.magic_items(&p.text)).unwrap_or_default();
    let rain_style = app.rain_style;
    let rain_frame = app.gutter_frame.clone().filter(|f| f.3.elapsed().as_secs() < 3);
    let App { renderer, windows, sessions, panels, .. } = app;
    let win = &mut windows[wi];
    win.dirty = false;
    win.rain = false;
    win.rain_shown = false;
    let l: Layout = win.layout.clone();
    if l.w < 10.0 || l.h < 10.0 {
        return;
    }
    let scale = l.scale;
    let s = scale as f32;
    let (cw, ch) = (l.cell_w, l.cell_h);
    let mut f = Frame::new(c(pal::BG));
    let mut hits = Hits::default();
    let mut animate = false;

    // ---- tab bar ------------------------------------------------------
    // The whole top bar is the zencode gradient; traffic lights sit on it.
    f.gradient_rect(0.0, 0.0, l.w, l.tabbar_h, 0.0, c(pal::GRADIENT_FROM), grad_to());
    let bar_at = |x: f32| c(pal::gradient(x / l.w.max(1.0)));
    let start = (TRAFFIC_LIGHTS_PT as f32 * s).round();
    let new_w = 34.0 * s;
    let n = win.tabs.len().max(1);
    let avail = l.w - start - new_w - 12.0 * s;
    let tab_w = (avail / n as f32).clamp(64.0 * s, 230.0 * s);
    let tab_top = 7.0 * s;
    let tab_h = l.tabbar_h - tab_top;
    let r = 9.0 * s;
    let dragging = win.drag.as_ref().filter(|d| d.moved).map(|d| (d.tab_id.clone(), d.mouse_x - d.grab_dx, d.detached));
    let mut active_x = None;
    for (i, tab) in win.tabs.iter().enumerate() {
        let mut x = start + i as f32 * tab_w;
        let is_drag = dragging.as_ref().is_some_and(|(id, _, _)| *id == tab.id);
        if let Some((_, dx, _)) = dragging.as_ref().filter(|_| is_drag) {
            x = dx.clamp(start, l.w - tab_w);
        }
        hits.tabs.push((tab.id.clone(), start + i as f32 * tab_w, tab_w));
        let active = i == win.active;
        let hovered = win.hover_tab.as_deref() == Some(tab.id.as_str());
        let sess = sessions.get(&tab.session);
        if active {
            active_x = Some(x);
        } else {
            if hovered {
                f.round_rect(x + 3.0 * s, tab_top + 2.0 * s, tab_w - 6.0 * s, tab_h - 7.0 * s, 7.0 * s, rgba(Rgb(255, 255, 255), 0.22));
            } else if i + 1 != win.active && i + 1 < win.tabs.len() {
                f.rect(x + tab_w - 1.0 * s, tab_top + 9.0 * s, 1.0 * s, tab_h - 18.0 * s, rgba(pal::ON_GRADIENT, 0.3));
            }
        }
        let mut title = tab.title.clone().unwrap_or_default();
        let mut job = None;
        if title.is_empty() {
            if let Some(ss) = sess {
                title = ss.title.clone();
                if title.is_empty() || !ss.s.is_shell_foreground() {
                    job = ss.s.job_name();
                }
            }
        }
        if title.is_empty() {
            title = job.clone().unwrap_or_else(|| "zenteru".into());
        }
        let pw = sess.is_some_and(|ss| ss.pwd.active);
        let bell = sess.and_then(|ss| ss.bell).is_some_and(|b| b.elapsed().as_millis() < 1200);
        let pad = 12.0 * s;
        let close_w = 18.0 * s;
        let mut tx = x + pad;
        let ty = tab_top + (tab_h - ch) / 2.0 - 1.0 * s;
        let max_cells = ((tab_w - 2.0 * pad - close_w) / cw).max(1.0) as usize;
        if active || is_drag {
            // Drawn after the loop so it sits on top.
        } else {
            let fg = if hovered { c(pal::ON_GRADIENT) } else { rgba(pal::ON_GRADIENT, 0.78) };
            if pw {
                tx += cw * renderer.text(&mut f, "🔒", tx, ty, fg, Style::Regular, 2, None, scale) as f32 + cw * 0.5;
            } else if bell {
                tx += cw * renderer.text(&mut f, "🔔", tx, ty, fg, Style::Regular, 2, None, scale) as f32 + cw * 0.5;
            }
            renderer.text(&mut f, &elide(&title, max_cells), tx, ty, fg, Style::Regular, max_cells, None, scale);
        }
        if active || hovered {
            let cx = x + tab_w - pad - close_w * 0.6;
            hits.close.push((tab.id.clone(), cx - 4.0 * s, tab_top, close_w, tab_h));
        }
        let _ = (title, job);
    }
    // Active (or dragged) tab on top, Chrome style: same colour as the
    // terminal, rounded top, gradient accent.
    if let Some(i) = Some(win.active).filter(|&i| i < win.tabs.len()) {
        let tab = &win.tabs[i];
        let x = active_x.unwrap_or(start + i as f32 * tab_w);
        f.round_rect(x, tab_top, tab_w, tab_h + r, r, c(pal::BG));
        // Chrome's outward curves at the tab's feet: BG squares carved by
        // gradient-coloured circles.
        f.rect(x - r, l.tabbar_h - r, r, r, c(pal::BG));
        f.round_rect(x - 2.0 * r, l.tabbar_h - 2.0 * r, 2.0 * r, 2.0 * r, r, bar_at(x - r));
        f.rect(x + tab_w, l.tabbar_h - r, r, r, c(pal::BG));
        f.round_rect(x + tab_w, l.tabbar_h - 2.0 * r, 2.0 * r, 2.0 * r, r, bar_at(x + tab_w + r));
        // Cover the corner patches back with the tab colour at the bottom edge.
        f.rect(x, l.tabbar_h - 1.0, tab_w, 1.0, c(pal::BG));
        f.gradient_rect(x + r * 0.6, tab_top, tab_w - r * 1.2, 2.0 * s, 1.0 * s, c(pal::GRADIENT_FROM), grad_to());
        let sess = sessions.get(&tab.session);
        let mut title = tab.title.clone().unwrap_or_default();
        if title.is_empty() {
            title = sess.map(|ss| ss.title.clone()).unwrap_or_default();
            if title.is_empty() || sess.is_some_and(|ss| !ss.s.is_shell_foreground()) {
                if let Some(j) = sess.and_then(|ss| ss.s.job_name()) {
                    if title.is_empty() {
                        title = j;
                    }
                }
            }
        }
        if title.is_empty() {
            title = "zenteru".into();
        }
        let pad = 12.0 * s;
        let close_w = 18.0 * s;
        let ty = tab_top + (tab_h - ch) / 2.0 - 1.0 * s;
        let max_cells = ((tab_w - 2.0 * pad - close_w) / cw).max(1.0) as usize;
        let mut tx = x + pad;
        let gspan = Grad { x0: x + pad, x1: x + tab_w - pad, to: grad_to() };
        if sess.is_some_and(|ss| ss.pwd.active) {
            tx += cw * renderer.text(&mut f, "🔒", tx, ty, c(pal::FG), Style::Regular, 2, None, scale) as f32 + cw * 0.5;
        }
        renderer.text(&mut f, &elide(&title, max_cells), tx, ty, c(pal::GRADIENT_FROM), Style::Bold, max_cells, Some(gspan), scale);
        let cx = x + tab_w - pad - close_w * 0.6;
        renderer.text(&mut f, "×", cx, ty, c(pal::LABEL), Style::Regular, 1, None, scale);
    }
    for (id, x, _, _, _) in &hits.close {
        if win.hover_tab.as_deref() == Some(id.as_str()) && Some(id.as_str()) != win.tabs.get(win.active).map(|t| t.id.as_str()) {
            let ty = tab_top + (tab_h - ch) / 2.0 - 1.0 * s;
            renderer.text(&mut f, "×", *x + 4.0 * s, ty, c(pal::ON_GRADIENT), Style::Regular, 1, None, scale);
        }
    }
    // New-tab button.
    let nx = start + win.tabs.len() as f32 * tab_w + 6.0 * s;
    let ny = tab_top + (tab_h - 24.0 * s) / 2.0;
    hits.new_tab = (nx, ny, 24.0 * s, 24.0 * s);
    renderer.text(&mut f, "+", nx + (24.0 * s - cw) / 2.0, tab_top + (tab_h - ch) / 2.0 - 1.0 * s, c(pal::ON_GRADIENT), Style::Bold, 1, None, scale);
    // A detached tab being dragged out: a ghost under the pointer.
    if let Some((_, dx, true)) = &dragging {
        f.gradient_border(*dx, tab_top, tab_w, tab_h, r, 2.0 * s, c(Rgb(255, 255, 255)), c(Rgb(255, 255, 255)));
    }

    // ---- terminal ----------------------------------------------------
    let focus_term = win.focus == Focus::Terminal && win.palette.is_none() && win.handle.is_key();
    if let Some(sid) = win.active_session().map(String::from) {
        if let Some(sess) = sessions.get_mut(&sid) {
            // A program other than the shell has the terminal (Claude Code,
            // vim, a build…): rain in the gutter, after a short grace period
            // so quick commands don't flicker it.
            let job = sess.s.job_name().unwrap_or_default();
            let job = job.rsplit('/').next().unwrap_or("").to_string();
            let shellish = sess.s.is_shell_foreground() || crate::magic::SHELLS.contains(&job.as_str()) || crate::magic::REMOTES.contains(&job.as_str());
            if shellish {
                sess.app_since = None;
            } else if sess.app_since.is_none() {
                sess.app_since = Some(Instant::now());
            }
            let app_mode = sess.app_since.is_some_and(|t| t.elapsed().as_millis() >= 500);
            if sess.app_since.is_some() && !app_mode {
                // Come back when the grace period is over.
                animate = true;
            }
            sess.s.dirty.store(false, Ordering::Release);
            let t = sess.s.term.lock().unwrap();
            let bg_default = t.bg_override.map(|(r, g, b)| Rgb(r, g, b)).unwrap_or(pal::BG);
            let fg_default = t.fg_override.map(|(r, g, b)| Rgb(r, g, b)).unwrap_or(pal::FG);
            if bg_default != pal::BG {
                f.rect(0.0, l.tabbar_h, l.panel_x, l.h - l.tabbar_h, c(bg_default));
            }
            // The gutter: in the shell, relative index (ゼロ, -1, -2 … upward
            // from the cursor's line) and each line's 3-character code; while
            // another program has the terminal, zencode rain. The clock sits at
            // the bottom whenever those rows are free.
            if l.gutter_w > 0.0 {
                let gx = l.gutter_x;
                let gw = l.term_x - cw * 0.5 - gx - 2.0 * s;
                let rows_shown = l.rows.min(t.rows);
                f.gradient_rect(l.term_x - cw * 0.5, l.term_y, 1.0 * s, (l.rows as f32) * ch, 0.0, rgba(pal::GRADIENT_FROM, 0.35), rgba(pal::GRADIENT_TO, 0.35));
                let clock_last_row = l.term_y + (l.rows.saturating_sub(1)) as f32 * ch;
                if app_mode || t.alt_screen() {
                    let rain_h = (l.rows.saturating_sub(2)) as f32 * ch - 4.0 * s;
                    let tsec = started().elapsed().as_secs_f32();
                    let still = crate::gutter::reduce_motion();
                    use crate::gutter::RainStyle;
                    match (rain_style, &rain_frame) {
                        (RainStyle::Drops, _) => crate::gutter::rain(&mut f, gx, l.term_y, gw, rain_h, ch, s, tsec, still),
                        (RainStyle::Custom, Some((fw, fh, px, _))) => crate::gutter::custom_rain(&mut f, gx, l.term_y, gw, rain_h, cw, s, *fw, *fh, px),
                        // Custom without a live driver falls back to blocks.
                        (RainStyle::Blocks | RainStyle::Custom, _) => crate::gutter::block_rain(&mut f, gx, l.term_y, gw, rain_h, cw, s, tsec, still),
                        (RainStyle::Off, _) => {}
                    }
                    let (mc, mr, _, _) = crate::gutter::matrix_dims(gw, rain_h, cw, s);
                    win.matrix = (mc, mr);
                    win.rain_shown = !matches!(rain_style, RainStyle::Off);
                    crate::gutter::clock(renderer, &mut f, gx, clock_last_row, gw, cw, ch, scale);
                    // Animate unless it's off or a custom driver paces it with its frames.
                    win.rain = !matches!(rain_style, RainStyle::Off) && !(rain_style == RainStyle::Custom && rain_frame.is_some());
                } else {
                    let anchor = zenteru_term::lineref::anchor(&t);
                    let mut last_numbered = 0usize;
                    for y in 0..rows_shown {
                        let abs = t.view_abs(y);
                        if abs > anchor {
                            break;
                        }
                        last_numbered = y + 1;
                        let Some(row) = t.view_row(y) else { continue };
                        let rel = abs as i64 - anchor as i64;
                        let py = l.term_y + y as f32 * ch;
                        let label = zenteru_term::lineref::rel_label(rel);
                        let lw = str_width(&label);
                        let lx = gx + (6usize.saturating_sub(lw)) as f32 * cw;
                        if rel == 0 {
                            let g = Grad { x0: lx, x1: lx + lw as f32 * cw, to: grad_to() };
                            renderer.text(&mut f, &label, lx, py, c(pal::GRADIENT_FROM), Style::Bold, lw, Some(g), scale);
                        } else {
                            renderer.text(&mut f, &label, lx, py, c(pal::MUTED), Style::Regular, lw, None, scale);
                        }
                        if row.id != 0 {
                            let code = zenteru_term::lineref::code(row.id);
                            let color = if rel == 0 { rgba(pal::LABEL, 0.9) } else { rgba(pal::LABEL, 0.45) };
                            renderer.text(&mut f, &code, gx + 7.0 * cw, py, color, Style::Regular, 3, None, scale);
                        }
                    }
                    // Line numbers win: the clock only takes free rows.
                    if last_numbered + 2 <= l.rows {
                        crate::gutter::clock(renderer, &mut f, gx, clock_last_row, gw, cw, ch, scale);
                    }
                }
            }
            let sel = sess.selection.as_ref().map(|s| s.ordered());
            let reverse = t.modes.reverse_video;
            let rows = l.rows.min(t.rows);
            let cols = l.cols.min(t.cols);
            for y in 0..rows {
                let Some(row) = t.view_row(y) else { continue };
                let abs = t.view_abs(y);
                let py = l.term_y + y as f32 * ch;
                for x in 0..cols {
                    let cell = &row.cells[x];
                    if cell.is_spacer() {
                        continue;
                    }
                    let wide = cell.is_wide();
                    let span = if wide { 2 } else { 1 };
                    let bold = cell.flags & flags::BOLD != 0;
                    let mut fg = resolve(cell.fg, fg_default, &t.palette, bold);
                    let mut bg = resolve(cell.bg, bg_default, &t.palette, false);
                    let mut bg_set = cell.bg != Color::Default;
                    if (cell.flags & flags::INVERSE != 0) != reverse {
                        std::mem::swap(&mut fg, &mut bg);
                        bg_set = true;
                    }
                    if cell.flags & flags::DIM != 0 {
                        fg = fg.lerp(bg, 0.45);
                    }
                    let selected = sel.is_some_and(|(a, b)| (abs, x) >= (a.line, a.col) && (abs, x) < (b.line, b.col) && !(a.line == b.line && a.col == b.col));
                    if selected {
                        bg = pal::SELECT_BG;
                        bg_set = true;
                    }
                    let px = l.term_x + x as f32 * cw;
                    if bg_set {
                        f.rect(px, py, cw * span as f32, ch, c(bg));
                    }
                    if cell.c == '\u{3000}' {
                        // Make 全角スペース visible.
                        f.round_rect(px + 1.0 * s, py + 2.0 * s, cw * 2.0 - 2.0 * s, ch - 4.0 * s, 2.0 * s, c(pal::ZENKAKU_SPACE_BG));
                    }
                    if cell.flags & flags::HIDDEN == 0 && (cell.c != ' ' || cell.extra.is_some()) {
                        let style = Style::from(bold, cell.flags & flags::ITALIC != 0);
                        renderer.push_glyph(&mut f, &cell.text(), style, span as u32, px, py, c(fg), None, scale);
                    }
                    if cell.flags & flags::ANY_UNDERLINE != 0 || cell.link != 0 {
                        let ul = match cell.ul {
                            Color::Default => fg,
                            other => resolve(other, fg, &t.palette, false),
                        };
                        let uy = py + l.cell_h - (l.cell_h - renderer_ascent(renderer, scale)) * 0.55;
                        let th = (1.0 * s).max(1.0);
                        let a = if cell.flags & flags::ANY_UNDERLINE == 0 { 0.5 } else { 1.0 };
                        f.rect(px, uy, cw * span as f32, th, rgba(ul, a));
                        if cell.flags & flags::DOUBLE_UNDERLINE != 0 {
                            f.rect(px, uy + 2.0 * th, cw * span as f32, th, c(ul));
                        }
                    }
                    if cell.flags & flags::STRIKE != 0 {
                        f.rect(px, py + ch * 0.55, cw * span as f32, (1.0 * s).max(1.0), c(fg));
                    }
                }
            }
            // Cursor (only when the view is at the bottom).
            let cx = t.cursor.x.min(cols.saturating_sub(1));
            let cy = t.cursor.y;
            let at_bottom = t.display_offset == 0;
            let px = l.term_x + cx as f32 * cw;
            let py = l.term_y + cy as f32 * ch;
            hits_ime(&mut hits, px, py, cw, ch);
            let cur_cell = t.screen().get(cy).and_then(|r| r.cells.get(cx)).cloned();
            let cur_w = if cur_cell.as_ref().is_some_and(|c| c.is_wide()) { 2.0 } else { 1.0 };
            let preedit = sess.preedit.clone();
            let pw = sess.pwd.active;
            if at_bottom && t.modes.cursor_visible && preedit.is_empty() && !pw && cy < rows {
                let cc = t.cursor_color.map(|(r, g, b)| Rgb(r, g, b)).unwrap_or(pal::CURSOR);
                let style = t.modes.cursor_style;
                if !focus_term {
                    f.gradient_border(px, py, cw * cur_w, ch, 1.0 * s, 1.0 * s, c(cc), c(cc));
                } else if matches!(style, 3 | 4) {
                    f.rect(px, py + ch - 2.0 * s, cw * cur_w, 2.0 * s, c(cc));
                } else if matches!(style, 5 | 6) {
                    f.rect(px, py, 2.0 * s, ch, c(cc));
                } else {
                    f.round_rect(px, py, cw * cur_w, ch, 1.5 * s, c(cc));
                    if let Some(cell) = cur_cell.filter(|c| c.c != ' ') {
                        renderer.push_glyph(&mut f, &cell.text(), Style::Regular, cur_w as u32, px, py, c(pal::BG), None, scale);
                    }
                }
            }
            let echo_warning = sess.echo_warning;
            let secret_in_line = sess.secret_in_line;
            let confirm = sess.confirm_enter;
            let pw_count = sess.pwd.count;
            let job = if pw { sess.s.job_name() } else { None };
            drop(t);

            // IME preedit (marked text) at the cursor, underlined in the gradient.
            if !preedit.is_empty() && at_bottom {
                let w = str_width(&preedit) as f32 * cw;
                f.rect(px, py, w, ch, c(pal::CUR_LINE_BG));
                renderer.text(&mut f, &preedit, px, py, c(pal::FG), Style::Regular, 400, None, scale);
                f.gradient_rect(px, py + ch - 2.0 * s, w.max(cw), 2.0 * s, 0.0, c(pal::GRADIENT_FROM), grad_to());
                hits_ime(&mut hits, px + w, py, cw, ch);
            }

            // Password field: unmistakable gradient frame with ● per key.
            if pw && at_bottom {
                let label = "  password · 비밀번호 · パスワード ";
                let min_cells = 26usize;
                let room = cols.saturating_sub(cx);
                let bx_col = if room < min_cells { cols.saturating_sub(min_cells) } else { cx };
                let bx = l.term_x + bx_col as f32 * cw - 3.0 * s;
                let bw = (cols - bx_col) as f32 * cw + 3.0 * s;
                let by = py - 3.0 * s;
                let bh = ch + 6.0 * s;
                f.round_rect(bx, by, bw, bh, 7.0 * s, c(Rgb(34, 27, 52)));
                f.gradient_border(bx, by, bw, bh, 7.0 * s, 2.0 * s, c(pal::GRADIENT_FROM), grad_to());
                let mut x = bx + 6.0 * s;
                x += renderer.text(&mut f, "🔒", x, py, c(pal::FG), Style::Regular, 2, None, scale) as f32 * cw + cw * 0.5;
                let label_cells = str_width(label);
                let dots_room = ((bx + bw - x) / cw) as usize;
                let dots_room = dots_room.saturating_sub(label_cells.min(dots_room / 2));
                let shown = pw_count.min(dots_room.saturating_sub(4));
                let dots: String = "●".repeat(shown) + if shown < pw_count { " …" } else { "" };
                let g = Grad { x0: x, x1: x + (dots_room as f32 * cw).max(cw), to: grad_to() };
                if pw_count == 0 {
                    renderer.text(&mut f, "hidden input", x, py, rgba(pal::LABEL, 0.7), Style::Italic, dots_room, None, scale);
                } else {
                    renderer.text(&mut f, &dots, x, py, c(pal::GRADIENT_FROM), Style::Bold, dots_room, Some(g), scale);
                }
                let lx = bx + bw - label_cells as f32 * cw - 6.0 * s;
                if lx > x + (shown as f32 + 2.0) * cw {
                    renderer.text(&mut f, label, lx, py, rgba(pal::LABEL, 0.85), Style::Regular, label_cells, None, scale);
                }
                // Touch ID / YubiKey hint for sudo.
                if job.as_deref() == Some("sudo") && !sudo_touch_id() && cy + 1 < rows {
                    let hint = "Tip: use Touch ID or a YubiKey for sudo — ⌘⇧P → “Set Up Touch ID / YubiKey for sudo”";
                    renderer.text(&mut f, hint, bx, py + ch + 6.0 * s, rgba(pal::ACCENT_GOLD, 0.9), Style::Italic, cols, None, scale);
                }
            }

            // Echo is on although the prompt asks for a secret.
            if echo_warning && at_bottom {
                let msg = "⚠ This prompt SHOWS what you type — echo is ON · 입력이 화면에 보입니다 · 入力が表示されます";
                banner(renderer, &mut f, &l, py, cy, msg, pal::ACCENT_GOLD, false, scale);
                let bx = l.term_x + cx as f32 * cw;
                f.gradient_border(bx - 2.0 * s, py - 2.0 * s, l.term_x + cols as f32 * cw - bx + 2.0 * s, ch + 4.0 * s, 5.0 * s, 2.0 * s, c(pal::ACCENT_GOLD), c(pal::ERROR));
            }
            // A remembered password typed into a normal field.
            if secret_in_line && at_bottom {
                animate = true;
                let msg = if confirm {
                    "⚠ PASSWORD IN A VISIBLE FIELD — press Enter again to send it anyway · Esc clears the line"
                } else {
                    "⚠ PASSWORD IN A VISIBLE FIELD — this matches a password you typed. Enter is blocked · Esc clears the line"
                };
                banner(renderer, &mut f, &l, py, cy, msg, pal::ERROR, true, scale);
                let pulse = 0.55 + 0.45 * ((started().elapsed().as_secs_f32() * 5.0).sin() * 0.5 + 0.5);
                f.gradient_border(l.term_x - 2.0 * s, py - 2.0 * s, cols as f32 * cw + 4.0 * s, ch + 4.0 * s, 4.0 * s, 3.0 * s, rgba(pal::ERROR, pulse), rgba(pal::GRADIENT_FROM, pulse));
            }

            // Completion menu.
            if let Some(menu) = &sess.menu {
                let visible = menu.items.len().min(10);
                let first = menu.selected.saturating_sub(visible - 1).min(menu.items.len() - visible);
                let width_cells = menu
                    .items
                    .iter()
                    .map(|i| str_width(&i.display) + if i.annotation.is_empty() { 0 } else { str_width(&i.annotation) + 3 } + 4)
                    .max()
                    .unwrap_or(10)
                    .clamp(12, 70);
                let mw = width_cells as f32 * cw;
                let mh = visible as f32 * ch + 8.0 * s;
                let mx = (px).min(l.term_x + l.cols as f32 * cw - mw).max(l.term_x);
                let below = py + ch + 4.0 * s;
                let my = if below + mh < l.h { below } else { (py - mh - 4.0 * s).max(l.term_y) };
                f.round_rect(mx, my, mw, mh, 8.0 * s, c(Rgb(30, 26, 44)));
                f.gradient_border(mx, my, mw, mh, 8.0 * s, 1.5 * s, c(pal::GRADIENT_FROM), grad_to());
                for (k, item) in menu.items[first..first + visible].iter().enumerate() {
                    let iy = my + 4.0 * s + k as f32 * ch;
                    let sel = first + k == menu.selected;
                    if sel {
                        f.gradient_rect(mx + 4.0 * s, iy, mw - 8.0 * s, ch, 5.0 * s, rgba(pal::GRADIENT_FROM, 0.85), rgba(pal::GRADIENT_TO, 0.85));
                    }
                    let fg = if sel { c(pal::ON_GRADIENT) } else { c(pal::FG) };
                    let tag = match item.source.as_str() {
                        "ime" => "あ",
                        "history" => "↺",
                        _ => "›",
                    };
                    let tag_color = if sel { fg } else if item.source == "ime" { c(pal::ACCENT_MINT) } else { c(pal::MUTED) };
                    renderer.text(&mut f, tag, mx + 8.0 * s, iy, tag_color, Style::Regular, 2, None, scale);
                    let label_x = mx + 8.0 * s + 2.5 * cw;
                    let ann_w = str_width(&item.annotation);
                    let max_label = width_cells.saturating_sub(ann_w + 6);
                    renderer.text(&mut f, &elide(&item.display, max_label), label_x, iy, fg, if sel { Style::Bold } else { Style::Regular }, max_label, None, scale);
                    if !item.annotation.is_empty() {
                        let ax = mx + mw - (ann_w as f32 + 1.0) * cw;
                        renderer.text(&mut f, &item.annotation, ax, iy, if sel { fg } else { c(pal::LABEL) }, Style::Italic, ann_w, None, scale);
                    }
                }
            }
            // Bell: brief gradient flash along the top of the terminal.
            if sess.bell.is_some_and(|b| b.elapsed().as_millis() < 250) {
                animate = true;
                f.gradient_rect(0.0, l.tabbar_h, l.panel_x, 3.0 * s, 0.0, c(pal::GRADIENT_FROM), grad_to());
            }
            // Scrolled back: show where we are.
            let off = sess.s.term.lock().unwrap().display_offset;
            if off > 0 {
                let label = format!(" ↑ {off} ");
                let wcells = str_width(&label);
                let bx = l.term_x + (l.cols - wcells) as f32 * cw;
                f.round_rect(bx, l.term_y, wcells as f32 * cw, ch, 6.0 * s, c(pal::CUR_LINE_BG));
                renderer.text(&mut f, &label, bx, l.term_y, c(pal::ACCENT_GOLD), Style::Regular, wcells, None, scale);
            }
        }
    }

    // ---- side panel ---------------------------------------------------
    if l.panel_w > 0.0 {
        if let Some(p) = win.panel.as_ref().and_then(|id| panels.get(id)) {
            let px = l.panel_x;
            let pw = l.panel_w;
            let top = l.tabbar_h;
            let pad = 10.0 * s;
            f.rect(px, top, pw, l.h - top, c(pal::BAR_BG));
            f.gradient_rect(px, top, 2.0 * s, l.h - top, 0.0, c(pal::GRADIENT_FROM), grad_to());
            let inner_x = px + pad;
            let inner_cells = ((pw - 2.0 * pad) / cw).max(8.0) as usize;
            // Header: title, modes, close.
            let mut y = top + 6.0 * s;
            let gt = Grad { x0: inner_x, x1: inner_x + inner_cells as f32 * cw * 0.6, to: grad_to() };
            renderer.text(&mut f, &p.title, inner_x, y, c(pal::GRADIENT_FROM), Style::Bold, inner_cells.saturating_sub(4), Some(gt), scale);
            let cx = px + pw - pad - cw * 1.5;
            renderer.text(&mut f, "×", cx, y, c(pal::LABEL), Style::Regular, 1, None, scale);
            hits.panel_close = [cx - 4.0 * s, y, cw * 2.0, ch];
            y += ch + 4.0 * s;
            if !p.modes.is_empty() {
                let mut mx = inner_x;
                for m in &p.modes {
                    let wc = str_width(m) + 2;
                    let w = wc as f32 * cw;
                    let on = *m == p.mode;
                    if on {
                        f.gradient_rect(mx, y, w, ch + 2.0 * s, 7.0 * s, c(pal::GRADIENT_FROM), grad_to());
                    } else {
                        f.gradient_border(mx, y, w, ch + 2.0 * s, 7.0 * s, 1.0 * s, c(pal::MUTED), c(pal::MUTED));
                    }
                    renderer.text(&mut f, m, mx + cw, y + 1.0 * s, if on { c(pal::ON_GRADIENT) } else { c(pal::LABEL) }, Style::Bold, wc, None, scale);
                    hits.modes.push((p.id.clone(), m.clone(), [mx, y, w, ch + 2.0 * s]));
                    mx += w + 6.0 * s;
                }
                renderer.text(&mut f, "⇥ switch", mx + 4.0 * s, y + 1.0 * s, c(pal::MUTED), Style::Italic, 10, None, scale);
                y += ch + 8.0 * s;
            }
            if !p.status.is_empty() {
                renderer.text(&mut f, &elide(&p.status, inner_cells), inner_x, y, c(pal::ACCENT_GOLD), Style::Italic, inner_cells, None, scale);
                y += ch + 4.0 * s;
            }
            let header_bottom = y;
            // Input box at the bottom.
            let shown_input = if p.input_secret { "●".repeat(p.input.chars().count()) } else { p.input.clone() };
            let input_lines = {
                let mut t = shown_input.clone();
                t.insert_str(crate::app::byte_at(&t, p.cursor), &p.preedit);
                wrap(&t, inner_cells.saturating_sub(2)).len().clamp(1, 6)
            };
            let ih = input_lines as f32 * ch + 10.0 * s;
            let iy = l.h - ih - pad;
            let focused = win.focus == Focus::Panel && win.handle.is_key();
            f.round_rect(px + pad - 4.0 * s, iy, pw - 2.0 * pad + 8.0 * s, ih, 8.0 * s, c(pal::BG));
            if focused || p.input_secret {
                f.gradient_border(px + pad - 4.0 * s, iy, pw - 2.0 * pad + 8.0 * s, ih, 8.0 * s, if p.input_secret { 2.0 * s } else { 1.5 * s }, c(pal::GRADIENT_FROM), grad_to());
            } else {
                f.gradient_border(px + pad - 4.0 * s, iy, pw - 2.0 * pad + 8.0 * s, ih, 8.0 * s, 1.0 * s, c(pal::MUTED), c(pal::MUTED));
            }
            hits.panel_input = [px, iy, pw, ih];
            let text_y = iy + 5.0 * s;
            let input_x = inner_x + cw * 0.5;
            if p.input.is_empty() && p.preedit.is_empty() {
                let ph = if p.input_secret { "hidden input — Enter to submit" } else { "message · Enter to send · Esc → terminal" };
                renderer.text(&mut f, ph, input_x, text_y, rgba(pal::LABEL, 0.6), Style::Italic, inner_cells - 1, None, scale);
            }
            {
                let before: String = shown_input.chars().take(p.cursor).collect();
                let after: String = shown_input.chars().skip(p.cursor).collect();
                let full = format!("{before}{}{after}", p.preedit);
                let lines = wrap(&full, inner_cells.saturating_sub(2));
                let first = lines.len().saturating_sub(6);
                let g = Grad { x0: input_x, x1: input_x + inner_cells as f32 * cw, to: grad_to() };
                for (k, line) in lines[first..].iter().enumerate() {
                    let ly = text_y + k as f32 * ch;
                    if p.input_secret {
                        renderer.text(&mut f, line, input_x, ly, c(pal::GRADIENT_FROM), Style::Bold, inner_cells, Some(g), scale);
                    } else {
                        renderer.text(&mut f, line, input_x, ly, c(pal::FG), Style::Regular, inner_cells, None, scale);
                    }
                }
                // Caret (after wrapping `before + preedit`).
                let pre = wrap(&format!("{before}{}", p.preedit), inner_cells.saturating_sub(2));
                let row = pre.len().saturating_sub(1).saturating_sub(first);
                let col = pre.last().map(|s| str_width(s)).unwrap_or(0);
                let (kx, ky) = (input_x + col as f32 * cw, text_y + row as f32 * ch);
                if focused {
                    f.rect(kx, ky, 2.0 * s, ch, c(pal::CURSOR));
                    hits_ime(&mut hits, kx, ky, cw, ch);
                }
                if !p.preedit.is_empty() {
                    let pre_x = input_x + str_width(&wrap(&before, inner_cells.saturating_sub(2)).last().cloned().unwrap_or_default()) as f32 * cw;
                    f.gradient_rect(pre_x, ky + ch - 2.0 * s, (str_width(&p.preedit) as f32 * cw).max(cw), 2.0 * s, 0.0, c(pal::GRADIENT_FROM), grad_to());
                }
            }
            // Messages, bottom-up.
            let mut lines: Vec<(Rgba, Style, String, Option<(u64, Vec<(String, String)>)>)> = Vec::new();
            for m in &p.messages {
                let (label, color) = match m.role.as_str() {
                    "user" => ("you", pal::ACCENT_MINT),
                    "assistant" => (p.title.as_str(), pal::GRADIENT_FROM),
                    "tool" => ("tool", pal::ACCENT_GOLD),
                    _ => ("·", pal::MUTED),
                };
                lines.push((c(color), Style::Bold, label.to_string(), None));
                let body_color = if m.role == "tool" || m.role == "system" { c(pal::LABEL) } else { c(pal::FG) };
                for wl in wrap(&m.text, inner_cells) {
                    lines.push((body_color, Style::Regular, wl, None));
                }
                if !m.actions.is_empty() {
                    lines.push((c(pal::FG), Style::Regular, String::new(), Some((m.id, m.actions.clone()))));
                }
                lines.push((c(pal::FG), Style::Regular, String::new(), None));
            }
            let avail_rows = ((iy - 6.0 * s - header_bottom) / ch).max(0.0) as usize;
            let end = lines.len().saturating_sub(p.scroll.min(lines.len().saturating_sub(avail_rows)));
            let begin = end.saturating_sub(avail_rows);
            for (k, (color, style, text, actions)) in lines[begin..end].iter().enumerate() {
                let ly = header_bottom + k as f32 * ch;
                if let Some((mid, acts)) = actions {
                    let mut ax = inner_x;
                    for (aid, label) in acts {
                        let wc = str_width(label) + 2;
                        let w = wc as f32 * cw;
                        f.gradient_rect(ax, ly, w, ch, 6.0 * s, rgba(pal::GRADIENT_FROM, 0.9), rgba(pal::GRADIENT_TO, 0.9));
                        renderer.text(&mut f, label, ax + cw, ly, c(pal::ON_GRADIENT), Style::Bold, wc, None, scale);
                        hits.actions.push((p.id.clone(), *mid, aid.clone(), [ax, ly, w, ch]));
                        ax += w + 6.0 * s;
                    }
                } else {
                    renderer.text(&mut f, text, inner_x, ly, *color, *style, inner_cells, None, scale);
                }
            }
        }
    }

    // ---- Black Magic (the @ command line, also ⌘⇧P) ---------------------
    if let Some(ps) = &win.palette {
        // Anchor: the cursor's row in the shell; the line above the bottom in
        // full-screen programs (vim keeps its own last line).
        let (row, col, alt) = win
            .active_session()
            .and_then(|sid| sessions.get(sid))
            .map(|ss| {
                let t = ss.s.term.lock().unwrap();
                (t.cursor.y, t.cursor.x, t.alt_screen())
            })
            .unwrap_or((0, 0, false));
        let row = if alt { l.rows.saturating_sub(2) } else { row.min(l.rows.saturating_sub(1)) };
        let min_cells = 44usize.min(l.cols);
        let col = if alt { 0 } else { col.min(l.cols.saturating_sub(min_cells)) };
        let bx = l.term_x + col as f32 * cw - 4.0 * s;
        let bw = l.term_x + l.cols as f32 * cw - bx + 4.0 * s;
        let by = l.term_y + row as f32 * ch - 3.0 * s;
        let bh = ch + 6.0 * s;
        f.gradient_rect(bx, by, bw, bh, 7.0 * s, c(pal::GRADIENT_FROM), grad_to());
        let ty = by + 3.0 * s;
        let mut x = bx + 8.0 * s;
        let chip = "Black Magic";
        let chip_w = str_width(chip) as f32 * cw + 12.0 * s;
        f.round_rect(x - 4.0 * s, by + 3.0 * s, chip_w, ch, 5.0 * s, rgba(pal::ON_GRADIENT, 0.88));
        let g = Grad { x0: x, x1: x + chip_w, to: grad_to() };
        renderer.text(&mut f, chip, x + 2.0 * s, ty, c(pal::GRADIENT_FROM), Style::Bold, str_width(chip), Some(g), scale);
        x += chip_w + cw * 0.5;
        x += renderer.text(&mut f, "@", x, ty, c(pal::ON_GRADIENT), Style::Bold, 1, None, scale) as f32 * cw;
        let room = ((bx + bw - x) / cw).max(1.0) as usize;
        let typed = format!("{}{}", ps.text, ps.preedit);
        if typed.is_empty() {
            renderer.text(&mut f, "command, or ask anything · @@ types @", x, ty, rgba(pal::ON_GRADIENT, 0.55), Style::Italic, room, None, scale);
        }
        let tw = renderer.text(&mut f, &ps.text, x, ty, c(pal::ON_GRADIENT), Style::Bold, room, None, scale) as f32 * cw;
        let pw_ = renderer.text(&mut f, &ps.preedit, x + tw, ty, c(pal::ON_GRADIENT), Style::Regular, room, None, scale) as f32 * cw;
        if !ps.preedit.is_empty() {
            f.rect(x + tw, ty + ch - 2.0 * s, pw_.max(cw), 2.0 * s, c(pal::ON_GRADIENT));
        }
        f.rect(x + tw + pw_, ty, 2.0 * s, ch, c(pal::ON_GRADIENT));
        hits_ime(&mut hits, x + tw, ty, cw, ch);
        if let Some(msg) = &ps.message {
            let mw = str_width(msg) as f32 * cw + 12.0 * s;
            let mx = bx + bw - mw - 6.0 * s;
            f.round_rect(mx, by + 3.0 * s, mw, ch, 5.0 * s, c(pal::BG));
            renderer.text(&mut f, msg, mx + 6.0 * s, ty, c(pal::ERROR), Style::Bold, str_width(msg), None, scale);
        }
        // Suggestions under the bar (above it near the bottom).
        let shown = magic_items.len().min(8);
        if shown > 0 {
            let lh = shown as f32 * ch + 8.0 * s;
            let ly = if by + bh + lh + 4.0 * s < l.h { by + bh + 4.0 * s } else { by - lh - 4.0 * s };
            f.round_rect(bx, ly, bw, lh, 7.0 * s, c(Rgb(30, 26, 44)));
            f.gradient_border(bx, ly, bw, lh, 7.0 * s, 1.5 * s, c(pal::GRADIENT_FROM), grad_to());
            let sel = ps.selected.min(magic_items.len() - 1);
            let first = sel.saturating_sub(shown - 1);
            let name_w = magic_items.iter().map(|c| str_width(&c.name)).max().unwrap_or(8).min(24) + 2;
            let cells = ((bw - 16.0 * s) / cw) as usize;
            for (k, cmd) in magic_items.iter().skip(first).take(shown).enumerate() {
                let iy = ly + 4.0 * s + k as f32 * ch;
                let on = first + k == sel;
                if on {
                    let a = if ps.moved { 0.95 } else { 0.35 };
                    f.gradient_rect(bx + 4.0 * s, iy, bw - 8.0 * s, ch, 5.0 * s, rgba(pal::GRADIENT_FROM, a), rgba(pal::GRADIENT_TO, a));
                }
                let strong = on && ps.moved;
                let fg = if strong { c(pal::ON_GRADIENT) } else { c(pal::FG) };
                renderer.text(&mut f, &format!("@{}", cmd.name), bx + 10.0 * s, iy, if strong { fg } else { c(pal::ACCENT_MINT) }, Style::Bold, name_w, None, scale);
                renderer.text(&mut f, &cmd.title, bx + 10.0 * s + (name_w + 1) as f32 * cw, iy, if strong { fg } else { c(pal::LABEL) }, Style::Regular, cells.saturating_sub(name_w + 1), None, scale);
            }
        }
    }

    win.hits = hits;
    if animate {
        win.dirty = true;
    }
    win.last_frame = Some(Instant::now());
    win.handle.present(renderer, &f, (l.w, l.h));
}

fn renderer_ascent(r: &mut crate::render::Renderer, scale: f64) -> f32 {
    r.fonts(scale).ascent
}

fn hits_ime(h: &mut Hits, x: f32, y: f32, w: f32, hh: f32) {
    h.ime = [x, y, w, hh];
}

/// A warning band on the row above the cursor (or below on the first row).
#[allow(clippy::too_many_arguments)]
fn banner(r: &mut crate::render::Renderer, f: &mut Frame, l: &Layout, cursor_py: f32, cursor_row: usize, msg: &str, color: Rgb, strong: bool, scale: f64) {
    let s = scale as f32;
    let ch = l.cell_h;
    let by = if cursor_row == 0 { cursor_py + ch + 4.0 * s } else { cursor_py - ch - 6.0 * s };
    let w = l.cols as f32 * l.cell_w;
    if strong {
        f.round_rect(l.term_x, by, w, ch + 2.0 * s, 5.0 * s, rgba(color, 0.92));
        r.text(f, msg, l.term_x + 6.0 * s, by + 1.0 * s, c(pal::ON_GRADIENT), Style::Bold, l.cols.saturating_sub(1), None, scale);
    } else {
        f.round_rect(l.term_x, by, w, ch + 2.0 * s, 5.0 * s, rgba(color, 0.16));
        f.gradient_border(l.term_x, by, w, ch + 2.0 * s, 5.0 * s, 1.0 * s, c(color), c(color));
        r.text(f, msg, l.term_x + 6.0 * s, by + 1.0 * s, c(color), Style::Bold, l.cols.saturating_sub(1), None, scale);
    }
}


#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_keeps_spaces_between_words() {
        assert_eq!(wrap("store it in the keychain", 8), ["store it", "in the", "keychain"]);
        assert_eq!(wrap("한글 텍스트를 감싸기", 8), ["한글", "텍스트를", "감싸기"]);
        assert_eq!(wrap("aaaaaaaaaaaa", 5), ["aaaaa", "aaaaa", "aa"]);
        assert_eq!(wrap("a\nb", 5), ["a", "b"]);
    }
}
