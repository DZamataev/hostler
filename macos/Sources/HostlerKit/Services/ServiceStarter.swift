import Foundation

/// Starts the service on the person's click (spec §9.7, owner P5-L3). Never automatic: only
/// the banner's Start button calls it; no connection state calls it.
public protocol ServiceStarting: Sendable {
    func start(_ offer: StartOffer) async -> Result<Void, HostlerAppError>
}

/// Runs `<registered exe> service start --json` through the CLI with `HOSTLER_HOME` removed
/// (the registration serves the default home only). The CLI asks launchd to start the
/// registered job; the app itself never installs, registers, replaces or spawns a service.
public struct CLIServiceStarter: ServiceStarting {
    let runner: any HostlerCommandRunning
    let workingDirectory: String

    public init(runner: any HostlerCommandRunning, workingDirectory: String) {
        self.runner = runner
        self.workingDirectory = workingDirectory
    }

    public func start(_ offer: StartOffer) async -> Result<Void, HostlerAppError> {
        let executable: String
        switch offer {
        case .start(let exe): executable = exe
        case .typeCommand(let command): return .failure(.nothingToStart(command: command))
        }
        let result = await runner.run(CLIInvocation(
            executable: executable, arguments: ["service", "start"], cwd: workingDirectory,
            timeout: CLIInvocation.defaultTimeout, keepsHostlerHome: false))
        if let error = result.appError { return .failure(error) }
        return .success(())
    }
}
