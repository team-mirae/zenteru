//! Rich panel content: plugins send `blocks` with `panel.append` /
//! `panel.update` instead of (or as well as) plain text.
//!
//! ```json
//! {"type": "text", "text": "…", "color": "#ff79b0" | "gradient" | "muted",
//!  "bold": true, "italic": false}
//! {"type": "code", "text": "…"}
//! {"type": "image", "base64": "…" | "path": "/abs/file.png",
//!  "max_width": 40, "max_height": 12, "alt": "…"}      (sizes in cells)
//! {"type": "progress", "value": 0.42, "label": "…"}
//! {"type": "divider"}
//! ```
//!
//! Images are decoded by macOS (PNG, JPEG, GIF, HEIC, TIFF, WebP, PDF…),
//! scaled to fit the panel and drawn from the glyph atlas.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::AnyThread;
use objc2_app_kit::NSImage;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSData;
use zenteru_term::json::Json;
use zenteru_text::palette::Rgb;

#[derive(Clone)]
pub struct Image {
    /// Unique per decoded image (atlas cache key).
    pub key: u64,
    pub cg: Rc<CFRetained<CGImage>>,
    pub w: u32,
    pub h: u32,
    pub max_cells: Option<usize>,
    pub max_rows: Option<usize>,
    pub alt: String,
}

#[derive(Clone)]
pub enum TextColor {
    Default,
    Rgb(Rgb),
    Gradient,
    Muted,
}

#[derive(Clone)]
pub enum Block {
    Text { text: String, color: TextColor, bold: bool, italic: bool },
    Code { text: String },
    Image(Image),
    Progress { value: f32, label: String },
    Divider,
}

const MAX_IMAGE_BYTES: usize = 64 << 20;
const MAX_SIDE: usize = 8192;

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

fn decode(bytes: &[u8]) -> Option<(CFRetained<CGImage>, u32, u32)> {
    let data = NSData::with_bytes(bytes);
    let img = NSImage::initWithData(NSImage::alloc(), &data)?;
    let cg = unsafe { img.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let (w, h) = (CGImage::width(Some(&cg)), CGImage::height(Some(&cg)));
    (w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE).then_some((cg.into(), w as u32, h as u32))
}

fn parse_color(v: &Json) -> TextColor {
    match v.as_str() {
        Some("gradient") => TextColor::Gradient,
        Some("muted") => TextColor::Muted,
        Some(s) => {
            let h = s.trim_start_matches('#');
            match (h.len(), u32::from_str_radix(h, 16)) {
                (6, Ok(v)) => TextColor::Rgb(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
                _ => TextColor::Default,
            }
        }
        None => TextColor::Default,
    }
}

/// Parse a `blocks` array. Images that can't be read or decoded become an
/// error so the plugin finds out.
pub fn parse(v: &Json) -> Result<Vec<Block>, String> {
    let mut out = Vec::new();
    for (i, b) in v.as_arr().iter().enumerate() {
        let ty = b.get("type").as_str().unwrap_or("text");
        let text = || b.get("text").as_str().unwrap_or("").to_string();
        out.push(match ty {
            "text" => Block::Text {
                text: text(),
                color: parse_color(b.get("color")),
                bold: b.get("bold").as_bool().unwrap_or(false),
                italic: b.get("italic").as_bool().unwrap_or(false),
            },
            "code" => Block::Code { text: text() },
            "divider" => Block::Divider,
            "progress" => Block::Progress {
                value: b.get("value").as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                label: b.get("label").as_str().unwrap_or("").to_string(),
            },
            "image" => {
                let bytes = if let Some(b64) = b.get("base64").as_str() {
                    zenteru_term::base64::decode(b64).ok_or_else(|| format!("blocks[{i}]: base64 is not valid"))?
                } else if let Some(path) = b.get("path").as_str() {
                    if !path.starts_with('/') {
                        return Err(format!("blocks[{i}]: path must be absolute"));
                    }
                    let meta = std::fs::metadata(path).map_err(|e| format!("blocks[{i}]: {path}: {e}"))?;
                    if meta.len() as usize > MAX_IMAGE_BYTES {
                        return Err(format!("blocks[{i}]: image larger than 64 MiB"));
                    }
                    std::fs::read(path).map_err(|e| format!("blocks[{i}]: {path}: {e}"))?
                } else {
                    return Err(format!("blocks[{i}]: image needs base64 or path"));
                };
                if bytes.len() > MAX_IMAGE_BYTES {
                    return Err(format!("blocks[{i}]: image larger than 64 MiB"));
                }
                let (cg, w, h) = decode(&bytes).ok_or_else(|| format!("blocks[{i}]: not an image macOS can decode (or larger than {MAX_SIDE} px)"))?;
                Block::Image(Image {
                    key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
                    cg: Rc::new(cg),
                    w,
                    h,
                    max_cells: b.get("max_width").as_i64().map(|v| v.max(1) as usize),
                    max_rows: b.get("max_height").as_i64().map(|v| v.max(1) as usize),
                    alt: b.get("alt").as_str().unwrap_or("image").to_string(),
                })
            }
            other => return Err(format!("blocks[{i}]: unknown type {other:?}")),
        });
    }
    Ok(out)
}
