// Draws the macOS app's placeholder art until real art exists (`scripts/macos.sh icon`):
//   1. an `AppIcon.iconset` (every size iconutil wants) at the first argument's path;
//   2. the status item's template glyph, a vector PDF, at the second argument's path.
// The glyph is a rack of three slots, each with a status light cut out: the machine resources
// hostler hands out. Plain CoreGraphics and ImageIO, so it runs headless.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    FileHandle.standardError.write(Data("usage: macos-icon.swift <out.iconset> <out.pdf>\n".utf8))
    exit(2)
}
let iconsetURL = URL(fileURLWithPath: arguments[1])
let pdfURL = URL(fileURLWithPath: arguments[2])

/// The rack in an 18 × 18 box, origin bottom-left: three slots with a light knocked out of each
/// (even-odd fill).
func rackPath(in rect: CGRect) -> CGPath {
    let scale = rect.width / 18
    let path = CGMutablePath()
    for row in 0..<3 {
        let y = rect.minY + (2 + CGFloat(row) * 5) * scale
        let slot = CGRect(x: rect.minX + 2 * scale, y: y, width: 14 * scale, height: 4 * scale)
        path.addRoundedRect(in: slot, cornerWidth: 1.2 * scale, cornerHeight: 1.2 * scale)
        let light = CGRect(x: rect.minX + 3.6 * scale, y: y + 1.1 * scale,
                           width: 1.8 * scale, height: 1.8 * scale)
        path.addEllipse(in: light)
    }
    return path
}

// MARK: - The status item template (vector PDF, 18 × 18 pt)

do {
    var box = CGRect(x: 0, y: 0, width: 18, height: 18)
    guard let context = CGContext(pdfURL as CFURL, mediaBox: &box, nil) else {
        fatalError("cannot create \(pdfURL.path)")
    }
    context.beginPDFPage(nil)
    context.setFillColor(CGColor(gray: 0, alpha: 1))
    context.addPath(rackPath(in: box))
    context.fillPath(using: .evenOdd)
    context.endPDFPage()
    context.closePDF()
}

// MARK: - The app icon (PNG per size)

func drawIcon(pixels: Int) -> CGImage {
    let size = CGFloat(pixels)
    guard let context = CGContext(
        data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { fatalError("cannot create a bitmap context") }
    // macOS's icon grid: an 824/1024 rounded square, centred.
    let inset = size * 100 / 1024
    let tile = CGRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
    let radius = tile.width * 0.225
    context.addPath(CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius,
                           transform: nil))
    context.clip()
    let colors = [CGColor(red: 0.10, green: 0.42, blue: 0.47, alpha: 1),
                  CGColor(red: 0.05, green: 0.25, blue: 0.31, alpha: 1)] as CFArray
    let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
                              colors: colors, locations: [0, 1])!
    context.drawLinearGradient(gradient, start: CGPoint(x: 0, y: tile.maxY),
                               end: CGPoint(x: 0, y: tile.minY), options: [])
    let glyph = tile.insetBy(dx: tile.width * 0.14, dy: tile.width * 0.14)
    context.setFillColor(CGColor(gray: 1, alpha: 1))
    context.addPath(rackPath(in: glyph))
    context.fillPath(using: .evenOdd)
    return context.makeImage()!
}

try FileManager.default.createDirectory(at: iconsetURL, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        let url = iconsetURL.appendingPathComponent(name)
        guard let destination = CGImageDestinationCreateWithURL(
            url as CFURL, UTType.png.identifier as CFString, 1, nil)
        else { fatalError("cannot write \(url.path)") }
        CGImageDestinationAddImage(destination, drawIcon(pixels: points * scale), nil)
        guard CGImageDestinationFinalize(destination) else { fatalError("cannot write \(url.path)") }
    }
}
