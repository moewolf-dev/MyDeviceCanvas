import Foundation

let session = try MdcSession()
let (width, height) = try session.surfaceSize()
let scene = session.sendScene()
if scene != -5 {
    fputs("expected scene unsupported -5, got \(scene)\n", stderr)
    exit(1)
}
let pixels = [UInt8](repeating: 0x11, count: Int(width) * Int(height) * 2)
try session.sendFrame(pixels)
session.close()

let again = try MdcSession()
again.close()
print("swift ok \(width)x\(height)")
