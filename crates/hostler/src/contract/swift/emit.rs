//! Reads the contract's `schemars` schemas into a small model of serde shapes and writes Swift
//! from it (plan P5 Task 3). Two schemas of every type are read: the deserialize contract (what
//! a reader must accept: a field with a serde default or an `Option` may be absent) and the
//! serialize contract (what the service writes: only a `skip_serializing_if` field may be
//! absent). A shape the emitter does not know is a panic that names the type, never a guess.
//!
//! **The direction rule (spec §6.3, P5-L8).** The generated decoders follow what the service
//! *serializes*, and the generated encoders must write what the service's *deserializer*
//! accepts. Swift reads only the service's answers and events and writes only requests, and it
//! always writes every field. So Rust's lenient read defaults for older clients are not mirrored
//! in Swift: a hand-written `Deserialize` that fills an absent field (`ProcessIdentity` and
//! `RunGroup` fill an absent `boot_id`) is invisible to `schemars`, and Swift still decodes that
//! field as required, which is right as long as the service always writes it.
//! `contract::swift::tests::custom_deserializers_are_classified` keeps that true: every
//! hand-written deserializer in `contract/` is listed with the fields it defaults, each of
//! those is always serialized (or optional in Swift), and every request fixture reads back in
//! Rust unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Map, Value, json};

/// The two types that hold a value or a command that may hold one; Swift keeps both in
/// `SecretText` (hand-written, `Contract/Support/SecretText.swift`).
pub const SECRET_TYPES: [&str; 2] = ["SecretValue", "RawCommand"];

/// Rust names that would shadow a name every Swift file sees: `Selector` is Objective-C's,
/// imported with Foundation, so the app could not name it without a module prefix.
const SWIFT_RENAMES: [(&str, &str); 1] = [("Selector", "ClaimSelector")];

/// A property's or a payload's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    String,
    Bool,
    /// The Swift fixed-width integer (`UInt8`, `Int64`, …).
    Int(&'static str),
    Double,
    /// `serde_json::Value`, the schema `true`.
    Json,
    /// `SecretValue` or `RawCommand`.
    Secret,
    /// A named type (its schema name).
    Ref(String),
    /// The type parameter of a generic type (`Envelope<T>`'s `T`).
    Param(String),
    Array(Box<Ty>),
    /// A JSON object of string keys (`BTreeMap<String, _>`).
    Map(Box<Ty>),
    /// `Option<T>`: the value may be `null`.
    Optional(Box<Ty>),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub wire: String,
    pub ty: Ty,
    /// A reader must find it (deserialize contract).
    pub de_required: bool,
    /// The service always writes it (serialize contract); `false` only for
    /// `skip_serializing_if`.
    pub ser_required: bool,
    /// The schema's `default`, if it has one.
    pub default: Option<Value>,
}

#[derive(Clone, Debug)]
pub enum Payload {
    Unit,
    Newtype(Ty),
    Fields(Vec<Field>),
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub wire: String,
    pub payload: Payload,
}

#[derive(Clone, Debug)]
pub enum Def {
    Struct(Vec<Field>),
    StringEnum(Vec<String>),
    /// `#[serde(transparent)]` over a string or an integer.
    Newtype(Ty),
    /// `#[serde(tag = "…")]`.
    Internal {
        tag: String,
        variants: Vec<Variant>,
    },
    /// `#[serde(tag = "…", content = "…")]`.
    Adjacent {
        tag: String,
        content: String,
        variants: Vec<Variant>,
    },
    /// serde's default: `"unit"` or `{"variant": payload}`.
    External(Vec<Variant>),
}

impl Def {
    pub fn variants(&self) -> &[Variant] {
        match self {
            Def::Internal { variants, .. }
            | Def::Adjacent { variants, .. }
            | Def::External(variants) => variants,
            _ => &[],
        }
    }
}

/// Every named type of the contract, by schema name.
pub struct Model {
    pub defs: BTreeMap<String, Def>,
}

impl Model {
    /// `de` and `ser` are the `$defs` of the two contracts, generated from the same roots.
    pub fn read(de: &Map<String, Value>, ser: &Map<String, Value>) -> Model {
        assert_eq!(
            de.keys().collect::<Vec<_>>(),
            ser.keys().collect::<Vec<_>>(),
            "the two contracts name different types"
        );
        let defs = de
            .iter()
            .filter(|(name, _)| !SECRET_TYPES.contains(&name.as_str()))
            .map(|(name, schema)| (name.clone(), read_def(name, schema, &ser[name])))
            .collect();
        Model { defs }
    }

    pub fn def(&self, name: &str) -> &Def {
        self.defs
            .get(name)
            .unwrap_or_else(|| panic!("{name}: not a contract type"))
    }

    /// Whether a value of the named type can hold a `SecretText`.
    pub fn holds_secret(&self, name: &str) -> bool {
        fn has(model: &Model, ty: &Ty, seen: &mut BTreeSet<String>) -> bool {
            match ty {
                Ty::Secret => true,
                Ty::Array(t) | Ty::Map(t) | Ty::Optional(t) => has(model, t, seen),
                Ty::Ref(name) => {
                    if !seen.insert(name.clone()) {
                        return false;
                    }
                    match model.def(name) {
                        Def::Struct(fields) => fields.iter().any(|f| has(model, &f.ty, seen)),
                        Def::Newtype(t) => has(model, t, seen),
                        Def::StringEnum(_) => false,
                        def => def.variants().iter().any(|v| match &v.payload {
                            Payload::Unit => false,
                            Payload::Newtype(t) => has(model, t, seen),
                            Payload::Fields(fs) => fs.iter().any(|f| has(model, &f.ty, seen)),
                        }),
                    }
                }
                _ => false,
            }
        }
        has(self, &Ty::Ref(name.into()), &mut BTreeSet::new())
    }

    /// Walks `value` as a `ty`, recording `(type, variant)` for every tagged variant met. A value
    /// the model does not describe (an unknown key, a missing field, an unknown tag) panics: the
    /// model, and so the Swift, would not read it.
    pub fn walk(&self, ty: &Ty, value: &Value, at: &str, hits: &mut BTreeSet<(String, String)>) {
        let fail = |what: &str| -> ! { panic!("{at}: {what}: {value}") };
        match ty {
            Ty::String | Ty::Secret => {
                value.as_str().unwrap_or_else(|| fail("not a string"));
            }
            Ty::Bool => {
                value.as_bool().unwrap_or_else(|| fail("not a bool"));
            }
            Ty::Int(_) => {
                if !value.is_i64() && !value.is_u64() {
                    fail("not an integer");
                }
            }
            Ty::Double => {
                value.as_f64().unwrap_or_else(|| fail("not a number"));
            }
            // A generic's `data`: the caller walks it with its concrete type.
            Ty::Json | Ty::Param(_) => {}
            Ty::Optional(t) => {
                if !value.is_null() {
                    self.walk(t, value, at, hits);
                }
            }
            Ty::Array(t) => {
                for (i, item) in value
                    .as_array()
                    .unwrap_or_else(|| fail("not an array"))
                    .iter()
                    .enumerate()
                {
                    self.walk(t, item, &format!("{at}[{i}]"), hits);
                }
            }
            Ty::Map(t) => {
                for (k, item) in value.as_object().unwrap_or_else(|| fail("not an object")) {
                    self.walk(t, item, &format!("{at}.{k}"), hits);
                }
            }
            Ty::Ref(name) => self.walk_def(name, value, &format!("{at}<{name}>"), hits),
        }
    }

    fn walk_fields(
        &self,
        fields: &[Field],
        object: &Map<String, Value>,
        skip: &[&str],
        at: &str,
        hits: &mut BTreeSet<(String, String)>,
    ) {
        for key in object.keys() {
            if !skip.contains(&key.as_str()) && !fields.iter().any(|f| &f.wire == key) {
                panic!("{at}: the unknown key {key}");
            }
        }
        for f in fields {
            match object.get(&f.wire) {
                Some(v) => self.walk(&f.ty, v, &format!("{at}.{}", f.wire), hits),
                None if f.ser_required => panic!("{at}: {} is missing", f.wire),
                None => {}
            }
        }
    }

    fn walk_def(&self, name: &str, value: &Value, at: &str, hits: &mut BTreeSet<(String, String)>) {
        let object = || {
            value
                .as_object()
                .unwrap_or_else(|| panic!("{at}: not an object: {value}"))
        };
        let variant = |variants: &'_ [Variant], wire: &str| -> Variant {
            variants
                .iter()
                .find(|v| v.wire == wire)
                .unwrap_or_else(|| panic!("{at}: the unknown variant {wire}"))
                .clone()
        };
        let payload = |v: &Variant, content: Option<&Value>, skip: &[&str], hits: &mut _| match (
            &v.payload, content,
        ) {
            (Payload::Unit, None) => {}
            (Payload::Newtype(t), Some(c)) => self.walk(t, c, at, hits),
            (Payload::Fields(fs), Some(c)) => {
                let o = c
                    .as_object()
                    .unwrap_or_else(|| panic!("{at}: not an object"));
                self.walk_fields(fs, o, skip, at, hits)
            }
            _ => panic!("{at}: {} has the wrong content", v.wire),
        };
        match self.def(name) {
            Def::Struct(fields) => self.walk_fields(fields, object(), &[], at, hits),
            Def::Newtype(t) => self.walk(t, value, at, hits),
            Def::StringEnum(cases) => {
                let s = value
                    .as_str()
                    .unwrap_or_else(|| panic!("{at}: not a string"));
                assert!(cases.iter().any(|c| c == s), "{at}: the unknown case {s}");
            }
            Def::Internal { tag, variants } => {
                let o = object();
                let wire = o[tag].as_str().unwrap_or_else(|| panic!("{at}: no tag"));
                let v = variant(variants, wire);
                hits.insert((name.into(), v.wire.clone()));
                match &v.payload {
                    Payload::Unit => assert_eq!(o.len(), 1, "{at}: a unit variant with fields"),
                    _ => payload(&v, Some(value), &[tag.as_str()], hits),
                }
            }
            Def::Adjacent {
                tag,
                content,
                variants,
            } => {
                let o = object();
                for key in o.keys() {
                    assert!(key == tag || key == content, "{at}: the unknown key {key}");
                }
                let wire = o[tag].as_str().unwrap_or_else(|| panic!("{at}: no tag"));
                let v = variant(variants, wire);
                hits.insert((name.into(), v.wire.clone()));
                payload(&v, o.get(content), &[], hits);
            }
            Def::External(variants) => {
                let (wire, content) = match value {
                    Value::String(s) => (s.clone(), None),
                    Value::Object(o) if o.len() == 1 => {
                        let (k, c) = o.iter().next().unwrap();
                        (k.clone(), Some(c))
                    }
                    _ => panic!("{at}: neither a string nor a one-key object"),
                };
                let v = variant(variants, &wire);
                hits.insert((name.into(), v.wire.clone()));
                payload(&v, content, &[], hits);
            }
        }
    }
}

fn read_def(name: &str, de: &Value, ser: &Value) -> Def {
    if let Some(branches) = de.get("oneOf").and_then(Value::as_array) {
        let ser_branches = ser["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: no oneOf in the serialize contract"));
        assert_eq!(branches.len(), ser_branches.len(), "{name}: the branches");
        return read_one_of(name, branches, ser_branches);
    }
    match de.get("type").and_then(Value::as_str) {
        Some("object") if de.get("additionalProperties").is_none_or(|a| a == false) => {
            Def::Struct(read_fields(name, de, ser))
        }
        Some("string") if de.get("enum").is_some() => Def::StringEnum(strings(name, &de["enum"])),
        Some("string" | "integer") => Def::Newtype(read_ty(name, de)),
        _ => panic!("{name}: an unknown shape: {de}"),
    }
}

fn strings(at: &str, v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("{at}: enum is not an array"))
        .iter()
        .map(|s| {
            s.as_str()
                .unwrap_or_else(|| panic!("{at}: a non-string case"))
                .to_owned()
        })
        .collect()
}

/// A branch that is only strings: `{"type":"string","enum":[…]}` or `{"type":"string","const":…}`.
fn unit_cases(at: &str, branch: &Value) -> Option<Vec<String>> {
    if branch.get("type") != Some(&json!("string")) || branch.get("properties").is_some() {
        return None;
    }
    if let Some(c) = branch.get("const") {
        return Some(vec![c.as_str().expect("a string const").to_owned()]);
    }
    branch.get("enum").map(|e| strings(at, e))
}

fn properties(branch: &Value) -> Option<&Map<String, Value>> {
    branch.get("properties").and_then(Value::as_object)
}

fn read_one_of(name: &str, de: &[Value], ser: &[Value]) -> Def {
    // Only strings: a plain enum (serde writes some variants' docs as separate branches).
    if let Some(cases) = de
        .iter()
        .map(|b| unit_cases(name, b))
        .collect::<Option<Vec<_>>>()
    {
        return Def::StringEnum(cases.concat());
    }
    // Every branch an object with the same `const` property: internally or adjacently tagged.
    let objects: Option<Vec<&Map<String, Value>>> = de.iter().map(properties).collect();
    if let Some(objects) = &objects {
        let tag = objects[0]
            .iter()
            .filter(|(_, s)| s.get("const").is_some())
            .map(|(k, _)| k.clone())
            .find(|k| {
                objects
                    .iter()
                    .all(|o| o.get(k).is_some_and(|s| s.get("const").is_some()))
            });
        if let Some(tag) = tag {
            let others: BTreeSet<&String> = objects
                .iter()
                .flat_map(|o| o.keys())
                .filter(|k| **k != tag)
                .collect();
            // An adjacently tagged enum read as internally tagged would be the same JSON, so the
            // test only decides which Swift is nicer.
            let adjacent = others.len() == 1 && {
                let content = others.iter().next().unwrap();
                objects.iter().all(|o| match o.get(*content) {
                    None => true,
                    Some(s) => s.get("$ref").is_some() || s.get("type") == Some(&json!("object")),
                })
            };
            let wire_of = |o: &Map<String, Value>| o[&tag]["const"].as_str().unwrap().to_owned();
            if adjacent {
                let content = (*others.iter().next().unwrap()).clone();
                let variants = de
                    .iter()
                    .zip(ser)
                    .map(|(d, s)| {
                        let wire = wire_of(properties(d).unwrap());
                        let at = format!("{name}.{wire}");
                        let payload = match (properties(d).unwrap().get(&content), properties(s)) {
                            (None, _) => Payload::Unit,
                            (Some(c), _) if c.get("$ref").is_some() => {
                                Payload::Newtype(read_ty(&at, c))
                            }
                            (Some(c), Some(sp)) => {
                                Payload::Fields(read_fields(&at, c, &sp[&content]))
                            }
                            _ => panic!("{at}: no serialize branch"),
                        };
                        Variant { wire, payload }
                    })
                    .collect();
                return Def::Adjacent {
                    tag,
                    content,
                    variants,
                };
            }
            let variants = de
                .iter()
                .zip(ser)
                .map(|(d, s)| {
                    let wire = wire_of(properties(d).unwrap());
                    let at = format!("{name}.{wire}");
                    let fields = read_fields_except(&at, d, s, Some(&tag));
                    let payload = if fields.is_empty() {
                        Payload::Unit
                    } else {
                        Payload::Fields(fields)
                    };
                    Variant { wire, payload }
                })
                .collect();
            return Def::Internal { tag, variants };
        }
    }
    // Strings and one-key objects: externally tagged.
    let mut variants = Vec::new();
    for (d, s) in de.iter().zip(ser) {
        if let Some(cases) = unit_cases(name, d) {
            variants.extend(cases.into_iter().map(|wire| Variant {
                wire,
                payload: Payload::Unit,
            }));
            continue;
        }
        let props = properties(d).unwrap_or_else(|| panic!("{name}: an unknown branch {d}"));
        assert!(
            props.len() == 1 && d.get("additionalProperties") == Some(&json!(false)),
            "{name}: an unknown branch {d}"
        );
        let (wire, content) = props.iter().next().unwrap();
        let at = format!("{name}.{wire}");
        let payload = if content.get("type") == Some(&json!("object"))
            && content.get("properties").is_some()
        {
            Payload::Fields(read_fields(&at, content, &properties(s).unwrap()[wire]))
        } else {
            Payload::Newtype(read_ty(&at, content))
        };
        variants.push(Variant {
            wire: wire.clone(),
            payload,
        });
    }
    Def::External(variants)
}

fn required(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .map(|r| strings("required", r))
        .unwrap_or_default()
}

/// The fields of an object schema in Rust's order: the serialize contract lists every field it
/// always writes in declaration order; the few it may skip follow, by name.
fn read_fields(at: &str, de: &Value, ser: &Value) -> Vec<Field> {
    read_fields_except(at, de, ser, None)
}

/// `read_fields` without the tag of an internally tagged variant.
fn read_fields_except(at: &str, de: &Value, ser: &Value, tag: Option<&str>) -> Vec<Field> {
    let props = properties(de).unwrap_or_else(|| panic!("{at}: an object without properties"));
    let de_required = required(de);
    let ser_required = required(ser);
    let mut order: Vec<&String> = ser_required.iter().collect();
    order.extend(props.keys().filter(|k| !ser_required.contains(k)));
    order
        .into_iter()
        .filter(|wire| Some(wire.as_str()) != tag)
        .map(|wire| {
            let schema = props
                .get(wire)
                .unwrap_or_else(|| panic!("{at}: {wire} is required but not a property"));
            Field {
                wire: wire.clone(),
                ty: read_ty(&format!("{at}.{wire}"), schema),
                de_required: de_required.contains(wire),
                ser_required: ser_required.contains(wire),
                default: schema.get("default").cloned(),
            }
        })
        .collect()
}

fn int_type(at: &str, format: Option<&str>) -> &'static str {
    match format {
        Some("int8") => "Int8",
        Some("int16") => "Int16",
        Some("int32") => "Int32",
        Some("int64") => "Int64",
        Some("int") => "Int",
        Some("uint8") => "UInt8",
        Some("uint16") => "UInt16",
        Some("uint32") => "UInt32",
        Some("uint64") => "UInt64",
        Some("uint") => "UInt",
        other => panic!("{at}: an integer of format {other:?}"),
    }
}

fn read_ty(at: &str, s: &Value) -> Ty {
    // A type parameter: `{"allOf": [{"x-hostler-type-param": "T"}]}` (`samples::TypeParam`).
    if let Some(name) = s
        .get("allOf")
        .and_then(Value::as_array)
        .filter(|all| all.len() == 1)
        .and_then(|all| all[0].get("x-hostler-type-param"))
        .and_then(Value::as_str)
    {
        return Ty::Param(name.to_owned());
    }
    // `true`, or a schema with only annotations (a documented `serde_json::Value`): any JSON.
    let shapes = ["$ref", "anyOf", "oneOf", "allOf", "type", "enum", "const"];
    if s == &Value::Bool(true)
        || s.as_object()
            .is_some_and(|o| !o.keys().any(|k| shapes.contains(&k.as_str())))
    {
        return Ty::Json;
    }
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let name = r
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("{at}: the reference {r}"));
        return if SECRET_TYPES.contains(&name) {
            Ty::Secret
        } else {
            Ty::Ref(name.into())
        };
    }
    if let Some(any) = s.get("anyOf").and_then(Value::as_array) {
        assert!(
            any.len() == 2 && any[1] == json!({"type": "null"}),
            "{at}: an untagged union {s}"
        );
        return Ty::Optional(Box::new(read_ty(at, &any[0])));
    }
    match s.get("type") {
        Some(Value::Array(types)) => {
            let rest: Vec<&Value> = types.iter().filter(|t| *t != "null").collect();
            assert!(
                types.len() == 2 && rest.len() == 1,
                "{at}: the types {types:?}"
            );
            let mut inner = s.clone();
            inner["type"] = rest[0].clone();
            Ty::Optional(Box::new(read_ty(at, &inner)))
        }
        Some(Value::String(t)) => match t.as_str() {
            "string" => {
                assert!(
                    s.get("enum").is_none() && s.get("const").is_none(),
                    "{at}: an inline enum {s}"
                );
                Ty::String
            }
            "boolean" => Ty::Bool,
            "integer" => Ty::Int(int_type(at, s.get("format").and_then(Value::as_str))),
            "number" => Ty::Double,
            "array" => Ty::Array(Box::new(read_ty(at, &s["items"]))),
            "object" if s.get("properties").is_none() => match s.get("additionalProperties") {
                Some(v) if v.is_object() || v == true => Ty::Map(Box::new(read_ty(at, v))),
                _ => panic!("{at}: an object without properties {s}"),
            },
            _ => panic!("{at}: an inline type {s}"),
        },
        _ => panic!("{at}: an unknown type {s}"),
    }
}

// ---- names ----

const SWIFT_KEYWORDS: &[&str] = &[
    "associatedtype",
    "class",
    "deinit",
    "enum",
    "extension",
    "fileprivate",
    "func",
    "import",
    "init",
    "inout",
    "internal",
    "let",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "rethrows",
    "static",
    "struct",
    "subscript",
    "typealias",
    "var",
    "break",
    "case",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "fallthrough",
    "for",
    "guard",
    "if",
    "in",
    "repeat",
    "return",
    "switch",
    "where",
    "while",
    "as",
    "catch",
    "false",
    "is",
    "nil",
    "self",
    "super",
    "throw",
    "throws",
    "true",
    "try",
    "Type",
    "Protocol",
];

/// `as_of_seq` → `asOfSeq`, `service.status` → `serviceStatus`.
pub fn camel(wire: &str) -> String {
    let mut out = String::new();
    for (i, part) in wire
        .split(['.', '_', '-'])
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        let mut chars = part.chars();
        let first = chars.next().unwrap();
        if i == 0 {
            out.extend(first.to_lowercase());
        } else {
            out.extend(first.to_uppercase());
        }
        out.extend(chars);
    }
    out
}

/// An identifier, escaped when it is a Swift keyword.
pub fn ident(name: &str) -> String {
    if SWIFT_KEYWORDS.contains(&name) {
        format!("`{name}`")
    } else {
        name.to_owned()
    }
}

/// The Swift name of a schema name: `Reading_uint8` → `ReadingUInt8`.
pub fn swift_name(schema_name: &str) -> String {
    if let Some((_, to)) = SWIFT_RENAMES.iter().find(|(from, _)| *from == schema_name) {
        return (*to).to_owned();
    }
    schema_name
        .split('_')
        .map(|part| match part {
            "boolean" => "Bool".to_owned(),
            "string" => "String".to_owned(),
            p if p.starts_with("int") || p.starts_with("uint") => {
                int_type(schema_name, Some(p)).to_owned()
            }
            p => {
                let mut c = p.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            }
        })
        .collect()
}

pub fn swift_ty(ty: &Ty) -> String {
    match ty {
        Ty::String => "String".into(),
        Ty::Bool => "Bool".into(),
        Ty::Int(t) => (*t).into(),
        Ty::Double => "Double".into(),
        Ty::Json => "JSONValue".into(),
        Ty::Secret => "SecretText".into(),
        Ty::Ref(name) => swift_name(name),
        Ty::Param(name) => name.clone(),
        Ty::Array(t) => format!("[{}]", swift_ty(t)),
        Ty::Map(t) => format!("[String: {}]", swift_ty(t)),
        Ty::Optional(t) => format!("{}?", swift_ty(t)),
    }
}

fn swift_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => write!(out, "\\u{{{:x}}}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Field {
    pub fn swift(&self) -> String {
        camel(&self.wire)
    }

    /// The Swift default of a field a reader may miss: the schema's, else serde's
    /// `Default::default()` of a primitive; `None` for a required field. A defaulted field of
    /// another type has no Swift spelling and panics.
    fn default_literal(&self, at: &str) -> Option<String> {
        if self.de_required {
            return None;
        }
        let literal = |v: &Value| match (&self.ty, v) {
            (_, Value::Null) => "nil".to_owned(),
            (Ty::Bool, Value::Bool(b)) => b.to_string(),
            (Ty::Int(_) | Ty::Double, Value::Number(n)) => n.to_string(),
            (Ty::String, Value::String(s)) => swift_string(s),
            (Ty::Array(_), Value::Array(a)) if a.is_empty() => "[]".to_owned(),
            (Ty::Map(_), Value::Object(o)) if o.is_empty() => "[:]".to_owned(),
            _ => panic!("{at}.{}: the default {v} has no Swift spelling", self.wire),
        };
        Some(match (&self.default, &self.ty) {
            (Some(Value::Null), Ty::Optional(_)) | (None, Ty::Optional(_)) => "nil".to_owned(),
            (Some(v), Ty::Optional(_)) => {
                panic!("{at}.{}: an optional with the default {v}", self.wire)
            }
            (Some(v), _) => literal(v),
            // `Option<serde_json::Value>`: absent and `null` are both `None`.
            (None, Ty::Json) => ".null".to_owned(),
            (None, Ty::Bool) => "false".to_owned(),
            (None, Ty::Int(_) | Ty::Double) => "0".to_owned(),
            (None, Ty::String) => "\"\"".to_owned(),
            (None, Ty::Array(_)) => "[]".to_owned(),
            (None, Ty::Map(_)) => "[:]".to_owned(),
            (None, _) => panic!("{at}.{}: a default of an unknown value", self.wire),
        })
    }

    fn decode_line(&self, at: &str, container: &str) -> String {
        let key = format!(".{}", ident(&self.swift()));
        match (&self.ty, self.default_literal(at)) {
            (ty, None) => format!(
                "try {container}.decode({}.self, forKey: {key})",
                swift_ty(ty)
            ),
            (Ty::Optional(inner), Some(_)) => format!(
                "try {container}.decodeIfPresent({}.self, forKey: {key})",
                swift_ty(inner)
            ),
            (ty, Some(default)) => format!(
                "try {container}.decodeIfPresent({}.self, forKey: {key}) ?? {default}",
                swift_ty(ty)
            ),
        }
    }

    fn encode_line(&self, at: &str, value: &str, container: &str) -> String {
        let key = format!(".{}", ident(&self.swift()));
        if self.ser_required {
            return format!("try {container}.encode({value}, forKey: {key})");
        }
        match (&self.ty, self.default_literal(at)) {
            (Ty::Optional(_), _) => {
                format!("try {container}.encodeIfPresent({value}, forKey: {key})")
            }
            (_, Some(default)) => {
                format!(
                    "if {value} != {default} {{ try {container}.encode({value}, forKey: {key}) }}"
                )
            }
            (_, None) => panic!("{at}.{}: skipped but required", self.wire),
        }
    }
}

// ---- the Swift ----

/// Lines at one indent level.
struct Out {
    text: String,
}

impl Out {
    fn line(&mut self, indent: usize, s: &str) {
        if s.is_empty() {
            self.text.push('\n');
        } else {
            writeln!(self.text, "{}{s}", "    ".repeat(indent)).unwrap();
        }
    }
}

/// The payload struct of an enum variant with fields; private to the generated file.
fn payload_name(enum_name: &str, variant: &Variant) -> String {
    format!("_{}{}", swift_name(enum_name), {
        let c = camel(&variant.wire);
        let mut chars = c.chars();
        chars
            .next()
            .map(|f| f.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    })
}

/// The type parameters a type names.
fn type_params(ty: &Ty, out: &mut BTreeSet<String>) {
    match ty {
        Ty::Param(name) => {
            out.insert(name.clone());
        }
        Ty::Array(t) | Ty::Map(t) | Ty::Optional(t) => type_params(t, out),
        _ => {}
    }
}

fn render_struct(out: &mut Out, access: &str, name: &str, at: &str, fields: &[Field]) {
    assert!(!fields.is_empty(), "{at}: a struct without fields");
    let names: BTreeSet<String> = fields.iter().map(Field::swift).collect();
    assert_eq!(
        names.len(),
        fields.len(),
        "{at}: two fields with one Swift name"
    );
    let mut params = BTreeSet::new();
    fields.iter().for_each(|f| type_params(&f.ty, &mut params));
    let generics = if params.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = params
            .iter()
            .map(|p| format!("{p}: Codable & Sendable & Equatable"))
            .collect();
        format!("<{}>", list.join(", "))
    };
    out.line(
        0,
        &format!("{access}struct {name}{generics}: Codable, Sendable, Equatable {{"),
    );
    for f in fields {
        out.line(
            1,
            &format!("{access}var {}: {}", ident(&f.swift()), swift_ty(&f.ty)),
        );
    }
    out.line(0, "");
    let params: Vec<String> = fields
        .iter()
        .map(|f| {
            let default = f
                .default_literal(at)
                .map(|d| format!(" = {d}"))
                .unwrap_or_default();
            format!("{}: {}{default}", ident(&f.swift()), swift_ty(&f.ty))
        })
        .collect();
    out.line(1, &format!("{access}init({}) {{", params.join(", ")));
    for f in fields {
        let n = ident(&f.swift());
        out.line(2, &format!("self.{n} = {n}"));
    }
    out.line(1, "}");
    out.line(0, "");
    out.line(1, "private enum CodingKeys: String, CodingKey {");
    for f in fields {
        let n = ident(&f.swift());
        if f.swift() == f.wire {
            out.line(2, &format!("case {n}"));
        } else {
            out.line(2, &format!("case {n} = {}", swift_string(&f.wire)));
        }
    }
    out.line(1, "}");
    out.line(0, "");
    out.line(
        1,
        &format!("{access}init(from decoder: any Decoder) throws {{"),
    );
    out.line(
        2,
        "let container = try decoder.container(keyedBy: CodingKeys.self)",
    );
    for f in fields {
        out.line(
            2,
            &format!(
                "self.{} = {}",
                ident(&f.swift()),
                f.decode_line(at, "container")
            ),
        );
    }
    out.line(1, "}");
    out.line(0, "");
    out.line(
        1,
        &format!("{access}func encode(to encoder: any Encoder) throws {{"),
    );
    out.line(
        2,
        "var container = encoder.container(keyedBy: CodingKeys.self)",
    );
    for f in fields {
        out.line(
            2,
            &f.encode_line(at, &format!("self.{}", ident(&f.swift())), "container"),
        );
    }
    out.line(1, "}");
    out.line(0, "}");
}

/// `case name(label: Type, …)` and the pattern `.name(let a, let b)` and the expression
/// `.name(label: a, …)` of one variant.
struct Case {
    decl: String,
    pattern: String,
    payload_struct: Option<String>,
}

fn case_of(enum_name: &str, v: &Variant) -> Case {
    let name = ident(&camel(&v.wire));
    match &v.payload {
        Payload::Unit => Case {
            decl: format!("case {name}"),
            pattern: format!(".{name}"),
            payload_struct: None,
        },
        Payload::Newtype(ty) => Case {
            decl: format!("case {name}({})", swift_ty(ty)),
            pattern: format!(".{name}(let value)"),
            payload_struct: None,
        },
        Payload::Fields(fields) => {
            let labels: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", ident(&f.swift()), swift_ty(&f.ty)))
                .collect();
            let binds: Vec<String> = fields
                .iter()
                .map(|f| format!("let {}", ident(&f.swift())))
                .collect();
            Case {
                decl: format!("case {name}({})", labels.join(", ")),
                pattern: format!(".{name}({})", binds.join(", ")),
                payload_struct: Some(payload_name(enum_name, v)),
            }
        }
    }
}

/// `.name(label: payload.label, …)` from a decoded payload struct.
fn construct_from(v: &Variant, payload: &str) -> String {
    let name = ident(&camel(&v.wire));
    match &v.payload {
        Payload::Fields(fields) => {
            let args: Vec<String> = fields
                .iter()
                .map(|f| {
                    let n = ident(&f.swift());
                    format!("{n}: {payload}.{n}")
                })
                .collect();
            format!(".{name}({})", args.join(", "))
        }
        _ => unreachable!(),
    }
}

/// `_Payload(label: label, …)` from the bound names.
fn payload_from_binds(v: &Variant, payload_struct: &str) -> String {
    match &v.payload {
        Payload::Fields(fields) => {
            let args: Vec<String> = fields
                .iter()
                .map(|f| {
                    let n = ident(&f.swift());
                    format!("{n}: {n}")
                })
                .collect();
            format!("{payload_struct}({})", args.join(", "))
        }
        _ => unreachable!(),
    }
}

fn render_enum(out: &mut Out, payloads: &mut Out, name: &str, def: &Def) {
    let swift = swift_name(name);
    let variants = def.variants();
    let cases: Vec<Case> = variants.iter().map(|v| case_of(name, v)).collect();
    let unique: BTreeSet<String> = variants.iter().map(|v| camel(&v.wire)).collect();
    assert_eq!(
        unique.len(),
        variants.len(),
        "{name}: two variants with one Swift name"
    );
    for (v, c) in variants.iter().zip(&cases) {
        if let (Payload::Fields(fields), Some(p)) = (&v.payload, &c.payload_struct) {
            render_struct(
                payloads,
                "fileprivate ",
                p,
                &format!("{name}.{}", v.wire),
                fields,
            );
            payloads.line(0, "");
        }
    }
    out.line(
        0,
        &format!("public enum {swift}: Codable, Sendable, Equatable {{"),
    );
    for c in &cases {
        out.line(1, &c.decl);
    }
    out.line(0, "");
    let lit = |v: &Variant| swift_string(&v.wire);
    match def {
        Def::Internal { tag, .. } | Def::Adjacent { tag, .. } => {
            let content = match def {
                Def::Adjacent { content, .. } => Some(content.as_str()),
                _ => None,
            };
            out.line(1, "private enum TagKeys: String, CodingKey {");
            out.line(2, &format!("case tag = {}", swift_string(tag)));
            if let Some(content) = content {
                out.line(2, &format!("case content = {}", swift_string(content)));
            }
            out.line(1, "}");
            out.line(0, "");
            out.line(1, "public init(from decoder: any Decoder) throws {");
            out.line(
                2,
                "let container = try decoder.container(keyedBy: TagKeys.self)",
            );
            out.line(
                2,
                "switch try container.decode(String.self, forKey: .tag) {",
            );
            for (v, c) in variants.iter().zip(&cases) {
                out.line(2, &format!("case {}:", lit(v)));
                match (&v.payload, content, &c.payload_struct) {
                    (Payload::Unit, _, _) => out.line(3, &format!("self = {}", c.pattern)),
                    (Payload::Newtype(ty), Some(_), _) => out.line(
                        3,
                        &format!(
                            "self = .{}(try container.decode({}.self, forKey: .content))",
                            ident(&camel(&v.wire)),
                            swift_ty(ty)
                        ),
                    ),
                    (Payload::Fields(_), Some(_), Some(p)) => {
                        out.line(
                            3,
                            &format!(
                                "let payload = try container.decode({p}.self, forKey: .content)"
                            ),
                        );
                        out.line(3, &format!("self = {}", construct_from(v, "payload")));
                    }
                    (Payload::Fields(_), None, Some(p)) => {
                        out.line(3, &format!("let payload = try {p}(from: decoder)"));
                        out.line(3, &format!("self = {}", construct_from(v, "payload")));
                    }
                    _ => panic!(
                        "{name}.{}: a newtype variant of an internally tagged enum",
                        v.wire
                    ),
                }
            }
            out.line(2, "default:");
            out.line(
                3,
                &format!(
                    "throw ContractCoding.unknownTag(forKey: .tag, in: container, type: {})",
                    swift_string(&swift)
                ),
            );
            out.line(2, "}");
            out.line(1, "}");
            out.line(0, "");
            out.line(1, "public func encode(to encoder: any Encoder) throws {");
            out.line(
                2,
                "var container = encoder.container(keyedBy: TagKeys.self)",
            );
            out.line(2, "switch self {");
            for (v, c) in variants.iter().zip(&cases) {
                out.line(2, &format!("case {}:", c.pattern));
                out.line(
                    3,
                    &format!("try container.encode({}, forKey: .tag)", lit(v)),
                );
                match (&v.payload, content, &c.payload_struct) {
                    (Payload::Unit, _, _) => {}
                    (Payload::Newtype(_), Some(_), _) => {
                        out.line(3, "try container.encode(value, forKey: .content)")
                    }
                    (Payload::Fields(_), Some(_), Some(p)) => out.line(
                        3,
                        &format!(
                            "try container.encode({}, forKey: .content)",
                            payload_from_binds(v, p)
                        ),
                    ),
                    (Payload::Fields(_), None, Some(p)) => out.line(
                        3,
                        &format!("try {}.encode(to: encoder)", payload_from_binds(v, p)),
                    ),
                    _ => unreachable!(),
                }
            }
            out.line(2, "}");
            out.line(1, "}");
        }
        Def::External(_) => {
            let keyed: Vec<&Variant> = variants
                .iter()
                .filter(|v| !matches!(v.payload, Payload::Unit))
                .collect();
            let units: Vec<&Variant> = variants
                .iter()
                .filter(|v| matches!(v.payload, Payload::Unit))
                .collect();
            if !keyed.is_empty() {
                out.line(1, "private enum CodingKeys: String, CodingKey {");
                for v in &keyed {
                    out.line(2, &format!("case {} = {}", ident(&camel(&v.wire)), lit(v)));
                }
                out.line(1, "}");
                out.line(0, "");
            }
            out.line(1, "public init(from decoder: any Decoder) throws {");
            if !units.is_empty() {
                out.line(
                    2,
                    "if let single = try? decoder.singleValueContainer(), let tag = try? single.decode(String.self) {",
                );
                out.line(3, "switch tag {");
                for v in &units {
                    out.line(
                        3,
                        &format!("case {}: self = .{}", lit(v), ident(&camel(&v.wire))),
                    );
                }
                out.line(
                    3,
                    &format!(
                        "default: throw ContractCoding.unknownVariant(decoder, type: {})",
                        swift_string(&swift)
                    ),
                );
                out.line(3, "}");
                out.line(3, "return");
                out.line(2, "}");
            }
            if keyed.is_empty() {
                out.line(
                    2,
                    &format!(
                        "throw ContractCoding.unknownVariant(decoder, type: {})",
                        swift_string(&swift)
                    ),
                );
            } else {
                out.line(
                    2,
                    "let container = try decoder.container(keyedBy: CodingKeys.self)",
                );
                out.line(
                    2,
                    "guard container.allKeys.count == 1, let key = container.allKeys.first else {",
                );
                out.line(
                    3,
                    &format!(
                        "throw ContractCoding.unknownVariant(decoder, type: {})",
                        swift_string(&swift)
                    ),
                );
                out.line(2, "}");
                out.line(2, "switch key {");
                for v in &keyed {
                    let key = ident(&camel(&v.wire));
                    out.line(2, &format!("case .{key}:"));
                    match &v.payload {
                        Payload::Newtype(ty) => out.line(
                            3,
                            &format!(
                                "self = .{key}(try container.decode({}.self, forKey: .{key}))",
                                swift_ty(ty)
                            ),
                        ),
                        Payload::Fields(_) => {
                            let p = payload_name(name, v);
                            out.line(
                                3,
                                &format!(
                                    "let payload = try container.decode({p}.self, forKey: .{key})"
                                ),
                            );
                            out.line(3, &format!("self = {}", construct_from(v, "payload")));
                        }
                        Payload::Unit => unreachable!(),
                    }
                }
                out.line(2, "}");
            }
            out.line(1, "}");
            out.line(0, "");
            out.line(1, "public func encode(to encoder: any Encoder) throws {");
            out.line(2, "switch self {");
            for (v, c) in variants.iter().zip(&cases) {
                out.line(2, &format!("case {}:", c.pattern));
                let key = ident(&camel(&v.wire));
                match (&v.payload, &c.payload_struct) {
                    (Payload::Unit, _) => {
                        out.line(3, "var container = encoder.singleValueContainer()");
                        out.line(3, &format!("try container.encode({})", lit(v)));
                    }
                    (Payload::Newtype(_), _) => {
                        out.line(
                            3,
                            "var container = encoder.container(keyedBy: CodingKeys.self)",
                        );
                        out.line(3, &format!("try container.encode(value, forKey: .{key})"));
                    }
                    (Payload::Fields(_), Some(p)) => {
                        out.line(
                            3,
                            "var container = encoder.container(keyedBy: CodingKeys.self)",
                        );
                        out.line(
                            3,
                            &format!(
                                "try container.encode({}, forKey: .{key})",
                                payload_from_binds(v, p)
                            ),
                        );
                    }
                    _ => unreachable!(),
                }
            }
            out.line(2, "}");
            out.line(1, "}");
        }
        _ => unreachable!(),
    }
    out.line(0, "}");
}

fn render_newtype(out: &mut Out, name: &str, ty: &Ty) {
    let swift = swift_name(name);
    let raw = swift_ty(ty);
    assert!(
        matches!(ty, Ty::String | Ty::Int(_)),
        "{name}: a transparent {raw}"
    );
    out.line(
        0,
        &format!("public struct {swift}: RawRepresentable, Codable, Sendable, Hashable {{"),
    );
    out.line(1, &format!("public var rawValue: {raw}"));
    out.line(0, "");
    out.line(1, &format!("public init(rawValue: {raw}) {{"));
    out.line(2, "self.rawValue = rawValue");
    out.line(1, "}");
    out.line(0, "");
    out.line(1, &format!("public init(_ rawValue: {raw}) {{"));
    out.line(2, "self.rawValue = rawValue");
    out.line(1, "}");
    out.line(0, "");
    out.line(1, "public init(from decoder: any Decoder) throws {");
    out.line(
        2,
        &format!("self.rawValue = try decoder.singleValueContainer().decode({raw}.self)"),
    );
    out.line(1, "}");
    out.line(0, "");
    out.line(1, "public func encode(to encoder: any Encoder) throws {");
    out.line(2, "var container = encoder.singleValueContainer()");
    out.line(2, "try container.encode(rawValue)");
    out.line(1, "}");
    out.line(0, "}");
}

fn render_string_enum(out: &mut Out, name: &str, cases: &[String]) {
    out.line(
        0,
        &format!(
            "public enum {}: String, Codable, Sendable, CaseIterable {{",
            swift_name(name)
        ),
    );
    let unique: BTreeSet<String> = cases.iter().map(|c| camel(c)).collect();
    assert_eq!(
        unique.len(),
        cases.len(),
        "{name}: two cases with one Swift name"
    );
    for c in cases {
        let n = camel(c);
        if &n == c {
            out.line(1, &format!("case {}", ident(&n)));
        } else {
            out.line(1, &format!("case {} = {}", ident(&n), swift_string(c)));
        }
    }
    out.line(0, "}");
}

/// Every contract type, in name order; the payload structs of enum variants follow.
pub fn render_types(model: &Model) -> String {
    let mut out = Out {
        text: String::new(),
    };
    let mut payloads = Out {
        text: String::new(),
    };
    for (name, def) in &model.defs {
        match def {
            Def::Struct(fields) => {
                render_struct(&mut out, "public ", &swift_name(name), name, fields)
            }
            Def::StringEnum(cases) => render_string_enum(&mut out, name, cases),
            Def::Newtype(ty) => render_newtype(&mut out, name, ty),
            _ => render_enum(&mut out, &mut payloads, name, def),
        }
        out.line(0, "");
    }
    out.line(0, "// MARK: - The payloads of enum variants with fields");
    out.line(0, "");
    out.text.push_str(&payloads.text);
    out.text
}

/// What the Swift of one method needs.
pub struct MethodView<'a> {
    pub wire: &'a str,
    pub variant: &'a Variant,
    pub timeout_ms: u64,
    pub gated: bool,
    pub app: bool,
    pub sensitive_request: bool,
    pub sensitive_answer: bool,
    /// The Swift type of the answer; `None` for JSON `null`.
    pub answer: Option<String>,
}

/// `Method`, `MethodSpec`, `Request.method` and the facade `HostlerClient`.
pub fn render_methods(methods: &[MethodView<'_>]) -> String {
    let mut out = Out {
        text: String::new(),
    };
    out.line(
        0,
        "/// Every wire method of the contract, in the Rust table's order. (Not `Method`: that is",
    );
    out.line(0, "/// Objective-C's, visible wherever Foundation is.)");
    out.line(
        0,
        "public enum HostlerMethod: String, Sendable, CaseIterable {",
    );
    for m in methods {
        out.line(
            1,
            &format!("case {} = {}", ident(&camel(m.wire)), swift_string(m.wire)),
        );
    }
    out.line(0, "}");
    out.line(0, "");
    out.line(
        0,
        "/// One row of the method table (`contract/methods.rs`).",
    );
    out.line(0, "public struct MethodSpec: Sendable, Equatable {");
    out.line(1, "public let method: HostlerMethod");
    out.line(1, "/// How long a client waits for the answer.");
    out.line(1, "public let timeout: Duration");
    out.line(
        1,
        "/// Refused without a live proof on the connection (spec §8.5).",
    );
    out.line(1, "public let gated: Bool");
    out.line(1, "/// Offered by the app's facade, `HostlerClient`.");
    out.line(1, "public let app: Bool");
    out.line(1, "/// The params may carry a value (`sensitive_request`).");
    out.line(1, "public let sensitiveRequest: Bool");
    out.line(
        1,
        "/// The answer carries values; it is never read as a `JSONValue` (`sensitive_answer`).",
    );
    out.line(1, "public let sensitiveAnswer: Bool");
    out.line(0, "}");
    out.line(0, "");
    out.line(0, "extension HostlerMethod {");
    out.line(1, "public var spec: MethodSpec {");
    out.line(2, "switch self {");
    for m in methods {
        let n = ident(&camel(m.wire));
        out.line(2, &format!("case .{n}:"));
        out.line(
            3,
            &format!(
                "MethodSpec(method: .{n}, timeout: .milliseconds({}), gated: {}, app: {}, sensitiveRequest: {}, sensitiveAnswer: {})",
                m.timeout_ms, m.gated, m.app, m.sensitive_request, m.sensitive_answer
            ),
        );
    }
    out.line(2, "}");
    out.line(1, "}");
    out.line(0, "}");
    out.line(0, "");
    out.line(0, "extension Request {");
    out.line(1, "/// The wire method of this request.");
    out.line(1, "public var method: HostlerMethod {");
    out.line(2, "switch self {");
    for m in methods {
        let n = ident(&camel(m.wire));
        out.line(2, &format!("case .{n}: .{n}"));
    }
    out.line(2, "}");
    out.line(1, "}");
    out.line(0, "}");
    out.line(0, "");
    out.line(
        0,
        "/// The app's typed calls: only the methods the table marks `app` (no materialize: the app",
    );
    out.line(
        0,
        "/// never builds one, though `Request` can express it). Timeouts come from the table.",
    );
    out.line(0, "public struct HostlerClient: Sendable {");
    out.line(1, "public let transport: any HostlerTransport");
    out.line(0, "");
    out.line(1, "public init(transport: any HostlerTransport) {");
    out.line(2, "self.transport = transport");
    out.line(1, "}");
    out.line(0, "}");
    out.line(0, "");
    out.line(0, "extension HostlerClient {");
    let mut first = true;
    for m in methods.iter().filter(|m| m.app) {
        if !first {
            out.line(0, "");
        }
        first = false;
        let name = ident(&camel(m.wire));
        let (params, args) = match &m.variant.payload {
            Payload::Unit => (String::new(), String::new()),
            Payload::Newtype(ty) => (format!("_ params: {}", swift_ty(ty)), "(params)".to_owned()),
            Payload::Fields(fields) => {
                let at = format!("Request.{}", m.wire);
                let params: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let default = f
                            .default_literal(&at)
                            .map(|d| format!(" = {d}"))
                            .unwrap_or_default();
                        format!("{}: {}{default}", ident(&f.swift()), swift_ty(&f.ty))
                    })
                    .collect();
                let args: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let n = ident(&f.swift());
                        format!("{n}: {n}")
                    })
                    .collect();
                (params.join(", "), format!("({})", args.join(", ")))
            }
        };
        let seconds = m.timeout_ms as f64 / 1000.0;
        let mut doc = format!("/// `{}`, at most {seconds} s.", m.wire);
        if m.gated {
            doc.push_str(" Gated: needs a live proof on the connection (`secretsProve`).");
        }
        if m.sensitive_answer {
            doc.push_str(" The answer holds values: read raw, kept in `SecretText`.");
        }
        out.line(1, &doc);
        let request = format!(".{name}{args}");
        match &m.answer {
            None => {
                out.line(
                    1,
                    &format!("public func {name}({params}) async throws(HostlerCallError) {{"),
                );
                out.line(2, &format!("try await unit({request})"));
            }
            Some(answer) => {
                out.line(
                    1,
                    &format!(
                        "public func {name}({params}) async throws(HostlerCallError) -> {answer} {{"
                    ),
                );
                let call = if m.sensitive_answer {
                    "sensitiveAnswer"
                } else {
                    "answer"
                };
                out.line(2, &format!("try await {call}({request})"));
            }
        }
        out.line(1, "}");
    }
    out.line(0, "}");
    out.text
}
