import Foundation

@main
struct MdcSwiftHello {
    static func main() async {
        let session = try! MdcSession()
        let (width, height) = try! session.surfaceSize()
        if session.sendScene() != -5 {
            fputs("expected scene unsupported\n", stderr)
            exit(1)
        }
        let pixels = [UInt8](repeating: 0x11, count: Int(width) * Int(height) * 2)
        try! session.sendFrame(pixels)

        let stream = session.events()
        let task = Task {
            var saw = false
            for await event in stream {
                if event.kind == 1 && event.phase == "down" && event.x == 3 && event.y == 4 {
                    saw = true
                    break
                }
            }
            return saw
        }
        if session.simulateInput(phase: "down", x: 3, y: 4) != 0 {
            fputs("simulate input failed\n", stderr)
            exit(1)
        }
        let saw = await task.value
        task.cancel()
        if !saw {
            fputs("async stream missed input\n", stderr)
            exit(1)
        }
        session.close()
        session.close()
        print("swift ok \(width)x\(height)")
    }
}
