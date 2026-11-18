import Foundation
import Testing
@testable import HostlerKit

@Suite(.timeLimit(.minutes(1))) struct ServiceStarterTests {
    @Test func aStartOfferRunsExactlyServiceStartWithoutHostlerHome() async {
        let runner = FakeCommandRunner()
        let starter = CLIServiceStarter(runner: runner, workingDirectory: "/Users/x")
        let result = await starter.start(.start(executable: "/opt/homebrew/bin/hostler"))
        #expect((try? result.get()) != nil)
        #expect(runner.invocations == [CLIInvocation(
            executable: "/opt/homebrew/bin/hostler", arguments: ["service", "start"],
            cwd: "/Users/x", timeout: .seconds(30), keepsHostlerHome: false)])
    }

    @Test func theCommandLineIsServiceStartJson() async {
        let launcher = FakeProcessLauncher(.exited(ProcessOutput(
            exit: 0, stdout: Data(#"{"ok":true,"data":null,"error":null}"#.utf8), stderrTail: Data())))
        let cli = HostlerCLI(launcher: launcher,
                             environment: ["HOSTLER_HOME": "/tmp/custom", "PATH": "/usr/bin"])
        let result = await CLIServiceStarter(runner: cli, workingDirectory: "/Users/x")
            .start(.start(executable: "/opt/hostler"))
        #expect((try? result.get()) != nil)
        #expect(launcher.launches.map(\.arguments) == [["service", "start", "--json"]])
        #expect(launcher.launches[0].environment == ["PATH": "/usr/bin"])
    }

    @Test func aTypeCommandOfferRunsNothing() async {
        let runner = FakeCommandRunner()
        let result = await CLIServiceStarter(runner: runner, workingDirectory: "/")
            .start(.typeCommand("hostler service install"))
        #expect(failure(result) == .nothingToStart(command: "hostler service install"))
        #expect(runner.invocations.isEmpty)
    }

    @Test func aFailedStartIsTheServicesError() async {
        let error = HostlerError(code: .serviceUnavailable, message: "launchd refused",
                                 details: .object([:]), recovery: "hostler doctor")
        let runner = FakeCommandRunner(result: .failed(error))
        let result = await CLIServiceStarter(runner: runner, workingDirectory: "/")
            .start(.start(executable: "/opt/hostler"))
        #expect(failure(result) == .service(message: "launchd refused", recovery: "hostler doctor"))
    }
}

/// The error of a failed `Result<Void, HostlerAppError>`; `nil` on success.
func failure(_ result: Result<Void, HostlerAppError>) -> HostlerAppError? {
    if case .failure(let error) = result { error } else { nil }
}
