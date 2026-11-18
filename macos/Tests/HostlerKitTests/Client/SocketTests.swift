import Darwin
import Foundation
import Testing
@testable import HostlerKit

/// The POSIX socket against a real Unix socket server (P5-L5: `read(2)`, every wait bounded).
@Suite(.timeLimit(.minutes(1))) struct SocketTests {
    @Test func persistentConnectionReadsLines() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let reader = LineReader(stream: stream)
        let peer = try await server.nextPeer()
        // The server keeps the connection open after the line: no EOF ever comes.
        try await peer.send(#"{"event":{"type":"resync","data":{"reason":"lagged"}},"seq":1}"#)
        let line = try await withDeadline("the line arrives") { try await reader.next() }
        #expect(line == Data(#"{"event":{"type":"resync","data":{"reason":"lagged"}},"seq":1}"#.utf8))
        try await peer.send("second")
        #expect(try await withDeadline("the next line") { try await reader.next() } == Data("second".utf8))
    }

    @Test func closeUnblocksAPendingRead() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        let peer = try await server.nextPeer()
        defer { peer.close() }
        let reader = LineReader(stream: stream)
        let pending = Task { try await reader.next() }
        try await Task.sleep(for: .milliseconds(200))
        stream.close()
        let line = try await withDeadline("close ends the read") { try await pending.value }
        #expect(line == nil)
    }

    @Test func linesSplitAcrossWritesAndManyInOneWrite() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let reader = LineReader(stream: stream)
        let peer = try await server.nextPeer()
        try await peer.sendRaw(Data("ab".utf8))
        try await Task.sleep(for: .milliseconds(50))
        try await peer.sendRaw(Data("c\nd\n\ne\n".utf8))
        var lines: [String] = []
        for _ in 0..<4 {
            let line = try await withDeadline("a line") { try await reader.next() }
            lines.append(String(decoding: line ?? Data(), as: UTF8.self))
        }
        #expect(lines == ["abc", "d", "", "e"])
    }

    @Test func theClientsWritesReachThePeer() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let peer = try await server.nextPeer()
        try await stream.write(Data("{\"id\":1}\n".utf8))
        #expect(try await peer.nextFrame() == .object(["id": .int(1)]))
    }

    @Test func aLineAtTheLimitPassesAndOneOverItClosesTheConnection() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let reader = LineReader(stream: stream)
        let peer = try await server.nextPeer()
        var exact = Data(repeating: UInt8(ascii: "x"), count: Contract.maxFrameBytes)
        exact.append(UInt8(ascii: "\n"))
        try await peer.sendRaw(exact)
        let line = try await withDeadline("the 8 MiB line") { try await reader.next() }
        #expect(line?.count == Contract.maxFrameBytes)

        defer { peer.close() }
        // The client stops reading once the line passes the limit, so this write may never
        // finish: it runs on its own, and the peer's close ends it.
        let over = Data(repeating: UInt8(ascii: "y"), count: Contract.maxFrameBytes + 1)
        Task { try? await peer.sendRaw(over) }
        await #expect(throws: SocketError.protocolError(
            "the service sent a frame longer than \(Contract.maxFrameBytes) bytes")) {
            _ = try await withDeadline("the over-long line") { try await reader.next() }
        }
        // The connection is closed: the peer sees EOF.
        #expect(try await withDeadline("the peer sees the close") {
            try await peer.nextFrameOrEnd()
        } == nil)
    }

    @Test func aPeerClosingMidLineEndsTheStreamWithoutAPartialLine() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let reader = LineReader(stream: stream)
        let peer = try await server.nextPeer()
        try await peer.sendRaw(Data("whole\n{\"id\":1,\"res".utf8))
        peer.close()
        #expect(try await withDeadline("the whole line") { try await reader.next() } == Data("whole".utf8))
        #expect(try await withDeadline("the end") { try await reader.next() } == nil)
    }

    @Test func aMissingSocketIsNotRunning() async throws {
        let path = "/tmp/hk-missing-\(UUID().uuidString.prefix(8))/hostler.sock"
        await #expect(throws: SocketError.notRunning(path: path)) {
            _ = try await POSIXSocketConnector().connect(path: path, timeout: .seconds(5))
        }
    }

    /// A socket file whose listener is gone. Made synchronously here (socket, bind, listen,
    /// close): the earlier version asked the fake server's accept thread to close its listener
    /// and waited at most 5 s for it, so under load the listener could still be open when
    /// `connect` ran (the flake seen in review r1).
    @Test func aStaleSocketFileIsNotRunning() async throws {
        var template = Array("/tmp/hk-stale-XXXXXX".utf8CString)
        let directory = String(cString: try #require(mkdtemp(&template)))
        defer { try? FileManager.default.removeItem(atPath: directory) }
        let path = directory + "/s.sock"
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        try #require(FakeSocketServer.bind(fd, to: path))
        #expect(listen(fd, 1) == 0)
        #expect(close(fd) == 0)
        #expect(FileManager.default.fileExists(atPath: path))
        await #expect(throws: SocketError.notRunning(path: path)) {
            _ = try await POSIXSocketConnector().connect(path: path, timeout: .seconds(5))
        }
    }

    @Test func anUnsearchableDirectoryIsDenied() async throws {
        let server = try FakeSocketServer()
        defer {
            chmod(server.directory, 0o700)
            server.stop()
        }
        chmod(server.directory, 0o000)
        await #expect(throws: SocketError.denied(path: server.path,
                                           reason: String(cString: strerror(EACCES)))) {
            _ = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        }
    }

    @Test func aSocketOfAnotherOwnerIsRefusedBeforeConnecting() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let other = geteuid() + 1
        await #expect(throws: SocketError.foreignOwner(path: server.path, uid: geteuid())) {
            _ = try await POSIXSocketConnector(owner: other).connect(path: server.path, timeout: .seconds(5))
        }
        // Nothing connected.
        await #expect(throws: DeadlineExceeded.self) {
            _ = try await withDeadline("no client", .milliseconds(300)) {
                try await server.nextPeer()
            }
        }
    }

    /// A CLI child holds none of the app's sockets. Foundation's `Process` already closes them
    /// in the child; `FD_CLOEXEC` keeps it so for any other spawn (defence in depth).
    @Test func aChildProcessInheritsNoSocket() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        defer { stream.close() }
        let peer = try await server.nextPeer()
        defer { peer.close() }
        let client = try #require(stream as? POSIXSocketStream)
        let fds = [client.fd, peer.fd]
        let script = fds.map { "[ -e /dev/fd/\($0) ] && echo leaked \($0);" }.joined() + " echo done"
        let outcome = await FoundationProcessLauncher().launch(
            executable: "/bin/sh", arguments: ["-c", script], environment: [:], cwd: "/tmp",
            timeout: .seconds(10), stdoutLimit: 1024, stderrLimit: 1024)
        #expect(outcome == .exited(ProcessOutput(exit: 0, stdout: Data("done\n".utf8),
                                                 stderrTail: Data())))
    }

    @Test func aConnectThatNeverCompletesTimesOut() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        // `connect(2)` answering "try again" for ever: a listener whose queue stays full.
        let connector = POSIXSocketConnector(connectCall: { _, _, _ in
            errno = EAGAIN
            return -1
        })
        let clock = ContinuousClock()
        let start = clock.now
        await #expect(throws: SocketError.timedOut(path: server.path)) {
            _ = try await connector.connect(path: server.path, timeout: .milliseconds(300))
        }
        #expect(clock.now - start < .seconds(3))
    }

    @Test func cancellingAConnectEndsItAtOnce() async throws {
        let server = try FakeSocketServer()
        defer { server.stop() }
        let connector = POSIXSocketConnector(connectCall: { _, _, _ in
            errno = EINPROGRESS
            return -1
        })
        let task = Task { () -> SocketError? in
            do throws(SocketError) {
                _ = try await connector.connect(path: server.path, timeout: .seconds(60))
                return nil
            } catch {
                return error
            }
        }
        try await Task.sleep(for: .milliseconds(200))
        task.cancel()
        #expect(try await withDeadline("the cancel ends the connect") { await task.value }
            == .cancelled)
    }
}

