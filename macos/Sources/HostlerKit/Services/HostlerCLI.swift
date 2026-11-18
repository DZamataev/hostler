import Foundation

/// One run of the `hostler` CLI (spec §9.7: claim takeovers, `hostler doctor` and Start run
/// through the service's own binary with `--json`, so each procedure exists once, in Rust).
public struct CLIInvocation: Sendable, Equatable {
    /// The binary: `ServiceStatus.exe`, or the registered one while the service is down
    /// (`HostlerCLI.binary(status:registration:)`).
    public var executable: String
    /// The arguments, without `--json` (always appended).
    public var arguments: [String]
    /// The working directory (a workspace's path for the commands that resolve one).
    public var cwd: String
    public var timeout: Duration
    /// Whether the child keeps the app's `HOSTLER_HOME`. Start removes it: the registration
    /// serves the default home only.
    public var keepsHostlerHome: Bool

    /// The default bound of a command.
    public static let defaultTimeout: Duration = .seconds(30)
    /// A takeover (`hostler release --force`, `hostler hold --force`) stops processes first.
    public static let takeoverTimeout: Duration = .seconds(120)

    public init(executable: String, arguments: [String], cwd: String,
                timeout: Duration = CLIInvocation.defaultTimeout, keepsHostlerHome: Bool = true) {
        self.executable = executable
        self.arguments = arguments
        self.cwd = cwd
        self.timeout = timeout
        self.keepsHostlerHome = keepsHostlerHome
    }

    /// The command line as the person would type it (for an error text).
    public var commandLine: String {
        (["hostler"] + arguments + ["--json"]).joined(separator: " ")
    }
}

/// What a CLI run gave.
public enum CLIResult: Sendable, Equatable {
    /// `{"ok": true, "data": …}` at exit status 0.
    case ok(JSONValue)
    /// A complete `{"ok": true, "data": …}` envelope at a **non-zero** exit status: `hostler
    /// doctor` exits 1 while a check remains a problem, and its report is still the answer
    /// (P5-L15).
    case answered(JSONValue, exit: Int32)
    /// The envelope's error (`{"ok": false, "error": …}`).
    case failed(HostlerError)
    /// No envelope: the exit status and the end of stderr (at most `HostlerCLI.stderrLimit`).
    case cliFailed(exit: Int32, stderr: String)
    /// The binary could not be started.
    case notStarted(String)
    /// No exit within the bound; the child was ended (SIGTERM, then SIGKILL).
    case timedOut(command: String)
    /// The calling task was cancelled; the child was ended (SIGTERM, then SIGKILL).
    case cancelled(command: String)

    /// The failure as a user-visible error; `nil` for `.ok`.
    public var appError: HostlerAppError? {
        switch self {
        case .ok: nil
        case .answered(_, let exit): .cliFailed(exit: exit, stderr: "")
        case .failed(let error): .service(message: error.message, recovery: error.recovery)
        case .cliFailed(let exit, let stderr): .cliFailed(exit: exit, stderr: stderr)
        case .notStarted(let why): .cliNotStarted(why)
        case .timedOut(let command): .cliTimedOut(command: command)
        case .cancelled(let command): .cliCancelled(command: command)
        }
    }
}

/// Runs the `hostler` CLI (fakes in tests: `FakeCommandRunner`).
public protocol HostlerCommandRunning: Sendable {
    func run(_ invocation: CLIInvocation) async -> CLIResult
}

/// A finished child process.
public struct ProcessOutput: Sendable, Equatable {
    public var exit: Int32
    /// stdout, at most the launcher's stdout limit.
    public var stdout: Data
    /// The last bytes of stderr, at most the launcher's stderr limit.
    public var stderrTail: Data
    /// stdout was longer than its limit.
    public var stdoutTruncated: Bool

    public init(exit: Int32, stdout: Data, stderrTail: Data, stdoutTruncated: Bool = false) {
        self.exit = exit
        self.stdout = stdout
        self.stderrTail = stderrTail
        self.stdoutTruncated = stdoutTruncated
    }
}

public enum ProcessOutcome: Sendable, Equatable {
    case exited(ProcessOutput)
    case notStarted(String)
    case timedOut
    case cancelled
}

/// Starts a child process and collects its output (OS-facing; fakes in tests).
public protocol ProcessLaunching: Sendable {
    func launch(executable: String, arguments: [String], environment: [String: String],
                cwd: String, timeout: Duration, stdoutLimit: Int, stderrLimit: Int) async
        -> ProcessOutcome
}

/// The CLI runner: `--json` appended, the environment scrubbed of every `HOSTLER_*` except
/// `HOSTLER_HOME`, stdout decoded as the generated `Envelope<JSONValue>`, stderr kept for the
/// error text (its last 16 KiB).
public struct HostlerCLI: HostlerCommandRunning {
    public static let stderrLimit = 16 * 1024
    public static let stdoutLimit = Contract.maxFrameBytes

    let launcher: any ProcessLaunching
    let environment: [String: String]

    public init(launcher: any ProcessLaunching = FoundationProcessLauncher(),
                environment: [String: String] = ProcessInfo.processInfo.environment) {
        self.launcher = launcher
        self.environment = environment
    }

    /// The binary for a CLI run: the running service's own (`ServiceStatus.exe`), else the
    /// registered one.
    public static func binary(status: ServiceStatus?, registration: Registration?) -> String? {
        if let exe = status?.exe, !exe.isEmpty { return exe }
        return registration?.executable
    }

    /// The app's environment minus every `HOSTLER_*` (a test hook, a heavy-run marker) except
    /// `HOSTLER_HOME`, which goes too when `keepsHome` is false.
    public static func childEnvironment(_ environment: [String: String],
                                        keepsHome: Bool) -> [String: String] {
        environment.filter { name, _ in
            guard name.hasPrefix("HOSTLER_") else { return true }
            return keepsHome && name == SocketLocator.homeVariable
        }
    }

    public func run(_ invocation: CLIInvocation) async -> CLIResult {
        let outcome = await launcher.launch(
            executable: invocation.executable, arguments: invocation.arguments + ["--json"],
            environment: Self.childEnvironment(environment, keepsHome: invocation.keepsHostlerHome),
            cwd: invocation.cwd, timeout: invocation.timeout,
            stdoutLimit: Self.stdoutLimit, stderrLimit: Self.stderrLimit)
        // Logged by its subcommand only: the arguments may hold a confirmation token.
        let subcommand = invocation.arguments.first ?? ""
        switch outcome {
        case .notStarted(let why):
            return .notStarted(why)
        case .timedOut:
            Log.app.error("hostler \(subcommand, privacy: .public) timed out")
            return .timedOut(command: invocation.commandLine)
        case .cancelled:
            Log.app.info("hostler \(subcommand, privacy: .public) cancelled")
            return .cancelled(command: invocation.commandLine)
        case .exited(let output):
            let result = Self.interpret(output)
            if case .ok = result {} else {
                Log.app.info(
                    "hostler \(subcommand, privacy: .public) exited \(output.exit) (\(output.stdout.count) bytes out)")
            }
            return result
        }
    }

    /// The envelope's answer; without a readable envelope, the exit status and stderr's end.
    ///
    /// A complete, successful envelope at a non-zero exit is `.answered`, not a failure: some
    /// commands report a result and an exit status a person reads (`doctor` exits 1 while a check
    /// remains a problem), and dropping the answer would lose the report (P5-L15).
    public static func interpret(_ output: ProcessOutput) -> CLIResult {
        if !output.stdoutTruncated,
           let envelope = try? JSONDecoder().decode(Envelope<JSONValue>.self, from: output.stdout)
        {
            if let error = envelope.error { return .failed(error) }
            if envelope.ok {
                let data = envelope.data ?? .null
                return output.exit == 0 ? .ok(data) : .answered(data, exit: output.exit)
            }
        }
        return .cliFailed(exit: output.exit, stderr: stderrText(output.stderrTail))
    }

    static func stderrText(_ tail: Data) -> String {
        String(decoding: tail.suffix(stderrLimit), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// `Foundation.Process` with pipes. On a timeout, or when the calling task is cancelled, it ends
/// **its own** child: SIGTERM, then SIGKILL after `terminationGrace` (P5-L12). It never signals
/// a process it did not start.
public struct FoundationProcessLauncher: ProcessLaunching {
    let terminationGrace: Duration

    public init(terminationGrace: Duration = .seconds(2)) {
        self.terminationGrace = terminationGrace
    }

    public func launch(executable: String, arguments: [String], environment: [String: String],
                       cwd: String, timeout: Duration, stdoutLimit: Int, stderrLimit: Int) async
        -> ProcessOutcome
    {
        if Task.isCancelled { return .cancelled }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.environment = environment
        process.currentDirectoryURL = URL(fileURLWithPath: cwd)
        process.standardInput = FileHandle.nullDevice
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        let collector = OutputCollector(stdoutLimit: stdoutLimit, stderrLimit: stderrLimit)
        let child = OwnChild(process, grace: terminationGrace)

        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<ProcessOutcome, Never>) in
                collector.setOnDone { continuation.resume(returning: $0) }
                out.fileHandleForReading.readabilityHandler = { handle in
                    let data = handle.availableData
                    if data.isEmpty { handle.readabilityHandler = nil }
                    collector.stdout(data)
                }
                err.fileHandleForReading.readabilityHandler = { handle in
                    let data = handle.availableData
                    if data.isEmpty { handle.readabilityHandler = nil }
                    collector.stderr(data)
                }
                process.terminationHandler = { process in
                    collector.exited(process.terminationStatus)
                    // A grandchild may hold the pipes open; do not wait for it past a moment.
                    DispatchQueue.global().asyncAfter(deadline: .now() + 2) { collector.finish() }
                }
                do {
                    try process.run()
                } catch {
                    out.fileHandleForReading.readabilityHandler = nil
                    err.fileHandleForReading.readabilityHandler = nil
                    collector.fail(.notStarted("\(executable): \(error.localizedDescription)"))
                    return
                }
                child.started()
                // Cancelled while starting: the child goes at once.
                if collector.isDone { child.end() }
                DispatchQueue.global().asyncAfter(deadline: .now() + seconds(timeout)) {
                    if collector.fail(.timedOut) { child.end() }
                }
            }
        } onCancel: {
            if collector.fail(.cancelled) { child.end() }
        }
    }
}

func seconds(_ duration: Duration) -> Double {
    Double(duration.components.seconds) + Double(duration.components.attoseconds) / 1e18
}

/// The launcher's own child, ended at most once: SIGTERM, then SIGKILL after the grace if it is
/// still running. `Process` reaps it, so `isRunning` is false once it is gone and no reused pid
/// is ever signalled.
private final class OwnChild: @unchecked Sendable {
    private let lock = NSLock()
    private let process: Process
    private let grace: Duration
    private var running = false
    private var ending = false

    init(_ process: Process, grace: Duration) {
        self.process = process
        self.grace = grace
    }

    func started() { lock.withLock { running = true } }

    func end() {
        let go = lock.withLock { () -> Bool in
            guard running, !ending else { return false }
            ending = true
            return true
        }
        guard go, process.isRunning else { return }
        let pid = process.processIdentifier
        process.terminate()
        let process = self.process
        DispatchQueue.global().asyncAfter(deadline: .now() + seconds(grace)) {
            if process.isRunning { kill(pid, SIGKILL) }
        }
    }
}

/// Collects a child's output from the pipes' handlers and resumes once.
private final class OutputCollector: @unchecked Sendable {
    private let lock = NSLock()
    private let stdoutLimit: Int
    private let stderrLimit: Int
    private var out = Data()
    private var err = Data()
    private var truncated = false
    private var outClosed = false
    private var errClosed = false
    private var status: Int32?
    private var done = false
    private var onDone: (@Sendable (ProcessOutcome) -> Void)?
    private var outcome: ProcessOutcome?

    /// Sets where the outcome goes; an outcome that came first is delivered at once.
    func setOnDone(_ body: @escaping @Sendable (ProcessOutcome) -> Void) {
        let ready = lock.withLock { () -> ProcessOutcome? in
            onDone = body
            return outcome
        }
        if let ready { body(ready) }
    }

    var isDone: Bool { lock.withLock { done } }

    init(stdoutLimit: Int, stderrLimit: Int) {
        self.stdoutLimit = stdoutLimit
        self.stderrLimit = stderrLimit
    }

    func stdout(_ data: Data) {
        lock.withLock {
            if data.isEmpty { outClosed = true; return }
            let room = max(0, stdoutLimit - out.count)
            if data.count > room { truncated = true }
            out.append(data.prefix(room))
        }
        finishIfComplete()
    }

    func stderr(_ data: Data) {
        lock.withLock {
            if data.isEmpty { errClosed = true; return }
            err.append(data)
            if err.count > stderrLimit { err = Data(err.suffix(stderrLimit)) }
        }
        finishIfComplete()
    }

    func exited(_ code: Int32) {
        lock.withLock { status = code }
        finishIfComplete()
    }

    private func finishIfComplete() {
        let complete = lock.withLock { status != nil && outClosed && errClosed }
        if complete { finish() }
    }

    /// Ends with what was collected (after exit).
    func finish() {
        let result = lock.withLock { () -> ProcessOutcome? in
            guard let status else { return nil }
            return .exited(ProcessOutput(exit: status, stdout: out, stderrTail: err,
                                         stdoutTruncated: truncated))
        }
        if let result { deliver(result) }
    }

    /// Ends with `outcome` unless something ended first; whether this call ended it.
    @discardableResult
    func fail(_ outcome: ProcessOutcome) -> Bool {
        deliver(outcome)
    }

    @discardableResult
    private func deliver(_ result: ProcessOutcome) -> Bool {
        let target = lock.withLock { () -> ((@Sendable (ProcessOutcome) -> Void)?, Bool) in
            guard !done else { return (nil, false) }
            done = true
            outcome = result
            return (onDone, true)
        }
        target.0?(result)
        return target.1
    }
}
