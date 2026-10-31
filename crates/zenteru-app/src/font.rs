//! Fonts and glyph rasterization with CoreText.
//!
//! The default is a mono font with CJK: SF Mono (or Menlo) for Latin, with
//! zenkaku characters drawn from a per-script CJK font and centred in two
//! cells, so the grid stays perfectly monospaced. Sino characters use the
//! regional glyphs chosen by `SINO_DISPLAY` (JA default): Hiragino for JA,
//! Apple SD Gothic Neo for KO, PingFang SC for SG. Anything else falls back
//! through CoreText's own cascade (with the same language hint), so macOS
//! fonts take over smoothly for emoji, symbols and other scripts.
//!
//! `ZENTERU_FONT` / `ZENTERU_FONT_SIZE` override the primary font and size;
//! `ZENTERU_CJK_FONT` overrides the CJK font for every script.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{Type as _, CFDictionary, CFRetained, CFString, CFType, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_core_text::{CTFont, CTFontSymbolicTraits, CTLine, kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, kCTLanguageAttributeName};
use zenteru_text::sino::{Script, SinoDisplay, script};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Style {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Style {
    pub fn from(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Style::Regular,
            (true, false) => Style::Bold,
            (false, true) => Style::Italic,
            (true, true) => Style::BoldItalic,
        }
    }
}

/// A rasterized glyph: premultiplied BGRA, positioned relative to the
/// cell's top-left corner.
pub struct Raster {
    pub w: u32,
    pub h: u32,
    /// Offset of the bitmap's top-left from the cell's top-left, in pixels.
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
    /// Colour glyph (emoji): drawn as is, not tinted.
    pub color: bool,
}

pub struct Fonts {
    pub size_pt: f64,
    pub scale: f64,
    faces: HashMap<Style, CFRetained<CTFont>>,
    cjk: HashMap<(Script, Style), CFRetained<CTFont>>,
    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
    pub descent: f32,
    lang: CFRetained<CFString>,
    colorspace: CFRetained<CGColorSpace>,
}

const PRIMARY: &[&str] = &["SF Mono", "SFMono-Regular", "Menlo-Regular"];

fn cjk_names(s: Script, sino: SinoDisplay) -> &'static [&'static str] {
    match (s, sino) {
        (Script::Hangul, _) => &["AppleSDGothicNeo-Regular", "Apple SD Gothic Neo"],
        (Script::Kana, _) => &["HiraginoSans-W3", "Hiragino Sans", "HiraKakuProN-W3"],
        (Script::Han, SinoDisplay::Ja) => &["HiraginoSans-W3", "Hiragino Sans", "HiraKakuProN-W3"],
        (Script::Han, SinoDisplay::Ko) => &["AppleSDGothicNeo-Regular", "Apple SD Gothic Neo"],
        (Script::Han, SinoDisplay::Sg) => &["PingFangSC-Regular", "PingFang SC", "STHeitiSC-Light"],
        (Script::Other, _) => &[],
    }
}

fn bold_cjk(name: &str) -> String {
    match name {
        "HiraginoSans-W3" => "HiraginoSans-W6".into(),
        "HiraKakuProN-W3" => "HiraKakuProN-W6".into(),
        "AppleSDGothicNeo-Regular" => "AppleSDGothicNeo-Bold".into(),
        "PingFangSC-Regular" => "PingFangSC-Semibold".into(),
        n => n.into(),
    }
}

/// Create a font by name, returning `None` if CoreText substituted a
/// different family (it never fails, it falls back to Helvetica).
fn named(name: &str, size: f64) -> Option<CFRetained<CTFont>> {
    let n = CFString::from_str(name);
    let f = unsafe { CTFont::with_name(&n, size as CGFloat, std::ptr::null()) };
    let ps = unsafe { f.post_script_name() }.to_string();
    let fam = unsafe { f.family_name() }.to_string();
    let want = name.to_lowercase().replace(' ', "");
    let ok = ps.to_lowercase().replace(' ', "").starts_with(&want.split('-').next().unwrap_or(&want).to_string())
        || fam.to_lowercase().replace(' ', "") == want
        || ps.to_lowercase() == name.to_lowercase();
    ok.then_some(f)
}

/// SF Mono through the public API (it has no public name).
fn system_mono(size: f64) -> Option<CFRetained<CTFont>> {
    use objc2_app_kit::{NSFont, NSFontWeightRegular};
    let f = NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular });
    // NSFont is toll-free bridged with CTFont.
    let ptr = objc2::rc::Retained::into_raw(f) as *mut CTFont;
    std::ptr::NonNull::new(ptr).map(|p| unsafe { CFRetained::from_raw(p) })
}

fn with_traits(f: &CTFont, style: Style) -> CFRetained<CTFont> {
    let t = match style {
        Style::Regular => return f.retain(),
        Style::Bold => CTFontSymbolicTraits::BoldTrait,
        Style::Italic => CTFontSymbolicTraits::ItalicTrait,
        Style::BoldItalic => CTFontSymbolicTraits::BoldTrait | CTFontSymbolicTraits::ItalicTrait,
    };
    unsafe { f.copy_with_symbolic_traits(0.0, std::ptr::null(), t, t) }.unwrap_or_else(|| f.retain())
}

impl Fonts {
    pub fn new(size_pt: f64, scale: f64) -> Self {
        let size_px = size_pt * scale;
        let user = std::env::var("ZENTERU_FONT").ok();
        let mut names: Vec<&str> = Vec::new();
        if let Some(u) = user.as_deref() {
            names.push(u);
        }
        names.extend_from_slice(PRIMARY);
        let regular = user
            .as_deref()
            .and_then(|n| named(n, size_px))
            .or_else(|| system_mono(size_px))
            .or_else(|| names.iter().find_map(|n| named(n, size_px)))
            .unwrap_or_else(|| unsafe { CTFont::with_name(&CFString::from_str("Menlo-Regular"), size_px, std::ptr::null()) });
        let mut faces = HashMap::new();
        for s in [Style::Regular, Style::Bold, Style::Italic, Style::BoldItalic] {
            faces.insert(s, with_traits(&regular, s));
        }

        let sino = SinoDisplay::current();
        let user_cjk = std::env::var("ZENTERU_CJK_FONT").ok();
        let mut cjk = HashMap::new();
        for sc in [Script::Hangul, Script::Kana, Script::Han] {
            let mut list: Vec<String> = Vec::new();
            if let Some(u) = &user_cjk {
                list.push(u.clone());
            }
            list.extend(cjk_names(sc, sino).iter().map(|s| s.to_string()));
            let Some((name, f)) = list.iter().find_map(|n| named(n, size_px).map(|f| (n.clone(), f))) else { continue };
            let bold = named(&bold_cjk(&name), size_px).unwrap_or_else(|| with_traits(&f, Style::Bold));
            cjk.insert((sc, Style::Regular), f.clone());
            cjk.insert((sc, Style::Italic), f);
            cjk.insert((sc, Style::Bold), bold.clone());
            cjk.insert((sc, Style::BoldItalic), bold);
        }

        let r = &faces[&Style::Regular];
        let (ascent, descent, leading) = unsafe { (r.ascent(), r.descent(), r.leading()) };
        // Cell width: advance of '0' (all mono glyphs share it).
        let mut glyph: u16 = 0;
        let ch: u16 = '0' as u16;
        unsafe { r.glyphs_for_characters(NonNull::from(&ch), NonNull::from(&mut glyph), 1) };
        let mut adv = CGSize { width: 0.0, height: 0.0 };
        unsafe {
            r.advances_for_glyphs(objc2_core_text::CTFontOrientation::Horizontal, NonNull::from(&mut glyph), &mut adv, 1)
        };
        let cell_w = adv.width.max(1.0).round() as f32;
        let line_gap = (size_px * 0.18).max(leading);
        let cell_h = (ascent + descent + line_gap).ceil() as f32;
        let ascent = (ascent + line_gap / 2.0).round() as f32;
        Fonts {
            size_pt,
            scale,
            faces,
            cjk,
            cell_w,
            cell_h,
            ascent,
            descent: descent as f32,
            lang: CFString::from_str(sino.language_tag()),
            colorspace: CGColorSpace::new_device_rgb().expect("device RGB colour space"),
        }
    }

    /// e.g. "SF Mono 13pt + Hiragino Sans (SINO_DISPLAY=JA)".
    pub fn describe(&self) -> String {
        let fam = |f: &CTFont| {
            let n = unsafe { f.family_name() }.to_string();
            // The system monospaced font has a private name; it is SF Mono.
            if n.starts_with('.') && n.contains("Monospaced") { "SF Mono".to_string() } else { n }
        };
        let primary = fam(&self.faces[&Style::Regular]);
        let cjk = self.cjk.get(&(Script::Han, Style::Regular)).map(|f| fam(f));
        let sino = match SinoDisplay::current() {
            SinoDisplay::Ja => "JA",
            SinoDisplay::Ko => "KO",
            SinoDisplay::Sg => "SG",
        };
        match cjk {
            Some(c) => format!("{primary} {}pt + {c} (SINO_DISPLAY={sino})", self.size_pt),
            None => format!("{primary} {}pt", self.size_pt),
        }
    }

    fn font_for(&self, text: &str, style: Style) -> CFRetained<CTFont> {
        let c = text.chars().next().unwrap_or(' ');
        let sc = script(c);
        if sc != Script::Other {
            if let Some(f) = self.cjk.get(&(sc, style)) {
                return f.clone();
            }
        }
        self.faces[&style].clone()
    }

    /// Rasterize one grapheme cluster spanning `cells` columns.
    pub fn rasterize(&self, text: &str, style: Style, cells: u32) -> Option<Raster> {
        if text.trim().is_empty() && text != "\u{3000}" {
            return None;
        }
        if let Some(r) = self.box_drawing(text, cells) {
            return Some(r);
        }
        let font = self.font_for(text, style);
        let s = CFString::from_str(text);
        let yes = objc2_core_foundation::CFBoolean::new(true);
        let keys: [&CFString; 3] = unsafe { [kCTFontAttributeName, kCTLanguageAttributeName, kCTForegroundColorFromContextAttributeName] };
        let vals: [&CFType; 3] = [font.as_ref(), self.lang.as_ref(), yes.as_ref()];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &vals);
        let astr = unsafe { objc2_core_foundation::CFAttributedString::new(None, Some(&s), Some(attrs.as_opaque())) }?;
        let line = unsafe { CTLine::with_attributed_string(&astr) };
        let (mut asc, mut desc, mut lead): (CGFloat, CGFloat, CGFloat) = (0.0, 0.0, 0.0);
        let width = unsafe { line.typographic_bounds(&mut asc, &mut desc, &mut lead) };
        let span = self.cell_w as f64 * cells as f64;
        // Squeeze glyphs wider than their cells; centre narrower zenkaku ones.
        let sx = if width > span * 1.02 { span / width } else { 1.0 };
        let drawn_w = width * sx;
        let x_off = if cells == 2 || width < span * 0.9 { ((span - drawn_w) / 2.0).max(0.0) } else { 0.0 };

        let pad = (self.cell_h as f64 * 0.5).ceil() as usize;
        let w = span.ceil() as usize + 2 * pad;
        let h = self.cell_h as usize + 2 * pad;
        let mut data = vec![0u8; w * h * 4];
        let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        let ctx: CFRetained<CGContext> = unsafe {
            CGBitmapContextCreate(data.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, Some(&self.colorspace), info)
        }?;
        CGContext::set_should_antialias(Some(&ctx), true);
        CGContext::set_allows_font_smoothing(Some(&ctx), false);
        CGContext::set_should_smooth_fonts(Some(&ctx), false);
        CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
        // CoreGraphics origin is bottom-left; baseline sits `ascent` below the cell top.
        let baseline_y = (pad as f64) + (self.cell_h as f64 - self.ascent as f64);
        CGContext::translate_ctm(Some(&ctx), pad as f64 + x_off, baseline_y);
        if sx != 1.0 {
            CGContext::scale_ctm(Some(&ctx), sx, 1.0);
        }
        CGContext::set_text_position(Some(&ctx), 0.0, 0.0);
        unsafe { line.draw(&ctx) };
        drop(ctx);
        // Colour detection: any pixel with chroma means a colour glyph.
        let color = data.chunks_exact(4).any(|p| p[3] > 16 && (p[0].abs_diff(p[1]) > 24 || p[1].abs_diff(p[2]) > 24));
        Some(trim(Raster { w: w as u32, h: h as u32, left: -(pad as i32), top: -(pad as i32), data, color }))
    }

    /// Pixel-exact box drawing and block elements so lines join across cells.
    fn box_drawing(&self, text: &str, cells: u32) -> Option<Raster> {
        let c = text.chars().next()?;
        if text.chars().count() != 1 || cells != 1 {
            return None;
        }
        let (l, r, u, d, heavy) = match c {
            '─' => (1, 1, 0, 0, false),
            '━' => (1, 1, 0, 0, true),
            '│' => (0, 0, 1, 1, false),
            '┃' => (0, 0, 1, 1, true),
            '┌' | '╭' => (0, 1, 0, 1, false),
            '┐' | '╮' => (1, 0, 0, 1, false),
            '└' | '╰' => (0, 1, 1, 0, false),
            '┘' | '╯' => (1, 0, 1, 0, false),
            '├' => (0, 1, 1, 1, false),
            '┤' => (1, 0, 1, 1, false),
            '┬' => (1, 1, 0, 1, false),
            '┴' => (1, 1, 1, 0, false),
            '┼' => (1, 1, 1, 1, false),
            '█' | '▀' | '▄' | '▌' | '▐' => return Some(self.block(c)),
            _ => return None,
        };
        let w = self.cell_w as usize;
        let h = self.cell_h as usize;
        let t = ((self.scale * if heavy { 2.0 } else { 1.0 }).round() as usize).max(1);
        let mut data = vec![0u8; w * h * 4];
        let cx = w / 2 - t / 2;
        let cy = h / 2 - t / 2;
        let mut fill = |x0: usize, x1: usize, y0: usize, y1: usize| {
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    data[(y * w + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        };
        if l == 1 {
            fill(0, cx + t, cy, cy + t);
        }
        if r == 1 {
            fill(cx, w, cy, cy + t);
        }
        if u == 1 {
            fill(cx, cx + t, 0, cy + t);
        }
        if d == 1 {
            fill(cx, cx + t, cy, h);
        }
        Some(Raster { w: w as u32, h: h as u32, left: 0, top: 0, data, color: false })
    }

    fn block(&self, c: char) -> Raster {
        let w = self.cell_w as usize;
        let h = self.cell_h as usize;
        let (x0, x1, y0, y1) = match c {
            '▀' => (0, w, 0, h / 2),
            '▄' => (0, w, h / 2, h),
            '▌' => (0, w / 2, 0, h),
            '▐' => (w / 2, w, 0, h),
            _ => (0, w, 0, h),
        };
        let mut data = vec![0u8; w * h * 4];
        for y in y0..y1 {
            for x in x0..x1 {
                data[(y * w + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        Raster { w: w as u32, h: h as u32, left: 0, top: 0, data, color: false }
    }
}

/// Crop transparent borders so atlas space isn't wasted.
fn trim(r: Raster) -> Raster {
    let (w, h) = (r.w as usize, r.h as usize);
    let alpha = |x: usize, y: usize| r.data[(y * w + x) * 4 + 3];
    let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| alpha(x, y) > 0)).collect();
    let (Some(&y0), Some(&y1)) = (rows.first(), rows.last()) else {
        return Raster { w: 0, h: 0, data: Vec::new(), ..r };
    };
    let x0 = (0..w).find(|&x| (y0..=y1).any(|y| alpha(x, y) > 0)).unwrap_or(0);
    let x1 = (0..w).rev().find(|&x| (y0..=y1).any(|y| alpha(x, y) > 0)).unwrap_or(w - 1);
    let (nw, nh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut data = Vec::with_capacity(nw * nh * 4);
    for y in y0..=y1 {
        data.extend_from_slice(&r.data[(y * w + x0) * 4..(y * w + x1 + 1) * 4]);
    }
    Raster { w: nw as u32, h: nh as u32, left: r.left + x0 as i32, top: r.top + y0 as i32, data, color: r.color }
}

pub fn _unused(_: CGPoint, _: CGRect) {}
