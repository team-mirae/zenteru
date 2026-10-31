// Draws the zenteru logo: the zencode gradient (pink → violet) running 30°
// clockwise from rightwards, with ゼ as the type logo.
//
//   swift scripts/make_icon.swift      → resources/zenteru.icns, docs/logo.png
//
// The icon follows the macOS app icon grid: an 824-pt rounded square on a
// 1024-pt canvas.

import AppKit
import CoreText

let from = (r: 255.0, g: 121.0, b: 176.0)   // palette::GRADIENT_FROM
let to = (r: 130.0, g: 120.0, b: 255.0)     // palette::GRADIENT_TO
func color(_ c: (r: Double, g: Double, b: Double)) -> CGColor {
    CGColor(srgbRed: c.r / 255, green: c.g / 255, blue: c.b / 255, alpha: 1)
}

/// Render at `px` × `px`. `squircle` = macOS icon shape with margins;
/// otherwise the gradient fills the whole (square) canvas.
func render(px: Int, squircle: Bool) -> CGImage {
    let s = CGFloat(px)
    let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
                        space: CGColorSpace(name: CGColorSpace.sRGB)!,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let unit = s / 1024
    let rect = squircle ? CGRect(x: 100 * unit, y: 100 * unit, width: 824 * unit, height: 824 * unit)
                        : CGRect(x: 0, y: 0, width: s, height: s)
    if squircle {
        // Soft drop shadow like system icons.
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -10 * unit), blur: 28 * unit,
                      color: CGColor(gray: 0, alpha: 0.35))
        ctx.addPath(CGPath(roundedRect: rect, cornerWidth: 185 * unit, cornerHeight: 185 * unit, transform: nil))
        ctx.setFillColor(color(to))
        ctx.fillPath()
        ctx.restoreGState()
        ctx.addPath(CGPath(roundedRect: rect, cornerWidth: 185 * unit, cornerHeight: 185 * unit, transform: nil))
        ctx.clip()
    }
    // 30° clockwise from rightwards: right and slightly down on screen.
    // CoreGraphics' y axis points up, so "down" is negative y.
    let angle = -30.0 * Double.pi / 180
    let dir = CGPoint(x: cos(angle), y: sin(angle))
    let c = CGPoint(x: rect.midX, y: rect.midY)
    // Project the rect's corners onto the direction so the gradient spans it exactly.
    let half = abs(dir.x) * rect.width / 2 + abs(dir.y) * rect.height / 2
    let start = CGPoint(x: c.x - dir.x * half, y: c.y - dir.y * half)
    let end = CGPoint(x: c.x + dir.x * half, y: c.y + dir.y * half)
    let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
                              colors: [color(from), color(to)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: start, end: end, options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])

    // ゼ, heavy Hiragino, optically centred.
    let fontSize = rect.width * 0.62
    let font = CTFontCreateWithName("HiraginoSans-W8" as CFString, fontSize, nil)
    let attrs: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 1, alpha: 1),
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: "ゼ", attributes: attrs))
    let bounds = CTLineGetBoundsWithOptions(line, .useGlyphPathBounds)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -6 * unit), blur: 18 * unit,
                  color: CGColor(srgbRed: 0.09, green: 0.07, blue: 0.13, alpha: 0.28))
    ctx.textPosition = CGPoint(x: rect.midX - bounds.midX, y: rect.midY - bounds.midY)
    CTLineDraw(line, ctx)
    ctx.restoreGState()
    return ctx.makeImage()!
}

func writePNG(_ img: CGImage, _ path: String) {
    let rep = NSBitmapImageRep(cgImage: img)
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
}

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().path
let iconset = NSTemporaryDirectory() + "zenteru.iconset"
try? FileManager.default.removeItem(atPath: iconset)
try! FileManager.default.createDirectory(atPath: iconset, withIntermediateDirectories: true)
for pt in [16, 32, 128, 256, 512] {
    writePNG(render(px: pt, squircle: true), "\(iconset)/icon_\(pt)x\(pt).png")
    writePNG(render(px: pt * 2, squircle: true), "\(iconset)/icon_\(pt)x\(pt)@2x.png")
}
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", iconset, "-o", "\(root)/resources/zenteru.icns"]
try! p.run()
p.waitUntilExit()
writePNG(render(px: 512, squircle: true), "\(root)/docs/logo.png")
writePNG(render(px: 1024, squircle: false), "\(root)/docs/logo-square.png")
print("wrote resources/zenteru.icns, docs/logo.png, docs/logo-square.png")
