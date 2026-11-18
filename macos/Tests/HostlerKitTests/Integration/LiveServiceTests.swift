import Darwin
import Foundation
import Testing
@testable import HostlerKit

extension Tag {
    @Tag static var integration: Self
}

/// The `hostler` binary under test (`HOSTLER_TEST_BIN`, a debug build of this checkout).
let liveBinary: String? = {
    guard let path = ProcessInfo.processInfo.environment["HOSTLER_TEST_BIN"],
          FileManager.default.isExecutableFile(atPath: path) else { return nil }
    return path
}()

/// A `hostler service run` under a private temporary `HOSTLER_HOME`. A `$TMPDIR` home is long
/// on macOS, so the socket takes the `/tmp/hostler-<uid>-<hash>` fallback. The service also
/// gets `HOSTLER_TEST_OWNER_PID`, so it stops by itself if this test process dies; `stop()`
/// ends it normally. Device tools are an empty fake folder: no real adb or xcrun runs.
final class LiveService: @unchecked Sendable {
    let binary: String
    let root: URL
    let home: String
    private var process: Process?
    private let extra: [String: String]

    init(binary: String, extra: [String: String] = [:]) throws {
        self.binary = binary
        self.extra = extra
        root = FileManager.default.temporaryDirectory
            .appending(path: "hostler-swift-integration-\(UUID().uuidString)")
        home = root.appending(path: "home").path
        try FileManager.default.createDirectory(at: root.appending(path: "fake-tools/bin"),
                                                withIntermediateDirectories: true)
    }

    /// The environment of every `hostler` this test runs: the test's minus `HOSTLER_*`, plus
    /// the home and the test hooks.
    var environment: [String: String] {
        var env = ProcessInfo.processInfo.environment.filter { !$0.key.hasPrefix("HOSTLER_") }
        env["HOSTLER_HOME"] = home
        env["HOSTLER_LOOKY_LEDGER"] = root.appending(path: "no-looky-ledger.json").path
        env["HOSTLER_TEST_FAST_KDF"] = "1"
        env["HOSTLER_TEST_OWNER_PID"] = String(getpid())
        env["HOSTLER_TEST_FAKE_TOOLS"] = root.appending(path: "fake-tools").path
        env.merge(extra) { $1 }
        return env
    }

    var location: SocketLocation {
        SocketLocator.location(home: home, euid: geteuid())
    }

    /// Starts `hostler service run` and waits until its socket exists.
    func start() async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["service", "run"]
        process.environment = environment
        process.currentDirectoryURL = root
        process.standardInput = FileHandle.nullDevice
        let log = root.appending(path: "service-\(UUID().uuidString.prefix(6)).log")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        let handle = try FileHandle(forWritingTo: log)
        process.standardOutput = handle
        process.standardError = handle
        try process.run()
        self.process = process
        let socket = location.socketPath
        try await withDeadline("the service's socket appears") {
            while !FileManager.default.fileExists(atPath: socket) {
                try await Task.sleep(for: .milliseconds(50))
            }
        }
    }

    /// Ends the service this test started (SIGTERM to its own child) and waits for it.
    func stop() async {
        guard let process else { return }
        self.process = nil
        if process.isRunning { process.terminate() }
        _ = try? await withDeadline("the service exits") {
            while process.isRunning { try await Task.sleep(for: .milliseconds(50)) }
        }
        if process.isRunning { kill(process.processIdentifier, SIGKILL) }
    }

    /// Stops the service and removes the scratch folder and, for this home only, the
    /// `/tmp/hostler-<uid>-<hash>` socket folder the service left.
    func cleanUp() async {
        await stop()
        try? FileManager.default.removeItem(at: root)
        if location.isFallback {
            try? FileManager.default.removeItem(
                atPath: (location.socketPath as NSString).deletingLastPathComponent)
        }
    }

    /// Runs the CLI with this home; returns its exit status and stdout.
    func cli(_ arguments: [String], in directory: URL) async throws -> (Int32, String) {
        let outcome = await FoundationProcessLauncher().launch(
            executable: binary, arguments: arguments, environment: environment,
            cwd: directory.path, timeout: .seconds(30), stdoutLimit: 1 << 20,
            stderrLimit: 16 * 1024)
        guard case .exited(let output) = outcome else {
            throw SocketError.failed("hostler \(arguments) did not finish: \(outcome)")
        }
        return (output.exit, String(decoding: output.stdout, as: UTF8.self))
    }

    @MainActor
    func controller() -> ConnectionController {
        ConnectionController(environment: ConnectionEnvironment(
            location: .success(location),
            defaultHome: SocketLocator.defaultHome(homeDirectory: NSHomeDirectory()),
            euid: geteuid(), connector: POSIXSocketConnector(),
            registration: FakeRegistration(value: nil),
            fileExists: { FileManager.default.isExecutableFile(atPath: $0) },
            sleeper: TaskSleeper(), clientName: "hostler-app integration"))
    }

    /// A git repository in the scratch folder.
    func gitRepository(_ name: String) throws -> URL {
        let repo = root.appending(path: name)
        try FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        let git = Process()
        git.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        git.arguments = ["init", "-q", repo.path]
        git.standardOutput = FileHandle.nullDevice
        git.standardError = FileHandle.nullDevice
        try git.run()
        git.waitUntilExit()
        return repo
    }
}

/// Records every call of the controller, then hands it to the real store.
@MainActor
final class ForwardingSink: ConnectionEventSink {
    let store: HostlerStore
    let recorder = RecordingSink()

    init(store: HostlerStore) { self.store = store }

    func connected(instance: String?, connection: ConnectionToken) {
        recorder.connected(instance: instance, connection: connection)
        store.connected(instance: instance, connection: connection)
    }

    func dropped(connection: ConnectionToken) {
        recorder.dropped(connection: connection)
        store.dropped(connection: connection)
    }

    func receive(_ event: Event, seq: UInt64, connection: ConnectionToken, frameBytes: Int?) {
        recorder.receive(event, seq: seq, connection: connection, frameBytes: frameBytes)
        store.receive(event, seq: seq, connection: connection, frameBytes: frameBytes)
    }
}

@MainActor
@Suite(.tags(.integration), .enabled(if: liveBinary != nil, "set HOSTLER_TEST_BIN to a built hostler"),
       .serialized, .timeLimit(.minutes(1)))
struct LiveServiceTests {
    @Test func theAppTalksToARealService() async throws {
        let service = try LiveService(binary: liveBinary!)
        #expect(service.location.isFallback)
        try await service.start()
        let controller = service.controller()
        let store = HostlerStore(client: controller.client)
        let sink = ForwardingSink(store: store)
        controller.sink = sink
        do {
            try await Self.exercise(service, controller, store, sink)
        } catch {
            await controller.stop()
            store.shutdown()
            await service.cleanUp()
            throw error
        }
        await controller.stop()
        store.shutdown()
        await service.cleanUp()
    }

    static func exercise(_ service: LiveService, _ controller: ConnectionController,
                         _ store: HostlerStore, _ sink: ForwardingSink) async throws {
        controller.start()
        try await eventually("live") {
            if case .live = controller.state { true } else { false }
        }
        let instance = try #require(controller.helloReply?.instance)
        #expect(sink.recorder.connections == [.connected(instance: instance, connection: 1)])
        try await eventually("the store is live on connection 1") {
            store.syncState == .live && store.machine.connection == 1
        }
        let client = controller.client

        // Reads.
        let status = try await client.serviceStatus()
        #expect(status.contractVersion == Contract.version)
        #expect(status.exe.map { URL(fileURLWithPath: $0).resolvingSymlinksInPath().path }
            == URL(fileURLWithPath: liveBinary!).resolvingSymlinksInPath().path)
        let devices = try await client.devicesList(all: true)
        #expect(devices.devices.isEmpty)
        let queue = try await client.queueSnapshot()
        #expect(queue.running.isEmpty)

        // Events while a heavy task runs reach the store's model.
        let repo = try service.gitRepository("app")
        let heavy = Task { try await service.cli(["run", "--heavy", "--", "sleep", "2"], in: repo) }
        try await eventually("the store sees the heavy task") {
            store.model.queue?.running.contains { $0.command.contains("sleep") } == true
        }
        let received = sink.recorder.calls.compactMap { call -> (UInt64, Int?)? in
            if case .receive(let seq, 1, let bytes) = call { (seq, bytes) } else { nil }
        }
        #expect(!received.isEmpty)
        #expect(received.allSatisfy { ($0.1 ?? 0) > 0 })
        #expect(received.map(\.0) == received.map(\.0).sorted())
        let (heavyExit, _) = try await withDeadline("the heavy run ends") { try await heavy.value }
        #expect(heavyExit == 0)

        // Projects after the CLI resolves a workspace in a git repository.
        let (workspaceExit, _) = try await service.cli(["workspace", "--json"], in: repo)
        #expect(workspaceExit == 0)
        let projects = try await client.projectsSnapshot()
        let project = try #require(projects.projects.first { $0.info.name == "app" })
        #expect(project.workspaces.count == 1)
        let ref = ProjectRef(commonDir: project.info.commonDir, name: project.info.name)

        // A test database, the proof and a reveal.
        let password = SecretText("integration-master-password")
        let unlocked = try await client.secretsUnlock(
            Unlock(password: password, forgetKeyFile: false, create: true))
        #expect(unlocked.status.state == .unlocked)
        try await eventually("the unlock's grant") { controller.grant != nil }
        _ = try await client.projectApprove(ref)
        let value = "value-\(UUID().uuidString)"
        _ = try await client.secretSet(SecretSet(project: ref, name: "API_KEY",
                                                 value: SecretText(value)))
        let grant = try await client.secretsProve(Prove(password: password))
        try await eventually("the proof's grant") { controller.grant == grant }
        let revealed = try await client.secretReveal(project: ref, name: "API_KEY")
        #expect(revealed.value.reveal() == value)
        #expect(!String(reflecting: revealed).contains(value))
        await #expect(throws: HostlerCallError.self) {
            _ = try await client.secretsProve(Prove(password: SecretText("wrong")))
        }
        try await eventually("a failed prove drops the grant") { controller.grant == nil }

        // The service goes away, then comes back: not running, then live on connection n > 1.
        await service.stop()
        try await eventually("not running") {
            controller.state == .notRunning(
                offer: .typeCommand("HOSTLER_HOME=\(shellQuoted(service.home)) hostler service run"))
        }
        #expect(controller.grant == nil)
        try await eventually("the store is offline") { store.syncState != .live }
        try await service.start()
        controller.retryNow()
        try await eventually("live again") {
            if case .live = controller.state, controller.helloReply?.instance != instance {
                true
            } else {
                false
            }
        }
        let second = try #require(controller.helloReply?.instance)
        let token = try #require(controller.currentConnection)
        #expect(second != instance)
        #expect(token > 1)
        try await eventually("the store is live on the new connection") {
            store.syncState == .live && store.machine.connection == token
        }
        #expect(store.model.queue?.running.isEmpty == true)

        // Connection 1 was dropped once, and none of its frames came after its drop.
        let calls = sink.recorder.calls
        let droppedOne = try #require(calls.firstIndex(of: .dropped(1)))
        #expect(calls.filter { $0 == .dropped(1) }.count == 1)
        #expect(calls[droppedOne...].allSatisfy { call in
            if case .receive(_, 1, _) = call { false } else { true }
        })
        // A late frame of the replaced connection never reaches the model.
        let before = store.model
        let discarded = store.machine.foreignFramesDiscarded
        store.receive(.queueChanged(QueueSnapshot(capacity: 9, running: [], waiting: [],
                                                  foreignRunning: [], asOfSeq: 1_000_000)),
                      seq: 1_000_000, connection: 1, frameBytes: 100)
        #expect(store.model == before)
        #expect(store.machine.foreignFramesDiscarded == discarded + 1)
    }

    @Test func anOlderContractIsAMismatch() async throws {
        let service = try LiveService(binary: liveBinary!,
                                      extra: ["HOSTLER_TEST_CONTRACT_VERSION": "4"])
        try await service.start()
        let controller = service.controller()
        controller.start()
        let reached: Bool
        do {
            try await eventually("mismatch") {
                if case .mismatch = controller.state { true } else { false }
            }
            reached = true
        } catch {
            reached = false
        }
        let state = controller.state
        await controller.stop()
        await service.cleanUp()
        #expect(reached, "state: \(state)")
        guard case .mismatch(let client, let serviceContract, let version, let recovery) = state
        else { return }
        #expect(client == 5)
        #expect(serviceContract == 4)
        #expect(!version.isEmpty)
        #expect(recovery != nil)
    }
}
