import Foundation

/// What `HostlerClient` sends requests through (Task 7's connection implements it; tests fake
/// it). It returns the raw `result` of the answer.
public protocol HostlerTransport: Sendable {
    /// Sends `request` and returns the JSON of its `result`; a service error is
    /// `HostlerCallError.service`.
    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data
    /// The same for a method whose answer holds values (`MethodSpec.sensitiveAnswer`): the
    /// result's bytes never pass through a `JSONValue` or a `Data` the transport keeps.
    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
}

/// The raw JSON of an answer that holds values, kept in a `SecretText` (zeroed when the last
/// reference goes). Its description is `<redacted>`.
public struct SensitiveData: Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let bytes: SecretText

    public init(_ bytes: SecretText) {
        self.bytes = bytes
    }

    /// Decodes the answer in place. The decoder's own working copies are outside this type's
    /// reach (`SecretText`'s honest limit).
    public func decode<T: Decodable>(_ type: T.Type) throws -> T {
        try bytes.withBytes { buffer in
            guard let base = buffer.baseAddress, buffer.count > 0 else {
                return try JSONDecoder().decode(T.self, from: Data())
            }
            let view = Data(
                bytesNoCopy: UnsafeMutableRawPointer(mutating: base), count: buffer.count,
                deallocator: .none)
            return try JSONDecoder().decode(T.self, from: view)
        }
    }

    public var description: String { "<redacted>" }
    public var debugDescription: String { "<redacted>" }
}

extension HostlerClient {
    /// Sends `request` with its method's bound and decodes the answer.
    func answer<T: Decodable>(_ request: Request) async throws(HostlerCallError) -> T {
        let spec = request.method.spec
        let data = try await transport.call(request, timeout: spec.timeout)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw .undecodable(spec.method)
        }
    }

    /// Sends `request`, whose answer is JSON `null`.
    func unit(_ request: Request) async throws(HostlerCallError) {
        let spec = request.method.spec
        _ = try await transport.call(request, timeout: spec.timeout)
    }

    /// Sends `request` through the sensitive path and decodes the values it answers.
    func sensitiveAnswer<T: Decodable>(_ request: Request) async throws(HostlerCallError) -> T {
        let spec = request.method.spec
        let raw = try await transport.callSensitive(request, timeout: spec.timeout)
        do {
            return try raw.decode(T.self)
        } catch {
            throw .undecodable(spec.method)
        }
    }
}
