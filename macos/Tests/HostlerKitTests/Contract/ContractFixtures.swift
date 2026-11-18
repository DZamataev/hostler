import Foundation
import HostlerKit

/// The Rust-written fixtures in `Fixtures/contract` (plan P5 Task 3).
enum ContractFixtures {
    static var directory: URL {
        Bundle.module.resourceURL!.appending(path: "Fixtures/contract")
    }

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }

    static func json(_ name: String) throws -> JSONValue {
        try JSONValue(json: data(name))
    }

    static func decode<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        try JSONDecoder().decode(T.self, from: data(name))
    }
}

extension JSONValue {
    /// This value as JSON text.
    func encoded() throws -> Data {
        try JSONEncoder().encode(self)
    }

    /// This object without `key`.
    func removing(_ key: String) -> JSONValue {
        guard case .object(var members) = self else { return self }
        members[key] = nil
        return .object(members)
    }
}

/// `value` encoded and read back as a `JSONValue`, to compare with a fixture regardless of key
/// order.
func jsonOf<T: Encodable>(_ value: T) throws -> JSONValue {
    try JSONValue(json: JSONEncoder().encode(value))
}
