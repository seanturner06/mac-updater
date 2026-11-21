// Renders film.html frame by frame in WebKit (the same engine as the app) and writes PNGs.
//   render <url> <out-dir> <fps> [start-seconds] [end-seconds]
// Each frame: __seek(t) moves every animation and app cue to film time t, then WebKit snapshots
// the page. Deterministic, so a re-render gives the same video.
import AppKit
import WebKit

// WebKit's own takeSnapshot re-renders the page and gets iframes under fading layers wrong (grey
// patches, stale frames), so frames are grabbed from the window's real on-screen pixels.
// CGWindowListCreateImage is compiled out of the macOS 15+ SDK but still in CoreGraphics, and a
// process may capture its own window without Screen Recording permission.
typealias CaptureFn = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
let captureWindowImage: CaptureFn = {
    let cg = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW)
    guard let sym = dlsym(cg, "CGWindowListCreateImage") else { fatalError("no CGWindowListCreateImage") }
    return unsafeBitCast(sym, to: CaptureFn.self)
}()

let args = CommandLine.arguments
let url = URL(string: args[1])!
let outDir = args[2]
let fps = Double(args[3])!
let startAt = args.count > 4 ? Double(args[4])! : 0
let endAt = args.count > 5 ? Double(args[5]) : nil

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
// The stage is 1920×1080 CSS px, scaled to half in a 960×540pt window; on a Retina screen's 2×
// scale that's 1920×1080 pixels per frame. Scaled with a CSS transform, not pageZoom: zoom counts
// toward WebKit's minimum font size, which raised every app font under 18px to 18px.
let window = NSWindow(contentRect: NSRect(x: 40, y: 40, width: 960, height: 540),
                      styleMask: [.borderless], backing: .buffered, defer: false)
let config = WKWebViewConfiguration()
config.userContentController.addUserScript(WKUserScript(
    source: """
    document.head.insertAdjacentHTML("beforeend",
      "<style>#stage { transform: scale(0.5); transform-origin: 0 0; }</style>");
    """,
    injectionTime: .atDocumentEnd, forMainFrameOnly: true))
let web = WKWebView(frame: window.contentView!.bounds, configuration: config)
window.contentView!.addSubview(web)
window.orderFrontRegardless()
web.load(URLRequest(url: url))

if window.backingScaleFactor != 2 {
    FileHandle.standardError.write("note: not a Retina screen, frames will be \(Int(960 * window.backingScaleFactor))px wide\n".data(using: .utf8)!)
}

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

func frame(_ i: Int, _ last: Int) {
    if i > last { print("done"); exit(0) }
    let t = Double(i) / fps
    web.callAsyncJavaScript("return await window.__seek(t)", arguments: ["t": t], in: nil, in: .page) { r in
        if case .failure(let e) = r { fail("seek \(t): \(e)") }
        if let probe = ProcessInfo.processInfo.environment["PROBE_JS"] {
            web.evaluateJavaScript(probe) { v, _ in print(String(format: "t=%.2f ", t), v ?? "nil") }
        }
        // Two animation frames, then a moment for the window server, so the capture is this frame.
        web.callAsyncJavaScript(
            "await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))",
            arguments: [:], in: nil, in: .page
        ) { _ in
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.03) {
                // kCGWindowListOptionIncludingWindow, kCGWindowImageBoundsIgnoreFraming | NominalResolution off
                guard let img = captureWindowImage(.null, 1 << 3, UInt32(window.windowNumber), 1 << 0)?
                    .takeRetainedValue() else { fail("capture \(i) failed") }
                let png = NSBitmapImageRep(cgImage: img).representation(using: .png, properties: [:])
                try? png?.write(to: URL(fileURLWithPath: String(format: "%@/f%05d.png", outDir, i)))
                if i % Int(fps) == 0 { print(String(format: "%.0fs", t)); fflush(stdout) }
                frame(i + 1, last)
            }
        }
    }
}

// Wait for the film's script (up to 30s), then for its app frames.
func whenLoaded(_ tries: Int = 0, _ then: @escaping () -> Void) {
    web.evaluateJavaScript("typeof window.__ready") { v, _ in
        if (v as? String) == "function" { then(); return }
        if tries > 300 { fail("film.html never loaded (is the demo server running on :1499?)") }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { whenLoaded(tries + 1, then) }
    }
}
whenLoaded {
    web.callAsyncJavaScript("return await window.__ready()", arguments: [:], in: nil, in: .page) { r in
        guard case .success(let v) = r, let duration = v as? Double else { fail("not ready: \(r)") }
        let first = Int((startAt * fps).rounded())
        let last = Int(((endAt ?? duration) * fps).rounded()) - 1
        // Run the cues before the first frame in order, so a partial render starts in the right state.
        var t = 0.0
        func catchUp() {
            if t >= startAt { frame(first, last); return }
            web.callAsyncJavaScript("return await window.__seek(t)", arguments: ["t": t], in: nil, in: .page) { _ in
                t += 0.25
                catchUp()
            }
        }
        catchUp()
    }
}
app.run()
