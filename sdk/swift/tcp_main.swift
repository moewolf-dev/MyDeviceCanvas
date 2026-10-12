import Foundation

/// Handshake a TCP simulator peer, observe the connected event, then send one frame.
@main
struct MdcSwiftTcpHello {
    static func main() async {
        guard let address = ProcessInfo.processInfo.environment["MDC_TCP_ADDR"], !address.isEmpty else {
            fputs("MDC_TCP_ADDR is required\n", stderr)
            exit(1)
        }
        let session = try! MdcSession(tcp: address)
        let stream = session.events()
        let task = Task {
            var saw = false
            for await event in stream {
                if event.kind == 3 {
                    saw = true
                    break
                }
            }
            return saw
        }
        let saw = await task.value
        task.cancel()
        if !saw {
            fputs("async stream missed connected\n", stderr)
            exit(1)
        }
        let (width, height) = try! session.surfaceSize()
        let pixels = [UInt8](repeating: 0x11, count: Int(width) * Int(height) * 2)
        try! session.sendFrame(pixels)
        session.close()
        print("swift tcp ok \(width)x\(height)")
    }
}
