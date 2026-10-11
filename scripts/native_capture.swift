// Developer evidence recorder; not linked into the Rust product.
import Foundation
import CoreGraphics
import ScreenCaptureKit
import AVFoundation
import VideoToolbox
import ImageIO

func hardwareH264Supported() -> Bool {
    var session: VTCompressionSession?
    let spec = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true] as CFDictionary
    let status = VTCompressionSessionCreate(allocator: kCFAllocatorDefault, width: 16, height: 16, codecType: kCMVideoCodecType_H264, encoderSpecification: spec, imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &session)
    if let session { VTCompressionSessionInvalidate(session) }
    return status == noErr
}
struct Failure: Error, CustomStringConvertible { let description: String }
struct CaptureIssue {
    let domain: String
    let code: Int
    let message: String
    let hostTime: Double
    init(_ error: Error) {
        let error = error as NSError
        domain = error.domain; code = error.code; message = error.localizedDescription
        hostTime = ProcessInfo.processInfo.systemUptime
    }
    var metadata: [String: Any] { ["domain": domain, "code": code, "message": message] }
}
func completedCapture(writerCompleted: Bool, accepted: Int, appendError: String?, streamError: CaptureIssue?, stopError: CaptureIssue?) -> Bool {
    writerCompleted && accepted > 0 && appendError == nil && streamError == nil && stopError == nil
}
final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    let queue = DispatchQueue(label: "photocraft.capture.writer")
    let writer: AVAssetWriter
    let input: AVAssetWriterInput
    var first: CMTime?
    var last: CMTime?
    var lastSample: CMSampleBuffer?
    var firstHostTime: Double?
    var heldEndFrame = false
    var accepted = 0
    var dropped = 0
    var error: String?
    var streamError: CaptureIssue?
    var acceptingSamples = true
    var statuses: [Int: Int] = [:]
    var screenCallbacks = 0
    var invalidFrames = 0
    init(url: URL, width: Int, height: Int, fps: Int) throws {
        writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        let settings: [String: Any] = [AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoEncoderSpecificationKey: [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true],
            AVVideoCompressionPropertiesKey: [AVVideoAllowFrameReorderingKey: false, AVVideoProfileLevelKey: AVVideoProfileLevelH264BaselineAutoLevel, AVVideoAverageBitRateKey: min(30_000_000, width * height * fps / 5), AVVideoExpectedSourceFrameRateKey: fps, AVVideoMaxKeyFrameIntervalKey: fps * 2]]
        input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw Failure(description: "Hardware H264 writer unavailable") }
        writer.add(input)
        guard writer.startWriting() else { throw Failure(description: writer.error?.localizedDescription ?? "Writer failed") }
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        let issue = CaptureIssue(error)
        queue.async { if self.streamError == nil { self.streamError = issue } }
    }
    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, acceptingSamples else { return }
        screenCallbacks += 1
        guard sample.isValid, CMSampleBufferGetImageBuffer(sample) != nil else { invalidFrames += 1; return }
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int else { invalidFrames += 1; return }
        statuses[raw, default: 0] += 1
        guard SCFrameStatus(rawValue: raw) == .complete else { return }
        let time = CMSampleBufferGetPresentationTimeStamp(sample)
        if first == nil { first = time; firstHostTime = ProcessInfo.processInfo.systemUptime; writer.startSession(atSourceTime: time) }
        guard input.isReadyForMoreMediaData else { dropped += 1; return }
        if input.append(sample) { accepted += 1; last = time; lastSample = sample }
        else { error = writer.error.map { String(describing: $0) } ?? "Append failed" }
    }
}
@main struct Capture {
    static func main() async {
        do { try await run() } catch { fputs("native-capture: \(error)\n", stderr); exit(1) }
    }
    static func run() async throws {
        var args = Array(CommandLine.arguments.dropFirst())
        let browserCapture = args.first == "--browser" || args == ["--list-browser"]
        if args.first == "--browser" { args.removeFirst() }
        if args == ["--self-test"] {
            let stopped = CaptureIssue(NSError(domain: "SCStreamErrorDomain", code: -3808, userInfo: [NSLocalizedDescriptionKey: "already stopped"]))
            guard completedCapture(writerCompleted: true, accepted: 12, appendError: nil, streamError: nil, stopError: nil),
                  !completedCapture(writerCompleted: true, accepted: 12, appendError: nil, streamError: stopped, stopError: nil),
                  !completedCapture(writerCompleted: true, accepted: 12, appendError: nil, streamError: nil, stopError: stopped),
                  !completedCapture(writerCompleted: true, accepted: 0, appendError: nil, streamError: nil, stopError: nil),
                  !completedCapture(writerCompleted: false, accepted: 12, appendError: nil, streamError: nil, stopError: nil) else {
                throw Failure(description: "Capture completion classification regression")
            }
            print("capture shutdown policy: 5 cases passed")
            return
        }
        if args == ["--preflight"] {
            print("{\"screenCaptureAllowed\":\(CGPreflightScreenCaptureAccess()),\"hardwareH264Supported\":\(hardwareH264Supported())}")
            return
        }
        if args.count == 3 && args[0] == "--inspect" {
            let asset = AVURLAsset(url: URL(fileURLWithPath: args[1]))
            let duration = try await asset.load(.duration)
            let generator = AVAssetImageGenerator(asset: asset)
            generator.appliesPreferredTrackTransform = true
            generator.requestedTimeToleranceBefore = .zero
            generator.requestedTimeToleranceAfter = .zero
            print("videoDurationSeconds=\(CMTimeGetSeconds(duration))")
            let dir = URL(fileURLWithPath: args[2], isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            for (index, fraction) in [0.05, 0.5, 0.95].enumerated() {
                let time = CMTime(seconds: CMTimeGetSeconds(duration) * fraction, preferredTimescale: 600)
                let (image, actual) = try await generator.image(at: time)
                let url = dir.appendingPathComponent("frame-\(index).png")
                guard let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else { throw Failure(description: "Cannot create review frame") }
                CGImageDestinationAddImage(destination, image, nil)
                guard CGImageDestinationFinalize(destination) else { throw Failure(description: "Cannot write review frame") }
                print("\(url.path) actualSeconds=\(CMTimeGetSeconds(actual)) size=\(image.width)x\(image.height)")
            }
            return
        }
        guard CGPreflightScreenCaptureAccess() else { throw Failure(description: "Screen Recording permission is absent. Enable this terminal/helper in System Settings > Privacy & Security > Screen & System Audio Recording; this helper does not request permission.") }
        let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
        if args == ["--displays"] {
            for display in content.displays { print("id=\(display.displayID) frame=\(display.frame) pixels=\(display.width)x\(display.height)") }; return
        }
        let allowed = content.windows.filter {
            $0.owningApplication?.applicationName.lowercased().contains("photocraft") == true ||
            (browserCapture && $0.owningApplication?.bundleIdentifier == "com.google.Chrome" && $0.title?.contains("PhotoCraft") == true)
        }
        if args == ["--list"] || args == ["--list-browser"] {
            let rows = allowed.map { ["id": $0.windowID, "pid": $0.owningApplication?.processID ?? 0, "title": $0.title ?? "", "x": $0.frame.minX, "y": $0.frame.minY, "width": $0.frame.width, "height": $0.frame.height] as [String: Any] }
            print(String(decoding: try JSONSerialization.data(withJSONObject: rows, options: [.prettyPrinted, .sortedKeys]), as: UTF8.self)); return
        }
        guard args.count == 4, let seconds = Double(args[2]), seconds.isFinite, (0.1...600).contains(seconds), let fps = Int(args[3]), (1...60).contains(fps) else {
            throw Failure(description: "Usage: native_capture.sh --preflight | --list | --list-browser | [--browser] OUTPUT.mp4 WINDOW_ID[,WINDOW_ID...] SECONDS FPS")
        }
        let tokens = args[1].split(separator: ",", omittingEmptySubsequences: false)
        let ids = tokens.compactMap { UInt32($0) }
        guard !ids.isEmpty, ids.count == tokens.count, ids.count <= 4, ids.count == Set(ids).count else { throw Failure(description: "Choose 1–4 unique PhotoCraft windows (browser windows require --browser)") }
        let windows = ids.compactMap { id in allowed.first { $0.windowID == id } }
        guard windows.count == ids.count else { throw Failure(description: "Unknown window or window is not PhotoCraft; capture refused") }
        var rect = windows[0].frame
        for w in windows.dropFirst() { rect = rect.union(w.frame) }
        guard !content.displays.isEmpty else { throw Failure(description: "No capturable displays; display may be asleep or session locked. Wake normally; no lock bypass is attempted.") }
        guard let display = content.displays.first(where: { $0.frame.contains(rect) }) else { throw Failure(description: "Arrange selected windows on one display, fully inside it") }
        guard hardwareH264Supported() else { throw Failure(description: "Hardware H264 unsupported; software fallback refused") }
        let width = Int(ceil(rect.width / 2)) * 2, height = Int(ceil(rect.height / 2)) * 2
        guard width > 0, height > 0, width <= 7680, height <= 4320 else { throw Failure(description: "Invalid/oversize capture bounds") }
        let url = URL(fileURLWithPath: args[0]).standardizedFileURL
        guard !FileManager.default.fileExists(atPath: url.path) else { throw Failure(description: "Output already exists; refusing overwrite") }
        let recorder = try Recorder(url: url, width: width, height: height, fps: fps)
        // Only these windows are included; other windows, desktop and menu bar are excluded.
        let filter = SCContentFilter(display: display, including: windows)
        let config = SCStreamConfiguration()
        config.width = width; config.height = height
        config.sourceRect = rect.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
        config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
        config.queueDepth = 8; config.showsCursor = false; config.capturesAudio = false
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        let background = CGColor(gray: 0, alpha: 1)
        config.backgroundColor = background
        let queue = recorder.queue
        let stream = SCStream(filter: filter, configuration: config, delegate: recorder)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: queue)
        let started = Date()
        try await stream.startCapture()
        try await Task.sleep(for: .seconds(seconds))
        var stopError: CaptureIssue?
        do { try await stream.stopCapture() }
        catch { stopError = CaptureIssue(error) }
        withExtendedLifetime(background) {}
        queue.sync {
            recorder.acceptingSamples = false
            // SCK may emit no callbacks during a static period. Retain that last live image
            // to the real stop time; explicitly report this endpoint duplicate separately.
            if stopError == nil, recorder.streamError == nil, recorder.error == nil,
               let sample = recorder.lastSample, let first = recorder.first, let host = recorder.firstHostTime,
               recorder.input.isReadyForMoreMediaData {
                let elapsed = ProcessInfo.processInfo.systemUptime - host
                var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: Int32(fps)), presentationTimeStamp: CMTimeMaximum(first + CMTime(seconds: elapsed, preferredTimescale: 1_000_000), (recorder.last ?? first) + CMTime(value: 1, timescale: Int32(fps))), decodeTimeStamp: .invalid)
                var copy: CMSampleBuffer?
                if CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: sample, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &copy) == noErr,
                   let copy, recorder.input.append(copy) { recorder.heldEndFrame = true }
            }
            recorder.lastSample = nil
            if recorder.accepted > 0 && recorder.writer.status == .writing { recorder.input.markAsFinished() }
        }
        if recorder.accepted > 0 && recorder.writer.status == .writing {
            await recorder.writer.finishWriting()
        } else if recorder.writer.status == .writing {
            recorder.writer.cancelWriting()
        }
        // Drain delegate diagnostics even after stopCapture throws. An already-stopped
        // stream still has valid accepted frames that must be finalized and labelled partial.
        let snapshot = queue.sync { (recorder.streamError, recorder.error) }
        let writerCompleted = recorder.writer.status == .completed
        let elapsed = Date().timeIntervalSince(started)
        let duration = recorder.first.flatMap { first in recorder.last.map { CMTimeGetSeconds($0 - first) } } ?? 0
        let assetDuration = writerCompleted ? try? await AVURLAsset(url: url).load(.duration) : nil
        let videoSeconds = assetDuration.map(CMTimeGetSeconds) ?? 0
        let validVideo = writerCompleted && videoSeconds.isFinite && videoSeconds > 0
        let complete = validVideo && completedCapture(writerCompleted: writerCompleted, accepted: recorder.accepted, appendError: snapshot.1, streamError: snapshot.0, stopError: stopError)
        var metadata: [String: Any] = ["source": browserCapture ? "live PhotoCraft browser/native windows" : "live native PhotoCraft windows", "windowIDs": ids, "width": width, "height": height, "requestedFPS": fps, "requestedSeconds": seconds, "acceptedFrames": recorder.accepted, "writerBackpressureDrops": recorder.dropped, "videoDurationSeconds": videoSeconds.isFinite ? videoSeconds : 0, "sampleDurationSeconds": duration, "wallSeconds": elapsed, "startedUTC": ISO8601DateFormatter().string(from: started), "encoder": "H264; hardware required", "audio": false, "queueDepth": 8, "displaySleepPrevented": ProcessInfo.processInfo.environment["PHOTOCRAFT_CAPTURE_KEEP_AWAKE"] == "1", "heldEndFrame": recorder.heldEndFrame, "screenCallbacks": recorder.screenCallbacks, "invalidFrames": recorder.invalidFrames, "frameStatuses": Dictionary(uniqueKeysWithValues: recorder.statuses.map { (String($0.key), $0.value) }), "captureComplete": complete, "outcome": complete ? "complete" : (validVideo && recorder.accepted > 0 ? "partial" : "failed"), "writerCompleted": writerCompleted, "validVideo": validVideo]
        if let issue = snapshot.0 {
            metadata["didStopWithError"] = issue.metadata
            metadata["streamStoppedAfterSeconds"] = max(0, issue.hostTime - (recorder.firstHostTime ?? issue.hostTime))
        }
        if let issue = stopError { metadata["stopCaptureError"] = issue.metadata }
        if let error = snapshot.1 { metadata["appendError"] = error }
        if let error = recorder.writer.error { metadata["writerError"] = String(describing: error) }
        // Never leave a failed writer's unfinalized MP4 looking like usable evidence.
        if !validVideo { try? FileManager.default.removeItem(at: url) }
        let data = try JSONSerialization.data(withJSONObject: metadata, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: URL(fileURLWithPath: url.path + ".json"))
        print(String(decoding: data, as: UTF8.self))
        if !complete {
            let details = [snapshot.0.map { "didStopWithError: \($0.domain) \($0.code): \($0.message)" }, stopError.map { "stopCapture: \($0.domain) \($0.code): \($0.message)" }, snapshot.1, recorder.writer.error.map { String(describing: $0) }].compactMap { $0 }.joined(separator: "; ")
            throw Failure(description: "\(validVideo ? "Partial video finalized" : "Failed video removed") at \(url.path); see sidecar. \(details)")
        }
    }
}
