//! The functions scripts call: [`manifest`] declares them, [`call`] answers them.
//!
//! ```fn
//! val r = archSimulate()                         // the translated model, simulated once
//! val rt = archStat("defaultUsageScenario")      // mean response time of a scenario
//! val cost = archCost()                          // from the CostModel
//! val mq = archModularity()                      // cohesive MQ, containers as clusters (D32)
//! val u = archUtilisation("Server1")             // offered load of a container ("" = highest)
//! val t = archNoLoadTime("login")                // rate-weighted no-load response time, no simulation
//! val n = archReplicas()                         // replicas in use, fixed or autoscaled (D50)
//! val f = archFailure("login")                   // share of failed requests (D53)
//! val c = archChangeCoupling()                   // containers touched per change (D54)
//! val i = archInconsistency("login")             // stale reads + remote writes per request (D54)
//! val v = archLatencyViolation()                 // C1: over the latency budgets (D55)
//! val p = archPeakViolation()                    // C2: unscaled containers over their limit at the peak (D55)
//! val h = archHpaSimulate()                      // a dynamic run with Kubernetes-style autoscaling (crate::dynamic)
//! val c = archHpaCost()                          // its cost: replica-seconds under the HPA over the load profile
//! val v = archHpaSlo()                           // its share of requests over the service level
//! val l = archHpaStat("lag")                     // one of its other measures
//! ```

use crate::translate::Translation;
use palladio_sim_service::engine::{Engine, Outcome, Prepared, RunParams, SeriesStats};
use palladio_sim_service::protocol::{HeapKind, HeapObject, WireValue};
use serde_json::{Value, json};
use std::sync::Arc;

pub const PLUGIN_ID: &str = "palladio-arch-service";
pub const CONTRIBUTION_ID: &str = "palladio-arch";
pub const SESSION_NAME: &str = "functions";

#[derive(Clone, Copy)]
enum Ty {
    Int,
    Long,
    Double,
    NullableDouble,
    Boolean,
    String,
    NullableString,
    Record(&'static str),
    ListOf(&'static str),
    ListOfString,
    /// A model instance (or null), as `Any?`.
    Instance,
    ListOfAny,
    ListOfDouble,
}

#[derive(Clone, Copy)]
enum Default {
    None,
    Int(i32),
    Bool(bool),
    Str(&'static str),
}

#[derive(Clone, Copy)]
struct Param {
    name: &'static str,
    ty: Ty,
    default: Default,
}

const fn p(name: &'static str, ty: Ty, default: Default) -> Param {
    Param { name, ty, default }
}

const RUN_PARAMS: [Param; 5] = [
    p("seed", Ty::Int, Default::Int(1)),
    p("maxMeasurements", Ty::Int, Default::Int(10_000)),
    p("maxSimTime", Ty::Int, Default::Int(-1)),
    p("replications", Ty::Int, Default::Int(1)),
    p("simulateLinkingResources", Ty::Boolean, Default::Bool(false)),
];

const STAT_PARAMS: [Param; 8] = [
    p("element", Ty::String, Default::None),
    p("metric", Ty::String, Default::Str("Response Time")),
    p("statistic", Ty::String, Default::Str("mean")),
    RUN_PARAMS[0],
    RUN_PARAMS[1],
    RUN_PARAMS[2],
    RUN_PARAMS[3],
    RUN_PARAMS[4],
];

const UTIL_PARAMS: [Param; 1] = [p("element", Ty::String, Default::Str(""))];

const NO_LOAD_PARAMS: [Param; 1] = [p("exclude", Ty::String, Default::Str(""))];

const PCM_VISITS_PARAMS: [Param; 3] =
    [p("actions", Ty::ListOfAny, Default::None), p("weights", Ty::ListOfDouble, Default::None), p("exclude", Ty::String, Default::Str(""))];

const PCM_CALLS_PARAMS: [Param; 2] = [p("transitions", Ty::ListOfAny, Default::None), p("probabilities", Ty::ListOfDouble, Default::None)];

const PCM_FAILURE_PARAMS: [Param; 4] = [
    p("containers", Ty::ListOfAny, Default::None),
    p("unavailabilities", Ty::ListOfDouble, Default::None),
    p("transparent", Ty::ListOfAny, Default::None),
    p("exclude", Ty::String, Default::Str("")),
];

const PCM_RUN_PARAMS: [Param; 6] = [
    p("overrides", Ty::ListOf("PcmOverride"), Default::None),
    RUN_PARAMS[0],
    RUN_PARAMS[1],
    RUN_PARAMS[2],
    RUN_PARAMS[3],
    RUN_PARAMS[4],
];

const PCM_STAT_PARAMS: [Param; 9] = [
    p("overrides", Ty::ListOf("PcmOverride"), Default::None),
    STAT_PARAMS[0],
    STAT_PARAMS[1],
    STAT_PARAMS[2],
    RUN_PARAMS[0],
    RUN_PARAMS[1],
    RUN_PARAMS[2],
    RUN_PARAMS[3],
    RUN_PARAMS[4],
];

const PCM_MEAN_PARAMS: [Param; 1] = [p("expression", Ty::String, Default::None)];

const PCM_OP_VISITS_PARAMS: [Param; 2] = [p("actions", Ty::ListOfAny, Default::None), p("weights", Ty::ListOfDouble, Default::None)];

const PCM_OP_FAILURE_PARAMS: [Param; 3] = [
    p("containers", Ty::ListOfAny, Default::None),
    p("unavailabilities", Ty::ListOfDouble, Default::None),
    p("transparent", Ty::ListOfAny, Default::None),
];

const PCM_HPA_PARAMS: [Param; 12] = [
    p("groups", Ty::ListOf("PcmScalingGroup"), Default::None),
    p("services", Ty::ListOf("PcmServiceGroup"), Default::None),
    p("profile", Ty::ListOf("PcmLoadStep"), Default::None),
    p("budgetElements", Ty::ListOfAny, Default::None),
    p("budgetsMs", Ty::ListOfDouble, Default::None),
    p("warmUp", Ty::Double, Default::None),
    p("end", Ty::Double, Default::None),
    p("msPerTimeUnit", Ty::Double, Default::None),
    p("sloFactor", Ty::Double, Default::None),
    p("usageStreams", Ty::Boolean, Default::Bool(false)),
    p("seed", Ty::Int, Default::Int(1)),
    p("maxPods", Ty::Int, Default::Int(400)),
];

const PCM_RUN2_PARAMS: [Param; 8] = [
    p("overrides", Ty::ListOf("PcmOverride"), Default::None),
    p("profile", Ty::ListOf("PcmLoadStep"), Default::None),
    p("usageStreams", Ty::Boolean, Default::Bool(false)),
    RUN_PARAMS[0],
    RUN_PARAMS[1],
    RUN_PARAMS[2],
    RUN_PARAMS[3],
    RUN_PARAMS[4],
];

const PCM_RUN2_STAT_PARAMS: [Param; 11] = [
    p("overrides", Ty::ListOf("PcmOverride"), Default::None),
    p("profile", Ty::ListOf("PcmLoadStep"), Default::None),
    p("usageStreams", Ty::Boolean, Default::Bool(false)),
    STAT_PARAMS[0],
    STAT_PARAMS[1],
    STAT_PARAMS[2],
    RUN_PARAMS[0],
    RUN_PARAMS[1],
    RUN_PARAMS[2],
    RUN_PARAMS[3],
    RUN_PARAMS[4],
];

const HPA_PARAMS: [Param; 2] = [p("seed", Ty::Int, Default::Int(1)), p("exclude", Ty::String, Default::Str("login"))];

const HPA_STAT_PARAMS: [Param; 3] =
    [p("statistic", Ty::String, Default::None), p("seed", Ty::Int, Default::Int(1)), p("exclude", Ty::String, Default::Str("login"))];

/// The measures `archHpaStat` returns.
pub const HPA_STATISTICS: &[&str] = &[
    "cost",
    "idealCost",
    "readyReplicaSeconds",
    "slo",
    "sloSteps",
    "requests",
    "meanMs",
    "p95Ms",
    "lag",
    "lagMax",
    "flapping",
    "peakReplicas",
    "rescales",
    "createdPods",
    "capped",
];

struct Function {
    name: &'static str,
    params: &'static [Param],
    returns: Ty,
    simulates: bool,
}

const FUNCTIONS: &[Function] = &[
    Function { name: "archSimulate", params: &RUN_PARAMS, returns: Ty::Record("ArchResult"), simulates: true },
    Function { name: "archStat", params: &STAT_PARAMS, returns: Ty::Double, simulates: true },
    Function { name: "archCost", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archModularity", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archUtilisation", params: &UTIL_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archInfo", params: &[], returns: Ty::Record("ArchInfo"), simulates: false },
    Function { name: "archNoLoadTime", params: &NO_LOAD_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archReplicas", params: &UTIL_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archFailure", params: &NO_LOAD_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archChangeCoupling", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archInconsistency", params: &NO_LOAD_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archLatencyViolation", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archPeakUtilisation", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archPeakViolation", params: &[], returns: Ty::Double, simulates: false },
    Function { name: "archHpaSimulate", params: &HPA_PARAMS, returns: Ty::Record("ArchHpaResult"), simulates: false },
    Function { name: "archHpaCost", params: &HPA_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archHpaSlo", params: &HPA_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "archHpaStat", params: &HPA_STAT_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmResources", params: &[], returns: Ty::ListOf("PcmResource"), simulates: false },
    Function { name: "pcmScenarios", params: &[], returns: Ty::ListOf("PcmScenario"), simulates: false },
    Function { name: "pcmOperations", params: &[], returns: Ty::ListOf("PcmOperation"), simulates: false },
    Function { name: "pcmWeightedVisits", params: &PCM_VISITS_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmCallRates", params: &PCM_CALLS_PARAMS, returns: Ty::ListOf("PcmCall"), simulates: false },
    Function { name: "pcmFailure", params: &PCM_FAILURE_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmSimulate", params: &PCM_RUN_PARAMS, returns: Ty::Record("ArchResult"), simulates: false },
    Function { name: "pcmStat", params: &PCM_STAT_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmMean", params: &PCM_MEAN_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmWarnings", params: &[], returns: Ty::ListOfString, simulates: false },
    Function { name: "pcmOperationVisits", params: &PCM_OP_VISITS_PARAMS, returns: Ty::ListOf("PcmOperationValue"), simulates: false },
    Function { name: "pcmOperationFailure", params: &PCM_OP_FAILURE_PARAMS, returns: Ty::ListOf("PcmOperationValue"), simulates: false },
    Function { name: "pcmRun", params: &PCM_RUN2_PARAMS, returns: Ty::Record("ArchResult"), simulates: false },
    Function { name: "pcmRunStat", params: &PCM_RUN2_STAT_PARAMS, returns: Ty::Double, simulates: false },
    Function { name: "pcmHpaRun", params: &PCM_HPA_PARAMS, returns: Ty::Record("PcmHpaResult"), simulates: false },
];

const RECORDS: &[(&str, &[(&str, Ty)])] = &[
    (
        "ArchResult",
        &[
            ("series", Ty::ListOf("ArchSeries")),
            ("replications", Ty::Int),
            ("seed", Ty::Int),
            ("events", Ty::Long),
            ("measurements", Ty::Long),
            ("simulatedTime", Ty::Double),
        ],
    ),
    (
        "ArchSeries",
        &[
            ("measuringPoint", Ty::String),
            ("kind", Ty::String),
            ("element", Ty::String),
            ("elementName", Ty::NullableString),
            ("monitor", Ty::NullableString),
            ("metric", Ty::String),
            ("count", Ty::Long),
            ("mean", Ty::Double),
            ("stdDev", Ty::Double),
            ("min", Ty::Double),
            ("max", Ty::Double),
            ("p50", Ty::Double),
            ("p90", Ty::Double),
            ("p95", Ty::Double),
            ("p99", Ty::Double),
            ("timeWeightedMean", Ty::NullableDouble),
            ("ciHalfWidth", Ty::NullableDouble),
        ],
    ),
    (
        "ArchInfo",
        &[
            ("translated", Ty::Boolean),
            ("instances", Ty::Int),
            ("cost", Ty::Double),
            ("modularity", Ty::Double),
            ("maxUtilisation", Ty::Double),
            ("resources", Ty::ListOf("ArchResource")),
            ("warnings", Ty::ListOfString),
        ],
    ),
    (
        "ArchHpaResult",
        &[
            ("cost", Ty::Double),
            ("idealCost", Ty::Double),
            ("readyReplicaSeconds", Ty::Double),
            ("replicaSeconds", Ty::Double),
            ("slo", Ty::Double),
            ("sloSteps", Ty::Double),
            ("requests", Ty::Long),
            ("meanMs", Ty::Double),
            ("p95Ms", Ty::Double),
            ("lag", Ty::Double),
            ("lagMax", Ty::Double),
            ("flapping", Ty::Double),
            ("peakReplicas", Ty::Double),
            ("rescales", Ty::Long),
            ("createdPods", Ty::Long),
            ("capped", Ty::Boolean),
            ("events", Ty::Long),
            ("wallMs", Ty::Double),
            ("groups", Ty::ListOf("ArchHpaGroup")),
        ],
    ),
    (
        "ArchHpaGroup",
        &[
            ("name", Ty::String),
            ("meanReplicas", Ty::Double),
            ("idealReplicas", Ty::Double),
            ("maxReplicas", Ty::Int),
            ("rescales", Ty::Long),
            ("createdPods", Ty::Long),
            ("lag", Ty::Double),
            ("reversals", Ty::Int),
        ],
    ),
    (
        "PcmResource",
        &[
            ("resource", Ty::Instance),
            ("container", Ty::Instance),
            ("kind", Ty::String),
            ("rate", Ty::Double),
            ("replicas", Ty::Double),
            ("demand", Ty::Double),
            ("busy", Ty::Double),
            ("utilisation", Ty::Double),
        ],
    ),
    ("PcmScenario", &[("scenario", Ty::Instance), ("name", Ty::String), ("rate", Ty::Double), ("noLoadTime", Ty::Double)]),
    (
        "PcmOperation",
        &[("call", Ty::Instance), ("signature", Ty::Instance), ("callee", Ty::Instance), ("rate", Ty::Double), ("noLoadTime", Ty::Double)],
    ),
    ("PcmCall", &[("action", Ty::Instance), ("caller", Ty::Instance), ("callee", Ty::Instance), ("rate", Ty::Double)]),
    ("PcmOverride", &[("resource", Ty::Instance), ("replicas", Ty::Int), ("rate", Ty::Double)]),
    (
        "PcmOperationValue",
        &[
            ("call", Ty::Instance),
            ("signature", Ty::Instance),
            ("role", Ty::Instance),
            ("scenario", Ty::Instance),
            ("rate", Ty::Double),
            ("value", Ty::Double),
        ],
    ),
    ("PcmLoadStep", &[("scenario", Ty::Instance), ("time", Ty::Double), ("factor", Ty::Double)]),
    (
        "PcmScalingGroup",
        &[
            ("container", Ty::Instance),
            ("minReplicas", Ty::Int),
            ("maxReplicas", Ty::Int),
            ("targetUtilisation", Ty::Double),
            ("initialReplicas", Ty::Int),
            ("startup", Ty::Double),
            ("scaleDown", Ty::Double),
            ("cpuRequest", Ty::Double),
        ],
    ),
    ("PcmServiceGroup", &[("container", Ty::Instance), ("assembly", Ty::Instance), ("loadBalancer", Ty::Instance)]),
    (
        "PcmHpaResult",
        &[
            ("readyReplicaSeconds", Ty::Double),
            ("replicaSeconds", Ty::Double),
            ("peakReplicas", Ty::Double),
            ("rescales", Ty::Long),
            ("createdPods", Ty::Long),
            ("flapping", Ty::Double),
            ("slo", Ty::Double),
            ("requests", Ty::Long),
            ("meanMs", Ty::Double),
            ("p95Ms", Ty::Double),
            ("capped", Ty::Boolean),
            ("events", Ty::Long),
            ("wallMs", Ty::Double),
            ("groups", Ty::ListOf("PcmHpaGroup")),
        ],
    ),
    (
        "PcmHpaGroup",
        &[
            ("container", Ty::Instance),
            ("meanReplicas", Ty::Double),
            ("readyReplicaSeconds", Ty::Double),
            ("replicaSeconds", Ty::Double),
            ("maxReplicas", Ty::Int),
            ("rescales", Ty::Long),
            ("createdPods", Ty::Long),
            ("reversals", Ty::Int),
        ],
    ),
    (
        "ArchResource",
        &[
            ("name", Ty::String),
            ("container", Ty::String),
            ("kind", Ty::String),
            ("replicas", Ty::Double),
            ("rate", Ty::Double),
            ("busy", Ty::Double),
            ("utilisation", Ty::Double),
        ],
    ),
];

pub const STATISTICS: &[&str] =
    &["mean", "stdDev", "min", "max", "p50", "median", "p90", "p95", "p99", "count", "throughput", "timeWeightedMean", "ciHalfWidth"];

fn type_json(ty: Ty) -> Value {
    let builtin = |t: &str, nullable: bool| json!({"package": "builtin", "type": t, "isNullable": nullable});
    let record = |t: &str| json!({"package": format!("contrib/{CONTRIBUTION_ID}"), "type": t, "isNullable": false});
    match ty {
        Ty::Int => builtin("int", false),
        Ty::Long => builtin("long", false),
        Ty::Double => builtin("double", false),
        Ty::NullableDouble => builtin("double", true),
        Ty::Boolean => builtin("boolean", false),
        Ty::String => builtin("string", false),
        Ty::NullableString => builtin("string", true),
        Ty::Record(r) => record(r),
        Ty::ListOf(r) => json!({"package": "builtin", "type": "ReadonlyList", "isNullable": false, "typeArgs": {"T": record(r)}}),
        Ty::ListOfString => {
            json!({"package": "builtin", "type": "ReadonlyList", "isNullable": false, "typeArgs": {"T": builtin("string", false)}})
        }
        Ty::Instance => builtin("Any", true),
        Ty::ListOfAny => json!({"package": "builtin", "type": "ReadonlyList", "isNullable": false, "typeArgs": {"T": builtin("Any", true)}}),
        Ty::ListOfDouble => {
            json!({"package": "builtin", "type": "ReadonlyList", "isNullable": false, "typeArgs": {"T": builtin("double", false)}})
        }
    }
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
    for f in FUNCTIONS {
        let mut defaults = serde_json::Map::new();
        for prm in f.params {
            let t = type_json(prm.ty);
            let mut lit = match prm.default {
                Default::None => continue,
                Default::Int(v) => json!({"kind": "intLiteral", "value": v.to_string()}),
                Default::Bool(v) => json!({"kind": "booleanLiteral", "value": v}),
                Default::Str(v) => json!({"kind": "stringLiteral", "value": v}),
            };
            lit["evalType"] = json!(type_index(t));
            defaults.insert(prm.name.into(), lit);
        }
        let mut sig = json!({
            "signature": {
                "parameters": f.params.iter().map(|p| json!({"name": p.name, "type": type_json(p.ty)})).collect::<Vec<_>>(),
                "returnType": type_json(f.returns),
            },
            "implementation": {"kind": "external", "operation": f.name, "model": "readonly"},
        });
        if !defaults.is_empty() {
            sig["defaultValues"] = Value::Object(defaults);
        }
        functions.insert(f.name.into(), json!({"signatures": {"": sig}}));
    }
    let classes: serde_json::Map<String, Value> = RECORDS
        .iter()
        .map(|(name, fields)| {
            let fields: Vec<Value> = fields.iter().map(|(n, t)| json!({"name": n, "type": type_json(*t)})).collect();
            (name.to_string(), json!({"kind": "record", "fields": fields}))
        })
        .collect();
    json!({
        "id": PLUGIN_ID,
        "name": "Palladio Architecture",
        "description": "Translates architecture decisions on Palladio (PCM) models into PCM and simulates the result with SimOxide, as script functions",
        "icon": [
            ["path", {"d": "M4 4h6v6H4z"}],
            ["path", {"d": "M14 14h6v6h-6z"}],
            ["path", {"d": "M10 7h4v10"}]
        ],
        "languagePlugins": [],
        "contributionPlugins": [{
            "languageId": "script",
            "description": "archSimulate(), archStat(), archCost(), archModularity(), archUtilisation(), archNoLoadTime(), archReplicas(), archFailure(), archChangeCoupling(), archInconsistency(), archLatencyViolation(), archPeakUtilisation(), archPeakViolation(), archInfo(); archHpaSimulate(), archHpaCost(), archHpaSlo(), archHpaStat(): architecture decisions on Palladio models, translated to PCM and simulated, also with Kubernetes-style autoscaling over a load profile",
            "additionalKeywords": [],
            "serverContributionPlugins": [{
                "type": "script-language-contribution",
                "functions": functions,
                "expressions": {},
                "classes": classes,
                "types": types,
                "id": CONTRIBUTION_ID,
                "sessions": {SESSION_NAME: {"protocol": palladio_sim_service::protocol::PROTOCOL, "versions": [palladio_sim_service::protocol::VERSION]}}
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

struct Args<'a> {
    f: &'static Function,
    args: &'a [WireValue],
}

impl Args<'_> {
    fn get(&self, name: &str) -> Result<&WireValue, String> {
        let i = self.f.params.iter().position(|p| p.name == name).expect("declared parameter");
        self.args.get(i).ok_or_else(|| format!("{}: argument {name} is missing", self.f.name))
    }
    fn long(&self, name: &str) -> Result<i64, String> {
        match self.get(name)? {
            WireValue::Int(i) => Ok(*i as i64),
            WireValue::Long(i) => Ok(*i),
            other => Err(format!("{}: {name} must be a whole number, not {other:?}", self.f.name)),
        }
    }
    fn string(&self, name: &str) -> Result<String, String> {
        match self.get(name)? {
            WireValue::String(s) => Ok(s.clone()),
            other => Err(format!("{}: {name} must be a string, not {other:?}", self.f.name)),
        }
    }
    fn boolean(&self, name: &str) -> Result<bool, String> {
        match self.get(name)? {
            WireValue::Bool(b) => Ok(*b),
            other => Err(format!("{}: {name} must be a boolean, not {other:?}", self.f.name)),
        }
    }
    fn run_params(&self) -> Result<RunParams, String> {
        let replications = self.long("replications")?;
        Ok(RunParams {
            seed: self.long("seed")?,
            max_measurements: self.long("maxMeasurements")?,
            max_sim_time: self.long("maxSimTime")?,
            replications: u32::try_from(replications).map_err(|_| "replications must be positive".to_string())?,
            simulate_linking_resources: self.boolean("simulateLinkingResources")?,
        })
    }
}

pub fn is_operation(name: &str) -> bool {
    FUNCTIONS.iter().any(|f| f.name == name)
}

pub fn simulates(name: &str) -> bool {
    FUNCTIONS.iter().any(|f| f.name == name && f.simulates)
}

/// Whether `name` needs a dynamic run (`crate::dynamic`).
pub fn is_dynamic(name: &str) -> bool {
    name.starts_with("archHpa")
}

/// The parameters of a dynamic function's run.
pub fn hpa_params(operation: &str, args: &[WireValue]) -> Result<crate::dynamic::HpaParams, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let a = Args { f, args };
    Ok(crate::dynamic::HpaParams { seed: a.long("seed")?, exclude: excluded(&a)? })
}

/// Answers a dynamic function from the outcome of its run.
pub fn call_dynamic(
    operation: &str,
    args: &[WireValue],
    o: &crate::dynamic::HpaOutcome,
    next_id: &mut impl FnMut() -> i64,
) -> Result<Answer, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let a = Args { f, args };
    let double = |v: f64| Ok(Answer { value: WireValue::Double(v), objects: vec![] });
    match operation {
        "archHpaCost" => double(o.cost),
        "archHpaSlo" => double(o.slo),
        "archHpaStat" => {
            let st = a.string("statistic")?;
            double(hpa_stat(o, &st).ok_or_else(|| format!("unknown statistic '{st}'; one of {}", HPA_STATISTICS.join(", ")))?)
        }
        "archHpaSimulate" => Ok(hpa_result(o, next_id)),
        other => Err(format!("{other} is not a dynamic function")),
    }
}

/// One measure of a dynamic run (`archHpaStat`).
pub fn hpa_stat(o: &crate::dynamic::HpaOutcome, statistic: &str) -> Option<f64> {
    Some(match statistic {
        "cost" => o.cost,
        "idealCost" => o.ideal_cost,
        "readyReplicaSeconds" => o.ready_replica_seconds,
        "slo" => o.slo,
        "sloSteps" => o.slo_steps,
        "requests" => o.requests as f64,
        "meanMs" => o.mean_ms,
        "p95Ms" => o.p95_ms,
        "lag" => o.lag,
        "lagMax" => o.lag_max,
        "flapping" => o.flapping,
        "peakReplicas" => o.peak_replicas,
        "rescales" => o.rescales as f64,
        "createdPods" => o.created as f64,
        "capped" => f64::from(o.capped as u8),
        _ => return None,
    })
}

fn hpa_result(o: &crate::dynamic::HpaOutcome, next_id: &mut impl FnMut() -> i64) -> Answer {
    let list_id = next_id();
    let groups: Vec<WireValue> = o
        .groups
        .iter()
        .map(|g| WireValue::Record {
            class_name: "ArchHpaGroup".into(),
            fields: vec![
                ("name".into(), WireValue::String(g.name.clone())),
                ("meanReplicas".into(), WireValue::Double(g.mean_replicas)),
                ("idealReplicas".into(), WireValue::Double(g.ideal_replicas)),
                ("maxReplicas".into(), WireValue::Int(g.max_replicas)),
                ("rescales".into(), WireValue::Long(g.rescales as i64)),
                ("createdPods".into(), WireValue::Long(g.created as i64)),
                ("lag".into(), WireValue::Double(g.lag)),
                ("reversals".into(), WireValue::Int(g.reversals as i32)),
            ],
        })
        .collect();
    let value = WireValue::Record {
        class_name: "ArchHpaResult".into(),
        fields: vec![
            ("cost".into(), WireValue::Double(o.cost)),
            ("idealCost".into(), WireValue::Double(o.ideal_cost)),
            ("readyReplicaSeconds".into(), WireValue::Double(o.ready_replica_seconds)),
            ("replicaSeconds".into(), WireValue::Double(o.replica_seconds)),
            ("slo".into(), WireValue::Double(o.slo)),
            ("sloSteps".into(), WireValue::Double(o.slo_steps)),
            ("requests".into(), WireValue::Long(o.requests as i64)),
            ("meanMs".into(), WireValue::Double(o.mean_ms)),
            ("p95Ms".into(), WireValue::Double(o.p95_ms)),
            ("lag".into(), WireValue::Double(o.lag)),
            ("lagMax".into(), WireValue::Double(o.lag_max)),
            ("flapping".into(), WireValue::Double(o.flapping)),
            ("peakReplicas".into(), WireValue::Double(o.peak_replicas)),
            ("rescales".into(), WireValue::Long(o.rescales as i64)),
            ("createdPods".into(), WireValue::Long(o.created as i64)),
            ("capped".into(), WireValue::Bool(o.capped)),
            ("events".into(), WireValue::Long(o.events as i64)),
            ("wallMs".into(), WireValue::Double(o.wall_ms)),
            ("groups".into(), WireValue::Ref(list_id)),
        ],
    };
    Answer { value, objects: vec![HeapObject { id: list_id, kind: HeapKind::List, version: 0, elements: Some(groups), entries: None }] }
}

/// Answers a call. `prepared` is the translated model, compiled (for the simulating functions).
pub async fn call(
    engine: &Arc<Engine>,
    operation: &str,
    args: &[WireValue],
    translation: &Translation,
    prepared: Option<Arc<Prepared>>,
    next_id: &mut impl FnMut() -> i64,
) -> Result<Answer, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let a = Args { f, args };
    let entity = |n: &str| entity_of(translation, n);
    let double = |v: f64| Ok(Answer { value: WireValue::Double(v), objects: vec![] });
    match operation {
        "archCost" => double(translation.cost),
        "archModularity" => double(translation.modularity),
        "archUtilisation" => double(translation.utilisation(&a.string("element")?, entity)?),
        "archInfo" => Ok(info(translation, next_id)),
        "archReplicas" => double(translation.replicas_of(&a.string("element")?, entity)?),
        "archFailure" => {
            let per = translation.quality.failure.as_ref().ok_or("archFailure needs a Reliability in the model")?;
            double(crate::translate::weighted(per, &excluded(&a)?, entity))
        }
        "archChangeCoupling" => double(translation.quality.change_coupling.ok_or("archChangeCoupling needs ChangeUnits in the model")?),
        "archInconsistency" => {
            let per = translation.quality.inconsistency.as_ref().ok_or("archInconsistency needs a Consistency in the model")?;
            double(crate::translate::weighted(per, &excluded(&a)?, entity))
        }
        "archLatencyViolation" => double(translation.latency_violation()),
        "archPeakUtilisation" => double(translation.peak_utilisation().ok_or("archPeakUtilisation needs a PeakCapacity in the model")?.0),
        "archPeakViolation" => {
            let (u, limit) = translation.peak_utilisation().ok_or("archPeakViolation needs a PeakCapacity in the model")?;
            double((u - limit).max(0.0))
        }
        "archNoLoadTime" => double(translation.no_load_time(&excluded(&a)?, entity)),
        op if is_dynamic(op) => Err(format!("{op} needs a dynamic run (crate::dynamic)")),
        "archSimulate" => {
            let prepared = prepared.ok_or("no model")?;
            let params = a.run_params()?;
            let outcome = engine.simulate(prepared.clone(), params.clone()).await?;
            Ok(simulation_result(&outcome, &prepared, params.seed, next_id))
        }
        "archStat" => {
            let prepared = prepared.ok_or("no model")?;
            let params = a.run_params()?;
            let (element, metric, statistic) = (a.string("element")?, a.string("metric")?, a.string("statistic")?);
            if !STATISTICS.contains(&statistic.as_str()) {
                return Err(format!("unknown statistic '{statistic}'; one of {}", STATISTICS.join(", ")));
            }
            let outcome = engine.simulate(prepared.clone(), params).await?;
            double(stat(&outcome, &prepared, &element, &metric, &statistic)?)
        }
        _ => unreachable!(),
    }
}

/// Whether `name` is one of the generic Palladio functions (`crate::pcm`).
pub fn is_pcm_function(name: &str) -> bool {
    name.starts_with("pcm")
}

impl Args<'_> {
    /// A list argument: the elements of the collection it refers to.
    fn list<'l>(&self, name: &str, lists: &'l std::collections::HashMap<i64, Vec<WireValue>>) -> Result<&'l [WireValue], String> {
        match self.get(name)? {
            WireValue::Ref(id) => lists.get(id).map(|v| v.as_slice()).ok_or_else(|| format!("{}: the list {name} is not held", self.f.name)),
            WireValue::Null => Ok(&[]),
            other => Err(format!("{}: {name} must be a list, not {other:?}", self.f.name)),
        }
    }

    fn instances(&self, name: &str, lists: &std::collections::HashMap<i64, Vec<WireValue>>) -> Result<Vec<String>, String> {
        self.list(name, lists)?
            .iter()
            .map(|v| match v {
                WireValue::Instance(n) => Ok(n.clone()),
                other => Err(format!("{}: {name} must hold model elements, not {other:?}", self.f.name)),
            })
            .collect()
    }

    fn doubles(&self, name: &str, lists: &std::collections::HashMap<i64, Vec<WireValue>>) -> Result<Vec<f64>, String> {
        self.list(name, lists)?.iter().map(|v| number(v).ok_or_else(|| format!("{}: {name} must hold numbers, not {v:?}", self.f.name))).collect()
    }

    fn double(&self, name: &str) -> Result<f64, String> {
        number(self.get(name)?).ok_or_else(|| format!("{}: {name} must be a number", self.f.name))
    }

    /// The arguments of `pcmHpaRun` (times in model time units, converted to seconds).
    fn hpa_request(&self, lists: &std::collections::HashMap<i64, Vec<WireValue>>) -> Result<crate::pcmhpa::HpaRequest, String> {
        let ms = self.double("msPerTimeUnit")?;
        if !(ms > 0.0) {
            return Err("pcmHpaRun: msPerTimeUnit must be positive".into());
        }
        let s = ms / 1000.0; // seconds per model unit
        let records = |name: &str| -> Result<Vec<Vec<(String, WireValue)>>, String> {
            self.list(name, lists)?
                .iter()
                .map(|v| match v {
                    WireValue::Record { fields, .. } => Ok(fields.clone()),
                    other => Err(format!("{}: {name} must hold records, not {other:?}", self.f.name)),
                })
                .collect()
        };
        let inst = |f: &[(String, WireValue)], k: &str| -> Result<String, String> {
            match f.iter().find(|(n, _)| n == k).map(|(_, v)| v) {
                Some(WireValue::Instance(n)) => Ok(n.clone()),
                other => Err(format!("pcmHpaRun: {k} must be a model element, not {other:?}")),
            }
        };
        let num = |f: &[(String, WireValue)], k: &str| -> Result<f64, String> {
            f.iter().find(|(n, _)| n == k).and_then(|(_, v)| number(v)).ok_or_else(|| format!("pcmHpaRun: {k} must be a number"))
        };
        let mut groups = Vec::new();
        for f in records("groups")? {
            let scale_down = num(&f, "scaleDown")?;
            let request = num(&f, "cpuRequest")?;
            groups.push(crate::pcmhpa::ScalingGroup {
                container: inst(&f, "container")?,
                min: num(&f, "minReplicas")?.max(1.0) as u32,
                max: num(&f, "maxReplicas")?.max(1.0) as u32,
                target: num(&f, "targetUtilisation")?,
                initial: num(&f, "initialReplicas")?.max(1.0) as u32,
                startup_s: num(&f, "startup")? * s,
                scale_down_s: (scale_down >= 0.0).then_some(scale_down * s),
                cpu_request: (request > 0.0).then_some(request),
            });
        }
        let mut services = Vec::new();
        for f in records("services")? {
            services.push(crate::pcmhpa::ServiceGroup { container: inst(&f, "container")?, assembly: inst(&f, "assembly")?, load_balancer: inst(&f, "loadBalancer")? });
        }
        let profile = self.profile(lists)?.into_iter().map(|p| crate::pcmrun::LoadStep { time: p.time * s, ..p }).collect();
        let elements = self.instances("budgetElements", lists)?;
        let budgets = self.doubles("budgetsMs", lists)?;
        if elements.len() != budgets.len() {
            return Err(format!("pcmHpaRun: {} budget elements but {} budgets", elements.len(), budgets.len()));
        }
        let run = crate::detail::DetailRequest {
            profile,
            usage_streams: self.boolean("usageStreams")?,
            seed: self.long("seed")?,
            duration_s: self.double("end")? * s,
            warm_up_s: self.double("warmUp")? * s,
            bin_s: (self.double("end")? * s / 60.0).max(1e-9),
            time_unit_ms: ms,
            budgets_ms: elements.into_iter().zip(budgets).collect(),
            slo_factor: self.double("sloFactor")?,
            assembly_operations: false,
            ..crate::detail::DetailRequest::default()
        };
        if !(run.duration_s > run.warm_up_s) {
            return Err("pcmHpaRun: end must be after warmUp".into());
        }
        Ok(crate::pcmhpa::HpaRequest { groups, services, run, max_pods: self.long("maxPods")?.max(1) as u32 })
    }

    /// The `profile` argument: `PcmLoadStep` records (scenario null: every scenario).
    fn profile(&self, lists: &std::collections::HashMap<i64, Vec<WireValue>>) -> Result<Vec<crate::pcmrun::LoadStep>, String> {
        let mut out = Vec::new();
        for v in self.list("profile", lists)? {
            let WireValue::Record { fields, .. } = v else { return Err(format!("{}: profile must hold PcmLoadStep records", self.f.name)) };
            let field = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let scenario = match field("scenario") {
                Some(WireValue::Instance(n)) => Some(n.clone()),
                None | Some(WireValue::Null) => None,
                other => return Err(format!("{}: a load step's scenario must be a UsageScenario or null, not {other:?}", self.f.name)),
            };
            let time = field("time").and_then(number).ok_or("a load step needs a time")?;
            let factor = field("factor").and_then(number).ok_or("a load step needs a factor")?;
            out.push(crate::pcmrun::LoadStep { scenario, time, factor });
        }
        Ok(out)
    }

    /// The `overrides` argument: (resource spec, replicas, rate).
    fn overrides(&self, lists: &std::collections::HashMap<i64, Vec<WireValue>>) -> Result<Vec<(String, i32, f64)>, String> {
        let mut out = Vec::new();
        for v in self.list("overrides", lists)? {
            let WireValue::Record { fields, .. } = v else { return Err(format!("{}: overrides must hold PcmOverride records", self.f.name)) };
            let field = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let resource = match field("resource") {
                Some(WireValue::Instance(n)) => n.clone(),
                other => return Err(format!("{}: an override needs a resource, not {other:?}", self.f.name)),
            };
            let replicas = field("replicas").and_then(number).ok_or("an override needs replicas")?;
            let rate = field("rate").and_then(number).ok_or("an override needs a rate")?;
            out.push((resource, replicas as i32, rate));
        }
        Ok(out)
    }
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

fn instance_or_null(name: Option<&String>) -> WireValue {
    name.map_or(WireValue::Null, |n| WireValue::Instance(n.clone()))
}

fn list_answer(class: &str, items: Vec<Vec<(&str, WireValue)>>, next_id: &mut impl FnMut() -> i64) -> Answer {
    let id = next_id();
    let elements = items
        .into_iter()
        .map(|fields| WireValue::Record { class_name: class.into(), fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect() })
        .collect();
    Answer { value: WireValue::Ref(id), objects: vec![HeapObject { id, kind: HeapKind::List, version: 0, elements: Some(elements), entries: None }] }
}

/// The simulation of a `pcm*` call: the plain model with its overrides, compiled once per
/// distinct overrides (the last one is kept).
pub type PcmPrepared = Option<(Vec<(String, i32, f64)>, Arc<Prepared>)>;

/// Answers a generic Palladio function (`crate::pcm`).
pub async fn call_pcm(
    engine: &Arc<Engine>,
    operation: &str,
    args: &[WireValue],
    lists: &std::collections::HashMap<i64, Vec<WireValue>>,
    cache: &Arc<crate::pcm::PcmCache>,
    prepared: &mut PcmPrepared,
    next_id: &mut impl FnMut() -> i64,
) -> Result<Answer, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let a = Args { f, args };
    let double = |v: f64| Ok(Answer { value: WireValue::Double(v), objects: vec![] });
    let loads = &cache.loads;
    match operation {
        "pcmResources" => {
            let items = loads
                .resources
                .iter()
                .map(|r| {
                    vec![
                        ("resource", WireValue::Instance(r.spec.clone())),
                        ("container", WireValue::Instance(r.container.clone())),
                        ("kind", WireValue::String(r.kind.clone())),
                        ("rate", WireValue::Double(r.rate)),
                        ("replicas", WireValue::Double(r.replicas)),
                        ("demand", WireValue::Double(loads.demand.get(&r.spec).copied().unwrap_or(0.0))),
                        ("busy", WireValue::Double(r.busy)),
                        ("utilisation", WireValue::Double(r.utilisation())),
                    ]
                })
                .collect();
            Ok(list_answer("PcmResource", items, next_id))
        }
        "pcmScenarios" => {
            let items = loads
                .arrivals
                .iter()
                .map(|(sc, r)| {
                    vec![
                        ("scenario", WireValue::Instance(sc.clone())),
                        ("name", WireValue::String(cache.graph.entity(sc))),
                        ("rate", WireValue::Double(*r)),
                        ("noLoadTime", WireValue::Double(loads.times.get(sc).copied().unwrap_or(0.0))),
                    ]
                })
                .collect();
            Ok(list_answer("PcmScenario", items, next_id))
        }
        "pcmOperations" => {
            let items = loads
                .entry_calls
                .iter()
                .map(|((callee, call), rate)| {
                    let t = loads.entry_times.get(call).copied().unwrap_or(0.0);
                    // a call of a scenario without arrivals: its time per call, walked alone
                    let alone = || cache.per_call().get(call).map_or(0.0, |a| a.time);
                    vec![
                        ("call", WireValue::Instance(call.clone())),
                        ("signature", instance_or_null(cache.graph.ref1(call, "operationSignature").as_ref())),
                        ("callee", WireValue::Instance(callee.clone())),
                        ("rate", WireValue::Double(*rate)),
                        ("noLoadTime", WireValue::Double(if *rate > 0.0 { t / rate } else { alone() })),
                    ]
                })
                .collect();
            Ok(list_answer("PcmOperation", items, next_id))
        }
        "pcmWeightedVisits" => {
            let actions = a.instances("actions", lists)?;
            let weights = a.doubles("weights", lists)?;
            if actions.len() != weights.len() {
                return Err(format!("pcmWeightedVisits: {} actions but {} weights", actions.len(), weights.len()));
            }
            let pairs: Vec<(String, f64)> = actions.into_iter().zip(weights).collect();
            double(cache.weighted_visits(&pairs, &excluded(&a)?))
        }
        "pcmCallRates" => {
            let ts = a.instances("transitions", lists)?;
            let ps = a.doubles("probabilities", lists)?;
            if ts.len() != ps.len() {
                return Err(format!("pcmCallRates: {} transitions but {} probabilities", ts.len(), ps.len()));
            }
            let overrides: Vec<(String, f64)> = ts.into_iter().zip(ps).collect();
            let l = cache.call_rates(&overrides);
            let mut items = Vec::new();
            for ((caller, callee, action), rate) in &l.action_calls {
                items.push(vec![
                    ("action", WireValue::Instance(action.clone())),
                    ("caller", WireValue::Instance(caller.clone())),
                    ("callee", WireValue::Instance(callee.clone())),
                    ("rate", WireValue::Double(*rate)),
                ]);
            }
            for ((callee, action), rate) in &l.entry_calls {
                items.push(vec![
                    ("action", WireValue::Instance(action.clone())),
                    ("caller", instance_or_null(None)),
                    ("callee", WireValue::Instance(callee.clone())),
                    ("rate", WireValue::Double(*rate)),
                ]);
            }
            Ok(list_answer("PcmCall", items, next_id))
        }
        "pcmFailure" => {
            let cs = a.instances("containers", lists)?;
            let us = a.doubles("unavailabilities", lists)?;
            if cs.len() != us.len() {
                return Err(format!("pcmFailure: {} containers but {} unavailabilities", cs.len(), us.len()));
            }
            let unavailable: std::collections::HashMap<String, f64> = cs.into_iter().zip(us).collect();
            let transparent: std::collections::HashSet<String> = a.instances("transparent", lists)?.into_iter().collect();
            double(cache.failure(&unavailable, &transparent, &excluded(&a)?).0)
        }
        "pcmOperationVisits" => {
            let actions = a.instances("actions", lists)?;
            let weights = a.doubles("weights", lists)?;
            if actions.len() != weights.len() {
                return Err(format!("pcmOperationVisits: {} actions but {} weights", actions.len(), weights.len()));
            }
            let pairs: Vec<(String, f64)> = actions.into_iter().zip(weights).collect();
            let values: std::collections::HashMap<String, f64> = cache.operation_visits(&pairs).into_iter().collect();
            Ok(operation_values(cache, &values, next_id))
        }
        "pcmOperationFailure" => {
            let cs = a.instances("containers", lists)?;
            let us = a.doubles("unavailabilities", lists)?;
            if cs.len() != us.len() {
                return Err(format!("pcmOperationFailure: {} containers but {} unavailabilities", cs.len(), us.len()));
            }
            let unavailable: std::collections::HashMap<String, f64> = cs.into_iter().zip(us).collect();
            let transparent: std::collections::HashSet<String> = a.instances("transparent", lists)?.into_iter().collect();
            let values: std::collections::HashMap<String, f64> = cache.operation_failures(&unavailable, &transparent).0.into_iter().collect();
            Ok(operation_values(cache, &values, next_id))
        }
        "pcmRun" | "pcmRunStat" => {
            let overrides = a.overrides(lists)?;
            let p = pcm_prepared(engine, cache, prepared, overrides).await?;
            let opts = crate::pcmrun::RunOptions { profile: a.profile(lists)?, usage_streams: a.boolean("usageStreams")? };
            let params = a.run_params()?;
            let r = crate::pcmrun::runner(&engine.config).simulate(p.clone(), params.clone(), opts).await?;
            if operation == "pcmRun" {
                return Ok(simulation_result(&r.outcome, &p, params.seed, next_id));
            }
            let (element, metric, statistic) = (a.string("element")?, a.string("metric")?, a.string("statistic")?);
            if !STATISTICS.contains(&statistic.as_str()) {
                return Err(format!("unknown statistic '{statistic}'; one of {}", STATISTICS.join(", ")));
            }
            double(stat(&r.outcome, &p, &element, &metric, &statistic)?)
        }
        "pcmHpaRun" => {
            let req = a.hpa_request(lists)?;
            let key = format!("{req:?}");
            let hit = cache.hpa.lock().unwrap().as_ref().filter(|(k, _)| *k == key).map(|(_, r)| r.clone());
            let r = match hit {
                Some(r) => r,
                None => {
                    let runner = crate::pcmrun::runner(&engine.config);
                    let limits = runner.limits();
                    let _permit = runner.acquire().await?;
                    let c = cache.clone();
                    let rq = req.clone();
                    let r = tokio::task::spawn_blocking(move || {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::pcmhpa::run(&c, &rq, limits)))
                            .unwrap_or_else(|_| Err("the dynamic run panicked".into()))
                    })
                    .await
                    .map_err(|e| format!("the dynamic run was abandoned: {e}"))??;
                    let r = Arc::new(r.0);
                    *cache.hpa.lock().unwrap() = Some((key, r.clone()));
                    r
                }
            };
            Ok(pcm_hpa_result(&r, next_id))
        }
        "pcmMean" => {
            let e = a.string("expression")?;
            double(crate::load::mean_pcm(&e).ok_or_else(|| format!("pcmMean: {e:?} has no mean (variables, or not a StoEx)"))?)
        }
        "pcmWarnings" => {
            let id = next_id();
            let elements = loads.warnings.iter().map(|w| WireValue::String(w.clone())).collect();
            Ok(Answer { value: WireValue::Ref(id), objects: vec![HeapObject { id, kind: HeapKind::List, version: 0, elements: Some(elements), entries: None }] })
        }
        "pcmSimulate" | "pcmStat" => {
            let overrides = a.overrides(lists)?;
            let p = pcm_prepared(engine, cache, prepared, overrides).await?;
            let params = a.run_params()?;
            if operation == "pcmSimulate" {
                let outcome = engine.simulate(p.clone(), params.clone()).await?;
                return Ok(simulation_result(&outcome, &p, params.seed, next_id));
            }
            let (element, metric, statistic) = (a.string("element")?, a.string("metric")?, a.string("statistic")?);
            if !STATISTICS.contains(&statistic.as_str()) {
                return Err(format!("unknown statistic '{statistic}'; one of {}", STATISTICS.join(", ")));
            }
            let outcome = engine.simulate(p.clone(), params).await?;
            double(stat(&outcome, &p, &element, &metric, &statistic)?)
        }
        other => Err(format!("{other} is not a generic Palladio function")),
    }
}

/// The plain model with `overrides`, compiled (the last one kept per session).
async fn pcm_prepared(engine: &Arc<Engine>, cache: &crate::pcm::PcmCache, prepared: &mut PcmPrepared, overrides: Vec<(String, i32, f64)>) -> Result<Arc<Prepared>, String> {
    Ok(match prepared {
        Some((o, p)) if *o == overrides => p.clone(),
        _ => {
            let model = Arc::new(cache.with_overrides(&overrides));
            let p = engine.prepare(palladio_sim_service::schema::schema(), model).await?;
            *prepared = Some((overrides, p.clone()));
            p
        }
    })
}

/// One `PcmOperationValue` per entry-level call (in the order of [`crate::pcm::PcmCache::operations`]).
fn operation_values(cache: &crate::pcm::PcmCache, values: &std::collections::HashMap<String, f64>, next_id: &mut impl FnMut() -> i64) -> Answer {
    let items = cache
        .operations()
        .into_iter()
        .map(|o| {
            vec![
                ("call", WireValue::Instance(o.call.clone())),
                ("signature", instance_or_null(o.signature.as_ref())),
                ("role", instance_or_null(o.role.as_ref())),
                ("scenario", instance_or_null(o.scenario.as_ref())),
                ("rate", WireValue::Double(o.rate)),
                ("value", WireValue::Double(values.get(&o.call).copied().unwrap_or(0.0))),
            ]
        })
        .collect();
    list_answer("PcmOperationValue", items, next_id)
}

fn pcm_hpa_result(r: &crate::pcmhpa::HpaResult, next_id: &mut impl FnMut() -> i64) -> Answer {
    let groups = list_answer(
        "PcmHpaGroup",
        r.groups
            .iter()
            .map(|g| {
                vec![
                    ("container", WireValue::Instance(g.container.clone())),
                    ("meanReplicas", WireValue::Double(g.mean_replicas)),
                    ("readyReplicaSeconds", WireValue::Double(g.ready_replica_seconds)),
                    ("replicaSeconds", WireValue::Double(g.replica_seconds)),
                    ("maxReplicas", WireValue::Int(g.max_replicas)),
                    ("rescales", WireValue::Long(g.rescales as i64)),
                    ("createdPods", WireValue::Long(g.created as i64)),
                    ("reversals", WireValue::Int(g.reversals as i32)),
                ]
            })
            .collect(),
        next_id,
    );
    let value = WireValue::Record {
        class_name: "PcmHpaResult".into(),
        fields: vec![
            ("readyReplicaSeconds".into(), WireValue::Double(r.ready_replica_seconds)),
            ("replicaSeconds".into(), WireValue::Double(r.replica_seconds)),
            ("peakReplicas".into(), WireValue::Double(r.peak_replicas)),
            ("rescales".into(), WireValue::Long(r.rescales as i64)),
            ("createdPods".into(), WireValue::Long(r.created as i64)),
            ("flapping".into(), WireValue::Double(r.flapping)),
            ("slo".into(), WireValue::Double(r.slo)),
            ("requests".into(), WireValue::Long(r.requests as i64)),
            ("meanMs".into(), WireValue::Double(r.mean_ms)),
            ("p95Ms".into(), WireValue::Double(r.p95_ms)),
            ("capped".into(), WireValue::Bool(r.capped)),
            ("events".into(), WireValue::Long(r.events as i64)),
            ("wallMs".into(), WireValue::Double(r.wall_ms)),
            ("groups".into(), groups.value),
        ],
    };
    Answer { value, objects: groups.objects }
}

/// The `exclude` argument: comma-separated scenario names.
fn excluded(a: &Args) -> Result<Vec<String>, String> {
    Ok(a.string("exclude")?.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
}

fn entity_of(t: &Translation, name: &str) -> String {
    t.pcm
        .instances
        .iter()
        .find(|x| x.name == name)
        .and_then(|x| x.attributes.iter().find(|(k, _)| k == "entityName"))
        .and_then(|(_, v)| match v.first() {
            Some(WireValue::String(s)) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| name.to_string())
}

/// One statistic of one series.
pub fn stat(outcome: &Outcome, prepared: &Prepared, element: &str, metric: &str, statistic: &str) -> Result<f64, String> {
    let s = find_series(outcome, prepared, element, metric)?;
    Ok(match statistic {
        "mean" => s.mean,
        "stdDev" => s.std_dev,
        "min" => s.min,
        "max" => s.max,
        "p50" | "median" => s.p50,
        "p90" => s.p90,
        "p95" => s.p95,
        "p99" => s.p99,
        "count" => s.count as f64,
        "throughput" => s.count as f64 / (outcome.simulated_time * outcome.replications as f64).max(f64::MIN_POSITIVE),
        "timeWeightedMean" => s
            .time_weighted_mean
            .ok_or_else(|| format!("{} has no time-weighted mean (all its tuples are at one time)", s.measuring_point))?,
        "ciHalfWidth" => s.ci_half_width.ok_or("ciHalfWidth needs at least two replications")?,
        other => return Err(format!("unknown statistic '{other}'")),
    })
}

fn info(t: &Translation, next_id: &mut impl FnMut() -> i64) -> Answer {
    let list_id = next_id();
    let warn_id = next_id();
    let resources: Vec<WireValue> = t
        .loads
        .resources
        .iter()
        .map(|r| WireValue::Record {
            class_name: "ArchResource".into(),
            fields: vec![
                ("name".into(), WireValue::String(entity_of(t, &r.spec))),
                ("container".into(), WireValue::String(entity_of(t, &r.container))),
                ("kind".into(), WireValue::String(r.kind.clone())),
                ("replicas".into(), WireValue::Double(r.replicas)),
                ("rate".into(), WireValue::Double(r.rate)),
                ("busy".into(), WireValue::Double(r.busy)),
                ("utilisation".into(), WireValue::Double(r.utilisation())),
            ],
        })
        .collect();
    let max_u = t.utilisation("", |n| n.to_string()).unwrap_or(0.0);
    let value = WireValue::Record {
        class_name: "ArchInfo".into(),
        fields: vec![
            ("translated".into(), WireValue::Bool(t.translated)),
            ("instances".into(), WireValue::Int(t.pcm.instances.len() as i32)),
            ("cost".into(), WireValue::Double(t.cost)),
            ("modularity".into(), WireValue::Double(t.modularity)),
            ("maxUtilisation".into(), WireValue::Double(max_u)),
            ("resources".into(), WireValue::Ref(list_id)),
            ("warnings".into(), WireValue::Ref(warn_id)),
        ],
    };
    Answer {
        value,
        objects: vec![
            HeapObject { id: list_id, kind: HeapKind::List, version: 0, elements: Some(resources), entries: None },
            HeapObject {
                id: warn_id,
                kind: HeapKind::List,
                version: 0,
                elements: Some(t.warnings.iter().map(|w| WireValue::String(w.clone())).collect()),
                entries: None,
            },
        ],
    }
}

/// `UsageScenarioMeasuringPoint[_us1]` → `UsageScenario`.
pub fn kind_of(mp: &str) -> String {
    let t = mp.split('[').next().unwrap_or(mp);
    t.strip_suffix("MeasuringPoint").unwrap_or(t).to_string()
}

fn metric_key(m: &str) -> String {
    let m = m.trim();
    let m = m.strip_suffix(" Tuple").or_else(|| m.strip_suffix("_TUPLE")).unwrap_or(m);
    m.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The series `element` names with `metric` (as `pcmStat` finds it).
pub fn find_series<'a>(outcome: &'a Outcome, prepared: &Prepared, element: &str, metric: &str) -> Result<&'a SeriesStats, String> {
    let mk = metric_key(metric);
    let monitored: Vec<&Vec<String>> = prepared
        .monitors
        .iter()
        .filter(|(name, entity, _)| name == element || entity.as_deref() == Some(element))
        .map(|(_, _, targets)| targets)
        .collect();
    let names = |s: &SeriesStats| {
        s.measuring_point == element
            || s.elements.iter().any(|e| e == element || prepared.entity_names.get(e).is_some_and(|n| n == element))
            || s.monitor.as_deref() == Some(element)
            || monitored.iter().any(|t| !t.is_empty() && s.elements.iter().all(|e| t.contains(e)))
    };
    let matches: Vec<&SeriesStats> = outcome.series.iter().filter(|s| names(s) && metric_key(&s.metric) == mk).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => {
            let all: Vec<String> = outcome.series.iter().take(20).map(|s| format!("{} {}", s.measuring_point, s.metric)).collect();
            Err(format!("no '{metric}' measured for '{element}'; measured: {}", all.join(", ")))
        }
        more => Err(format!(
            "'{element}' names {} series with '{metric}': {}; name one by its measuring point",
            more.len(),
            more.iter().map(|s| s.measuring_point.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn simulation_result(outcome: &Outcome, prepared: &Prepared, seed: i64, next_id: &mut impl FnMut() -> i64) -> Answer {
    let list_id = next_id();
    let opt_double = |v: Option<f64>| v.map_or(WireValue::Null, WireValue::Double);
    let series: Vec<WireValue> = outcome
        .series
        .iter()
        .map(|s| WireValue::Record {
            class_name: "ArchSeries".into(),
            fields: vec![
                ("measuringPoint".into(), WireValue::String(s.measuring_point.clone())),
                ("kind".into(), WireValue::String(kind_of(&s.measuring_point))),
                ("element".into(), WireValue::String(s.elements.first().cloned().unwrap_or_default())),
                (
                    "elementName".into(),
                    s.elements
                        .first()
                        .and_then(|e| prepared.entity_names.get(e))
                        .map_or(WireValue::Null, |n| WireValue::String(n.clone())),
                ),
                ("monitor".into(), s.monitor.clone().map_or(WireValue::Null, WireValue::String)),
                ("metric".into(), WireValue::String(s.metric.clone())),
                ("count".into(), WireValue::Long(s.count as i64)),
                ("mean".into(), WireValue::Double(s.mean)),
                ("stdDev".into(), WireValue::Double(s.std_dev)),
                ("min".into(), WireValue::Double(s.min)),
                ("max".into(), WireValue::Double(s.max)),
                ("p50".into(), WireValue::Double(s.p50)),
                ("p90".into(), WireValue::Double(s.p90)),
                ("p95".into(), WireValue::Double(s.p95)),
                ("p99".into(), WireValue::Double(s.p99)),
                ("timeWeightedMean".into(), opt_double(s.time_weighted_mean)),
                ("ciHalfWidth".into(), opt_double(s.ci_half_width)),
            ],
        })
        .collect();
    let value = WireValue::Record {
        class_name: "ArchResult".into(),
        fields: vec![
            ("series".into(), WireValue::Ref(list_id)),
            ("replications".into(), WireValue::Int(outcome.replications as i32)),
            ("seed".into(), WireValue::Int(seed as i32)),
            ("events".into(), WireValue::Long(outcome.events as i64)),
            ("measurements".into(), WireValue::Long(outcome.measurements as i64)),
            ("simulatedTime".into(), WireValue::Double(outcome.simulated_time)),
        ],
    };
    Answer {
        value,
        objects: vec![HeapObject { id: list_id, kind: HeapKind::List, version: 0, elements: Some(series), entries: None }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_every_function_and_record() {
        let m = manifest();
        let c = &m["contributionPlugins"][0]["serverContributionPlugins"][0];
        assert_eq!(c["id"], CONTRIBUTION_ID);
        for f in FUNCTIONS {
            assert!(c["functions"][f.name].is_object(), "{}", f.name);
        }
        assert_eq!(c["classes"]["ArchSeries"]["fields"].as_array().unwrap().len(), 17);
    }
}
