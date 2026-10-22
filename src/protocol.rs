//! The `script-functions` protocol, version 1 (`website/docs/develop/script-functions-protocol.md`
//! in mdeo-cloud): the messages and values, their CBOR decoding and encoding.
//!
//! A polymorphic value is a two-element array, its type name and a map of its fields. Receivers
//! accept definite and indefinite lengths and ignore fields they do not know. Encoding is written
//! by hand so that a `double` is always a 64-bit and a `float` always a 32-bit CBOR float.

use ciborium::value::Value;

pub const PROTOCOL: &str = "script-functions";
pub const VERSION: u32 = 1;

pub const UNKNOWN_OBJECT: &str = "unknown-object";
pub const UNKNOWN_MODEL: &str = "unknown-model";

#[derive(Clone, Debug, PartialEq)]
pub enum WireValue {
    Null,
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    Ref(i64),
    Instance(String),
    Record { class_name: String, fields: Vec<(String, WireValue)> },
    Handle { class_name: String, id: i64 },
    Enum { enum_name: String, entry: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapKind {
    List,
    Set,
    OrderedSet,
    Bag,
    Map,
}

impl HeapKind {
    fn name(self) -> &'static str {
        match self {
            HeapKind::List => "list",
            HeapKind::Set => "set",
            HeapKind::OrderedSet => "orderedSet",
            HeapKind::Bag => "bag",
            HeapKind::Map => "map",
        }
    }

    fn parse(s: &str) -> Result<HeapKind, String> {
        Ok(match s {
            "list" => HeapKind::List,
            "set" => HeapKind::Set,
            "orderedSet" => HeapKind::OrderedSet,
            "bag" => HeapKind::Bag,
            "map" => HeapKind::Map,
            _ => return Err(format!("unknown collection kind '{s}'")),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeapObject {
    pub id: i64,
    pub kind: HeapKind,
    pub version: i64,
    pub elements: Option<Vec<WireValue>>,
    pub entries: Option<Vec<WireValue>>,
}

#[derive(Clone, Debug, Default)]
pub struct WireMetamodel {
    pub path: String,
    pub classes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WireModel {
    pub metamodel_path: String,
    pub instances: Vec<WireInstance>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WireInstance {
    pub name: String,
    pub class_name: String,
    /// Attribute values by name, in the order sent. Enum values are `String`s naming the entry.
    pub attributes: Vec<(String, Vec<WireValue>)>,
    /// Names of the referenced instances, by association end.
    pub references: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug)]
pub enum ClientMessage {
    Call { call_id: i64, operation: String, objects: Vec<HeapObject>, args: Vec<WireValue>, model_id: Option<i64> },
    Metamodel(WireMetamodel),
    Model { model_id: i64, model: WireModel },
    Release { ids: Vec<i64>, handles: Vec<i64> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServiceMessage {
    Result { call_id: i64, objects: Vec<HeapObject>, value: WireValue },
    Failure { call_id: i64, message: String, code: Option<String> },
}

// ---------------------------------------------------------------------------------------------
// Decoding

type R<T> = Result<T, String>;

fn poly(v: &Value) -> R<(&str, &[(Value, Value)])> {
    match v {
        Value::Array(a) if a.len() == 2 => match (&a[0], &a[1]) {
            (Value::Text(t), Value::Map(m)) => Ok((t.as_str(), m.as_slice())),
            _ => Err("a polymorphic value is [type name, fields]".into()),
        },
        _ => Err("a polymorphic value is [type name, fields]".into()),
    }
}

fn field<'a>(m: &'a [(Value, Value)], name: &str) -> Option<&'a Value> {
    m.iter().find(|(k, _)| matches!(k, Value::Text(t) if t == name)).map(|(_, v)| v).filter(|v| !v.is_null())
}

fn req<'a>(m: &'a [(Value, Value)], name: &str) -> R<&'a Value> {
    field(m, name).ok_or_else(|| format!("missing field '{name}'"))
}

fn int(v: &Value) -> R<i64> {
    match v {
        Value::Integer(i) => i64::try_from(i128::from(*i)).map_err(|_| "integer out of range".to_string()),
        _ => Err("expected an integer".into()),
    }
}

fn float(v: &Value) -> R<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Integer(_) => Ok(int(v)? as f64),
        _ => Err("expected a number".into()),
    }
}

fn text(v: &Value) -> R<String> {
    match v {
        Value::Text(t) => Ok(t.clone()),
        _ => Err("expected a text string".into()),
    }
}

fn boolean(v: &Value) -> R<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err("expected a boolean".into()),
    }
}

fn array(v: &Value) -> R<&[Value]> {
    match v {
        Value::Array(a) => Ok(a),
        _ => Err("expected an array".into()),
    }
}

fn map(v: &Value) -> R<&[(Value, Value)]> {
    match v {
        Value::Map(m) => Ok(m),
        _ => Err("expected a map".into()),
    }
}

fn list<T>(m: &[(Value, Value)], name: &str, f: impl Fn(&Value) -> R<T>) -> R<Vec<T>> {
    match field(m, name) {
        None => Ok(Vec::new()),
        Some(v) => array(v)?.iter().map(f).collect(),
    }
}

pub fn decode_value(v: &Value) -> R<WireValue> {
    let (t, m) = poly(v)?;
    Ok(match t {
        "null" => WireValue::Null,
        "bool" => WireValue::Bool(boolean(req(m, "value")?)?),
        "int" => WireValue::Int(int(req(m, "value")?)? as i32),
        "long" => WireValue::Long(int(req(m, "value")?)?),
        "float" => WireValue::Float(float(req(m, "value")?)? as f32),
        "double" => WireValue::Double(float(req(m, "value")?)?),
        "string" => WireValue::String(text(req(m, "value")?)?),
        "ref" => WireValue::Ref(int(req(m, "id")?)?),
        "instance" => WireValue::Instance(text(req(m, "name")?)?),
        "record" => WireValue::Record {
            class_name: text(req(m, "className")?)?,
            fields: map(req(m, "fields")?)?
                .iter()
                .map(|(k, v)| Ok((text(k)?, decode_value(v)?)))
                .collect::<R<_>>()?,
        },
        "handle" => WireValue::Handle { class_name: text(req(m, "className")?)?, id: int(req(m, "id")?)? },
        "enum" => WireValue::Enum { enum_name: text(req(m, "enumName")?)?, entry: text(req(m, "entry")?)? },
        other => return Err(format!("unknown value type '{other}'")),
    })
}

fn decode_heap(v: &Value) -> R<HeapObject> {
    let m = map(v)?;
    let values = |name: &str| -> R<Option<Vec<WireValue>>> {
        match field(m, name) {
            None => Ok(None),
            Some(v) => Ok(Some(array(v)?.iter().map(decode_value).collect::<R<_>>()?)),
        }
    };
    Ok(HeapObject {
        id: int(req(m, "id")?)?,
        kind: HeapKind::parse(&text(req(m, "kind")?)?)?,
        version: field(m, "version").map(int).transpose()?.unwrap_or(0),
        elements: values("elements")?,
        entries: values("entries")?,
    })
}

fn decode_model(v: &Value) -> R<WireModel> {
    let m = map(v)?;
    let instances = list(m, "instances", |v| {
        let m = map(v)?;
        let attributes = match field(m, "attributes") {
            None => Vec::new(),
            Some(a) => map(a)?
                .iter()
                .map(|(k, v)| Ok((text(k)?, array(v)?.iter().map(decode_value).collect::<R<_>>()?)))
                .collect::<R<_>>()?,
        };
        let references = match field(m, "references") {
            None => Vec::new(),
            Some(a) => map(a)?
                .iter()
                .map(|(k, v)| Ok((text(k)?, array(v)?.iter().map(text).collect::<R<_>>()?)))
                .collect::<R<_>>()?,
        };
        Ok(WireInstance {
            name: text(req(m, "name")?)?,
            class_name: text(req(m, "className")?)?,
            attributes,
            references,
        })
    })?;
    Ok(WireModel { metamodel_path: text(req(m, "metamodelPath")?)?, instances })
}

pub fn decode_client(bytes: &[u8]) -> R<ClientMessage> {
    let v: Value = ciborium::de::from_reader(bytes).map_err(|e| format!("not CBOR: {e}"))?;
    let (t, m) = poly(&v)?;
    Ok(match t {
        "call" => ClientMessage::Call {
            call_id: int(req(m, "callId")?)?,
            operation: text(req(m, "operation")?)?,
            objects: list(m, "objects", decode_heap)?,
            args: list(m, "args", decode_value)?,
            model_id: field(m, "modelId").map(int).transpose()?,
        },
        "metamodel" => {
            let mm = map(req(m, "metamodel")?)?;
            ClientMessage::Metamodel(WireMetamodel {
                path: text(req(mm, "path")?)?,
                classes: list(mm, "classes", |c| text(req(map(c)?, "name")?))?,
            })
        }
        "model" => ClientMessage::Model { model_id: int(req(m, "modelId")?)?, model: decode_model(req(m, "model")?)? },
        "release" => ClientMessage::Release { ids: list(m, "ids", int)?, handles: list(m, "handles", int)? },
        other => return Err(format!("unknown message type '{other}'")),
    })
}

// ---------------------------------------------------------------------------------------------
// Encoding

/// A minimal CBOR writer: definite lengths, explicit float widths.
#[derive(Default)]
pub struct Cbor(pub Vec<u8>);

impl Cbor {
    fn head(&mut self, major: u8, n: u64) {
        let m = major << 5;
        match n {
            0..=23 => self.0.push(m | n as u8),
            24..=0xff => self.0.extend_from_slice(&[m | 24, n as u8]),
            0x100..=0xffff => {
                self.0.push(m | 25);
                self.0.extend_from_slice(&(n as u16).to_be_bytes());
            }
            0x1_0000..=0xffff_ffff => {
                self.0.push(m | 26);
                self.0.extend_from_slice(&(n as u32).to_be_bytes());
            }
            _ => {
                self.0.push(m | 27);
                self.0.extend_from_slice(&n.to_be_bytes());
            }
        }
    }
    pub fn int(&mut self, v: i64) {
        if v >= 0 { self.head(0, v as u64) } else { self.head(1, (-1 - v) as u64) }
    }
    pub fn text(&mut self, s: &str) {
        self.head(3, s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    pub fn array(&mut self, n: usize) {
        self.head(4, n as u64);
    }
    pub fn map(&mut self, n: usize) {
        self.head(5, n as u64);
    }
    pub fn bool(&mut self, b: bool) {
        self.0.push(if b { 0xf5 } else { 0xf4 });
    }
    pub fn null(&mut self) {
        self.0.push(0xf6);
    }
    pub fn f64(&mut self, v: f64) {
        self.0.push(0xfb);
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.0.push(0xfa);
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    /// `[type, {` with `fields` entries to follow.
    fn poly(&mut self, t: &str, fields: usize) {
        self.array(2);
        self.text(t);
        self.map(fields);
    }

    pub fn value(&mut self, v: &WireValue) {
        match v {
            WireValue::Null => self.poly("null", 0),
            WireValue::Bool(b) => {
                self.poly("bool", 1);
                self.text("value");
                self.bool(*b);
            }
            WireValue::Int(i) => {
                self.poly("int", 1);
                self.text("value");
                self.int(*i as i64);
            }
            WireValue::Long(i) => {
                self.poly("long", 1);
                self.text("value");
                self.int(*i);
            }
            WireValue::Float(f) => {
                self.poly("float", 1);
                self.text("value");
                self.f32(*f);
            }
            WireValue::Double(f) => {
                self.poly("double", 1);
                self.text("value");
                self.f64(*f);
            }
            WireValue::String(s) => {
                self.poly("string", 1);
                self.text("value");
                self.text(s);
            }
            WireValue::Ref(id) => {
                self.poly("ref", 1);
                self.text("id");
                self.int(*id);
            }
            WireValue::Instance(n) => {
                self.poly("instance", 1);
                self.text("name");
                self.text(n);
            }
            WireValue::Record { class_name, fields } => {
                self.poly("record", 2);
                self.text("className");
                self.text(class_name);
                self.text("fields");
                self.map(fields.len());
                for (k, v) in fields {
                    self.text(k);
                    self.value(v);
                }
            }
            WireValue::Handle { class_name, id } => {
                self.poly("handle", 2);
                self.text("className");
                self.text(class_name);
                self.text("id");
                self.int(*id);
            }
            WireValue::Enum { enum_name, entry } => {
                self.poly("enum", 2);
                self.text("enumName");
                self.text(enum_name);
                self.text("entry");
                self.text(entry);
            }
        }
    }

    fn heap(&mut self, h: &HeapObject) {
        self.map(5);
        self.text("id");
        self.int(h.id);
        self.text("kind");
        self.text(h.kind.name());
        self.text("version");
        self.int(h.version);
        for (name, vs) in [("elements", &h.elements), ("entries", &h.entries)] {
            self.text(name);
            match vs {
                None => self.null(),
                Some(vs) => {
                    self.array(vs.len());
                    for v in vs {
                        self.value(v);
                    }
                }
            }
        }
    }
}

pub fn encode_service(msg: &ServiceMessage) -> Vec<u8> {
    let mut c = Cbor::default();
    match msg {
        ServiceMessage::Result { call_id, objects, value } => {
            c.poly("result", 3);
            c.text("callId");
            c.int(*call_id);
            c.text("objects");
            c.array(objects.len());
            for h in objects {
                c.heap(h);
            }
            c.text("value");
            c.value(value);
        }
        ServiceMessage::Failure { call_id, message, code } => {
            c.poly("failure", 3);
            c.text("callId");
            c.int(*call_id);
            c.text("message");
            c.text(message);
            c.text("code");
            match code {
                Some(s) => c.text(s),
                None => c.null(),
            }
        }
    }
    c.0
}

// ---------------------------------------------------------------------------------------------
// The client side, for tests and load generation

impl Cbor {
    fn model(&mut self, m: &WireModel) {
        self.map(2);
        self.text("metamodelPath");
        self.text(&m.metamodel_path);
        self.text("instances");
        self.array(m.instances.len());
        for i in &m.instances {
            self.map(4);
            self.text("name");
            self.text(&i.name);
            self.text("className");
            self.text(&i.class_name);
            self.text("attributes");
            self.map(i.attributes.len());
            for (k, vs) in &i.attributes {
                self.text(k);
                self.array(vs.len());
                for v in vs {
                    self.value(v);
                }
            }
            self.text("references");
            self.map(i.references.len());
            for (k, ts) in &i.references {
                self.text(k);
                self.array(ts.len());
                for t in ts {
                    self.text(t);
                }
            }
        }
    }
}

pub fn encode_client(msg: &ClientMessage) -> Vec<u8> {
    let mut c = Cbor::default();
    match msg {
        ClientMessage::Call { call_id, operation, objects, args, model_id } => {
            c.poly("call", 5);
            c.text("callId");
            c.int(*call_id);
            c.text("operation");
            c.text(operation);
            c.text("objects");
            c.array(objects.len());
            for h in objects {
                c.heap(h);
            }
            c.text("args");
            c.array(args.len());
            for a in args {
                c.value(a);
            }
            c.text("modelId");
            match model_id {
                Some(id) => c.int(*id),
                None => c.null(),
            }
        }
        ClientMessage::Metamodel(mm) => {
            c.poly("metamodel", 1);
            c.text("metamodel");
            c.map(2);
            c.text("path");
            c.text(&mm.path);
            c.text("classes");
            c.array(mm.classes.len());
            for name in &mm.classes {
                c.map(1);
                c.text("name");
                c.text(name);
            }
        }
        ClientMessage::Model { model_id, model } => {
            c.poly("model", 2);
            c.text("modelId");
            c.int(*model_id);
            c.text("model");
            c.model(model);
        }
        ClientMessage::Release { ids, handles } => {
            c.poly("release", 2);
            c.text("ids");
            c.array(ids.len());
            for i in ids {
                c.int(*i);
            }
            c.text("handles");
            c.array(handles.len());
            for i in handles {
                c.int(*i);
            }
        }
    }
    c.0
}

pub fn decode_service(bytes: &[u8]) -> R<ServiceMessage> {
    let v: Value = ciborium::de::from_reader(bytes).map_err(|e| format!("not CBOR: {e}"))?;
    let (t, m) = poly(&v)?;
    Ok(match t {
        "result" => ServiceMessage::Result {
            call_id: int(req(m, "callId")?)?,
            objects: list(m, "objects", decode_heap)?,
            value: field(m, "value").map(decode_value).transpose()?.unwrap_or(WireValue::Null),
        },
        "failure" => ServiceMessage::Failure {
            call_id: int(req(m, "callId")?)?,
            message: text(req(m, "message")?)?,
            code: field(m, "code").map(text).transpose()?,
        },
        other => return Err(format!("unknown message type '{other}'")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// The example of the protocol specification, as the reference implementation writes it
    /// (indefinite lengths).
    #[test]
    fn decodes_the_reference_example() {
        let bytes = hex(concat!(
            "9f6463616c6cbf6663616c6c496401696f7065726174696f6e626f70676f626a656374739fbf62696401646b696e64646c69",
            "73746776657273696f6e0368656c656d656e74739f9f66646f75626c65bf6576616c7565fb4000000000000000ffffff6765",
            "6e7472696573f6ffff64617267739f9f63726566bf62696401ffff9f646e756c6cbfffffff676d6f64656c4964f6ffff"
        ));
        let ClientMessage::Call { call_id, operation, objects, args, model_id } = decode_client(&bytes).unwrap() else {
            panic!("not a call")
        };
        assert_eq!(call_id, 1);
        assert_eq!(operation, "op");
        assert_eq!(model_id, None);
        assert_eq!(
            objects,
            vec![HeapObject {
                id: 1,
                kind: HeapKind::List,
                version: 3,
                elements: Some(vec![WireValue::Double(2.0)]),
                entries: None
            }]
        );
        assert_eq!(args, vec![WireValue::Ref(1), WireValue::Null]);
    }

    #[test]
    fn encoding_round_trips_through_a_generic_decoder() {
        let msg = ServiceMessage::Result {
            call_id: 7,
            objects: vec![HeapObject {
                id: -1,
                kind: HeapKind::List,
                version: 0,
                elements: Some(vec![WireValue::Double(1.0), WireValue::Float(0.5), WireValue::Long(-3)]),
                entries: None,
            }],
            value: WireValue::Record {
                class_name: "R".into(),
                fields: vec![("xs".into(), WireValue::Ref(-1)), ("s".into(), WireValue::String("ä".into()))],
            },
        };
        let bytes = encode_service(&msg);
        let v: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let (t, m) = poly(&v).unwrap();
        assert_eq!(t, "result");
        assert_eq!(int(req(m, "callId").unwrap()).unwrap(), 7);
        let h = decode_heap(&array(req(m, "objects").unwrap()).unwrap()[0]).unwrap();
        assert_eq!(h.elements.unwrap()[0], WireValue::Double(1.0));
        // a double stays a 64-bit float on the wire, a float a 32-bit one
        assert!(bytes.windows(9).any(|w| w == [0xfb, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0]));
        assert!(bytes.windows(5).any(|w| w == [0xfa, 0x3f, 0, 0, 0]));
    }
}
