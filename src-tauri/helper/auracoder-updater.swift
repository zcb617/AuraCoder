import AppKit
import Foundation

/// 独立的 macOS 更新器。
final class AuraCoderUpdaterDelegate: NSObject, NSApplicationDelegate {
    /// 更新任务配置。
    private let job: UpdateJob
    /// 从目标应用路径解析出的安全应用名称，供更新器界面和备份文件名复用。
    private let applicationName: String
    /// 更新窗口。
    private var window: NSWindow?
    /// 更新阶段标题。
    private var statusLabel: NSTextField?
    /// 更新阶段详情。
    private var detailLabel: NSTextField?
    /// 更新进度条。
    private var progressIndicator: NSProgressIndicator?
    /// 更新百分比。
    private var percentageLabel: NSTextField?
    /// 失败后重开按钮。
    private var reopenButton: NSButton?
    /// 串行日志队列。
    private let logQueue = DispatchQueue(label: "com.auracoder.updater.log")

    /// 创建并显示更新窗口。
    func applicationDidFinishLaunching(_ notification: Notification) {
        createWindow()
        window?.makeKeyAndOrderFront(nil)
        do {
            try writeAtomically(Data("ready\n".utf8), to: URL(fileURLWithPath: job.readyPath))
            appendLog("ready 文件已创建")
        } catch {
            appendLog("创建 ready 文件失败: \(error.localizedDescription)")
            showError("无法启动更新器", detail: error.localizedDescription)
            return
        }
        DispatchQueue(label: "com.auracoder.updater.worker", qos: .userInitiated).async { [weak self] in
            self?.runJob()
        }
    }

    /// 配置窗口。
    private func createWindow() {
        let contentRect = NSRect(x: 0, y: 0, width: 520, height: 235)
        let createdWindow = NSWindow(contentRect: contentRect, styleMask: [.titled, .closable], backing: .buffered, defer: false)
        createdWindow.title = "正在更新 \(applicationName)"
        createdWindow.center()
        let contentView = NSView(frame: NSRect(origin: .zero, size: contentRect.size))
        let title = NSTextField(labelWithString: "正在更新 \(applicationName)")
        title.font = NSFont.systemFont(ofSize: 20, weight: .semibold)
        title.translatesAutoresizingMaskIntoConstraints = false
        let detail = NSTextField(labelWithString: "正在准备更新…")
        detail.textColor = .secondaryLabelColor
        detail.translatesAutoresizingMaskIntoConstraints = false
        let progress = NSProgressIndicator()
        progress.isIndeterminate = false
        progress.minValue = 0
        progress.maxValue = 100
        progress.style = .bar
        progress.translatesAutoresizingMaskIntoConstraints = false
        let percentage = NSTextField(labelWithString: "0%")
        percentage.alignment = .right
        percentage.translatesAutoresizingMaskIntoConstraints = false
        let reopen = NSButton(title: "重新打开 \(applicationName)", target: self, action: #selector(reopenAuraCoder))
        reopen.isHidden = true
        reopen.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(title)
        contentView.addSubview(detail)
        contentView.addSubview(progress)
        contentView.addSubview(percentage)
        contentView.addSubview(reopen)
        NSLayoutConstraint.activate([
            title.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 28),
            title.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -28),
            title.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 28),
            detail.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            detail.trailingAnchor.constraint(equalTo: title.trailingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 12),
            progress.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            progress.trailingAnchor.constraint(equalTo: percentage.leadingAnchor, constant: -12),
            progress.topAnchor.constraint(equalTo: detail.bottomAnchor, constant: 25),
            percentage.trailingAnchor.constraint(equalTo: title.trailingAnchor),
            percentage.centerYAnchor.constraint(equalTo: progress.centerYAnchor),
            percentage.widthAnchor.constraint(equalToConstant: 48),
            reopen.trailingAnchor.constraint(equalTo: title.trailingAnchor),
            reopen.topAnchor.constraint(equalTo: progress.bottomAnchor, constant: 25),
        ])
        window = createdWindow
        createdWindow.contentView = contentView
        statusLabel = title
        detailLabel = detail
        progressIndicator = progress
        percentageLabel = percentage
        reopenButton = reopen
        createdWindow.standardWindowButton(.closeButton)?.isEnabled = false
    }

    /// 根据任务模式执行正常重启或完整安装流程。
    private func runJob() {
        appendLog("开始执行 mode=\(job.mode.rawValue), oldProcessId=\(job.oldProcessId)")
        do {
            try terminateOldProcess()
            switch job.mode {
            case .relaunch:
                try writeCompletionMarker()
                try launchTargetApplication()
            case .install:
                try installApplication()
            }
        } catch {
            appendLog("更新失败: \(error.localizedDescription)")
            showError("更新失败", detail: error.localizedDescription)
        }
    }

    /// 正常请求旧 AuraCoder 退出并等待其 PID 在 60 秒内结束。
    private func terminateOldProcess() throws {
        updateUI(status: "正在关闭 \(applicationName)", detail: "正在等待旧版本正常退出…", progress: 3)
        guard let application = NSRunningApplication(processIdentifier: job.oldProcessId) else {
            appendLog("旧进程 PID=\(job.oldProcessId) 已结束")
            updateUI(status: "正在关闭 \(applicationName)", detail: "旧版本已关闭", progress: 10)
            return
        }
        appendLog("请求正常关闭旧进程 PID=\(job.oldProcessId)")
        if !application.isTerminated && !application.terminate() && !application.isTerminated {
            throw UpdaterError.terminateFailed(job.oldProcessId)
        }
        let deadline = Date().addingTimeInterval(60)
        while !application.isTerminated && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.2)
        }
        guard application.isTerminated else {
            throw UpdaterError.terminateTimedOut(job.oldProcessId)
        }
        appendLog("旧进程 PID=\(job.oldProcessId) 已结束")
        updateUI(status: "正在关闭 \(applicationName)", detail: "旧版本已关闭", progress: 10)
    }

    /// 使用系统 tar 命令按真实条目输出进度解压并安装新版应用。
    private func installApplication() throws {
        guard let archivePath = job.archivePath else {
            throw UpdaterError.invalidJob("install 模式缺少 archivePath")
        }
        let archiveURL = URL(fileURLWithPath: archivePath)
        let targetURL = URL(fileURLWithPath: job.targetAppPath)
        guard FileManager.default.fileExists(atPath: archiveURL.path) else {
            throw UpdaterError.fileNotFound(archiveURL.path)
        }
        guard FileManager.default.fileExists(atPath: targetURL.path) else {
            throw UpdaterError.fileNotFound(targetURL.path)
        }
        updateUI(status: "正在准备更新", detail: "正在读取更新包…", progress: 10)
        let listing = try runProcess(executable: "/usr/bin/tar", arguments: ["-tzf", archiveURL.path], logName: "tar -tzf")
        let entryCount = listing.stdout.split(whereSeparator: { $0 == "\n" || $0 == "\r" })
            .filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
            .count
        guard entryCount > 0 else {
            throw UpdaterError.invalidArchive("更新包没有可解压条目")
        }
        let replacementDirectory = try FileManager.default.url(
            for: .itemReplacementDirectory,
            in: .userDomainMask,
            appropriateFor: targetURL,
            create: true
        )
        appendLog("创建同卷替换目录: \(replacementDirectory.path)")
        let extractedCount = try extractArchive(archiveURL: archiveURL, destinationURL: replacementDirectory, entryCount: entryCount)
        guard extractedCount > 0 else {
            throw UpdaterError.invalidArchive("更新包解压结果为空")
        }
        let appURLs = try FileManager.default.contentsOfDirectory(
            at: replacementDirectory,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: []
        ).filter { $0.pathExtension == "app" && $0.hasDirectoryPath }
        guard appURLs.count == 1, let newAppURL = appURLs.first else {
            throw UpdaterError.invalidArchive("更新包中必须包含唯一的 .app")
        }
        try validateApplication(newAppURL)
        updateUI(status: "正在安装更新", detail: "正在备份并替换 \(applicationName).app…", progress: 78)
        let backupName = "\(applicationName)-backup-\(UUID().uuidString)"
        do {
            _ = try FileManager.default.replaceItemAt(
                targetURL,
                withItemAt: newAppURL,
                backupItemName: backupName,
                options: [.usingNewMetadataOnly]
            )
            appendLog("replaceItemAt 成功，backup=\(backupName)，targetExists=\(FileManager.default.fileExists(atPath: targetURL.path))")
        } catch {
            appendLog("replaceItemAt 失败，targetExists=\(FileManager.default.fileExists(atPath: targetURL.path)): \(error.localizedDescription)")
            throw UpdaterError.replacementFailed(error.localizedDescription)
        }
        updateUI(status: "正在安装更新", detail: "新版 \(applicationName).app 已替换", progress: 90)
        try writeCompletionMarker()
        try launchTargetApplication()
    }

    /// 用系统 tar 解压归档，按 tar verbose 输出更新真实进度。
    private func extractArchive(archiveURL: URL, destinationURL: URL, entryCount: Int) throws -> Int {
        var buffer = Data()
        var extractedCount = 0
        let lock = NSLock()
        let result = try runProcess(
            executable: "/usr/bin/tar",
            arguments: ["-xzvf", archiveURL.path, "-C", destinationURL.path],
            logName: "tar -xzvf"
        ) { [weak self] output, stream in
            guard stream == .standardError else { return }
            let data = Data(output.utf8)
            lock.lock()
            buffer.append(data)
            let lines = String(decoding: buffer, as: UTF8.self).split(separator: "\n", omittingEmptySubsequences: false)
            let completeLines = lines.dropLast()
            buffer = Data(String(lines.last ?? "").utf8)
            extractedCount += completeLines.filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }.count
            let currentCount = extractedCount
            lock.unlock()
            let ratio = min(1, Double(currentCount) / Double(max(entryCount, 1)))
            self?.updateUI(status: "正在解压更新", detail: "已处理 \(currentCount)/\(entryCount) 个条目", progress: 10 + ratio * 65)
        }
        lock.lock()
        if !String(decoding: buffer, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            extractedCount += 1
        }
        let finalCount = extractedCount
        lock.unlock()
        updateUI(status: "正在解压更新", detail: "解压完成", progress: 75)
        _ = result
        return finalCount
    }

    /// 校验解压结果中的应用标识和目标版本。
    private func validateApplication(_ appURL: URL) throws {
        let infoURL = appURL.appendingPathComponent("Contents/Info.plist")
        guard let info = NSDictionary(contentsOf: infoURL) as? [String: Any] else {
            throw UpdaterError.invalidArchive("无法读取新版应用 Info.plist")
        }
        let identifier = info["CFBundleIdentifier"] as? String
        let version = info["CFBundleShortVersionString"] as? String
        guard identifier == job.expectedBundleIdentifier else {
            throw UpdaterError.invalidArchive("新版应用标识不匹配")
        }
        guard version == job.expectedVersion else {
            throw UpdaterError.invalidArchive("新版应用版本不匹配，期望 \(job.expectedVersion)，实际 \(version ?? "未知")")
        }
        appendLog("校验新版应用通过: identifier=\(identifier ?? ""), version=\(version ?? "")")
    }

    /// 执行系统命令并记录标准输出、错误和退出码。
    private func runProcess(executable: String, arguments: [String], logName: String, outputHandler: ((String, ProcessOutputStream) -> Void)? = nil) throws -> ProcessOutput {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        let outputPipe = Pipe()
        let errorPipe = Pipe()
        process.standardOutput = outputPipe
        process.standardError = errorPipe
        let outputLock = NSLock()
        var streamedOutput = Data()
        var streamedError = Data()
        let drainGroup = DispatchGroup()
        let drain = { (handle: FileHandle, isError: Bool) in
            drainGroup.enter()
            DispatchQueue(label: "com.auracoder.updater.stream-drain", qos: .userInitiated).async {
                defer { drainGroup.leave() }
                while true {
                    guard let data = try? handle.read(upToCount: 64 * 1024), !data.isEmpty else { break }
                    outputLock.lock()
                    if isError {
                        streamedError.append(data)
                    } else {
                        streamedOutput.append(data)
                    }
                    outputLock.unlock()
                    if let outputHandler {
                        let stream: ProcessOutputStream = isError ? .standardError : .standardOutput
                        outputHandler(String(decoding: data, as: UTF8.self), stream)
                    }
                }
            }
        }
        drain(outputPipe.fileHandleForReading, false)
        drain(errorPipe.fileHandleForReading, true)
        do {
            try process.run()
        } catch {
            outputPipe.fileHandleForWriting.closeFile()
            errorPipe.fileHandleForWriting.closeFile()
            drainGroup.wait()
            appendLog("命令启动失败 \(logName): \(error.localizedDescription)")
            throw error
        }
        process.waitUntilExit()
        drainGroup.wait()
        outputLock.lock()
        let stdout = String(decoding: streamedOutput, as: UTF8.self)
        let stderr = String(decoding: streamedError, as: UTF8.self)
        outputLock.unlock()
        appendLog("命令 \(logName) 退出码=\(process.terminationStatus), stderr=\(stderr.trimmingCharacters(in: .whitespacesAndNewlines))")
        guard process.terminationStatus == 0 else {
            throw UpdaterError.commandFailed(logName, process.terminationStatus, stderr)
        }
        return ProcessOutput(stdout: stdout, stderr: stderr)
    }

    /// 创建更新器。
    init(job: UpdateJob) {
        self.job = job
        let appFileName = URL(fileURLWithPath: job.targetAppPath).lastPathComponent
        let parsedApplicationName = appFileName.hasSuffix(".app")
            ? String(appFileName.dropLast(4))
            : appFileName
        self.applicationName = parsedApplicationName.isEmpty ? "AuraCoder" : parsedApplicationName
        super.init()
    }

    /// 原子写入完成标记，供下一次启动识别更新已经完成。
    private func writeCompletionMarker() throws {
        try writeAtomically(Data("\(job.expectedVersion)\n".utf8), to: URL(fileURLWithPath: job.completionPath))
        appendLog("completion 文件已创建: \(job.completionPath)")
    }

    /// 通过 macOS 应用启动机制打开目标应用。
    private func launchTargetApplication() throws {
        updateUI(status: "正在启动 \(applicationName)", detail: "正在通过 macOS 应用启动机制启动新版…", progress: 96)
        let targetURL = URL(fileURLWithPath: job.targetAppPath)
        guard FileManager.default.fileExists(atPath: targetURL.path) else {
            throw UpdaterError.fileNotFound(targetURL.path)
        }
        let semaphore = DispatchSemaphore(value: 0)
        var launchError: Error?
        NSWorkspace.shared.openApplication(at: targetURL, configuration: NSWorkspace.OpenConfiguration()) { application, error in
            launchError = error
            self.appendLog("NSWorkspace 启动结果 application=\(application?.processIdentifier ?? 0), error=\(error?.localizedDescription ?? "无")")
            semaphore.signal()
        }
        guard semaphore.wait(timeout: .now() + 30) == .success else {
            throw UpdaterError.launchFailed("等待 NSWorkspace 启动结果超时")
        }
        if let launchError {
            throw UpdaterError.launchFailed(launchError.localizedDescription)
        }
        updateUI(status: "更新完成，正在启动 \(applicationName)", detail: "新版 \(applicationName) 已提交给 macOS 启动", progress: 100)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
            NSApp.terminate(nil)
        }
    }

    /// 在主队列更新窗口中的状态文字和真实进度。
    private func updateUI(status: String, detail: String, progress: Double) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.statusLabel?.stringValue = status
            self.detailLabel?.stringValue = detail
            self.progressIndicator?.doubleValue = progress
            self.percentageLabel?.stringValue = "\(Int(progress.rounded()))%"
        }
        appendLog("阶段: \(status), detail=\(detail), progress=\(Int(progress.rounded()))%")
    }

    /// 在失败状态保留窗口、显示错误并允许用户重新打开当前应用。
    private func showError(_ status: String, detail: String) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.statusLabel?.stringValue = status
            self.detailLabel?.stringValue = detail
            self.reopenButton?.isHidden = false
            self.window?.standardWindowButton(.closeButton)?.isEnabled = true
        }
    }

    /// 通过 macOS 应用启动机制重新打开目标应用。
    @objc private func reopenAuraCoder() {
        DispatchQueue(label: "com.auracoder.updater.reopen", qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            do {
                try self.launchTargetApplication()
            } catch {
                self.appendLog("重新打开 \(self.applicationName) 失败: \(error.localizedDescription)")
                self.showError("无法重新打开 \(self.applicationName)", detail: error.localizedDescription)
            }
        }
    }

    /// 把日志追加到任务指定文件并记录时间、模式和进程。
    private func appendLog(_ message: String) {
        let timestamp = ISO8601DateFormatter().string(from: Date())
        let line = "[\(timestamp)] mode=\(job.mode.rawValue) pid=\(ProcessInfo.processInfo.processIdentifier) \(message)\n"
        logQueue.sync {
            let path = URL(fileURLWithPath: self.job.logPath)
            do {
                try FileManager.default.createDirectory(at: path.deletingLastPathComponent(), withIntermediateDirectories: true)
                if FileManager.default.fileExists(atPath: path.path) {
                    let handle = try FileHandle(forWritingTo: path)
                    try handle.seekToEnd()
                    try handle.write(contentsOf: Data(line.utf8))
                    try handle.close()
                } else {
                    try Data(line.utf8).write(to: path, options: .atomic)
                }
            } catch {
                fputs("写入更新日志失败: \(error.localizedDescription)\n", stderr)
            }
        }
    }

    /// 以原子方式写入 ready 或 completion 文件。
    private func writeAtomically(_ data: Data, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: .atomic)
    }
}

/// 更新器从 Rust 接收的 JSON 任务描述。
struct UpdateJob: Codable {
    /// 当前任务模式，install 执行替换，relaunch 只执行过渡重启。
    let mode: UpdateMode
    /// 需要正常关闭的旧 AuraCoder 进程 PID。
    let oldProcessId: Int32
    /// 待安装的更新归档绝对路径，relaunch 模式为空。
    let archivePath: String?
    /// 当前运行的应用绝对路径。
    let targetAppPath: String
    /// 需要校验并写入完成标记的目标版本。
    let expectedVersion: String
    /// 需要校验的应用 Bundle Identifier。
    let expectedBundleIdentifier: String
    /// updater 启动完成后创建的 ready 文件路径。
    let readyPath: String
    /// 更新完成后创建的 completion 文件路径。
    let completionPath: String
    /// 更新器追加日志的绝对路径。
    let logPath: String

    /// JSON 字段名使用与 Rust job 一致的 camelCase。
    private enum CodingKeys: String, CodingKey {
        case mode
        case oldProcessId
        case archivePath
        case targetAppPath
        case expectedVersion
        case expectedBundleIdentifier
        case readyPath
        case completionPath
        case logPath
    }
}

/// updater 支持的两种任务模式。
enum UpdateMode: String, Codable {
    /// 解压、校验并替换应用后启动新版。
    case install
    /// 仅正常关闭并通过应用启动机制重启应用。
    case relaunch
}

/// 系统命令的标准输出和标准错误结果。
struct ProcessOutput {
    /// 命令标准输出文本。
    let stdout: String
    /// 命令标准错误文本。
    let stderr: String
}

/// 标识系统命令实时输出来自标准输出还是标准错误。
enum ProcessOutputStream {
    /// 标准输出流。
    case standardOutput
    /// 标准错误流。
    case standardError
}

/// 更新器向窗口展示的可读错误。
enum UpdaterError: LocalizedError {
    /// 更新任务 JSON 缺少必需字段或字段值无效。
    case invalidJob(String)
    /// 需要的文件不存在。
    case fileNotFound(String)
    /// 旧进程不存在。
    case processNotFound(Int32)
    /// 请求旧进程正常退出失败。
    case terminateFailed(Int32)
    /// 旧进程在限定时间内没有退出。
    case terminateTimedOut(Int32)
    /// 更新归档内容不满足安装要求。
    case invalidArchive(String)
    /// 系统命令返回非零退出码。
    case commandFailed(String, Int32, String)
    /// 应用替换失败。
    case replacementFailed(String)
    /// 应用启动失败。
    case launchFailed(String)

    /// 返回错误在更新窗口中显示的业务描述。
    var errorDescription: String? {
        switch self {
        case .invalidJob(let message): return message
        case .fileNotFound(let path): return "文件不存在: \(path)"
        case .processNotFound(let pid): return "找不到旧应用进程 PID=\(pid)"
        case .terminateFailed(let pid): return "无法请求旧应用进程正常退出，PID=\(pid)"
        case .terminateTimedOut(let pid): return "旧应用进程未在 60 秒内退出，PID=\(pid)"
        case .invalidArchive(let message): return message
        case .commandFailed(let command, let code, let stderr): return "命令 \(command) 失败，退出码 \(code): \(stderr)"
        case .replacementFailed(let message): return "替换应用失败: \(message)"
        case .launchFailed(let message): return "启动应用失败: \(message)"
        }
    }
}

/// 从命令行严格读取 --job 指定的 JSON 文件并启动 AppKit 事件循环。
@main
struct AuraCoderUpdaterMain {
    /// 校验命令行、解析任务并启动独立更新器窗口。
    static func main() {
        guard CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--job" else {
            fputs("用法: AuraCoderUpdater --job <job.json>\n", stderr)
            Foundation.exit(2)
        }
        let jobURL = URL(fileURLWithPath: CommandLine.arguments[2])
        do {
            let data = try Data(contentsOf: jobURL)
            let job = try JSONDecoder().decode(UpdateJob.self, from: data)
            let application = NSApplication.shared
            let delegate = AuraCoderUpdaterDelegate(job: job)
            application.delegate = delegate
            application.setActivationPolicy(.regular)
            application.run()
        } catch {
            fputs("解析更新任务失败: \(error.localizedDescription)\n", stderr)
            Foundation.exit(1)
        }
    }
}
