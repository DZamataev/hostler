import Foundation
import Testing
@testable import HostlerKit

/// Start only on the default home with a matching registration (spec §9.7, R1-10).
@Suite(.timeLimit(.minutes(1))) struct StartPolicyTests {
    static let user = "/Users/someone"
    static let defaultHome = SocketLocator.defaultHome(homeDirectory: user)
    static let registered = Registration(executable: "/opt/homebrew/bin/hostler",
                                         setsEnvironment: false)

    static func offer(env: [String: String], registration: Registration? = registered,
                      existing: Set<String> = ["/opt/homebrew/bin/hostler"]) throws -> StartOffer {
        let location = try SocketLocator.resolve(env: env, homeDirectory: user, euid: 501)
        return startOffer(home: location, defaultHome: defaultHome, registration: registration,
                          euid: 501, fileExists: { existing.contains($0) })
    }

    @Test func theDefaultHomeWithARegistrationAndItsBinaryOffersStart() throws {
        #expect(try Self.offer(env: [:]) == .start(executable: "/opt/homebrew/bin/hostler"))
    }

    @Test func aScratchHostlerHomeWithTheSameRegistrationOffersOnlyTheCommand() throws {
        #expect(try Self.offer(env: ["HOSTLER_HOME": "/tmp/scratch home"])
            == .typeCommand("HOSTLER_HOME='/tmp/scratch home' hostler service run"))
        #expect(try Self.offer(env: ["HOSTLER_HOME": "/tmp/scratch"])
            == .typeCommand("HOSTLER_HOME=/tmp/scratch hostler service run"))
    }

    @Test func aHostlerHomeEqualToTheDefaultWithATrailingSlashOffersStart() throws {
        #expect(try Self.offer(env: ["HOSTLER_HOME": Self.defaultHome + "/"])
            == .start(executable: "/opt/homebrew/bin/hostler"))
        #expect(try Self.offer(env: ["HOSTLER_HOME": Self.defaultHome])
            == .start(executable: "/opt/homebrew/bin/hostler"))
    }

    @Test func theDefaultHomeSpelledDifferentlyOnAFallbackSocketIsNotTheRegisteredOnes() {
        // A long user folder: the default home's socket is the /tmp fallback, whose hash is
        // over the spelling. A trailing slash names the same folder but another socket, so
        // the registered service would never be found there.
        let user = "/Users/" + String(repeating: "u", count: 60)
        let defaultHome = SocketLocator.defaultHome(homeDirectory: user)
        let slashed = SocketLocator.location(home: defaultHome + "/", euid: 501)
        #expect(slashed.isFallback)
        let offer = startOffer(home: slashed, defaultHome: defaultHome,
                               registration: Self.registered, euid: 501, fileExists: { _ in true })
        #expect(offer == .typeCommand("HOSTLER_HOME='\(defaultHome)/' hostler service run"))
    }

    @Test func aRegistrationWithAnEnvironmentOffersInstall() throws {
        let withEnvironment = Registration(executable: "/opt/homebrew/bin/hostler",
                                           setsEnvironment: true)
        #expect(try Self.offer(env: [:], registration: withEnvironment)
            == .typeCommand("hostler service install"))
    }

    @Test func noPlistOffersInstall() throws {
        #expect(try Self.offer(env: [:], registration: nil)
            == .typeCommand("hostler service install"))
    }

    @Test func aMissingBinaryOffersInstall() throws {
        #expect(try Self.offer(env: [:], existing: [])
            == .typeCommand("hostler service install"))
    }
}

@Suite(.timeLimit(.minutes(1))) struct RegistrationTests {
    static func plist(_ body: String) -> Data {
        Data("""
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict>
        <key>Label</key><string>io.github.dzamataev.hostler</string>
        \(body)
        </dict></plist>
        """.utf8)
    }

    static let arguments = """
        <key>ProgramArguments</key><array><string>/opt/hostler</string><string>service</string><string>run</string><string>--managed</string></array>
        """

    @Test func hostlersOwnPlistIsARegistrationWithoutEnvironment() {
        #expect(LaunchAgentRegistration.parse(Self.plist(Self.arguments))
            == Registration(executable: "/opt/hostler", setsEnvironment: false))
    }

    @Test func anEnvironmentIsNoticed() {
        let body = Self.arguments
            + "<key>EnvironmentVariables</key><dict><key>HOSTLER_HOME</key><string>/x</string></dict>"
        #expect(LaunchAgentRegistration.parse(Self.plist(body))
            == Registration(executable: "/opt/hostler", setsEnvironment: true))
        let empty = Self.arguments + "<key>EnvironmentVariables</key><dict></dict>"
        #expect(LaunchAgentRegistration.parse(Self.plist(empty))?.setsEnvironment == false)
    }

    @Test func noProgramArgumentsOrGarbageIsNoRegistration() {
        #expect(LaunchAgentRegistration.parse(Self.plist("")) == nil)
        #expect(LaunchAgentRegistration.parse(Data("not a plist".utf8)) == nil)
    }

    @Test func anAbsentFileIsNoRegistration() {
        let reader = LaunchAgentRegistration(plistPath: "/nonexistent/hostler-\(UUID()).plist")
        #expect(reader.registration() == nil)
    }

    @Test func thePlistIsTheLaunchAgentOfTheLabel() {
        #expect(LaunchAgentRegistration(homeDirectory: "/Users/someone").plistPath
            == "/Users/someone/Library/LaunchAgents/io.github.dzamataev.hostler.plist")
    }

    @Test func aFileOnDiskIsRead() throws {
        let path = FileManager.default.temporaryDirectory
            .appending(path: "hostler-registration-\(UUID()).plist").path
        try Self.plist(Self.arguments).write(to: URL(fileURLWithPath: path))
        defer { try? FileManager.default.removeItem(atPath: path) }
        #expect(LaunchAgentRegistration(plistPath: path).registration()?.executable == "/opt/hostler")
    }
}
