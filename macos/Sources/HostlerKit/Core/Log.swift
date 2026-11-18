import OSLog

/// Shared `os.Logger` categories (the reference app's `Core/Log.swift`).
///
/// Never log a frame, a value or a password. Interpolate anything that comes from the service
/// or the person with `privacy: .private`; log method names and byte counts, not contents.
public enum Log {
    public static let subsystem = "io.github.dzamataev.hostler.app"
    public static let app = Logger(subsystem: subsystem, category: "app")
    public static let client = Logger(subsystem: subsystem, category: "client")
    public static let store = Logger(subsystem: subsystem, category: "store")
    public static let ui = Logger(subsystem: subsystem, category: "ui")
}
