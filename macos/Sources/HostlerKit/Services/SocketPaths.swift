import CryptoKit
import Foundation

/// Where the app looks for the service: the home and its socket, found exactly as the CLI
/// finds them (`crates/hostler/src/paths.rs`, spec §9.7).
public struct SocketLocation: Sendable, Equatable {
    /// The home, spelled as it was given (`HOSTLER_HOME`) or the per-user default.
    public let home: String
    /// The socket file the service listens on for this home.
    public let socketPath: String
    /// The socket is the `/tmp/hostler-<euid>-<hash>` fallback for a long home.
    public let isFallback: Bool
    /// The home came from `HOSTLER_HOME`.
    public let fromEnvironment: Bool

    public init(home: String, socketPath: String, isFallback: Bool, fromEnvironment: Bool) {
        self.home = home
        self.socketPath = socketPath
        self.isFallback = isFallback
        self.fromEnvironment = fromEnvironment
    }
}

/// `paths.rs` in Swift: `HOSTLER_HOME` (absolute only) or the per-user data directory, and
/// `<home>/hostler.sock` when that path is at most 100 bytes, else
/// `/tmp/hostler-<euid>-<hex of the first 8 bytes of SHA-256(home)>/hostler.sock`.
public enum SocketLocator {
    public static let homeVariable = "HOSTLER_HOME"
    /// `paths.rs`'s `MAX_SOCKET_FILE_LEN`.
    public static let maxSocketFileLength = 100
    /// `ProjectDirs::from("io.github", "dzamataev", "hostler").data_dir()` on macOS.
    public static let defaultHomeComponents = ["Library", "Application Support",
                                               "io.github.dzamataev.hostler"]

    /// The per-user default home under `homeDirectory` (the user's home folder).
    public static func defaultHome(homeDirectory: String) -> String {
        var path = homeDirectory
        for component in defaultHomeComponents {
            path = join(path, component)
        }
        return path
    }

    /// The user's home folder as Rust's `dirs` finds it: `HOME` when set and not empty, else
    /// the password database's.
    public static func userHomeDirectory(env: [String: String]) -> String {
        if let home = env["HOME"], !home.isEmpty { return home }
        return NSHomeDirectoryForUser(NSUserName()) ?? NSHomeDirectory()
    }

    /// The location for the app's environment. A relative `HOSTLER_HOME` is refused, as the
    /// CLI refuses it (L45): the socket name is derived from its spelling.
    public static func resolve(env: [String: String], homeDirectory: String,
                               euid: UInt32) throws(HostlerAppError) -> SocketLocation {
        guard let custom = env[homeVariable] else {
            return location(home: defaultHome(homeDirectory: homeDirectory), euid: euid,
                            fromEnvironment: false)
        }
        guard custom.hasPrefix("/") else {
            throw .badHome(custom)
        }
        return location(home: custom, euid: euid, fromEnvironment: true)
    }

    /// The socket of `home`, byte for byte as `paths.rs`'s `socket_name`.
    public static func location(home: String, euid: UInt32,
                                fromEnvironment: Bool = true) -> SocketLocation {
        let preferred = join(home, "hostler.sock")
        if preferred.utf8.count <= maxSocketFileLength {
            return SocketLocation(home: home, socketPath: preferred, isFallback: false,
                                  fromEnvironment: fromEnvironment)
        }
        let digest = SHA256.hash(data: Data(home.utf8))
        let hash = digest.prefix(8).map { String(format: "%02x", $0) }.joined()
        return SocketLocation(home: home, socketPath: "/tmp/hostler-\(euid)-\(hash)/hostler.sock",
                              isFallback: true, fromEnvironment: fromEnvironment)
    }

    /// Whether two absolute paths name the same directory as Rust's `Path ==` compares them:
    /// by components, so a trailing slash, a repeated slash or a `.` component do not count.
    /// Symbolic links and `..` are not resolved (Rust does not resolve them either).
    public static func sameDirectory(_ lhs: String, _ rhs: String) -> Bool {
        components(lhs) == components(rhs)
    }

    static func components(_ path: String) -> [Substring] {
        path.split(separator: "/", omittingEmptySubsequences: true).filter { $0 != "." }
    }

    /// `Path::join` for a relative `name`: one slash between, none added after a trailing one.
    static func join(_ base: String, _ name: String) -> String {
        base.hasSuffix("/") ? base + name : base + "/" + name
    }
}
