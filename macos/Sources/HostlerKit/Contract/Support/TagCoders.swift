import Foundation

/// The errors of the generated enums' coders (`Generated.swift`): an unknown tag or variant
/// names the Swift type, never the input (it may be anything a peer sent).
enum ContractCoding {
    /// A tagged enum (`#[serde(tag = …)]`) met a tag it does not know.
    static func unknownTag<Key: CodingKey>(
        forKey key: Key, in container: KeyedDecodingContainer<Key>, type: String
    ) -> DecodingError {
        DecodingError.dataCorruptedError(
            forKey: key, in: container, debugDescription: "\(type): an unknown tag")
    }

    /// An externally tagged enum met neither a known string nor a one-key object it knows.
    static func unknownVariant(_ decoder: any Decoder, type: String) -> DecodingError {
        DecodingError.dataCorrupted(
            DecodingError.Context(
                codingPath: decoder.codingPath, debugDescription: "\(type): an unknown variant"))
    }
}
