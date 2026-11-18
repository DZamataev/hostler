//! The Swift contract of the macOS app (plan P5 Task 3, spec §6.3: "the app's Swift types are
//! generated from the Rust contract, never hand-written, and a test fails when the generated
//! file is out of date").
//!
//! `generated_swift_is_current` renders, from the `schemars` schemas of the contract types and
//! the method table:
//! - `macos/Sources/HostlerKit/Contract/Generated.swift`: every wire type, `Method` and its
//!   table, and the app's facade `HostlerClient`;
//! - `macos/Sources/HostlerKit/Contract/contract-schema.json`: the schema, key-sorted, so a
//!   review sees a contract change as JSON;
//! - `macos/Tests/HostlerKitTests/Contract/GeneratedSendableCheck.swift` and
//!   `GeneratedFixtureIndex.swift`;
//! - `macos/Tests/HostlerKitTests/Fixtures/contract/*.json`, the values Swift must round-trip,
//!   and `Fixtures/socket-paths.json` (Unix: the socket path vectors).
//!
//! It compares them with the files on disk and fails when one differs; with
//! `HOSTLER_WRITE_SWIFT=1` (`scripts/macos.sh contract --write`) it writes them instead.

mod emit;
mod samples;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use schemars::generate::{Contract, SchemaSettings};
use schemars::{Schema, SchemaGenerator};
use serde_json::{Map, Value, json};

use self::emit::{Def, MethodView, Model, Payload, Ty, camel, swift_name, swift_ty};
use self::samples::AnswerSample;
use super::methods::{Answer, METHODS};
use super::wire::{self, ClientFrame, Event, Request, ServerFrame};
use super::{CONTRACT_VERSION, Envelope, ErrorCode, HostlerError};

const GENERATED: &str = "macos/Sources/HostlerKit/Contract/Generated.swift";
const SCHEMA: &str = "macos/Sources/HostlerKit/Contract/contract-schema.json";
const SENDABLE_CHECK: &str = "macos/Tests/HostlerKitTests/Contract/GeneratedSendableCheck.swift";
const FIXTURE_INDEX: &str = "macos/Tests/HostlerKitTests/Contract/GeneratedFixtureIndex.swift";
const FIXTURES: &str = "macos/Tests/HostlerKitTests/Fixtures/contract";
#[cfg(unix)]
const SOCKET_PATHS: &str = "macos/Tests/HostlerKitTests/Fixtures/socket-paths.json";

const STALE: &str = "the generated Swift contract is stale; run: scripts/macos.sh contract --write";

/// The hand-written Swift types next to the generated ones (`Contract/Support`); no contract
/// type may take their names.
const SUPPORT_TYPES: [&str; 9] = [
    "JSONValue",
    "SecretText",
    "SensitiveData",
    "HostlerCallError",
    "HostlerTransport",
    "HostlerClient",
    "HostlerMethod",
    "MethodSpec",
    "Contract",
];

fn header(what: &str) -> String {
    format!(
        "// {what}\n//\n// Generated from the Rust contract by crates/hostler/src/contract/swift (plan P5 \
         Task 3).\n// Never edit by hand: run scripts/macos.sh contract --write.\n"
    )
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `$defs` name a `$ref` schema points at.
fn ref_name(schema: &Schema) -> String {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .unwrap_or_else(|| panic!("not a named type: {schema:?}"))
        .to_owned()
}

/// What an answer is in Swift.
enum AnswerTy {
    Unit,
    One(String),
    List(String),
}

impl AnswerTy {
    fn swift(&self) -> Option<String> {
        match self {
            AnswerTy::Unit => None,
            AnswerTy::One(n) => Some(swift_name(n)),
            AnswerTy::List(n) => Some(format!("[{}]", swift_name(n))),
        }
    }

    fn schema(&self) -> Value {
        match self {
            AnswerTy::Unit => json!({ "type": "null" }),
            AnswerTy::One(n) => json!({ "$ref": format!("#/$defs/{n}") }),
            AnswerTy::List(n) => {
                json!({ "type": "array", "items": { "$ref": format!("#/$defs/{n}") } })
            }
        }
    }
}

/// Every root registered in one generator, so each contract's `$defs` holds every type.
struct Registered {
    defs: Map<String, Value>,
    /// Per distinct answer of the method table.
    answers: BTreeMap<String, AnswerTy>,
    /// `DoctorReport` and the like: name → schema name.
    other_answers: Vec<(String, String)>,
    /// The schema name of each extra value, in `samples::extra_values()`'s order.
    extra: Vec<String>,
    /// The schema name of each envelope's `data`, in `samples::envelopes()`'s order.
    envelope_data: Vec<String>,
}

fn answer_key(a: Answer) -> String {
    format!("{a:?}")
}

fn register(contract: Contract) -> Registered {
    let mut g: SchemaGenerator = SchemaSettings::default()
        .with(|s| s.contract = contract)
        .into_generator();
    let named = |g: &mut SchemaGenerator, f: fn(&mut SchemaGenerator) -> Schema| ref_name(&f(g));
    assert_eq!(ref_name(&g.subschema_for::<Request>()), "Request");
    assert_eq!(ref_name(&g.subschema_for::<Event>()), "Event");
    assert_eq!(ref_name(&g.subschema_for::<HostlerError>()), "HostlerError");
    let mut answers = BTreeMap::new();
    for m in METHODS {
        let ty = match samples::answer(m.answer) {
            AnswerSample::Unit => AnswerTy::Unit,
            AnswerSample::One(s) => AnswerTy::One(named(&mut g, s.schema)),
            AnswerSample::List(s) => AnswerTy::List(named(&mut g, s.schema)),
        };
        answers.insert(answer_key(m.answer), ty);
    }
    let other_answers = samples::other_answers()
        .into_iter()
        .map(|(key, s)| (key.to_owned(), named(&mut g, s.schema)))
        .collect();
    let extra = samples::extra_values()
        .into_iter()
        .map(|s| named(&mut g, s.schema))
        .collect();
    // The CLI's `--json` envelope, generic over its `data` (spec §6.3, P5-L7).
    assert_eq!(
        ref_name(&g.subschema_for::<Envelope<samples::TypeParam>>()),
        "Envelope"
    );
    let envelope_data = samples::envelopes()
        .into_iter()
        .map(|e| named(&mut g, e.data.schema))
        .collect();
    Registered {
        defs: g.take_definitions(true),
        answers,
        other_answers,
        extra,
        envelope_data,
    }
}

/// Removes the doc texts: a comment edit in Rust is not a contract change.
fn strip_docs(schema: &mut Value) {
    match schema {
        Value::Object(o) => {
            o.remove("description");
            for (k, v) in o.iter_mut() {
                match k.as_str() {
                    "properties" | "$defs" => {
                        if let Value::Object(named) = v {
                            named.values_mut().for_each(strip_docs);
                        }
                    }
                    "default" | "const" | "enum" | "examples" => {}
                    _ => strip_docs(v),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip_docs),
        _ => {}
    }
}

/// Everything the generator knows, read once.
struct Generator {
    de: Registered,
    model: Model,
}

impl Generator {
    fn new() -> Generator {
        let de = register(Contract::Deserialize);
        let ser = register(Contract::Serialize);
        let model = Model::read(&de.defs, &ser.defs);
        Generator { de, model }
    }

    fn request_variants(&self) -> &[emit::Variant] {
        match self.model.def("Request") {
            Def::Adjacent { variants, .. } => variants,
            _ => panic!("Request is not adjacently tagged"),
        }
    }

    fn methods(&self) -> Vec<MethodView<'_>> {
        let variants = self.request_variants();
        assert_eq!(variants.len(), METHODS.len(), "Request and METHODS differ");
        METHODS
            .iter()
            .map(|m| MethodView {
                wire: m.name,
                variant: variants
                    .iter()
                    .find(|v| v.wire == m.name)
                    .unwrap_or_else(|| panic!("{} is not a Request variant", m.name)),
                timeout_ms: m.timeout_ms,
                gated: m.gated,
                app: m.app,
                sensitive_request: wire::sensitive_request(m.name),
                sensitive_answer: wire::sensitive_answer(m.name),
                answer: self.de.answers[&answer_key(m.answer)].swift(),
            })
            .collect()
    }

    fn generated_swift(&self) -> String {
        let mut out = header("The wire contract of hostler, for the app.");
        out.push_str("\nimport Foundation\n\n");
        out.push_str(
            "/// The contract's version and frame limit (`CONTRACT_VERSION`, `MAX_FRAME_BYTES`).\n",
        );
        out.push_str("public enum Contract {\n");
        out.push_str(&format!(
            "    public static let version: UInt32 = {CONTRACT_VERSION}\n"
        ));
        out.push_str(&format!(
            "    public static let maxFrameBytes = {}\n}}\n\n",
            crate::service::server::MAX_FRAME_BYTES
        ));
        out.push_str("// MARK: - Methods\n\n");
        out.push_str(&emit::render_methods(&self.methods()));
        out.push_str("\n// MARK: - Types\n\n");
        out.push_str(emit::render_types(&self.model).trim_end());
        out.push('\n');
        out
    }

    fn schema_json(&self) -> String {
        let mut defs = Value::Object(self.de.defs.clone());
        strip_docs(&mut defs);
        let methods: Vec<Value> = METHODS
            .iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "answer": self.de.answers[&answer_key(m.answer)].schema(),
                    "timeout_ms": m.timeout_ms,
                    "gated": m.gated,
                    "app": m.app,
                    "sensitive_request": wire::sensitive_request(m.name),
                    "sensitive_answer": wire::sensitive_answer(m.name),
                })
            })
            .collect();
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "hostler contract",
            "contract_version": CONTRACT_VERSION,
            "max_frame_bytes": crate::service::server::MAX_FRAME_BYTES,
            "request": { "$ref": "#/$defs/Request" },
            "event": { "$ref": "#/$defs/Event" },
            "error": { "$ref": "#/$defs/HostlerError" },
            "methods": methods,
            "$defs": defs,
        });
        pretty(&schema)
    }

    /// Every fixture: file name → (the type it is, its JSON).
    fn fixtures(&self) -> BTreeMap<String, (Fixture, Value)> {
        let mut out = BTreeMap::new();
        let mut put = |name: String, kind: Fixture, value: Value| {
            assert!(
                out.insert(name.clone(), (kind, value)).is_none(),
                "{name} twice"
            );
        };
        for (i, request) in wire::tests::all_requests().into_iter().enumerate() {
            let method = super::methods::method_name(&request);
            let frame = ClientFrame {
                id: i as u64 + 1,
                request,
            };
            put(
                format!("request.{method}.json"),
                Fixture::Request,
                serde_json::to_value(frame).unwrap(),
            );
        }
        for (i, event) in wire::tests::all_events().into_iter().enumerate() {
            let kind = event.kind();
            let frame = ServerFrame::Event {
                event,
                seq: i as u64 + 1,
            };
            put(
                format!("event.{kind}.json"),
                Fixture::Event,
                serde_json::to_value(frame).unwrap(),
            );
        }
        let mut answered = BTreeSet::new();
        for m in METHODS {
            let key = answer_key(m.answer);
            if !answered.insert(key.clone()) {
                continue;
            }
            let value = match samples::answer(m.answer) {
                AnswerSample::Unit => continue,
                AnswerSample::One(s) => s.value,
                AnswerSample::List(s) => Value::Array(vec![s.value]),
            };
            let ty = self.de.answers[&key].swift().unwrap();
            put(format!("answer.{key}.json"), Fixture::Value(ty), value);
        }
        for ((key, name), (_, s)) in self.de.other_answers.iter().zip(samples::other_answers()) {
            put(
                format!("answer.{key}.json"),
                Fixture::Value(swift_name(name)),
                s.value,
            );
        }
        for code in self.error_codes() {
            let wire = serde_json::to_value(code).unwrap();
            put(
                format!("error.{}.json", wire.as_str().unwrap()),
                Fixture::Value("HostlerError".into()),
                serde_json::to_value(samples::error(code)).unwrap(),
            );
        }
        for (data, e) in self.de.envelope_data.iter().zip(samples::envelopes()) {
            put(
                format!("envelope.{}.json", e.name),
                Fixture::Envelope(swift_name(data)),
                e.envelope,
            );
        }
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (name, s) in self.de.extra.iter().zip(samples::extra_values()) {
            let n = counts.entry(name).or_default();
            *n += 1;
            put(
                format!("value.{}.{n}.json", swift_name(name)),
                Fixture::Value(swift_name(name)),
                s.value,
            );
        }
        put(
            "frame.response.json".into(),
            Fixture::Frame,
            serde_json::to_value(ServerFrame::Response {
                id: 7,
                result: match samples::answer(Answer::QueueSnapshot) {
                    AnswerSample::One(s) => s.value,
                    _ => unreachable!(),
                },
            })
            .unwrap(),
        );
        put(
            "frame.error.json".into(),
            Fixture::Frame,
            serde_json::to_value(ServerFrame::Error {
                id: 8,
                error: samples::error(ErrorCode::ProofRequired),
            })
            .unwrap(),
        );
        put(
            "frame.event.json".into(),
            Fixture::Frame,
            serde_json::to_value(ServerFrame::Event {
                event: Event::Resync {
                    reason: wire::ResyncReason::Lagged,
                },
                seq: 9,
            })
            .unwrap(),
        );
        out
    }

    /// Every `ErrorCode`, read from its schema (the sample's exhaustive match keeps them in step).
    fn error_codes(&self) -> Vec<ErrorCode> {
        match self.model.def("ErrorCode") {
            Def::StringEnum(cases) => cases
                .iter()
                .map(|c| serde_json::from_value(json!(c)).unwrap())
                .collect(),
            _ => panic!("ErrorCode is not a string enum"),
        }
    }

    fn sendable_check(&self) -> String {
        let mut out = header(
            "Every generated type is Sendable (R2-8): this file does not compile otherwise.",
        );
        // Every framework the app sees: a generated name that one of them also declares is
        // ambiguous here and fails the build (as `Method` and `Selector` would).
        out.push_str(
            "\nimport AppKit\nimport Foundation\nimport HostlerKit\nimport OSLog\nimport SwiftUI\nimport \
             Testing\n\n",
        );
        out.push_str("private func assertSendable<T: Sendable>(_: T.Type) {}\n\n");
        out.push_str("@Test func everyGeneratedTypeIsSendable() {\n");
        // A generic type is checked with `JSONValue` for its parameter.
        let mut names: Vec<String> = self
            .model
            .defs
            .iter()
            .map(|(n, def)| match def {
                Def::Struct(fields) if fields.iter().any(|f| has_param(&f.ty)) => {
                    format!("{}<JSONValue>", swift_name(n))
                }
                _ => swift_name(n),
            })
            .collect();
        names.extend(
            [
                "HostlerClient",
                "HostlerMethod",
                "MethodSpec",
                "SecretText",
                "SensitiveData",
                "JSONValue",
                "HostlerCallError",
            ]
            .map(String::from),
        );
        names.sort();
        for n in &names {
            out.push_str(&format!("    assertSendable({n}.self)\n"));
        }
        out.push_str("}\n\n");
        out.push_str("// Each type that can hold a SecretText crosses into a detached task.\n");
        for name in self
            .model
            .defs
            .keys()
            .filter(|n| self.model.holds_secret(n))
        {
            let n = swift_name(name);
            out.push_str(&format!(
                "\nprivate func cross(_ value: {n}) async {{\n    await Task.detached {{ _ = value }}.value\n}}\n"
            ));
        }
        out
    }

    fn fixture_index(&self, fixtures: &BTreeMap<String, (Fixture, Value)>) -> String {
        let mut out = header("The contract fixtures and the type each one decodes into.");
        out.push_str("\nimport Foundation\nimport HostlerKit\n\n");
        out.push_str("/// Every file in `Fixtures/contract`, as Rust wrote them.\n");
        out.push_str("let contractFixtureNames: [String] = [\n");
        for name in fixtures.keys() {
            out.push_str(&format!("    \"{name}\",\n"));
        }
        out.push_str("]\n\n");
        out.push_str(
            "/// Decodes a value fixture as its type and encodes it again; `nil` for a request, an \
             event or\n/// a frame, which the tests take apart themselves.\n",
        );
        out.push_str(
            "func reencodeContractFixture(_ name: String, _ data: Data) throws -> Data? {\n",
        );
        out.push_str("    switch name {\n");
        for (name, (kind, _)) in fixtures {
            let ty = match kind {
                Fixture::Value(ty) => ty.clone(),
                Fixture::Envelope(data) => format!("Envelope<{data}>"),
                _ => continue,
            };
            out.push_str(&format!(
                "    case \"{name}\": try reencode({ty}.self, data)\n"
            ));
        }
        out.push_str("    default: nil\n    }\n}\n\n");
        out.push_str(
            "private func reencode<T: Codable>(_: T.Type, _ data: Data) throws -> Data {\n    try \
             JSONEncoder().encode(JSONDecoder().decode(T.self, from: data))\n}\n",
        );
        out
    }

    /// Every output: path from the repository root → content.
    fn outputs(&self) -> BTreeMap<String, String> {
        let fixtures = self.fixtures();
        let mut out = BTreeMap::new();
        out.insert(GENERATED.to_owned(), self.generated_swift());
        out.insert(SCHEMA.to_owned(), self.schema_json());
        out.insert(SENDABLE_CHECK.to_owned(), self.sendable_check());
        out.insert(FIXTURE_INDEX.to_owned(), self.fixture_index(&fixtures));
        for (name, (_, value)) in &fixtures {
            out.insert(format!("{FIXTURES}/{name}"), pretty(value));
        }
        #[cfg(unix)]
        out.insert(SOCKET_PATHS.to_owned(), socket_vectors());
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Fixture {
    /// A client frame: `{"id", "method", "params"}`.
    Request,
    /// An event frame: `{"event", "seq"}`.
    Event,
    /// A response, error or event frame.
    Frame,
    /// A value of the named Swift type.
    Value(String),
    /// A CLI `--json` envelope whose `data` is of the named Swift type.
    Envelope(String),
}

fn has_param(ty: &Ty) -> bool {
    match ty {
        Ty::Param(_) => true,
        Ty::Array(t) | Ty::Map(t) | Ty::Optional(t) => has_param(t),
        _ => false,
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

/// The socket paths `paths.rs` gives for a few homes (Task 7's vectors): the preferred
/// `<home>/hostler.sock` up to 100 bytes, else `/tmp/hostler-<euid>-<hash>/hostler.sock`. The
/// euid is written as a fixed 501, so the file is the same for every user.
#[cfg(unix)]
fn socket_vectors() -> String {
    const UID: u32 = 501;
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    let euid = unsafe { libc::geteuid() };
    let at_limit = format!(
        "/h/{}",
        "a".repeat(100 - "/h/".len() - "/hostler.sock".len())
    );
    let homes = [
        "/Users/someone/Library/Application Support/io.github.dzamataev.hostler".to_owned(),
        "/tmp/hostler-home".to_owned(),
        at_limit.clone(),
        format!("{at_limit}b"),
        "/var/folders/7x/k2m4n6p8q0r2s4t6v8w0y2z40000gn/T/hostler-swift-integration-0123456789abcdef/home".to_owned(),
        "/Users/someone/Проекты/hostler-домашний-каталог-с-длинным-именем/home".to_owned(),
    ];
    let cases: Vec<Value> = homes
        .iter()
        .map(|home| {
            let paths = crate::paths::Paths::under(PathBuf::from(home), true);
            let crate::paths::SocketName::File(socket) = paths.socket else {
                panic!("a Unix socket is a file");
            };
            let socket = socket.to_str().unwrap().to_owned();
            let ours = format!("/tmp/hostler-{euid}-");
            let fallback = socket.starts_with(&ours);
            let socket = if fallback {
                socket.replacen(&ours, &format!("/tmp/hostler-{UID}-"), 1)
            } else {
                socket
            };
            json!({ "home": home, "socket": socket, "fallback": fallback })
        })
        .collect();
    pretty(&json!({
        "uid": UID,
        "max_socket_file_len": 100,
        "cases": cases,
    }))
}

/// The file on disk, with Windows line endings read as `\n`; `None` when it is missing.
fn on_disk(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.replace("\r\n", "\n"))
}

/// Fixture files on disk that no sample writes any more.
fn leftover_fixtures(outputs: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let dir = repo_root().join(FIXTURES);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            !outputs.contains_key(&format!("{FIXTURES}/{name}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_swift_is_current() {
        let outputs = Generator::new().outputs();
        let root = repo_root();
        if std::env::var_os("HOSTLER_WRITE_SWIFT").is_some_and(|v| v == "1") {
            for (path, content) in &outputs {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                if on_disk(&path).as_deref() != Some(content.as_str()) {
                    std::fs::write(&path, content).unwrap();
                }
            }
            for path in leftover_fixtures(&outputs) {
                std::fs::remove_file(path).unwrap();
            }
            return;
        }
        let mut stale: Vec<String> = outputs
            .iter()
            .filter(|(path, content)| {
                on_disk(&root.join(path)).as_deref() != Some(content.as_str())
            })
            .map(|(path, _)| path.clone())
            .collect();
        stale.extend(
            leftover_fixtures(&outputs)
                .iter()
                .map(|p| format!("{} (left over)", p.display())),
        );
        assert!(stale.is_empty(), "{STALE}\n{}", stale.join("\n"));
    }

    /// The facade offers exactly the `app` methods, so it has no materialize call.
    #[test]
    fn every_app_method_has_a_facade_call() {
        let swift = Generator::new().generated_swift();
        let facade = swift
            .split("extension HostlerClient {\n")
            .nth(1)
            .expect("the facade")
            .split("\n}\n")
            .next()
            .unwrap();
        let calls: BTreeSet<String> = facade
            .lines()
            .filter_map(|l| l.trim().strip_prefix("public func "))
            .map(|l| l.split('(').next().unwrap().to_owned())
            .collect();
        let app: BTreeSet<String> = METHODS
            .iter()
            .filter(|m| m.app)
            .map(|m| camel(m.name))
            .collect();
        assert_eq!(calls, app);
        for materialize in ["secretMaterialize", "accountMaterialize"] {
            assert!(
                !facade.contains(materialize),
                "the facade calls {materialize}"
            );
        }
        for m in METHODS.iter().filter(|m| m.gated) {
            assert!(calls.contains(&camel(m.name)), "{}", m.name);
        }
        // The two value answers go through the sensitive path.
        for name in ["secretReveal", "accountReveal"] {
            let body = facade
                .split(&format!("public func {name}("))
                .nth(1)
                .unwrap();
            let body = body.split("\n    }").next().unwrap();
            assert!(body.contains("try await sensitiveAnswer("), "{name}");
        }
    }

    /// Every request and event variant, every answer type of the table and every error code
    /// has its fixture.
    #[test]
    fn samples_cover_every_variant() {
        let g = Generator::new();
        let fixtures = g.fixtures();
        for v in g.request_variants() {
            assert!(
                fixtures.contains_key(&format!("request.{}.json", v.wire)),
                "request {}",
                v.wire
            );
        }
        let Def::Adjacent { variants, .. } = g.model.def("Event") else {
            panic!("Event is not adjacently tagged");
        };
        for v in variants {
            assert!(
                fixtures.contains_key(&format!("event.{}.json", v.wire)),
                "event {}",
                v.wire
            );
        }
        for m in METHODS {
            if m.answer != Answer::Unit {
                assert!(
                    fixtures.contains_key(&format!("answer.{:?}.json", m.answer)),
                    "answer {:?}",
                    m.answer
                );
            }
        }
        let codes = g.error_codes();
        assert!(codes.len() >= 40, "{} codes", codes.len());
        for code in codes {
            let wire = serde_json::to_value(code).unwrap();
            let name = format!("error.{}.json", wire.as_str().unwrap());
            let (_, value) = &fixtures[&name];
            assert_eq!(value["code"], wire, "{name}");
        }
        for name in [
            "answer.MaterializedSecret.json",
            "answer.MaterializedAccount.json",
            "answer.UnlockOutcome.json",
            "answer.DoctorReport.json",
            "frame.response.json",
            "frame.error.json",
            "frame.event.json",
            "envelope.DoctorReport.json",
            "envelope.Claim.json",
            "envelope.SeizureRecord.json",
            "envelope.error.json",
        ] {
            assert!(fixtures.contains_key(name), "{name}");
        }
    }

    /// Every fixture reads as the model's type, and together they reach every variant of every
    /// tagged enum, so the Swift tests run every hand-rolled coder branch.
    #[test]
    fn fixtures_reach_every_enum_variant() {
        let g = Generator::new();
        let mut hits = BTreeSet::new();
        for (name, (kind, value)) in g.fixtures() {
            let (ty, value) = match kind {
                Fixture::Request => {
                    let mut v = value.clone();
                    v.as_object_mut().unwrap().remove("id").expect("an id");
                    (Ty::Ref("Request".into()), v)
                }
                Fixture::Event => {
                    assert!(value["seq"].is_u64(), "{name}");
                    (Ty::Ref("Event".into()), value["event"].clone())
                }
                Fixture::Frame => continue,
                Fixture::Envelope(data) => {
                    let data_ty = Ty::Optional(Box::new(Ty::Ref(schema_name(&g, &data))));
                    g.model.walk(&data_ty, &value["data"], &name, &mut hits);
                    (Ty::Ref("Envelope".into()), value)
                }
                Fixture::Value(swift) => {
                    let ty = if let Some(inner) = swift.strip_prefix('[') {
                        let inner = inner.strip_suffix(']').unwrap();
                        Ty::Array(Box::new(Ty::Ref(schema_name(&g, inner))))
                    } else {
                        Ty::Ref(schema_name(&g, &swift))
                    };
                    (ty, value)
                }
            };
            g.model.walk(&ty, &value, &name, &mut hits);
        }
        let mut missing = Vec::new();
        for (name, def) in &g.model.defs {
            for v in def.variants() {
                if !hits.contains(&(name.clone(), v.wire.clone())) {
                    missing.push(format!("{name}.{}", v.wire));
                }
            }
        }
        assert!(missing.is_empty(), "no fixture reaches {missing:?}");
    }

    fn schema_name(g: &Generator, swift: &str) -> String {
        g.model
            .defs
            .keys()
            .find(|n| swift_name(n) == swift)
            .unwrap_or_else(|| panic!("{swift}: no such type"))
            .clone()
    }

    /// Swift names are unique, never a support type's, and the renamed `Selector` is gone.
    #[test]
    fn swift_names_are_unique() {
        let g = Generator::new();
        let names: Vec<String> = g.model.defs.keys().map(|n| swift_name(n)).collect();
        let unique: BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        for support in SUPPORT_TYPES {
            assert!(!unique.contains(&support.to_owned()), "{support}");
        }
        assert!(unique.contains(&"ClaimSelector".to_owned()));
        assert!(unique.contains(&"Envelope".to_owned()));
        assert!(!unique.contains(&"Selector".to_owned()));
        assert!(unique.contains(&"ReadingUInt8".to_owned()));
        assert!(unique.contains(&"ReadingBool".to_owned()));
    }

    /// The values and commands never leave `SecretText` in Swift.
    #[test]
    fn values_are_secret_text() {
        let g = Generator::new();
        let field = |ty: &str, wire: &str| -> Ty {
            let Def::Struct(fields) = g.model.def(ty) else {
                panic!("{ty} is not a struct");
            };
            fields.iter().find(|f| f.wire == wire).unwrap().ty.clone()
        };
        for (ty, wire) in [
            ("MaterializedSecret", "value"),
            ("Unlock", "password"),
            ("Prove", "password"),
            ("SecretSet", "value"),
            ("ImportEntry", "value"),
            ("AccountSet", "value"),
            ("Materialize", "command"),
            ("AccountMaterialize", "command"),
        ] {
            assert_eq!(field(ty, wire), Ty::Secret, "{ty}.{wire}");
        }
        let swift = g.generated_swift();
        assert!(swift.contains("public var value: SecretText\n"));
        assert!(!swift.contains("SecretValue"));
        assert!(!swift.contains("RawCommand"));
    }

    /// The emitter's reading of each serde shape, on the contract's own examples.
    #[test]
    fn reads_every_serde_shape() {
        let g = Generator::new();
        assert!(
            matches!(g.model.def("LockState"), Def::StringEnum(c) if c == &["missing", "locked", "unlocked"])
        );
        assert!(matches!(
            g.model.def("ResourceId"),
            Def::Newtype(Ty::String)
        ));
        let Def::Internal { tag, variants } = g.model.def("RunGroup") else {
            panic!("RunGroup");
        };
        assert_eq!(tag, "kind");
        assert_eq!(variants[0].wire, "unix_group");
        let Payload::Fields(fields) = &variants[1].payload else {
            panic!("windows_job");
        };
        let session = fields.iter().find(|f| f.wire == "session").unwrap();
        assert_eq!(session.ty, Ty::Optional(Box::new(Ty::Int("UInt32"))));
        assert!(!session.ser_required && !session.de_required);
        let Def::External(variants) = g.model.def("Reading_uint8") else {
            panic!("Reading_uint8");
        };
        let wires: Vec<&str> = variants.iter().map(|v| v.wire.as_str()).collect();
        assert_eq!(wires, ["emulated", "unknown", "known"]);
        assert!(matches!(
            &variants[2].payload,
            Payload::Newtype(Ty::Int("UInt8"))
        ));
        let Def::Adjacent {
            tag,
            content,
            variants,
        } = g.model.def("Request")
        else {
            panic!("Request");
        };
        assert_eq!((tag.as_str(), content.as_str()), ("method", "params"));
        let shutdown = variants
            .iter()
            .find(|v| v.wire == "service.shutdown")
            .unwrap();
        let Payload::Fields(fields) = &shutdown.payload else {
            panic!("service.shutdown");
        };
        let refuse = &fields[1];
        assert_eq!(refuse.wire, "refuse_if_unlocked");
        assert!(!refuse.ser_required && !refuse.de_required);
        assert!(matches!(
            &variants
                .iter()
                .find(|v| v.wire == "service.status")
                .unwrap()
                .payload,
            Payload::Unit
        ));
        // Field order is Rust's: `HeavyEntry` starts with id, ticket, kind.
        let Def::Struct(fields) = g.model.def("HeavyEntry") else {
            panic!("HeavyEntry");
        };
        let order: Vec<&str> = fields.iter().take(3).map(|f| f.wire.as_str()).collect();
        assert_eq!(order, ["id", "ticket", "kind"]);
        assert_eq!(swift_ty(&fields[0].ty), "String");
    }

    /// Every hand-written deserializer of a contract type and the fields it fills when they are
    /// absent (P5-L8): `schemars` cannot see them, so Swift decodes these fields as the service
    /// serializes them. A new hand-written deserializer fails
    /// `custom_deserializers_are_classified` until it is listed here.
    const CUSTOM_DESERIALIZERS: &[(&str, &[&str])] = &[
        ("ProcessIdentity", &["boot_id"]),
        ("RunGroup", &["boot_id", "session"]),
        ("SecretValue", &[]),
        ("RawCommand", &[]),
    ];

    /// The hand-written deserializers in `contract/*.rs`: `impl Deserialize` blocks, and any
    /// `deserialize_with`, `#[serde(from …)]` or `#[serde(try_from …)]` (none today) by file.
    fn scan_custom_deserializers() -> BTreeSet<String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contract");
        let mut found = BTreeSet::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines() {
                if let Some(rest) = line.split("Deserialize<'de> for ").nth(1) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    found.insert(name);
                }
                for attr in ["deserialize_with", "serde(from", "serde(try_from"] {
                    if line.contains(attr) {
                        found.insert(format!("{} in {}", attr, path.display()));
                    }
                }
            }
        }
        found
    }

    #[test]
    fn custom_deserializers_are_classified() {
        let listed: BTreeSet<String> = CUSTOM_DESERIALIZERS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        assert_eq!(
            scan_custom_deserializers(),
            listed,
            "a hand-written deserializer in contract/ is not in CUSTOM_DESERIALIZERS: list it \
             with the fields it fills when absent, and check its serialized form (P5-L8)"
        );
    }

    /// The values of each listed type the checks below read and write.
    fn custom_samples() -> Vec<(&'static str, Value)> {
        use crate::contract::process::{ProcessIdentity, RunGroup};
        let identity = |boot: &str| ProcessIdentity {
            pid: 42,
            boot_id: boot.into(),
            start_time: 500,
        };
        vec![
            ("ProcessIdentity", json!((identity("boot-1")))),
            // An empty boot id (a sandboxed client's) is still written.
            ("ProcessIdentity", json!((identity("")))),
            (
                "RunGroup",
                json!(
                    (RunGroup::UnixGroup {
                        pgid: 42,
                        boot_id: "boot-1".into(),
                    })
                ),
            ),
            (
                "RunGroup",
                json!(
                    (RunGroup::WindowsJob {
                        name: "hostler-h1".into(),
                        boot_id: "boot-1".into(),
                        session: Some(2),
                    })
                ),
            ),
            (
                "RunGroup",
                json!(
                    (RunGroup::WindowsJob {
                        name: "hostler-h1".into(),
                        boot_id: "boot-1".into(),
                        session: None,
                    })
                ),
            ),
        ]
    }

    /// Reads `value` as the named custom type.
    fn read_custom(name: &str, value: Value) -> Result<Value, serde_json::Error> {
        use crate::contract::process::{ProcessIdentity, RunGroup};
        use crate::contract::values::{RawCommand, SecretValue};
        match name {
            "ProcessIdentity" => serde_json::from_value::<ProcessIdentity>(value).map(|v| json!(v)),
            "RunGroup" => serde_json::from_value::<RunGroup>(value).map(|v| json!(v)),
            "SecretValue" => serde_json::from_value::<SecretValue>(value).map(|v| json!(v)),
            "RawCommand" => serde_json::from_value::<RawCommand>(value).map(|v| json!(v)),
            other => panic!("{other}: not a listed custom type"),
        }
    }

    /// The model's field `wire` of `name` (a struct, or the variant of an internally tagged
    /// enum that `value` is).
    fn model_field<'a>(g: &'a Generator, name: &str, value: &Value, wire: &str) -> &'a emit::Field {
        let fields = match g.model.def(name) {
            Def::Struct(fields) => fields,
            Def::Internal { tag, variants } => {
                let v = variants
                    .iter()
                    .find(|v| Some(v.wire.as_str()) == value[tag].as_str())
                    .unwrap();
                match &v.payload {
                    Payload::Fields(fields) => fields,
                    _ => panic!("{name}.{}: no fields", v.wire),
                }
            }
            _ => panic!("{name}: not a struct or internally tagged enum"),
        };
        fields.iter().find(|f| f.wire == wire).unwrap()
    }

    /// Each field a custom deserializer fills is either always written by the service (so
    /// Swift's required decode holds) or optional in Swift (`decodeIfPresent`).
    #[test]
    fn defaulted_fields_are_always_serialized_or_optional() {
        let g = Generator::new();
        for (name, value) in custom_samples() {
            let defaulted = CUSTOM_DESERIALIZERS
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap()
                .1;
            for wire in defaulted.iter() {
                // `session` exists only in `windows_job`.
                if name == "RunGroup" && value["kind"] == "unix_group" && *wire == "session" {
                    continue;
                }
                let field = model_field(&g, name, &value, wire);
                if field.de_required {
                    assert!(field.ser_required, "{name}.{wire}: Swift requires it");
                    assert!(
                        value.get(*wire).is_some(),
                        "{name}.{wire} is not serialized in {value}"
                    );
                } else {
                    assert!(
                        matches!(field.ty, Ty::Optional(_)),
                        "{name}.{wire}: optional in the schema, not in Swift"
                    );
                }
            }
        }
    }

    /// The custom deserializers fill exactly the listed fields: dropping any other field from a
    /// serialized value is refused, dropping a listed one is read. A change to what they accept
    /// fails here, though no schema or generated file changes.
    #[test]
    fn custom_defaults_are_exactly_the_listed_fields() {
        for (name, value) in custom_samples() {
            let read = read_custom(name, value.clone()).unwrap();
            // An empty boot id is read as this machine's (`procs::local_boot`).
            if value["boot_id"] != "" {
                assert_eq!(read, value, "{name}");
            }
            let defaulted = CUSTOM_DESERIALIZERS
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap()
                .1;
            for key in value.as_object().unwrap().keys() {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert_eq!(
                    read_custom(name, missing).is_ok(),
                    defaulted.contains(&key.as_str()),
                    "{name} without {key}"
                );
            }
        }
        for name in ["SecretValue", "RawCommand"] {
            assert_eq!(
                read_custom(name, json!("fixture")).unwrap(),
                json!("fixture")
            );
            assert!(read_custom(name, json!(1)).is_err(), "{name}");
        }
    }

    /// Every request fixture (what Swift encodes: `FixtureRoundTripTests` checks Swift's encoding
    /// equals the fixture) reads back through Rust's deserializer, custom ones included, and
    /// serializes to the same JSON.
    #[test]
    fn request_fixtures_read_back_in_rust() {
        let g = Generator::new();
        let mut checked = 0;
        for (name, (kind, value)) in g.fixtures() {
            if kind != Fixture::Request {
                continue;
            }
            let frame: ClientFrame =
                serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(serde_json::to_value(frame).unwrap(), value, "{name}");
            checked += 1;
        }
        assert_eq!(checked, METHODS.len());
    }

    /// The CLI's `--json` envelope is one generic Swift type over the generated `data` types
    /// (P5-L7): `ok`, `data` and `error` always written, `data`/`error` `null` when absent.
    #[test]
    fn the_cli_envelope_is_generic() {
        let g = Generator::new();
        let Def::Struct(fields) = g.model.def("Envelope") else {
            panic!("Envelope is not a struct");
        };
        let shape: Vec<(&str, String, bool)> = fields
            .iter()
            .map(|f| (f.wire.as_str(), swift_ty(&f.ty), f.ser_required))
            .collect();
        assert_eq!(
            shape,
            [
                ("ok", "Bool".to_owned(), true),
                ("data", "T?".to_owned(), true),
                ("error", "HostlerError?".to_owned(), true),
            ]
        );
        let swift = g.generated_swift();
        assert!(swift.contains(
            "public struct Envelope<T: Codable & Sendable & Equatable>: Codable, Sendable, \
             Equatable {\n"
        ));
        assert!(!g.model.defs.contains_key("T"));
        let check = g.sendable_check();
        assert!(check.contains("assertSendable(Envelope<JSONValue>.self)"));
        let fixtures = g.fixtures();
        let (kind, error) = &fixtures["envelope.error.json"];
        assert_eq!(kind, &Fixture::Envelope("Claim".into()));
        assert_eq!(error["ok"], json!(false));
        assert_eq!(error["data"], Value::Null);
        assert_eq!(error["error"]["code"], json!("force_required"));
    }

    #[test]
    fn swift_identifiers() {
        assert_eq!(camel("as_of_seq"), "asOfSeq");
        assert_eq!(camel("service.status"), "serviceStatus");
        assert_eq!(camel("secret.request_withdraw"), "secretRequestWithdraw");
        assert_eq!(emit::ident("internal"), "`internal`");
        assert_eq!(emit::ident("default"), "`default`");
        assert_eq!(emit::ident("kind"), "kind");
        assert_eq!(swift_name("Reading_uint8"), "ReadingUInt8");
        assert_eq!(swift_name("Reading_boolean"), "ReadingBool");
        assert_eq!(swift_name("Selector"), "ClaimSelector");
    }

    /// The vectors follow `paths.rs`: the boundary at 100 bytes, the fallback hash.
    #[cfg(unix)]
    #[test]
    fn socket_vectors_follow_paths() {
        let v: Value = serde_json::from_str(&socket_vectors()).unwrap();
        let cases = v["cases"].as_array().unwrap();
        let fallback: Vec<bool> = cases
            .iter()
            .map(|c| c["fallback"].as_bool().unwrap())
            .collect();
        assert_eq!(fallback, [false, false, false, true, true, true]);
        assert_eq!(cases[2]["socket"].as_str().unwrap().len(), 100);
        for c in &cases[3..] {
            let s = c["socket"].as_str().unwrap();
            let hash = s
                .strip_prefix("/tmp/hostler-501-")
                .and_then(|r| r.strip_suffix("/hostler.sock"))
                .unwrap();
            assert_eq!(hash.len(), 16, "{s}");
            assert!(
                hash.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
    }
}
