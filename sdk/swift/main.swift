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

        // N63: structured scene set/clear.
        if session.setScene(json: #"{"name":"root","kind":"rect"}"#) != 0 {
            fputs("scene set failed\n", stderr)
            exit(1)
        }
        if session.clearScene() != 0 {
            fputs("scene clear failed\n", stderr)
            exit(1)
        }

        // Form B mirror over the async stream (simulator board with a GPU).
        let gpu = try! MdcSession(boardId: "linux-virt")
        let mirrorTask = Task {
            for await frame in gpu.mirrorFrames() {
                if frame.width == 800 && frame.height == 480 && !frame.pixels.isEmpty {
                    return true
                }
            }
            return false
        }
        let sawMirror = await mirrorTask.value
        mirrorTask.cancel()
        if !sawMirror {
            fputs("async stream missed mirror frame\n", stderr)
            exit(1)
        }
        gpu.close()

        session.close()
        session.close()
        print("swift ok \(width)x\(height)")
    }
}
