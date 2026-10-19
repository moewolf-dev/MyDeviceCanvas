import Foundation

@main
struct MdcSwiftHello {
    static func main() async {
        let session = try! MdcSession()
        let (width, height) = try! session.surfaceSize()
        // N307: the surface format enum is exposed; only RGB565 is implemented.
        if session.surfaceFormat(at: 0) != .rgb565 || !session.pixelFormatIsImplemented(.rgb565) {
            fputs("unexpected surface pixel format\n", stderr)
            exit(1)
        }
        if session.pixelFormatIsImplemented(.rgba8888) {
            fputs("rgba8888 must not be implemented\n", stderr)
            exit(1)
        }
        // N319: the diagnostics channel is independent of the frame path.
        if session.emitMetric("fps", 29.5, unit: "fps") < 1 {
            fputs("emit metric failed\n", stderr)
            exit(1)
        }
        guard let diagnostic = session.takeDiagnostic(),
              diagnostic.kind == .metric,
              diagnostic.name == "fps",
              diagnostic.value == 29.5,
              diagnostic.unit == "fps" else {
            fputs("metric diagnostic mismatch\n", stderr)
            exit(1)
        }
        if session.diagnosticCount() != 0 {
            fputs("diagnostic queue not drained\n", stderr)
            exit(1)
        }
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

        // N246/N247: config round trip and package readiness.
        if session.setConfig("brightness", "200") != 0 {
            fputs("set config failed\n", stderr)
            exit(1)
        }
        if session.getConfig("brightness") != "200" {
            fputs("get config mismatch\n", stderr)
            exit(1)
        }
        if session.installPackage([1, 2, 3]) != 0 || session.packageReady() != 0 {
            fputs("bad package should not become ready\n", stderr)
            exit(1)
        }

        // Form B mirror over the async stream (simulator board with a GPU).
        let gpu = try! MdcSession(boardId: "linux-virt")
        // N549: the mode is selected explicitly (the board supports dirty-region).
        let mirrorTask = Task {
            for await frame in gpu.mirrorFrames(mode: .dirtyRegion) {
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
        if gpu.mirrorStats().received < 1 {
            fputs("mirror mode did not produce a frame\n", stderr)
            exit(1)
        }
        // N549/N578: the buffered-frame skip counter is readable (parity with C/Node).
        _ = gpu.mirrorSkips()
        // N578: the surface-carrying take matches the C ABI / Node SDK.
        _ = gpu.emitMirrorFrame()
        guard let tagged = gpu.takeMirrorFrameWithSurface() else {
            fputs("takeMirrorFrameWithSurface returned no frame\n", stderr)
            exit(1)
        }
        if tagged.surfaceId != "main" {
            fputs("takeMirrorFrameWithSurface reported \(tagged.surfaceId)\n", stderr)
            exit(1)
        }
        // N618/N638: the tile reconciliation matches the C/Node smoke sequence,
        // including the `bytesSent`/`consistent` fields Node also reports.
        let tiles = gpu.tileReconciliation()
        if tiles.sent != 0 || tiles.received != 0 || tiles.missing != 0 {
            fputs("tile reconciliation not empty for a fresh mirror\n", stderr)
            exit(1)
        }
        if tiles.bytesSent != 0 || !tiles.consistent {
            fputs("tile reconciliation fields are wrong for a fresh mirror\n", stderr)
            exit(1)
        }
        // N568: the negotiated mode is reported (parity with C/Node).
        if gpu.mirrorMode() != "dirty-region" {
            fputs("mirror mode not reported\n", stderr)
            exit(1)
        }
        // N508: the bounded (latest-wins) mirror stream also yields a frame.
        let bounded = try! MdcSession(boardId: "linux-virt")
        let boundedTask = Task {
            for await frame in bounded.mirrorFramesBounded(maxFps: 15) {
                if frame.width == 800 && frame.height == 480 {
                    return true
                }
            }
            return false
        }
        let sawBounded = await boundedTask.value
        boundedTask.cancel()
        if !sawBounded {
            fputs("bounded mirror stream missed a frame\n", stderr)
            exit(1)
        }
        bounded.close()
        gpu.close()

        // N447: the host-side session counters are readable.
        guard let stats = session.stats(), stats.packetsSent >= 1 else {
            fputs("session stats unavailable\n", stderr)
            exit(1)
        }

        // N483: a down+up pair is reported as a tap gesture.
        _ = session.simulateInput(phase: "down", x: 2, y: 3)
        _ = session.simulateInput(phase: "up", x: 2, y: 3)
        var sawGesture = false
        for _ in 0..<16 {
            if let event = session.pollEvent(), event.gestureKind == "tap" {
                sawGesture = true
                break
            }
        }
        if !sawGesture {
            fputs("gesture not reported\n", stderr)
            exit(1)
        }

        // N409: the host can send a pointer event to a touch-capable device.
        if session.sendInput(pointerId: 1, phase: "down", x: 3, y: 4) != 0 {
            fputs("sendInput failed\n", stderr)
            exit(1)
        }

        session.close()
        session.close()
        print("swift ok \(width)x\(height)")
    }
}
