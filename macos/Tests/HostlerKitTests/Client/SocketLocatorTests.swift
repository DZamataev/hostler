import Foundation
import Testing
@testable import HostlerKit

/// The socket path is computed exactly as `paths.rs` computes it (vectors written by Rust's
/// `Paths::under`, Task 3).
@Suite(.timeLimit(.minutes(1))) struct SocketLocatorTests {
    struct Vectors: Decodable {
        struct Case: Decodable {
            let home: String
            let socket: String
            let fallback: Bool
        }
        let cases: [Case]
        let uid: UInt32
        let max_socket_file_len: Int
    }

    static func vectors() throws -> Vectors {
        let url = Bundle.module.resourceURL!.appending(path: "Fixtures/socket-paths.json")
        return try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: url))
    }

    @Test func everyRustVectorGivesTheSameSocket() throws {
        let vectors = try Self.vectors()
        #expect(vectors.max_socket_file_len == SocketLocator.maxSocketFileLength)
        #expect(vectors.cases.count == 6)
        for vector in vectors.cases {
            let location = SocketLocator.location(home: vector.home, euid: vectors.uid)
            #expect(location.socketPath == vector.socket, "home \(vector.home)")
            #expect(location.isFallback == vector.fallback, "home \(vector.home)")
            #expect(location.home == vector.home)
        }
    }

    @Test func theVectorsCoverBothSidesOfTheLimit() throws {
        let lengths = try Self.vectors().cases.map { ($0.home + "/hostler.sock").utf8.count }
        #expect(lengths.contains(100))
        #expect(lengths.contains(101))
    }

    @Test func noHostlerHomeIsTheDefaultHome() throws {
        let location = try SocketLocator.resolve(env: [:], homeDirectory: "/Users/someone",
                                                 euid: 501)
        #expect(location.home
            == "/Users/someone/Library/Application Support/io.github.dzamataev.hostler")
        #expect(location.socketPath
            == "/Users/someone/Library/Application Support/io.github.dzamataev.hostler/hostler.sock")
        #expect(location.fromEnvironment == false)
        #expect(location.isFallback == false)
    }

    @Test func anAbsoluteHostlerHomeIsUsedAsSpelled() throws {
        let location = try SocketLocator.resolve(env: ["HOSTLER_HOME": "/tmp/hostler-home"],
                                                 homeDirectory: "/Users/someone", euid: 501)
        #expect(location.home == "/tmp/hostler-home")
        #expect(location.socketPath == "/tmp/hostler-home/hostler.sock")
        #expect(location.fromEnvironment)
    }

    @Test func aTrailingSlashJoinsAsRustJoins() {
        #expect(SocketLocator.location(home: "/tmp/h/", euid: 501).socketPath
            == "/tmp/h/hostler.sock")
    }

    @Test func aRelativeHostlerHomeIsRefused() {
        #expect(throws: HostlerAppError.badHome("relative/home")) {
            try SocketLocator.resolve(env: ["HOSTLER_HOME": "relative/home"],
                                      homeDirectory: "/Users/someone", euid: 501)
        }
        #expect(throws: HostlerAppError.badHome("")) {
            try SocketLocator.resolve(env: ["HOSTLER_HOME": ""], homeDirectory: "/Users/someone",
                                      euid: 501)
        }
    }

    @Test func theUserHomeIsHOMEWhenSet() {
        #expect(SocketLocator.userHomeDirectory(env: ["HOME": "/Users/x"]) == "/Users/x")
        #expect(SocketLocator.userHomeDirectory(env: ["HOME": ""]) == NSHomeDirectoryForUser(NSUserName()))
    }

    @Test func directoriesCompareByComponents() {
        #expect(SocketLocator.sameDirectory("/a/b", "/a/b/"))
        #expect(SocketLocator.sameDirectory("/a//b/./", "/a/b"))
        #expect(!SocketLocator.sameDirectory("/a/b", "/a/bc"))
        #expect(!SocketLocator.sameDirectory("/a/b/..", "/a"))
    }
}
