//! The one function scripts call, `pcmEvaluate`: [`manifest`] declares it and its records,
//! [`call`] answers it. It simulates the plain PCM part of the model (`crate::pcm`) with
//! SimOxide and returns the measurements (`crate::evaluate`; `API-pcmEvaluate.md`).
//!
//! ```fn
//! val e = pcmEvaluate(end = 1320.0, warmUp = 120.0, profile = steps, groups = scaled)
//! ```

use crate::dynamic::Controller;
use crate::evaluate::{self, Evaluation, LoadStep, Request, ResponseStat, ScalingGroup, ServiceGroup, VERSION};
use crate::config::Config;
use crate::protocol::{HeapKind, HeapObject, WireValue};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub const PLUGIN_ID: &str = "palladio-arch-service";
pub const CONTRIBUTION_ID: &str = "palladio-arch";
pub const SESSION_NAME: &str = "functions";

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Int,
    Long,
    Double,
    Boolean,
    String,
    ListOf(&'static str),
    /// A nullable list of records.
    NullableListOf(&'static str),
    ListOfString,
    /// A model instance (or null), as `Any?`.
    Instance,
    ListOfAny,
    /// A nullable list of model instances.
    NullableListOfAny,
    ListOfDouble,
    NullableListOfDouble,
    ListOfLong,
    Record(&'static str),
}

#[derive(Clone, Copy)]
enum Default {
    None,
    Null,
    Int(i32),
    Long(i64),
    Double(f64),
    Bool(bool),
    Str(&'static str),
}

/// A parameter or a record field.
#[derive(Clone, Copy)]
struct Param {
    name: &'static str,
    ty: Ty,
    default: Default,
}

const fn p(name: &'static str, ty: Ty, default: Default) -> Param {
    Param { name, ty, default }
}

const fn f(name: &'static str, ty: Ty) -> Param {
    Param { name, ty, default: Default::None }
}

const PCM_EVALUATE_PARAMS: &[Param] = &[
    p("end", Ty::Double, Default::None),
    p("warmUp", Ty::Double, Default::Double(0.0)),
    p("binSeconds", Ty::Double, Default::Double(10.0)),
    p("msPerTimeUnit", Ty::Double, Default::Double(1000.0)),
    p("profile", Ty::NullableListOf("PcmLoadStep"), Default::Null),
    p("groups", Ty::NullableListOf("PcmScalingGroup"), Default::Null),
    p("services", Ty::NullableListOf("PcmServiceGroup"), Default::Null),
    p("overrides", Ty::NullableListOf("PcmOverride"), Default::Null),
    p("percentiles", Ty::NullableListOfDouble, Default::Null),
    p("detailCalls", Ty::Boolean, Default::Bool(false)),
    p("histogramPerDecade", Ty::Int, Default::Int(10)),
    p("seed", Ty::Int, Default::Int(1)),
    p("usageStreams", Ty::Boolean, Default::Bool(true)),
    p("fixDoubleResume", Ty::Boolean, Default::Bool(true)),
    p("maxPods", Ty::Int, Default::Int(400)),
    p("hpaSyncSeconds", Ty::Double, Default::Double(15.0)),
    p("hpaTolerance", Ty::Double, Default::Double(0.1)),
    p("hpaDownscaleStabilisationSeconds", Ty::Double, Default::Double(300.0)),
    p("hpaCpuInitialisationSeconds", Ty::Double, Default::Double(300.0)),
    p("maxPathSets", Ty::Int, Default::Int(10_000)),
    p("wallSeconds", Ty::Double, Default::Double(60.0)),
    p("maxEvents", Ty::Long, Default::Long(0)),
    // revision 4 (last: positional calls of revision 2 stay valid)
    p("asyncPaths", Ty::Boolean, Default::Bool(false)),
    p("asyncIgnoreForks", Ty::NullableListOfAny, Default::Null),
];

struct Function {
    name: &'static str,
    params: &'static [Param],
    returns: Ty,
}

const FUNCTIONS: &[Function] = &[Function { name: "pcmEvaluate", params: PCM_EVALUATE_PARAMS, returns: Ty::Record("PcmEvaluation") }];

const RESPONSE_FIELDS: &[Param] = &[
    f("kind", Ty::String),
    f("key", Ty::String),
    f("scenario", Ty::Instance),
    f("call", Ty::Instance),
    f("role", Ty::Instance),
    f("signature", Ty::Instance),
    f("assembly", Ty::Instance),
    f("count", Ty::Long),
    f("meanMs", Ty::Double),
    f("maxMs", Ty::Double),
    f("percentilesMs", Ty::ListOfDouble),
    f("histogramEdgesMs", Ty::ListOfDouble),
    f("histogramCounts", Ty::ListOfLong),
    f("binCounts", Ty::ListOfLong),
    f("binMeanMs", Ty::ListOfDouble),
    f("binPercentilesMs", Ty::ListOfDouble),
];

const RECORDS: &[(&str, &[Param])] = &[
    ("PcmOverride", &[f("resource", Ty::Instance), f("replicas", Ty::Int), f("rate", Ty::Double)]),
    (
        "PcmScalingGroup",
        &[
            f("container", Ty::Instance),
            p("minReplicas", Ty::Int, Default::Int(1)),
            p("maxReplicas", Ty::Int, Default::Int(10)),
            p("targetUtilisation", Ty::Double, Default::Double(0.7)),
            p("initialReplicas", Ty::Int, Default::Int(1)),
            p("startupSeconds", Ty::Double, Default::Double(30.0)),
            p("scaleDownSeconds", Ty::Double, Default::Double(-1.0)),
            p("cpuRequest", Ty::Double, Default::Double(0.0)),
            p("behaviorJson", Ty::String, Default::Str("")),
        ],
    ),
    ("PcmServiceGroup", &[f("container", Ty::Instance), f("assembly", Ty::Instance), f("loadBalancer", Ty::Instance)]),
    ("PcmLoadStep", &[f("scenario", Ty::Instance), f("time", Ty::Double), f("factor", Ty::Double)]),
    ("PcmResponseStat", RESPONSE_FIELDS),
    (
        "PcmResourceStat",
        &[
            f("container", Ty::Instance),
            f("resource", Ty::Instance),
            f("kind", Ty::String),
            f("autoscaled", Ty::Boolean),
            f("cores", Ty::Double),
            f("meanUtilisation", Ty::Double),
            f("binUtilisation", Ty::ListOfDouble),
            f("maxBinUtilisation", Ty::Double),
            f("meanReadyReplicas", Ty::Double),
            f("peakReadyReplicas", Ty::Double),
            f("binReadyReplicas", Ty::ListOfDouble),
            f("binReplicas", Ty::ListOfDouble),
            f("replicaCoreSeconds", Ty::Double),
            f("readyReplicaCoreSeconds", Ty::Double),
            f("rescales", Ty::Long),
            f("createdPods", Ty::Long),
            f("reversals", Ty::Long),
        ],
    ),
    ("PcmCallCount", &[f("action", Ty::Instance), f("count", Ty::Long)]),
    ("PcmPathSet", &[f("scenario", Ty::Instance), f("containers", Ty::ListOfAny), f("count", Ty::Long)]),
    (
        "PcmAsyncPathSet",
        &[
            f("scenario", Ty::Instance),
            f("assembly", Ty::Instance),
            f("seff", Ty::Instance),
            f("signature", Ty::Instance),
            f("context", Ty::ListOfAny),
            f("localContext", Ty::ListOfAny),
            f("containers", Ty::ListOfAny),
            f("count", Ty::Long),
        ],
    ),
    (
        "PcmEvaluation",
        &[
            f("failed", Ty::Boolean),
            f("capped", Ty::Boolean),
            f("timedOut", Ty::Boolean),
            f("message", Ty::String),
            f("version", Ty::String),
            f("windowSeconds", Ty::Double),
            f("binSeconds", Ty::Double),
            f("bins", Ty::Int),
            f("percentiles", Ty::ListOfDouble),
            f("simulatedSeconds", Ty::Double),
            f("events", Ty::Long),
            f("wallMs", Ty::Double),
            f("operations", Ty::ListOf("PcmResponseStat")),
            f("sessions", Ty::ListOf("PcmResponseStat")),
            f("calls", Ty::ListOf("PcmResponseStat")),
            f("assemblyOperations", Ty::ListOf("PcmResponseStat")),
            f("resources", Ty::ListOf("PcmResourceStat")),
            f("callCounts", Ty::ListOf("PcmCallCount")),
            f("pathSets", Ty::ListOf("PcmPathSet")),
            f("pathSetsTruncated", Ty::Boolean),
            f("asyncPathsMeasured", Ty::Boolean),
            f("asyncPathSets", Ty::ListOf("PcmAsyncPathSet")),
            f("asyncPathSetsTruncated", Ty::Boolean),
            f("createdPods", Ty::Long),
            f("rescales", Ty::Long),
            f("peakPods", Ty::Double),
            f("reversals", Ty::Long),
            f("warnings", Ty::ListOfString),
        ],
    ),
];

fn type_json(ty: Ty) -> Value {
    let builtin = |t: &str, nullable: bool| json!({"package": "builtin", "type": t, "isNullable": nullable});
    let record = |t: &str| json!({"package": format!("contrib/{CONTRIBUTION_ID}"), "type": t, "isNullable": false});
    let list = |t: Value, nullable: bool| json!({"package": "builtin", "type": "ReadonlyList", "isNullable": nullable, "typeArgs": {"T": t}});
    match ty {
        Ty::Int => builtin("int", false),
        Ty::Long => builtin("long", false),
        Ty::Double => builtin("double", false),
        Ty::Boolean => builtin("boolean", false),
        Ty::String => builtin("string", false),
        Ty::Record(r) => record(r),
        Ty::ListOf(r) => list(record(r), false),
        Ty::NullableListOf(r) => list(record(r), true),
        Ty::ListOfString => list(builtin("string", false), false),
        Ty::Instance => builtin("Any", true),
        Ty::ListOfAny => list(builtin("Any", true), false),
        Ty::NullableListOfAny => list(builtin("Any", true), true),
        Ty::ListOfDouble => list(builtin("double", false), false),
        Ty::NullableListOfDouble => list(builtin("double", false), true),
        Ty::ListOfLong => list(builtin("long", false), false),
    }
}

/// The default's literal, as the platform evaluates it.
fn literal(d: Default, ty: Ty, type_index: &mut impl FnMut(Value) -> usize) -> Option<Value> {
    let mut lit = match d {
        Default::None => return None,
        Default::Null => json!({"kind": "nullLiteral"}),
        Default::Int(v) => json!({"kind": "intLiteral", "value": v.to_string()}),
        Default::Long(v) => json!({"kind": "longLiteral", "value": v.to_string()}),
        Default::Double(v) => json!({"kind": "doubleLiteral", "value": format!("{v:?}")}),
        Default::Bool(v) => json!({"kind": "booleanLiteral", "value": v}),
        Default::Str(v) => json!({"kind": "stringLiteral", "value": v}),
    };
    lit["evalType"] = json!(type_index(type_json(ty)));
    Some(lit)
}

/// The plugin manifest, as `GET /` serves it.
pub fn manifest() -> Value {
    let mut types: Vec<Value> = Vec::new();
    let mut type_index = |t: Value| -> usize {
        match types.iter().position(|x| *x == t) {
            Some(i) => i,
            None => {
                types.push(t);
                types.len() - 1
            }
        }
    };
    let mut functions = serde_json::Map::new();
    for fun in FUNCTIONS {
        let mut defaults = serde_json::Map::new();
        for prm in fun.params {
            if let Some(l) = literal(prm.default, prm.ty, &mut type_index) {
                defaults.insert(prm.name.into(), l);
            }
        }
        let mut sig = json!({
            "signature": {
                "parameters": fun.params.iter().map(|p| json!({"name": p.name, "type": type_json(p.ty)})).collect::<Vec<_>>(),
                "returnType": type_json(fun.returns),
            },
            "implementation": {"kind": "external", "operation": fun.name, "model": "readonly"},
        });
        if !defaults.is_empty() {
            sig["defaultValues"] = Value::Object(defaults);
        }
        functions.insert(fun.name.into(), json!({"signatures": {"": sig}}));
    }
    let mut classes = serde_json::Map::new();
    for (name, fields) in RECORDS {
        let fields: Vec<Value> = fields
            .iter()
            .map(|p| {
                let mut v = json!({"name": p.name, "type": type_json(p.ty)});
                if let Some(l) = literal(p.default, p.ty, &mut type_index) {
                    v["defaultValue"] = l;
                }
                v
            })
            .collect();
        classes.insert(name.to_string(), json!({"kind": "record", "fields": fields}));
    }
    json!({
        "id": PLUGIN_ID,
        "name": "Palladio Simulation",
        "version": VERSION,
        "description": format!("Simulates the plain Palladio (PCM) part of a model with SimOxide and returns the measurements (version {VERSION})"),
        "icon": [
            ["path", {"d": "M4 4h6v6H4z"}],
            ["path", {"d": "M14 14h6v6h-6z"}],
            ["path", {"d": "M10 7h4v10"}]
        ],
        "languagePlugins": [],
        "contributionPlugins": [{
            "languageId": "script",
            "description": format!("pcmEvaluate(): one SimOxide simulation of the model's plain PCM (per-scenario load profile, SPD + HPA autoscaling) and its measurements: response times, utilisation, pods, call counts, synchronous paths (version {VERSION})"),
            "additionalKeywords": [],
            "serverContributionPlugins": [{
                "type": "script-language-contribution",
                "functions": functions,
                "expressions": {},
                "classes": classes,
                "types": types,
                "id": CONTRIBUTION_ID,
                "sessions": {SESSION_NAME: {"protocol": crate::protocol::PROTOCOL, "versions": [crate::protocol::VERSION]}}
            }]
        }]
    })
}

// ---------------------------------------------------------------------------------------------
// Calls

pub struct Answer {
    pub value: WireValue,
    pub objects: Vec<HeapObject>,
}

type Lists = HashMap<i64, Vec<WireValue>>;
type Fields = Vec<(String, WireValue)>;

struct Args<'a> {
    f: &'static Function,
    args: &'a [WireValue],
}

fn number(v: &WireValue) -> Option<f64> {
    match v {
        WireValue::Double(x) => Some(*x),
        WireValue::Float(x) => Some(*x as f64),
        WireValue::Int(x) => Some(*x as f64),
        WireValue::Long(x) => Some(*x as f64),
        _ => None,
    }
}

fn default_value(d: Default) -> Option<WireValue> {
    match d {
        Default::Int(v) => Some(WireValue::Int(v)),
        Default::Long(v) => Some(WireValue::Long(v)),
        Default::Double(v) => Some(WireValue::Double(v)),
        Default::Bool(v) => Some(WireValue::Bool(v)),
        Default::Str(v) => Some(WireValue::String(v.into())),
        Default::Null => Some(WireValue::Null),
        Default::None => None,
    }
}

/// A record field: its value, or the declared default when the record lacks it.
fn field(record: &'static str, r: &Fields, k: &str) -> Option<WireValue> {
    if let Some((_, v)) = r.iter().find(|(n, _)| n == k) {
        return Some(v.clone());
    }
    let (_, fields) = RECORDS.iter().find(|(n, _)| *n == record)?;
    default_value(fields.iter().find(|p| p.name == k)?.default)
}

impl Args<'_> {
    /// An argument, or its declared default when the call leaves it out.
    fn get(&self, name: &str) -> Result<WireValue, String> {
        let i = self.f.params.iter().position(|p| p.name == name).expect("declared parameter");
        if let Some(v) = self.args.get(i) {
            return Ok(v.clone());
        }
        default_value(self.f.params[i].default).ok_or_else(|| format!("{}: argument {name} is missing", self.f.name))
    }
    fn long(&self, name: &str) -> Result<i64, String> {
        match self.get(name)? {
            WireValue::Int(i) => Ok(i as i64),
            WireValue::Long(i) => Ok(i),
            other => Err(format!("{}: {name} must be a whole number, not {other:?}", self.f.name)),
        }
    }
    fn boolean(&self, name: &str) -> Result<bool, String> {
        match self.get(name)? {
            WireValue::Bool(b) => Ok(b),
            other => Err(format!("{}: {name} must be a boolean, not {other:?}", self.f.name)),
        }
    }
    fn double(&self, name: &str) -> Result<f64, String> {
        number(&self.get(name)?).ok_or_else(|| format!("{}: {name} must be a number", self.f.name))
    }

    /// A list argument (`None`: null): the elements of the collection it refers to.
    fn list<'l>(&self, name: &str, lists: &'l Lists) -> Result<Option<&'l [WireValue]>, String> {
        match self.get(name)? {
            WireValue::Ref(id) => lists.get(&id).map(|v| Some(v.as_slice())).ok_or_else(|| format!("{}: the list {name} is not held", self.f.name)),
            WireValue::Null => Ok(None),
            other => Err(format!("{}: {name} must be a list, not {other:?}", self.f.name)),
        }
    }

    fn records(&self, name: &str, lists: &Lists) -> Result<Vec<Fields>, String> {
        self.list(name, lists)?
            .unwrap_or(&[])
            .iter()
            .map(|v| match v {
                WireValue::Record { fields, .. } => Ok(fields.clone()),
                other => Err(format!("{}: {name} must hold records, not {other:?}", self.f.name)),
            })
            .collect()
    }

    /// The arguments of `pcmEvaluate`.
    fn request(&self, lists: &Lists) -> Result<Request, String> {
        let fname = self.f.name;
        let inst = |rec: &'static str, r: &Fields, k: &str| -> Result<String, String> {
            match field(rec, r, k) {
                Some(WireValue::Instance(n)) => Ok(n),
                other => Err(format!("{fname}: {rec}.{k} must be a model element, not {other:?}")),
            }
        };
        let num = |rec: &'static str, r: &Fields, k: &str| -> Result<f64, String> {
            field(rec, r, k).as_ref().and_then(number).ok_or_else(|| format!("{fname}: {rec}.{k} must be a number"))
        };
        let mut overrides = Vec::new();
        for r in self.records("overrides", lists)? {
            const O: &str = "PcmOverride";
            overrides.push((inst(O, &r, "resource")?, num(O, &r, "replicas")? as i32, num(O, &r, "rate")?));
        }
        let mut groups = Vec::new();
        for r in self.records("groups", lists)? {
            const G: &str = "PcmScalingGroup";
            let scale_down = num(G, &r, "scaleDownSeconds")?;
            let request = num(G, &r, "cpuRequest")?;
            let behavior = match field(G, &r, "behaviorJson") {
                Some(WireValue::String(s)) if !s.trim().is_empty() => {
                    let v: Value = serde_json::from_str(&s).map_err(|e| format!("{fname}: behaviorJson is not JSON: {e}"))?;
                    if !v.is_object() {
                        return Err(format!("{fname}: behaviorJson must be a JSON object"));
                    }
                    Some(v)
                }
                _ => None,
            };
            groups.push(ScalingGroup {
                container: inst(G, &r, "container")?,
                min: num(G, &r, "minReplicas")?.max(1.0) as u32,
                max: num(G, &r, "maxReplicas")?.max(1.0) as u32,
                target: num(G, &r, "targetUtilisation")?,
                initial: num(G, &r, "initialReplicas")?.max(1.0) as u32,
                startup_s: num(G, &r, "startupSeconds")?.max(0.0),
                scale_down_s: (scale_down >= 0.0).then_some(scale_down),
                cpu_request: (request > 0.0).then_some(request),
                behavior,
            });
        }
        let mut services = Vec::new();
        for r in self.records("services", lists)? {
            const S: &str = "PcmServiceGroup";
            services.push(ServiceGroup { container: inst(S, &r, "container")?, assembly: inst(S, &r, "assembly")?, load_balancer: inst(S, &r, "loadBalancer")? });
        }
        let mut profile = Vec::new();
        for r in self.records("profile", lists)? {
            const L: &str = "PcmLoadStep";
            let scenario = match field(L, &r, "scenario") {
                Some(WireValue::Instance(n)) => Some(n),
                None | Some(WireValue::Null) => None,
                other => return Err(format!("{fname}: a load step's scenario must be a UsageScenario or null, not {other:?}")),
            };
            profile.push(LoadStep { scenario, time: num(L, &r, "time")?, factor: num(L, &r, "factor")? });
        }
        let percentiles = match self.list("percentiles", lists)? {
            None => evaluate::DEFAULT_PERCENTILES.to_vec(),
            Some(v) => v.iter().map(|x| number(x).ok_or_else(|| format!("{fname}: percentiles must hold numbers"))).collect::<Result<_, _>>()?,
        };
        let r = Request {
            overrides,
            groups,
            services,
            profile,
            warm_up_s: self.double("warmUp")?,
            end_s: self.double("end")?,
            bin_s: self.double("binSeconds")?,
            ms_per_unit: self.double("msPerTimeUnit")?,
            percentiles,
            detail_calls: self.boolean("detailCalls")?,
            histogram_per_decade: self.long("histogramPerDecade")?.max(0) as usize,
            seed: self.long("seed")?,
            usage_streams: self.boolean("usageStreams")?,
            fix_double_resume: self.boolean("fixDoubleResume")?,
            max_pods: self.long("maxPods")?.max(1) as u32,
            controller: Controller {
                sync_s: self.double("hpaSyncSeconds")?,
                tolerance: self.double("hpaTolerance")?,
                downscale_stabilisation_s: self.double("hpaDownscaleStabilisationSeconds")?,
                cpu_initialisation_s: self.double("hpaCpuInitialisationSeconds")?,
            },
            max_path_sets: self.long("maxPathSets")?.max(1) as usize,
            wall_s: self.double("wallSeconds")?,
            max_events: self.long("maxEvents")?.max(0) as u64,
            async_paths: self.boolean("asyncPaths")?,
            async_ignore_forks: match self.list("asyncIgnoreForks", lists)? {
                None => Vec::new(),
                Some(v) => v
                    .iter()
                    .map(|x| match x {
                        WireValue::Instance(n) => Ok(n.clone()),
                        other => Err(format!("{fname}: asyncIgnoreForks must hold ForkActions, not {other:?}")),
                    })
                    .collect::<Result<_, _>>()?,
            },
        };
        r.check().map_err(|e| format!("{fname}: {e}"))?;
        Ok(r)
    }
}

pub fn is_operation(name: &str) -> bool {
    FUNCTIONS.iter().any(|f| f.name == name)
}

/// The held lists of an answer.
struct Heap<'a, F: FnMut() -> i64> {
    objects: Vec<HeapObject>,
    next_id: &'a mut F,
}

impl<F: FnMut() -> i64> Heap<'_, F> {
    fn list(&mut self, elements: Vec<WireValue>) -> WireValue {
        let id = (self.next_id)();
        self.objects.push(HeapObject { id, kind: HeapKind::List, version: 0, elements: Some(elements), entries: None });
        WireValue::Ref(id)
    }
    fn doubles(&mut self, v: &[f64]) -> WireValue {
        self.list(v.iter().map(|x| WireValue::Double(*x)).collect())
    }
    fn longs(&mut self, v: &[u64]) -> WireValue {
        self.list(v.iter().map(|x| WireValue::Long(*x as i64)).collect())
    }
}

fn record(class: &str, fields: Vec<(&str, WireValue)>) -> WireValue {
    WireValue::Record { class_name: class.into(), fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
}

fn inst(name: &str) -> WireValue {
    WireValue::Instance(name.to_string())
}

fn opt_inst(name: &Option<String>) -> WireValue {
    name.as_deref().map_or(WireValue::Null, inst)
}

/// `PcmEvaluation` with its lists.
fn evaluation(e: &Evaluation, next_id: &mut impl FnMut() -> i64) -> Answer {
    let mut h = Heap { objects: Vec::new(), next_id };
    let d = WireValue::Double;
    let l = |n: u64| WireValue::Long(n as i64);
    let responses = |v: &[ResponseStat], h: &mut Heap<_>| -> WireValue {
        let mut items = Vec::new();
        for s in v {
            let fields = vec![
                ("kind", WireValue::String(s.kind.into())),
                ("key", WireValue::String(s.key.clone())),
                ("scenario", opt_inst(&s.scenario)),
                ("call", opt_inst(&s.call)),
                ("role", opt_inst(&s.role)),
                ("signature", opt_inst(&s.signature)),
                ("assembly", opt_inst(&s.assembly)),
                ("count", l(s.count)),
                ("meanMs", d(s.mean_ms)),
                ("maxMs", d(s.max_ms)),
                ("percentilesMs", h.doubles(&s.percentiles_ms)),
                ("histogramEdgesMs", h.doubles(&s.histogram_edges_ms)),
                ("histogramCounts", h.longs(&s.histogram_counts)),
                ("binCounts", h.longs(&s.bin_counts)),
                ("binMeanMs", h.doubles(&s.bin_mean_ms)),
                ("binPercentilesMs", h.doubles(&s.bin_percentiles_ms)),
            ];
            items.push(record("PcmResponseStat", fields));
        }
        h.list(items)
    };
    let operations = responses(&e.operations, &mut h);
    let sessions = responses(&e.sessions, &mut h);
    let calls = responses(&e.calls, &mut h);
    let asm = responses(&e.assembly_operations, &mut h);
    let mut items = Vec::new();
    for c in &e.resources {
        let fields = vec![
            ("container", inst(&c.container)),
            ("resource", inst(&c.resource)),
            ("kind", WireValue::String(c.kind.clone())),
            ("autoscaled", WireValue::Bool(c.autoscaled)),
            ("cores", d(c.cores)),
            ("meanUtilisation", d(c.mean_utilisation)),
            ("binUtilisation", h.doubles(&c.bin_utilisation)),
            ("maxBinUtilisation", d(c.max_bin_utilisation)),
            ("meanReadyReplicas", d(c.mean_ready_replicas)),
            ("peakReadyReplicas", d(c.peak_ready_replicas)),
            ("binReadyReplicas", h.doubles(&c.bin_ready_replicas)),
            ("binReplicas", h.doubles(&c.bin_replicas)),
            ("replicaCoreSeconds", d(c.replica_core_seconds)),
            ("readyReplicaCoreSeconds", d(c.ready_replica_core_seconds)),
            ("rescales", l(c.rescales)),
            ("createdPods", l(c.created_pods)),
            ("reversals", l(c.reversals)),
        ];
        items.push(record("PcmResourceStat", fields));
    }
    let resources = h.list(items);
    let counts = e.call_counts.iter().map(|(a, n)| record("PcmCallCount", vec![("action", inst(a)), ("count", l(*n))])).collect();
    let call_counts = h.list(counts);
    let mut sets = Vec::new();
    for s in &e.path_sets {
        let cs = h.list(s.containers.iter().map(|c| inst(c)).collect());
        sets.push(record("PcmPathSet", vec![("scenario", inst(&s.scenario)), ("containers", cs), ("count", l(s.count))]));
    }
    let path_sets = h.list(sets);
    let mut asets = Vec::new();
    for s in &e.async_path_sets {
        let ctx = h.list(s.context.iter().map(|c| inst(c)).collect());
        let local = h.list(s.local_context.iter().map(|c| inst(c)).collect());
        let cs = h.list(s.containers.iter().map(|c| inst(c)).collect());
        let scen = s.scenario.as_deref().map_or(WireValue::Null, inst);
        asets.push(record(
            "PcmAsyncPathSet",
            vec![
                ("scenario", scen),
                ("assembly", inst(&s.assembly)),
                ("seff", inst(&s.seff)),
                ("signature", inst(&s.signature)),
                ("context", ctx),
                ("localContext", local),
                ("containers", cs),
                ("count", l(s.count)),
            ],
        ));
    }
    let async_path_sets = h.list(asets);
    let percentiles = h.doubles(&e.percentiles);
    let warnings = h.list(e.warnings.iter().map(|w| WireValue::String(w.clone())).collect());
    let value = record(
        "PcmEvaluation",
        vec![
            ("failed", WireValue::Bool(e.failed)),
            ("capped", WireValue::Bool(e.capped)),
            ("timedOut", WireValue::Bool(e.timed_out)),
            ("message", WireValue::String(e.message.clone())),
            ("version", WireValue::String(VERSION.into())),
            ("windowSeconds", d(e.window_s)),
            ("binSeconds", d(e.bin_s)),
            ("bins", WireValue::Int(e.bins as i32)),
            ("percentiles", percentiles),
            ("simulatedSeconds", d(e.simulated_s)),
            ("events", l(e.events)),
            ("wallMs", d(e.wall_ms)),
            ("operations", operations),
            ("sessions", sessions),
            ("calls", calls),
            ("assemblyOperations", asm),
            ("resources", resources),
            ("callCounts", call_counts),
            ("pathSets", path_sets),
            ("pathSetsTruncated", WireValue::Bool(e.path_sets_truncated)),
            ("asyncPathsMeasured", WireValue::Bool(e.async_paths_measured)),
            ("asyncPathSets", async_path_sets),
            ("asyncPathSetsTruncated", WireValue::Bool(e.async_path_sets_truncated)),
            ("createdPods", l(e.created_pods)),
            ("rescales", l(e.rescales)),
            ("peakPods", d(e.peak_pods)),
            ("reversals", l(e.reversals)),
            ("warnings", warnings),
        ],
    );
    Answer { value, objects: h.objects }
}

/// Answers a call on the model's plain PCM `cache`.
pub async fn call(
    config: &Arc<Config>,
    operation: &str,
    args: &[WireValue],
    lists: &Lists,
    cache: &Arc<crate::pcm::PcmCache>,
    dumped: Option<&std::path::Path>,
    next_id: &mut impl FnMut() -> i64,
) -> Result<Answer, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let req = Args { f, args }.request(lists)?;
    // with SIM_DUMP_DIR: the request next to the dumped model, for `palladio-sim-service evaluate`
    if let Some(m) = dumped {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::write(m.with_extension(format!("request-{n:04}.json")), req.to_json().to_string());
    }
    let req = req.canonical(&cache.canon);
    let e = evaluate::evaluator(config).evaluate(cache, req).await.map_err(|e| cache.canon.orig_text(&e, false))?;
    Ok(evaluation(&e.to_caller(&cache.canon), next_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_function_and_records_with_defaults() {
        let m = manifest();
        assert_eq!(m["version"], VERSION);
        let c = &m["contributionPlugins"][0]["serverContributionPlugins"][0];
        assert_eq!(c["id"], CONTRIBUTION_ID);
        let sig = &c["functions"]["pcmEvaluate"]["signatures"][""];
        assert_eq!(sig["signature"]["parameters"].as_array().unwrap().len(), PCM_EVALUATE_PARAMS.len());
        let defaults = sig["defaultValues"].as_object().unwrap();
        assert_eq!(defaults.len(), PCM_EVALUATE_PARAMS.len() - 1, "every parameter but end");
        assert_eq!(defaults["wallSeconds"]["kind"], "doubleLiteral");
        assert_eq!(defaults["wallSeconds"]["value"], "60.0");
        assert_eq!(defaults["profile"]["kind"], "nullLiteral");
        let types = c["types"].as_array().unwrap();
        for d in defaults.values() {
            assert!(d["evalType"].as_u64().unwrap() < types.len() as u64);
        }
        for (r, fields) in RECORDS {
            assert_eq!(c["classes"][r]["fields"].as_array().unwrap().len(), fields.len(), "{r}");
        }
        let g = c["classes"]["PcmScalingGroup"]["fields"].as_array().unwrap();
        assert!(g[0].get("defaultValue").is_none());
        assert_eq!(g[3]["defaultValue"]["value"], "0.7");
        assert_eq!(c["classes"]["PcmEvaluation"]["fields"].as_array().unwrap().len(), 28);
    }
}
