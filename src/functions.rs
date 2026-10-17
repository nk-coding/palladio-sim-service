//! The functions scripts call, declared once: [`manifest`] builds the contribution payload from
//! [`FUNCTIONS`] and [`RECORDS`], and [`call`] answers them.
//!
//! ```fn
//! // Everything the monitors measured (one run, measured as SimuLizar measures it):
//! val result = pcmSimulate(seed = 1, maxMeasurements = 10000)
//! // One number, e.g. for an objective; calls with the same model and run parameters share one
//! // simulation:
//! val rt = pcmStat("_us1", "Response Time")            // mean response time of a usage scenario
//! val util = pcmStat("cpu1", "Utilization of Active Resource", "timeWeightedMean")
//! ```

use crate::engine::{Engine, Outcome, Prepared, RunParams, SeriesStats};
use crate::protocol::{HeapKind, HeapObject, WireValue};
use serde_json::{Value, json};
use std::sync::Arc;

pub const PLUGIN_ID: &str = "palladio-sim-service";
pub const CONTRIBUTION_ID: &str = "palladio";
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

/// The parameters every simulating function ends with.
const RUN_PARAMS: [Param; 5] = [
    // `int`, not `long`: scripts do not pass an int literal where a long is declared.
    p("seed", Ty::Int, Default::Int(1)),
    p("maxMeasurements", Ty::Int, Default::Int(10_000)),
    p("maxSimTime", Ty::Int, Default::Int(-1)),
    p("replications", Ty::Int, Default::Int(1)),
    p("simulateLinkingResources", Ty::Boolean, Default::Bool(false)),
];

struct Function {
    name: &'static str,
    params: &'static [Param],
    returns: Ty,
}

const SIMULATE_PARAMS: [Param; 5] = RUN_PARAMS;
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

const FUNCTIONS: &[Function] = &[
    Function { name: "pcmSimulate", params: &SIMULATE_PARAMS, returns: Ty::Record("SimResult") },
    Function { name: "pcmStat", params: &STAT_PARAMS, returns: Ty::Double },
];

const RECORDS: &[(&str, &[(&str, Ty)])] = &[
    (
        "SimResult",
        &[
            ("series", Ty::ListOf("SimSeries")),
            ("replications", Ty::Int),
            ("seed", Ty::Int),
            ("events", Ty::Long),
            ("measurements", Ty::Long),
            ("simulatedTime", Ty::Double),
        ],
    ),
    (
        "SimSeries",
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
];

/// The statistics `pcmStat` can return.
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
            let lit = match prm.default {
                Default::None => continue,
                Default::Int(v) => json!({"kind": "intLiteral", "value": v.to_string()}),
                Default::Bool(v) => json!({"kind": "booleanLiteral", "value": v}),
                Default::Str(v) => json!({"kind": "stringLiteral", "value": v}),
            };
            let mut lit = lit;
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
        "name": "Palladio Simulation",
        "description": "Simulates Palladio (PCM) models with SimOxide, with SimuLizar 5.2.2's semantics, as script functions",
        "icon": [
            ["path", {"d": "M3 12h4l3-8 4 16 3-8h4"}]
        ],
        "languagePlugins": [],
        "contributionPlugins": [{
            "languageId": "script",
            "description": "pcmSimulate() and pcmStat(): performance simulation of the Palladio model a script runs on",
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

/// What an operation answers: the value, and the collections it created.
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

/// Answers a call on the model `prepared`. `next_id` hands out the (negative) ids of created
/// collections.
pub async fn call(
    engine: &Arc<Engine>,
    operation: &str,
    args: &[WireValue],
    prepared: Arc<Prepared>,
    next_id: &mut impl FnMut() -> i64,
) -> Result<Answer, String> {
    let f = FUNCTIONS.iter().find(|f| f.name == operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    let a = Args { f, args };
    let params = a.run_params()?;
    match operation {
        "pcmSimulate" => {
            let outcome = engine.simulate(prepared.clone(), params.clone()).await?;
            Ok(simulation_result(&outcome, &prepared, params.seed, next_id))
        }
        "pcmStat" => {
            let (element, metric, statistic) = (a.string("element")?, a.string("metric")?, a.string("statistic")?);
            if !STATISTICS.contains(&statistic.as_str()) {
                return Err(format!("unknown statistic '{statistic}'; one of {}", STATISTICS.join(", ")));
            }
            let outcome = engine.simulate(prepared.clone(), params).await?;
            let s = find_series(&outcome, &prepared, &element, &metric)?;
            let value = match statistic.as_str() {
                "mean" => s.mean,
                "stdDev" => s.std_dev,
                "min" => s.min,
                "max" => s.max,
                "p50" | "median" => s.p50,
                "p90" => s.p90,
                "p95" => s.p95,
                "p99" => s.p99,
                "count" => s.count as f64,
                "throughput" => {
                    s.count as f64 / (outcome.simulated_time * outcome.replications as f64).max(f64::MIN_POSITIVE)
                }
                "timeWeightedMean" => s
                    .time_weighted_mean
                    .ok_or_else(|| format!("{} has no time-weighted mean (all its tuples are at one time)", s.measuring_point))?,
                "ciHalfWidth" => s.ci_half_width.ok_or("ciHalfWidth needs at least two replications")?,
                _ => unreachable!(),
            };
            Ok(Answer { value: WireValue::Double(value), objects: vec![] })
        }
        _ => unreachable!(),
    }
}

/// `UsageScenarioMeasuringPoint[_us1]` → `UsageScenario`.
fn kind_of(mp: &str) -> String {
    let t = mp.split('[').next().unwrap_or(mp);
    t.strip_suffix("MeasuringPoint").unwrap_or(t).to_string()
}

/// Normalises a metric name: `Response Time`, `response time`, `RESPONSE_TIME` and
/// `Response Time Tuple` are the same metric.
fn metric_key(m: &str) -> String {
    let m = m.trim();
    let m = m.strip_suffix(" Tuple").or_else(|| m.strip_suffix("_TUPLE")).unwrap_or(m);
    m.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The series `element` names with `metric`. `element` is the instance or entity name of a
/// measured element (usage scenario, entry-level call, processing resource, role, signature, ...),
/// a monitor's instance or entity name, or a whole measuring point key.
fn find_series<'a>(outcome: &'a Outcome, prepared: &Prepared, element: &str, metric: &str) -> Result<&'a SeriesStats, String> {
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
            let for_element: Vec<String> =
                outcome.series.iter().filter(|s| names(s)).map(|s| format!("{} ({})", s.metric, s.measuring_point)).collect();
            if for_element.is_empty() {
                let all: Vec<String> = outcome.series.iter().take(20).map(|s| format!("{} {}", s.measuring_point, s.metric)).collect();
                Err(format!("nothing measured for '{element}'; measured: {}", all.join(", ")))
            } else {
                Err(format!("no '{metric}' measured for '{element}'; measured: {}", for_element.join(", ")))
            }
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
            class_name: "SimSeries".into(),
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
        class_name: "SimResult".into(),
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
    fn metrics_are_matched_loosely() {
        assert_eq!(metric_key("Response Time"), metric_key("RESPONSE_TIME"));
        assert_eq!(metric_key("Response Time Tuple"), metric_key("response time"));
        assert_ne!(metric_key("Response Time"), metric_key("Resource Demand"));
    }

    #[test]
    fn manifest_declares_every_function_and_record() {
        let m = manifest();
        let c = &m["contributionPlugins"][0]["serverContributionPlugins"][0];
        assert_eq!(c["id"], CONTRIBUTION_ID);
        assert!(c["functions"]["pcmSimulate"]["signatures"][""]["defaultValues"]["seed"]["evalType"].is_number());
        assert_eq!(c["classes"]["SimSeries"]["fields"].as_array().unwrap().len(), 17);
        assert_eq!(c["sessions"]["functions"]["protocol"], "script-functions");
    }
}
