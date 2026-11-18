import Foundation
import Testing
@testable import HostlerKit

@Suite(.timeLimit(.minutes(1))) struct HostlerCLITests {
    static let invocation = CLIInvocation(executable: "/opt/hostler", arguments: ["doctor"],
                                          cwd: "/work")

    static func output(_ stdout: String, exit: Int32 = 0, stderr: String = "") -> ProcessOutcome {
        .exited(ProcessOutput(exit: exit, stdout: Data(stdout.utf8), stderrTail: Data(stderr.utf8)))
    }

    @Test func anOkEnvelopeIsItsData() async {
        let launcher = FakeProcessLauncher(Self.output(#"{"ok":true,"data":{"n":1},"error":null}"#))
        let result = await HostlerCLI(launcher: launcher, environment: [:]).run(Self.invocation)
        #expect(result == .ok(.object(["n": .int(1)])))
    }

    @Test func theGeneratedEnvelopeFixturesDecode() async throws {
        let ok = try ContractFixtures.data("envelope.ReleaseOutcome.json")
        let launcher = FakeProcessLauncher(.exited(ProcessOutput(exit: 0, stdout: ok, stderrTail: Data())))
        let result = await HostlerCLI(launcher: launcher, environment: [:]).run(Self.invocation)
        #expect(result == .ok(try ContractFixtures.json("envelope.ReleaseOutcome.json")["data"]!))

        let failed = try ContractFixtures.data("envelope.error.json")
        let expected = try ContractFixtures.decode(Envelope<JSONValue>.self, "envelope.error.json").error!
        let errorLauncher = FakeProcessLauncher(.exited(ProcessOutput(exit: 1, stdout: failed, stderrTail: Data())))
        #expect(await HostlerCLI(launcher: errorLauncher, environment: [:]).run(Self.invocation)
            == .failed(expected))
    }

    @Test func aNonZeroExitWithAnEnvelopeIsTheEnvelopesError() async {
        let launcher = FakeProcessLauncher(Self.output("""
            {"ok":false,"data":null,"error":{"code":"resource_in_use","message":"held","details":{},"recovery":"hostler status"}}
            """, exit: 75, stderr: "noise"))
        let result = await HostlerCLI(launcher: launcher, environment: [:]).run(Self.invocation)
        #expect(result == .failed(HostlerError(code: .resourceInUse, message: "held",
                                               details: .object([:]), recovery: "hostler status")))
        #expect(result.appError == .service(message: "held", recovery: "hostler status"))
    }

    @Test func aNonZeroExitWithoutAnEnvelopeIsCliFailedWithStderrsTail() async {
        let launcher = FakeProcessLauncher(Self.output("garbage", exit: 2, stderr: "error: unknown flag\n"))
        let result = await HostlerCLI(launcher: launcher, environment: [:]).run(Self.invocation)
        #expect(result == .cliFailed(exit: 2, stderr: "error: unknown flag"))
        #expect(result.appError == .cliFailed(exit: 2, stderr: "error: unknown flag"))
    }

    /// P5-L15: a complete, successful envelope at a non-zero exit keeps its answer (`doctor`
    /// exits 1 while a check remains a problem).
    @Test func anOkEnvelopeWithANonZeroExitKeepsItsAnswer() async {
        let launcher = FakeProcessLauncher(Self.output(
            #"{"ok":true,"data":{"checks":[]},"error":null}"#, exit: 1))
        let result = await HostlerCLI(launcher: launcher, environment: [:]).run(Self.invocation)
        #expect(result == .answered(.object(["checks": .array([])]), exit: 1))
        // It is not a success for a caller that only looks at the exit status.
        #expect(result.appError == .cliFailed(exit: 1, stderr: ""))
    }

    @Test func jsonIsAlwaysAppendedAndTheBoundsArePassed() async {
        let launcher = FakeProcessLauncher(Self.output(#"{"ok":true,"data":null,"error":null}"#))
        let takeover = CLIInvocation(executable: "/opt/hostler",
                                     arguments: ["release", "c1", "--force", "--confirmation", "t"],
                                     cwd: "/work", timeout: CLIInvocation.takeoverTimeout)
        _ = await HostlerCLI(launcher: launcher, environment: [:]).run(takeover)
        let launch = launcher.launches[0]
        #expect(launch.executable == "/opt/hostler")
        #expect(launch.arguments == ["release", "c1", "--force", "--confirmation", "t", "--json"])
        #expect(launch.cwd == "/work")
        #expect(launch.timeout == .seconds(120))
        #expect(launch.stderrLimit == 16 * 1024)
        #expect(CLIInvocation.defaultTimeout == .seconds(30))
        #expect(Self.invocation.timeout == .seconds(30))
    }

    @Test func everyHostlerVariableButTheHomeIsScrubbed() async {
        let environment = ["PATH": "/usr/bin", "HOME": "/Users/x", "HOSTLER_HOME": "/tmp/h",
                           "HOSTLER_TEST_FAKE_TOOLS": "/fake", "HOSTLER_HEAVY_ID": "h1",
                           "HOSTLER_LOOKY_LEDGER": "/l"]
        let launcher = FakeProcessLauncher(Self.output(#"{"ok":true,"data":null,"error":null}"#))
        _ = await HostlerCLI(launcher: launcher, environment: environment).run(Self.invocation)
        #expect(launcher.launches[0].environment
            == ["PATH": "/usr/bin", "HOME": "/Users/x", "HOSTLER_HOME": "/tmp/h"])
        var withoutHome = Self.invocation
        withoutHome.keepsHostlerHome = false
        _ = await HostlerCLI(launcher: launcher, environment: environment).run(withoutHome)
        #expect(launcher.launches[1].environment == ["PATH": "/usr/bin", "HOME": "/Users/x"])
    }

    @Test func notStartedAndTimedOutAreTheirOwnErrors() async {
        let missing = await HostlerCLI(launcher: FakeProcessLauncher(.notStarted("no such file")),
                                       environment: [:]).run(Self.invocation)
        #expect(missing == .notStarted("no such file"))
        #expect(missing.appError == .cliNotStarted("no such file"))
        let slow = await HostlerCLI(launcher: FakeProcessLauncher(.timedOut), environment: [:])
            .run(Self.invocation)
        #expect(slow == .timedOut(command: "hostler doctor --json"))
    }

    @Test func aCancelledRunIsItsOwnResult() async {
        let result = await HostlerCLI(launcher: FakeProcessLauncher(.cancelled), environment: [:])
            .run(Self.invocation)
        #expect(result == .cancelled(command: "hostler doctor --json"))
        #expect(result.appError == .cliCancelled(command: "hostler doctor --json"))
    }

    @Test func theBinaryIsTheServicesThenTheRegistered() {
        let status = ServiceStatus(serviceVersion: "0.0.4", contractVersion: 5, pid: 1,
                                   runningHeavy: 0, waitingHeavy: 0, exe: "/svc/hostler")
        let registered = Registration(executable: "/reg/hostler", setsEnvironment: false)
        #expect(HostlerCLI.binary(status: status, registration: registered) == "/svc/hostler")
        #expect(HostlerCLI.binary(status: nil, registration: registered) == "/reg/hostler")
        #expect(HostlerCLI.binary(status: nil, registration: nil) == nil)
    }
}

/// The real `Process` launcher, with `/bin/sh` standing in for hostler.
@Suite(.timeLimit(.minutes(1))) struct FoundationProcessLauncherTests {
    static func run(_ script: String, timeout: Duration = .seconds(10),
                    environment: [String: String] = [:]) async -> ProcessOutcome {
        await FoundationProcessLauncher().launch(
            executable: "/bin/sh", arguments: ["-c", script], environment: environment,
            cwd: "/tmp", timeout: timeout, stdoutLimit: 1024, stderrLimit: 16 * 1024)
    }

    @Test func itCollectsStdoutStderrAndTheExitStatus() async {
        let outcome = await Self.run("printf '{\"ok\":true}'; printf 'oops' >&2; exit 3")
        #expect(outcome == .exited(ProcessOutput(exit: 3, stdout: Data(#"{"ok":true}"#.utf8),
                                                 stderrTail: Data("oops".utf8))))
    }

    @Test func stderrKeepsItsLast16KiB() async {
        // 20000 'a' then 'END': the tail is 16 KiB ending in END.
        let outcome = await Self.run("head -c 20000 /dev/zero | tr '\\\\0' a >&2; printf END >&2")
        guard case .exited(let output) = outcome else {
            Issue.record("did not exit: \(outcome)")
            return
        }
        #expect(output.stderrTail.count == 16 * 1024)
        #expect(output.stderrTail.suffix(3) == Data("END".utf8))
    }

    @Test func stdoutOverItsLimitIsMarkedTruncated() async {
        let outcome = await Self.run("head -c 5000 /dev/zero")
        guard case .exited(let output) = outcome else {
            Issue.record("did not exit")
            return
        }
        #expect(output.stdout.count == 1024)
        #expect(output.stdoutTruncated)
        #expect(HostlerCLI.interpret(output) == .cliFailed(exit: 0, stderr: ""))
    }

    @Test func theEnvironmentIsExactlyTheOneGiven() async {
        let outcome = await Self.run("printf '%s' \"${HOSTLER_TEST_X-unset}:$ONLY\"",
                                     environment: ["ONLY": "yes"])
        #expect(outcome == .exited(ProcessOutput(exit: 0, stdout: Data("unset:yes".utf8),
                                                 stderrTail: Data())))
    }

    @Test func aMissingBinaryIsNotStarted() async {
        let outcome = await FoundationProcessLauncher().launch(
            executable: "/nonexistent/hostler", arguments: [], environment: [:], cwd: "/tmp",
            timeout: .seconds(5), stdoutLimit: 10, stderrLimit: 10)
        guard case .notStarted = outcome else {
            Issue.record("expected notStarted, got \(outcome)")
            return
        }
    }

    /// A pid file and a check that the process is gone (reaped, not only signalled).
    static func pidFile() -> String {
        FileManager.default.temporaryDirectory.appending(path: "hostler-cli-\(UUID().uuidString).pid").path
    }

    static func pid(in file: String) async throws -> pid_t {
        try await withDeadline("the child writes its pid") {
            while true {
                if let text = try? String(contentsOfFile: file, encoding: .utf8),
                   let pid = pid_t(text.trimmingCharacters(in: .whitespacesAndNewlines)) {
                    return pid
                }
                try await Task.sleep(for: .milliseconds(20))
            }
        }
    }

    static func gone(_ pid: pid_t) async throws {
        try await withDeadline("process \(pid) is gone") {
            while kill(pid, 0) == 0 { try await Task.sleep(for: .milliseconds(20)) }
        }
    }

    @Test func aChildPastItsBoundIsReportedTimedOutAndEnded() async throws {
        let file = Self.pidFile()
        defer { try? FileManager.default.removeItem(atPath: file) }
        let clock = ContinuousClock()
        let start = clock.now
        let outcome = await Self.run("echo $$ > \(file); exec sleep 30", timeout: .milliseconds(300))
        #expect(outcome == .timedOut)
        #expect(clock.now - start < .seconds(2))
        try await Self.gone(try await Self.pid(in: file))
    }

    @Test func cancellingALaunchEndsItsChildAtOnce() async throws {
        let file = Self.pidFile()
        defer { try? FileManager.default.removeItem(atPath: file) }
        let launch = Task {
            await FoundationProcessLauncher().launch(
                executable: "/bin/sh", arguments: ["-c", "echo $$ > \(file); exec sleep 30"],
                environment: [:], cwd: "/tmp", timeout: .seconds(60), stdoutLimit: 1024,
                stderrLimit: 1024)
        }
        let pid = try await Self.pid(in: file)
        launch.cancel()
        #expect(try await withDeadline("the cancel") { await launch.value } == .cancelled)
        try await Self.gone(pid)
    }

    @Test func aChildIgnoringSIGTERMIsKilledAfterTheGrace() async throws {
        let file = Self.pidFile()
        defer { try? FileManager.default.removeItem(atPath: file) }
        let launch = Task {
            await FoundationProcessLauncher(terminationGrace: .milliseconds(300)).launch(
                executable: "/bin/sh",
                arguments: ["-c", "trap '' TERM; echo $$ > \(file); while :; do sleep 0.1; done"],
                environment: [:], cwd: "/tmp", timeout: .seconds(60), stdoutLimit: 1024,
                stderrLimit: 1024)
        }
        let pid = try await Self.pid(in: file)
        launch.cancel()
        #expect(try await withDeadline("the cancel") { await launch.value } == .cancelled)
        try await Task.sleep(for: .milliseconds(100))
        #expect(kill(pid, 0) == 0, "SIGTERM is ignored: still running inside the grace")
        try await Self.gone(pid)
    }

    @Test func aLaunchCancelledBeforeItStartsRunsNothing() async {
        let launch = Task {
            while !Task.isCancelled { await Task.yield() }
            return await FoundationProcessLauncher().launch(
                executable: "/bin/sh", arguments: ["-c", "exit 0"], environment: [:], cwd: "/tmp",
                timeout: .seconds(5), stdoutLimit: 10, stderrLimit: 10)
        }
        launch.cancel()
        #expect(await launch.value == .cancelled)
    }
}
