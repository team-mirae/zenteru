//! Metal renderer. Everything on screen — cells, glyphs, the tab bar,
//! panels, menus — is one instanced draw of quads:
//!
//! * solid / gradient rectangles with optional rounded corners,
//! * gradient borders (the zencode pink → violet ring for password fields),
//! * glyphs from a BGRA atlas, tinted (optionally along a gradient) or, for
//!   colour emoji, drawn as is.

use std::collections::HashMap;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice,
    MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSize, MTLStoreAction,
    MTLTexture, MTLTextureDescriptor, MTLTextureUsage, MTLClearColor,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::font::{Fonts, Style};

const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Inst {
    float4 rect;    // x, y, w, h in pixels
    float4 uv;      // u0, v0, u1, v1
    float4 color;
    float4 color2;
    float4 extra;   // grad_x0, grad_x1, border thickness, -
    uint kind;
    float radius;
    float2 pad;
};

struct VOut {
    float4 pos [[position]];
    float2 uv;
    float2 local;   // pixels from the rect centre
    float2 half_size;
    float4 color;
    float4 color2;
    float4 extra;
    float px_x;
    uint kind [[flat]];
    float radius;
};

vertex VOut vmain(uint vid [[vertex_id]], uint iid [[instance_id]],
                  const device Inst* insts [[buffer(0)]],
                  constant float2& viewport [[buffer(1)]]) {
    Inst i = insts[iid];
    float2 corner = float2(vid & 1, vid >> 1);
    float2 p = i.rect.xy + corner * i.rect.zw;
    VOut o;
    o.pos = float4(p.x / viewport.x * 2.0 - 1.0, 1.0 - p.y / viewport.y * 2.0, 0, 1);
    o.uv = mix(i.uv.xy, i.uv.zw, corner);
    o.half_size = i.rect.zw * 0.5;
    o.local = (corner - 0.5) * i.rect.zw;
    o.color = i.color;
    o.color2 = i.color2;
    o.extra = i.extra;
    o.px_x = p.x;
    o.kind = i.kind;
    o.radius = i.radius;
    return o;
}

float sdf_round_rect(float2 p, float2 b, float r) {
    float2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float4 grad(VOut v, float t) {
    return mix(v.color, v.color2, clamp(t, 0.0, 1.0));
}

fragment float4 fmain(VOut v [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
    constexpr sampler s(filter::nearest);
    float4 c = v.color;
    if (v.extra.y > v.extra.x) {
        c = grad(v, (v.px_x - v.extra.x) / (v.extra.y - v.extra.x));
    }
    float mask = 1.0;
    if (v.radius > 0.0 || v.kind == 4) {
        float d = sdf_round_rect(v.local, v.half_size, v.radius);
        mask = clamp(0.5 - d, 0.0, 1.0);
        if (v.kind == 4) {
            float inner = clamp(0.5 - (-d - v.extra.z), 0.0, 1.0);
            mask = mask * inner;
        }
    }
    switch (v.kind) {
        case 1: { // tinted glyph
            float a = atlas.sample(s, v.uv).a;
            return float4(c.rgb * c.a, c.a) * a;
        }
        case 2: { // colour glyph, premultiplied already
            return atlas.sample(s, v.uv) * c.a;
        }
        case 3: { // horizontal gradient across the rect itself
            float t = (v.local.x + v.half_size.x) / max(v.half_size.x * 2.0, 1.0);
            float4 g = grad(v, t);
            return float4(g.rgb * g.a, g.a) * mask;
        }
        case 5: { // vertical gradient across the rect (top → bottom)
            float t = (v.local.y + v.half_size.y) / max(v.half_size.y * 2.0, 1.0);
            float4 g = mix(v.color, v.color2, clamp(t, 0.0, 1.0));
            return float4(g.rgb * g.a, g.a) * mask;
        }
        default:
            return float4(c.rgb * c.a, c.a) * mask;
    }
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Inst {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub color2: [f32; 4],
    pub extra: [f32; 4],
    pub kind: u32,
    pub radius: f32,
    pub pad: [f32; 2],
}

pub type Rgba = [f32; 4];

pub fn rgba(c: zenteru_text::palette::Rgb, a: f32) -> Rgba {
    [c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0, a]
}

#[derive(Clone, Copy, Debug)]
struct GlyphEntry {
    uv: [f32; 4],
    w: f32,
    h: f32,
    left: f32,
    top: f32,
    color: bool,
}

/// Gradient span for glyph tinting: screen x range mapped to color → color2.
#[derive(Clone, Copy, Debug)]
pub struct Grad {
    pub x0: f32,
    pub x1: f32,
    pub to: Rgba,
}

pub struct Frame {
    pub insts: Vec<Inst>,
    pub clear: Rgba,
}

impl Frame {
    pub fn new(clear: Rgba) -> Self {
        Frame { insts: Vec::with_capacity(8192), clear }
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        if w > 0.0 && h > 0.0 && color[3] > 0.0 {
            self.insts.push(Inst { rect: [x, y, w, h], color, ..Default::default() });
        }
    }

    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
        self.insts.push(Inst { rect: [x, y, w, h], color, radius: r, ..Default::default() });
    }

    pub fn gradient_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, from: Rgba, to: Rgba) {
        self.insts.push(Inst { rect: [x, y, w, h], color: from, color2: to, kind: 3, radius: r, ..Default::default() });
    }

    /// Vertical gradient (top colour → bottom colour, alpha included).
    pub fn vgradient_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, top: Rgba, bottom: Rgba) {
        if w > 0.0 && h > 0.0 {
            self.insts.push(Inst { rect: [x, y, w, h], color: top, color2: bottom, kind: 5, radius: r, ..Default::default() });
        }
    }

    /// Rounded border of thickness `t`, coloured along a horizontal gradient.
    pub fn gradient_border(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, t: f32, from: Rgba, to: Rgba) {
        self.insts.push(Inst {
            rect: [x, y, w, h],
            color: from,
            color2: to,
            extra: [x, x + w, t, 0.0],
            kind: 4,
            radius: r.max(0.5),
            ..Default::default()
        });
    }
}

pub struct Renderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    atlas: Retained<ProtocolObject<dyn MTLTexture>>,
    atlas_size: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    glyphs: HashMap<(String, Style, u32, u32), Option<GlyphEntry>>,
    fonts: HashMap<u32, Fonts>,
    pub font_size: f64,
}

impl Renderer {
    pub fn new(font_size: f64) -> Renderer {
        let device = MTLCreateSystemDefaultDevice().expect("no Metal device");
        let queue = device.newCommandQueue().expect("command queue");
        let src = NSString::from_str(SHADER);
        let lib = device.newLibraryWithSource_options_error(&src, None).expect("shader compile");
        let vf = lib.newFunctionWithName(&NSString::from_str("vmain")).unwrap();
        let ff = lib.newFunctionWithName(&NSString::from_str("fmain")).unwrap();
        let desc = MTLRenderPipelineDescriptor::new();
        desc.setVertexFunction(Some(&vf));
        desc.setFragmentFunction(Some(&ff));
        let att = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        att.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        att.setBlendingEnabled(true);
        att.setSourceRGBBlendFactor(MTLBlendFactor::One);
        att.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        att.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        att.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device.newRenderPipelineStateWithDescriptor_error(&desc).expect("pipeline");
        let atlas_size = 4096;
        let atlas = Self::make_atlas(&device, atlas_size);
        Renderer {
            device,
            queue,
            pipeline,
            atlas,
            atlas_size,
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            glyphs: HashMap::new(),
            fonts: HashMap::new(),
            font_size,
        }
    }

    fn make_atlas(device: &ProtocolObject<dyn MTLDevice>, size: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let td = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                size as usize,
                size as usize,
                false,
            )
        };
        td.setUsage(MTLTextureUsage::ShaderRead);
        device.newTextureWithDescriptor(&td).expect("atlas texture")
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    fn key(scale: f64) -> u32 {
        (scale * 100.0).round() as u32
    }

    /// Fonts (and cell metrics) for a backing scale factor.
    pub fn fonts(&mut self, scale: f64) -> &Fonts {
        let size = self.font_size;
        self.fonts.entry(Self::key(scale)).or_insert_with(|| Fonts::new(size, scale))
    }

    pub fn set_font_size(&mut self, pt: f64) {
        self.font_size = pt.clamp(6.0, 72.0);
        self.fonts.clear();
        self.glyphs.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
    }

    fn glyph(&mut self, text: &str, style: Style, cells: u32, scale: f64) -> Option<GlyphEntry> {
        let key = (text.to_string(), style, cells, Self::key(scale));
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let raster = {
            let fonts = self.fonts(scale);
            fonts.rasterize(text, style, cells)
        };
        let entry = raster.and_then(|r| {
            if r.w == 0 || r.h == 0 {
                return None;
            }
            let (x, y) = self.alloc(r.w + 1, r.h + 1)?;
            let region = MTLRegion { origin: MTLOrigin { x: x as usize, y: y as usize, z: 0 }, size: MTLSize { width: r.w as usize, height: r.h as usize, depth: 1 } };
            unsafe {
                self.atlas.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region,
                    0,
                    NonNull::new(r.data.as_ptr() as *mut _).unwrap(),
                    r.w as usize * 4,
                )
            };
            let s = self.atlas_size as f32;
            Some(GlyphEntry {
                uv: [x as f32 / s, y as f32 / s, (x + r.w) as f32 / s, (y + r.h) as f32 / s],
                w: r.w as f32,
                h: r.h as f32,
                left: r.left as f32,
                top: r.top as f32,
                color: r.color,
            })
        });
        self.glyphs.insert(key, entry);
        entry
    }

    /// Shelf packing; when the atlas is full it is cleared and refilled lazily.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.atlas_size || h > self.atlas_size {
            return None;
        }
        if self.shelf_x + w > self.atlas_size {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }
        if self.shelf_y + h > self.atlas_size {
            self.glyphs.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_h = 0;
        }
        let pos = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        self.shelf_h = self.shelf_h.max(h);
        Some(pos)
    }

    /// Queue a glyph whose cell's top-left is at (x, y).
    #[allow(clippy::too_many_arguments)]
    pub fn push_glyph(&mut self, f: &mut Frame, text: &str, style: Style, cells: u32, x: f32, y: f32, color: Rgba, grad: Option<Grad>, scale: f64) {
        let Some(g) = self.glyph(text, style, cells, scale) else { return };
        let (color2, extra) = match grad {
            Some(gr) => (gr.to, [gr.x0, gr.x1, 0.0, 0.0]),
            None => (color, [0.0; 4]),
        };
        f.insts.push(Inst {
            rect: [x + g.left, y + g.top, g.w, g.h],
            uv: g.uv,
            color: if g.color { [1.0, 1.0, 1.0, color[3]] } else { color },
            color2,
            extra: if g.color { [0.0; 4] } else { extra },
            kind: if g.color { 2 } else { 1 },
            ..Default::default()
        });
    }

    /// Draw a string of UI text in cells starting at (x, y). Returns the
    /// number of cells used. Stops before `max_cells`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, f: &mut Frame, s: &str, x: f32, y: f32, color: Rgba, style: Style, max_cells: usize, grad: Option<Grad>, scale: f64) -> usize {
        use unicode_segmentation_shim::graphemes;
        let cw = self.fonts(scale).cell_w;
        let mut col = 0usize;
        for g in graphemes(s) {
            let w = zenteru_text::text::grapheme_width(g, col).max(if g == "\t" { 1 } else { 0 });
            if w == 0 {
                continue;
            }
            if col + w > max_cells {
                break;
            }
            if g != " " && g != "\t" {
                self.push_glyph(f, g, style, w as u32, x + col as f32 * cw, y, color, grad, scale);
            }
            col += w;
        }
        col
    }

    pub fn render(&mut self, layer: &CAMetalLayer, frame: &Frame, size: (f32, f32)) {
        let Some(drawable) = layer.nextDrawable() else { return };
        let texture = drawable.texture();
        let pass = MTLRenderPassDescriptor::new();
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(&texture));
        ca.setLoadAction(MTLLoadAction::Clear);
        ca.setStoreAction(MTLStoreAction::Store);
        let c = frame.clear;
        ca.setClearColor(MTLClearColor { red: c[0] as f64, green: c[1] as f64, blue: c[2] as f64, alpha: c[3] as f64 });
        let Some(cmd) = self.queue.commandBuffer() else { return };
        let Some(enc) = cmd.renderCommandEncoderWithDescriptor(&pass) else { return };
        if !frame.insts.is_empty() {
            enc.setRenderPipelineState(&self.pipeline);
            let bytes = frame.insts.len() * std::mem::size_of::<Inst>();
            let buf = unsafe {
                self.device.newBufferWithBytes_length_options(
                    NonNull::new(frame.insts.as_ptr() as *mut _).unwrap(),
                    bytes,
                    objc2_metal::MTLResourceOptions::StorageModeShared,
                )
            };
            let Some(buf) = buf else {
                enc.endEncoding();
                return;
            };
            let vp: [f32; 2] = [size.0, size.1];
            unsafe {
                enc.setVertexBuffer_offset_atIndex(Some(&buf), 0, 0);
                enc.setVertexBytes_length_atIndex(NonNull::new(vp.as_ptr() as *mut _).unwrap(), 8, 1);
                enc.setFragmentTexture_atIndex(Some(&self.atlas), 0);
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(MTLPrimitiveType::TriangleStrip, 0, 4, frame.insts.len());
            }
        }
        enc.endEncoding();
        let d: &ProtocolObject<dyn objc2_metal::MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        cmd.presentDrawable(d);
        cmd.commit();
    }
}

/// Grapheme iteration without pulling unicode-segmentation into this crate
/// directly (it comes through zenteru-text).
mod unicode_segmentation_shim {
    pub fn graphemes(s: &str) -> Vec<&str> {
        zenteru_text::seg::graphemes(s)
    }
}

pub fn _unused(_: &dyn CAMetalDrawable) {}
