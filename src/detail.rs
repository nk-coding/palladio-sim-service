//! The design detail of a plain PCM model: the simulation `pcmEvaluate` runs for the same
//! request ([`crate::evaluate`]), with more monitors, reduced to generic PCM statistics named by
//! PCM elements (the explorer's backend maps them to its operations, request types, sessions and
//! services; DESIGN.md §H of the domain project). The evaluation of the run is part of it
//! (`evaluation`), so the explorer shows exactly what the search evaluated.
//!
//! - **Monitoring.** The model's own monitors are replaced by a complete set: Response Time of
//!   every usage scenario, entry-level system call, system operation (delegated provided role ×
//!   signature) and assembly operation (assembly × provided role × signature with a SEFF, measured
//!   when a connector calls it); State of Active Resource of every core of every processing
//!   resource (replica 0 of a multi-core one adds SimuLizar's overall utilisation) and its
//!   Resource Demand. An evaluation monitors the first three only; monitors do not change the run.
//! - **Run.** `pcmEvaluate`'s: SimOxide's fast mode with the request's seed, per-scenario load
//!   profile, usage streams (on by default), SPD and the HPA for the scaled containers.
//! - **Output** (JSON, times in ms and s via `timeUnitMs`, the model's time unit in ms): per
//!   response-time series count, mean, percentiles, max, a log-spaced histogram and per-bin mean,
//!   p95 and throughput; per resource the time-weighted utilisation and queue length (overall and
//!   per bin) and the mean wait (Little: L/X − S); per container its CPU's utilisation and replicas
//!   per bin (ready pods when autoscaled); service-level violations per bin for the series that
//!   have a budget; the load profile per scenario. Statistics cover `[warmUpS, durationS]`; the
//!   bins cover the whole run.

use crate::evaluate::{self, LoadStep, Monitors, Request};
use crate::graph::{Graph, is_a};
use crate::pcm::PcmCache;
use palladio_sim_service::protocol::{WireModel, WireValue};
use serde_json::{Value, json};
use simoxide_sim::Limits;
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

/// What a detail run does: the evaluation's request and the detail's own options.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailRequest {
    pub run: Request,
    /// Latency budgets (ms) by series key or element name (see [`reduce`]).
    pub budgets_ms: BTreeMap<String, f64>,
    /// A request violates its service level above `slo_factor` × its budget.
    pub slo_factor: f64,
    pub histogram_per_decade: usize,
    /// Monitor every assembly operation (the operations inside the system).
    pub assembly_operations: bool,
}

impl Default for DetailRequest {
    fn default() -> Self {
        DetailRequest { run: Request::default(), budgets_ms: BTreeMap::new(), slo_factor: 1.0, histogram_per_decade: 10, assembly_operations: true }
    }
}

impl DetailRequest {
    /// From JSON: [`Request::from_json`]'s fields plus `budgetsMs: {key: ms}`, `sloFactor`,
    /// `histogramPerDecade`, `assemblyOperations`, every field optional.
    pub fn from_json(v: &Value) -> Result<DetailRequest, String> {
        let o = v.as_object().ok_or("a detail request is a JSON object")?;
        for k in o.keys() {
            if ![
                "overrides",
                "groups",
                "services",
                "maxPods",
                "percentiles",
                "detailCalls",
                "fixDoubleResume",
                "hpaSyncS",
                "hpaTolerance",
                "hpaDownscaleStabilisationS",
                "hpaCpuInitialisationS",
                "maxPathSets",
                "wallSeconds",
                "maxEvents",
                "profile",
                "usageStreams",
                "seed",
                "durationS",
                "warmUpS",
                "binS",
                "timeUnitMs",
                "budgetsMs",
                "sloFactor",
                "histogramPerDecade",
                "assemblyOperations",
                "model",
                "modelText",
                "autoscaling",
            ]
            .contains(&k.as_str())
            {
                return Err(format!("detail request: unknown field {k}"));
            }
        }
        let mut r = DetailRequest { run: Request::from_json(v).map_err(|e| format!("detail request: {e}"))?, ..DetailRequest::default() };
        let num = |k: &str| o.get(k).and_then(Value::as_f64);
        if let Some(b) = o.get("assemblyOperations").and_then(Value::as_bool) {
            r.assembly_operations = b;
        }
        r.slo_factor = num("sloFactor").unwrap_or(r.slo_factor);
        if let Some(k) = num("histogramPerDecade") {
            r.histogram_per_decade = (k as usize).max(1);
        }
        if let Some(b) = o.get("budgetsMs").and_then(Value::as_object) {
            for (k, v) in b {
                r.budgets_ms.insert(k.clone(), v.as_f64().ok_or("budgetsMs holds numbers")?);
            }
        }
        Ok(r)
    }
}

/// The detail of a JSON request (CLI `detail`, `POST /pcm/detail`).
pub fn detail_json(model: &WireModel, v: &Value, limits: Limits) -> Result<Value, String> {
    detail(model, &DetailRequest::from_json(v)?, limits)
}

// ---------------------------------------------------------------------------------------------
// Monitoring

fn is_monitoring(class: &str) -> bool {
    ["MonitorRepository", "Monitor", "MeasurementSpecification", "ProcessingType", "StatisticalCharacterization", "MeasuringPointRepository", "MeasuringPoint"]
        .iter()
        .any(|c| is_a(class, c))
}

/// Replaces the model's monitoring (in place) by the evaluation's monitors (Response Time of
/// every usage scenario, entry-level system call and system operation) and those of `mon`.
pub fn monitor(g: &mut Graph, mon: Monitors) {
    let old: std::collections::HashSet<String> = g.insts.iter().filter(|x| is_monitoring(&x.class_name)).map(|x| x.name.clone()).collect();
    g.remove_all(&old);
    let mrep = g.create("detail_monitors", "MonitorRepository");
    g.set_attr(&mrep, "entityName", WireValue::String("detail monitors".into()));
    let prep = g.create("detail_points", "MeasuringPointRepository");
    let add = |g: &mut Graph, class: &str, refs: &[(&str, &str)], replica: Option<i32>, metrics: &[&str]| {
        let mp = g.create("dmp", class);
        for (f, t) in refs {
            g.link(&mp, f, t);
        }
        if let Some(k) = replica {
            g.set_attr(&mp, "replicaID", WireValue::Int(k));
        }
        g.link(&prep, "measuringPoints", &mp);
        let mon = g.create("dmon", "Monitor");
        g.set_attr(&mon, "entityName", WireValue::String(format!("detail {class}")));
        g.set_attr(&mon, "activated", WireValue::Bool(true));
        g.link(&mrep, "monitors", &mon);
        g.link(&mon, "measuringPoint", &mp);
        for m in metrics {
            let spec = g.create("dspec", "MeasurementSpecification");
            g.set_attr(&spec, "metricDescription", WireValue::String((*m).into()));
            g.set_attr(&spec, "triggersSelfAdaptations", WireValue::Bool(false));
            let ft = g.create("dft", "FeedThrough");
            g.link(&spec, "processingType", &ft);
            g.link(&mon, "measurementSpecifications", &spec);
        }
    };
    for sc in g.all("UsageScenario") {
        add(g, "UsageScenarioMeasuringPoint", &[("usageScenario", &sc)], None, &["RESPONSE_TIME"]);
    }
    for call in g.all("EntryLevelSystemCall") {
        add(g, "EntryLevelSystemCallMeasuringPoint", &[("entryLevelSystemCall", &call)], None, &["RESPONSE_TIME"]);
    }
    // system operations: the delegated provided roles of each system × their signatures
    let mut seen = std::collections::HashSet::new();
    for d in g.all("ProvidedDelegationConnector") {
        let (Some(role), Some(sys)) = (g.ref1(&d, "outerProvidedRole"), g.container(&d)) else { continue };
        let Some(iface) = g.ref1(&role, "providedInterface") else { continue };
        for sig in g.refs(&iface, "signatures") {
            if seen.insert((role.clone(), sig.clone())) {
                add(g, "SystemOperationMeasuringPoint", &[("role", &role), ("operationSignature", &sig), ("system", &sys)], None, &["RESPONSE_TIME"]);
            }
        }
    }
    if mon.assembly_operations {
        let recursive = recursive_operations(g);
        for asm in g.all("AssemblyContext") {
            let Some(comp) = g.ref1(&asm, "encapsulatedComponent") else { continue };
            let served: std::collections::HashSet<String> =
                g.refs(&comp, "serviceEffectSpecifications").iter().filter_map(|s| g.ref1(s, "describedService")).collect();
            for role in g.refs(&comp, "providedRoles") {
                if g.class(&role) != "OperationProvidedRole" {
                    continue;
                }
                let Some(iface) = g.ref1(&role, "providedInterface") else { continue };
                for sig in g.refs(&iface, "signatures").into_iter().filter(|s| served.contains(s) && !recursive.contains(&(asm.clone(), s.clone()))) {
                    add(g, "AssemblyOperationMeasuringPoint", &[("role", &role), ("operationSignature", &sig), ("assembly", &asm)], None, &["RESPONSE_TIME"]);
                }
            }
        }
    }
    if mon.resources {
        for c in g.all("ResourceContainer") {
            for spec in g.refs(&c, "activeResourceSpecifications") {
                let n = g.num_attr(&spec, "numberOfReplicas").unwrap_or(1.0).max(1.0) as i32;
                let ps = g.str_attr(&spec, "schedulingPolicy").as_deref() == Some("PROCESSOR_SHARING");
                let cores = if ps { n } else { 1 };
                for k in 0..cores {
                    let metrics: &[&str] = if k == 0 { &["STATE_OF_ACTIVE_RESOURCE", "RESOURCE_DEMAND"] } else { &["STATE_OF_ACTIVE_RESOURCE"] };
                    add(g, "ActiveResourceMeasuringPoint", &[("activeResource", &spec)], Some(k), metrics);
                }
            }
        }
    }
}

/// The assembly operations (assembly, signature) that can call themselves (directly or through
/// others): SimuLizar aborts when a process re-enters a monitored operation (MEAS-1.5), so these
/// are not monitored.
fn recursive_operations(g: &Graph) -> std::collections::HashSet<(String, String)> {
    let mut connectors: HashMap<(String, String), String> = HashMap::new();
    for c in g.all("AssemblyConnector") {
        if let (Some(a), Some(r), Some(p)) = (g.ref1(&c, "requiringAssemblyContext"), g.ref1(&c, "requiredRole"), g.ref1(&c, "providingAssemblyContext")) {
            connectors.insert((a, r), p);
        }
    }
    let mut asms_of: HashMap<String, Vec<String>> = HashMap::new();
    for a in g.all("AssemblyContext") {
        if let Some(c) = g.ref1(&a, "encapsulatedComponent") {
            asms_of.entry(c).or_default().push(a);
        }
    }
    let mut edges: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
    for call in g.all("ExternalCallAction") {
        let (Some(role), Some(called)) = (g.ref1(&call, "role"), g.ref1(&call, "calledService")) else { continue };
        let mut cur = g.container(&call);
        while let Some(c) = &cur {
            if g.class(c) == "ResourceDemandingSEFF" {
                break;
            }
            cur = g.container(c);
        }
        let Some(seff) = cur else { continue };
        let (Some(comp), Some(sig)) = (g.container(&seff), g.ref1(&seff, "describedService")) else { continue };
        for asm in asms_of.get(&comp).into_iter().flatten() {
            if let Some(target) = connectors.get(&(asm.clone(), role.clone())) {
                edges.entry((asm.clone(), sig.clone())).or_default().push((target.clone(), called.clone()));
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    for start in edges.keys() {
        let mut stack: Vec<&(String, String)> = edges[start].iter().collect();
        let mut seen: std::collections::HashSet<&(String, String)> = std::collections::HashSet::new();
        while let Some(n) = stack.pop() {
            if n == start {
                out.insert(start.clone());
                break;
            }
            if seen.insert(n) {
                stack.extend(edges.get(n).into_iter().flatten());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The run

/// The detail of `model` (any model; its PCM part is simulated) as JSON.
pub fn detail(model: &WireModel, req: &DetailRequest, limits: Limits) -> Result<Value, String> {
    let t0 = Instant::now();
    let cache = PcmCache::new(model);
    let o = evaluate::run(&cache, &req.run, Monitors { assembly_operations: req.assembly_operations, resources: true }, limits)?;
    let k = 1000.0 / req.run.ms_per_unit;
    let (Some(g), Some(raw)) = (&o.graph, &o.raw) else {
        return Ok(json!({"kind": "pcm-detail", "version": 2, "failed": true, "message": o.eval.message, "evaluation": o.eval.to_json()}));
    };
    let mut out = reduce(g, &cache, req, &raw.meas, raw.end);
    let bins: Vec<f64> = out["t"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    for pg in &o.groups {
        let series = raw.ready_series(&pg.name, pg.initial as i32, k);
        let per_bin: Vec<f64> = bins.iter().map(|&b| step_mean(&series, b, (b + req.run.bin_s).min(req.run.end_s))).collect();
        if let Some(cs) = out["containers"].as_array_mut() {
            for c in cs.iter_mut().filter(|c| c["container"] == pg.container.as_str()) {
                c["replicas"] = json!(per_bin);
                c["autoscaled"] = json!(true);
            }
        }
    }
    out["evaluation"] = o.eval.to_json();
    out["simulator"] = json!({
        "name": "SimOxide", "mode": "fast", "seed": req.run.seed, "usageStreams": req.run.usage_streams,
        "durationS": req.run.end_s, "warmUpS": req.run.warm_up_s, "simulatedS": raw.end / k, "events": raw.events,
        "measurements": raw.meas.count, "autoscaling": if o.groups.is_empty() { Value::Null } else { json!("SPD + HPA") },
        "capped": o.eval.capped, "failed": o.eval.failed, "message": o.eval.message,
        "simulationMs": raw.wall_ms, "computeMs": t0.elapsed().as_secs_f64() * 1e3,
    });
    Ok(out)
}

/// Response-time tuples and step series of one run, reduced (`end` in model units).
pub fn reduce(g: &Graph, cache: &PcmCache, req: &DetailRequest, meas: &simoxide_sim::Measurements, end: f64) -> Value {
    let k = 1000.0 / req.run.ms_per_unit;
    let ms = req.run.ms_per_unit;
    let (w0, w1) = (req.run.warm_up_s, req.run.end_s.min(end / k).max(req.run.warm_up_s));
    let nbins = ((req.run.end_s / req.run.bin_s).ceil() as usize).max(1);
    let bins: Vec<f64> = (0..nbins).map(|i| i as f64 * req.run.bin_s).collect();
    let name = |id: &str| g.entity(id);
    let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut slo_bins = vec![(0u64, 0u64); nbins];
    let mut slo_total = (0u64, 0u64);
    // resources: spec -> per core state rows, overall utilisation rows, demand rows
    let mut states: BTreeMap<String, Vec<&[(f64, f64)]>> = BTreeMap::new();
    let mut overall: HashMap<String, &[(f64, f64)]> = HashMap::new();
    let mut demands: HashMap<String, &[(f64, f64)]> = HashMap::new();
    for (i, def) in meas.series.iter().enumerate() {
        let rows = &meas.rows[i];
        let mp: &str = &def.mp;
        let kind = mp.split('[').next().unwrap_or("");
        let ids = palladio_sim_service::engine::key_elements(mp);
        if kind == "ActiveResourceMeasuringPoint" {
            let Some(spec) = ids.first() else { continue };
            match def.metric {
                "State of Active Resource Tuple" => states.entry(spec.clone()).or_default().push(rows),
                "Utilization of Active Resource Tuple" => {
                    overall.insert(spec.clone(), rows);
                }
                "Resource Demand Tuple" => {
                    demands.insert(spec.clone(), rows);
                }
                _ => {}
            }
            continue;
        }
        if !def.metric.starts_with("Response Time") {
            continue;
        }
        let (group, key, extra) = match kind {
            "UsageScenarioMeasuringPoint" => ("scenarios", ids.join("|"), json!({"scenario": ids.first(), "name": ids.first().map(|x| name(x))})),
            "EntryLevelSystemCallMeasuringPoint" => {
                let call = ids.first().cloned().unwrap_or_default();
                let sig = g.ref1(&call, "operationSignature");
                let role = g.ref1(&call, "providedRole");
                let sc = cache.scenario_of(&call);
                (
                    "calls",
                    call.clone(),
                    json!({"call": call, "name": name(&call), "signature": sig, "signatureName": sig.as_deref().map(name),
                           "role": role, "scenario": sc, "scenarioName": sc.as_deref().map(name)}),
                )
            }
            "SystemOperationMeasuringPoint" => {
                let (role, sig) = (ids.first().cloned().unwrap_or_default(), ids.get(1).cloned().unwrap_or_default());
                ("systemOperations", format!("{role}|{sig}"), json!({"role": role, "signature": sig, "name": name(&sig), "roleName": name(&role)}))
            }
            "AssemblyOperationMeasuringPoint" => {
                let (role, sig, asm) = (ids.first().cloned().unwrap_or_default(), ids.get(1).cloned().unwrap_or_default(), ids.get(2).cloned().unwrap_or_default());
                let comp = g.ref1(&asm, "encapsulatedComponent");
                let container = g
                    .referrers(&asm, "assemblyContext")
                    .into_iter()
                    .find(|a| g.class(a) == "AllocationContext")
                    .and_then(|a| g.ref1(&a, "resourceContainer"));
                (
                    "assemblyOperations",
                    format!("{asm}|{role}|{sig}"),
                    json!({"assembly": asm, "assemblyName": name(&asm), "component": comp, "componentName": comp.as_deref().map(name),
                           "role": role, "signature": sig, "name": name(&sig), "container": container, "containerName": container.as_deref().map(name)}),
                )
            }
            _ => continue,
        };
        if rows.is_empty() {
            continue;
        }
        // a budget by the series' key, its element's name, or (system operations) its signature
        let budget = req
            .budgets_ms
            .get(&key)
            .or_else(|| extra.get("name").and_then(Value::as_str).and_then(|n| req.budgets_ms.get(n)))
            .or_else(|| (group == "systemOperations").then(|| extra.get("signature").and_then(Value::as_str).and_then(|n| req.budgets_ms.get(n))).flatten())
            .copied();
        let mut st = response_stats(rows, &bins, req, budget);
        if let Some(b) = budget {
            let limit = req.slo_factor * b / ms;
            for &(t, v) in rows.iter() {
                let ts = t / k;
                let bi = ((ts / req.run.bin_s) as usize).min(nbins - 1);
                let bad = (v > limit) as u64;
                slo_bins[bi].0 += 1;
                slo_bins[bi].1 += bad;
                if ts >= w0 && ts <= w1 {
                    slo_total.0 += 1;
                    slo_total.1 += bad;
                }
            }
        }
        st["key"] = json!(key);
        for (kk, vv) in extra.as_object().unwrap() {
            st[kk] = vv.clone();
        }
        groups.entry(group).or_default().push(st);
    }
    // ------------------------------------------------ resources and containers
    let mut resources = Vec::new();
    let mut containers = Vec::new();
    for c in g.all("ResourceContainer") {
        let mut cpu_util: Option<Vec<f64>> = None;
        let mut cpu_replicas = 0.0;
        for spec in g.refs(&c, "activeResourceSpecifications") {
            let kind = g.str_attr(&spec, "activeResourceType").unwrap_or_else(|| "CPU".into());
            let policy = g.str_attr(&spec, "schedulingPolicy").unwrap_or_default();
            let n = g.num_attr(&spec, "numberOfReplicas").unwrap_or(1.0).max(1.0);
            let cores = states.get(&spec).cloned().unwrap_or_default();
            // queue length: Σ cores; utilisation: the overall series (multi-core) or state > 0
            let queue_bins: Vec<f64> = (0..nbins).map(|b| cores.iter().map(|r| step_mean(r, bins[b] * k, (bins[b] + req.run.bin_s).min(req.run.end_s) * k)).sum()).collect();
            let queue = cores.iter().map(|r| step_mean(r, w0 * k, w1 * k)).sum::<f64>();
            let delay = policy == "DELAY";
            let util_of = |a: f64, b: f64| -> Option<f64> {
                if delay {
                    return None;
                }
                Some(match overall.get(&spec) {
                    Some(r) => step_mean(r, a, b),
                    None => cores.first().map_or(0.0, |r| step_mean_by(r, a, b, |v| if v > 0.0 { 1.0 } else { 0.0 })),
                })
            };
            let util = util_of(w0 * k, w1 * k);
            let util_bins: Option<Vec<f64>> =
                if delay { None } else { Some((0..nbins).map(|b| util_of(bins[b] * k, (bins[b] + req.run.bin_s).min(req.run.end_s) * k).unwrap_or(0.0)).collect()) };
            // wait: Little's R = L / X, minus the mean service time
            let (mut x, mut s_sum) = (0u64, 0.0);
            if let Some(d) = demands.get(&spec) {
                for &(t, v) in d.iter() {
                    if t >= w0 * k && t <= w1 * k {
                        x += 1;
                        s_sum += v;
                    }
                }
            }
            let window = (w1 - w0) * k;
            let wait_ms = if x > 0 && window > 0.0 {
                let thr = x as f64 / window;
                ((queue / thr) - s_sum / x as f64).max(0.0) * ms
            } else {
                0.0
            };
            if kind == "CPU" && cpu_util.is_none() {
                cpu_util = util_bins.clone();
                cpu_replicas = n;
            }
            resources.push(json!({
                "resource": spec, "container": c, "containerName": name(&c), "kind": kind, "policy": policy,
                "replicas": n, "utilisation": util,
                "queueLength": queue, "waitMs": wait_ms, "demands": x,
                "t": bins, "utilisationOverTime": util_bins, "queueOverTime": queue_bins,
            }));
        }
        containers.push(json!({
            "container": c, "name": name(&c), "t": bins,
            "utilisation": cpu_util.unwrap_or_else(|| vec![0.0; nbins]),
            "replicas": vec![cpu_replicas; nbins],
        }));
    }
    // ------------------------------------------------ load
    let mut factors = serde_json::Map::new();
    let mut base = serde_json::Map::new();
    for sc in g.all("UsageScenario") {
        let f = |t: f64| -> f64 {
            let named: Vec<&LoadStep> = req.run.profile.iter().filter(|s| s.scenario.as_deref() == Some(sc.as_str()) || s.scenario.as_deref() == Some(name(&sc).as_str())).collect();
            let steps: Vec<&LoadStep> = if named.is_empty() { req.run.profile.iter().filter(|s| s.scenario.is_none()).collect() } else { named };
            steps.iter().filter(|s| s.time <= t).max_by(|a, b| a.time.total_cmp(&b.time)).map_or(1.0, |s| s.factor)
        };
        factors.insert(sc.clone(), json!(bins.iter().map(|&t| f(t)).collect::<Vec<_>>()));
        // measured: the sessions completed in the window per second and unit of the profile's
        // factor (null without sessions)
        let done = meas
            .series
            .iter()
            .position(|d| d.metric.starts_with("Response Time") && *d.mp == *format!("UsageScenarioMeasuringPoint[{sc}]"))
            .map_or(0, |i| meas.rows[i].iter().filter(|r| r.0 >= w0 * k && r.0 <= w1 * k).count());
        let steps = 1000usize;
        let mean_factor = (0..steps).map(|i| f(w0 + (w1 - w0) * (i as f64 + 0.5) / steps as f64)).sum::<f64>() / steps as f64;
        let rate = (done > 0 && mean_factor > 0.0 && w1 > w0).then(|| done as f64 / (w1 - w0) / mean_factor);
        base.insert(sc.clone(), json!(rate));
    }
    let slo = json!({
        "factor": req.slo_factor,
        "t": bins,
        "violationShare": slo_bins.iter().map(|&(n, b)| if n == 0 { 0.0 } else { b as f64 / n as f64 }).collect::<Vec<_>>(),
        "total": {"requests": slo_total.0, "violations": slo_total.1, "share": if slo_total.0 == 0 { 0.0 } else { slo_total.1 as f64 / slo_total.0 as f64 }},
    });
    let mut out = json!({
        "kind": "pcm-detail",
        "version": 2,
        "timeUnitMs": req.run.ms_per_unit,
        "binS": req.run.bin_s,
        "t": bins,
        "load": {"t": bins, "baseRatePerS": base, "factor": factors},
        "resources": resources,
        "containers": containers,
        "slo": slo,
    });
    for g in ["scenarios", "calls", "systemOperations", "assemblyOperations"] {
        out[g] = json!(groups.remove(g).unwrap_or_default());
    }
    out
}

/// Statistics of response-time tuples (model units) in ms: over the window, and per bin.
fn response_stats(rows: &[(f64, f64)], bins: &[f64], req: &DetailRequest, budget: Option<f64>) -> Value {
    let k = 1000.0 / req.run.ms_per_unit;
    let ms = req.run.ms_per_unit;
    let nb = bins.len();
    let mut v: Vec<f64> = rows.iter().filter(|r| r.0 / k >= req.run.warm_up_s && r.0 / k <= req.run.end_s).map(|r| r.1 * ms).collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let pct = |p: f64| if n == 0 { 0.0 } else { v[((p * n as f64).ceil() as usize).clamp(1, n) - 1] };
    let mean = if n == 0 { 0.0 } else { v.iter().sum::<f64>() / n as f64 };
    // log-spaced histogram
    let (mut edges, mut counts) = (Vec::new(), Vec::new());
    let positive: Vec<f64> = v.iter().copied().filter(|x| *x > 0.0).collect();
    if let (Some(lo), Some(hi)) = (positive.first(), positive.last()) {
        let d = req.histogram_per_decade as f64;
        let a = (lo.log10() * d).floor() as i64;
        let b = ((hi.log10() * d).floor() as i64) + 1;
        edges = (a..=b).map(|i| 10f64.powf(i as f64 / d)).collect();
        counts = vec![0u64; edges.len() - 1];
        for x in &positive {
            let i = (((x.log10() * d).floor() as i64) - a).clamp(0, counts.len() as i64 - 1) as usize;
            counts[i] += 1;
        }
        let zeros = n - positive.len();
        if zeros > 0 {
            edges.insert(0, 0.0);
            counts.insert(0, zeros as u64);
        }
    }
    // per bin
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); nb];
    for &(t, x) in rows {
        let bi = ((t / k / req.run.bin_s) as usize).min(nb - 1);
        per[bi].push(x * ms);
    }
    let mut means = Vec::with_capacity(nb);
    let mut p95s = Vec::with_capacity(nb);
    let mut thr = Vec::with_capacity(nb);
    for (i, b) in per.iter_mut().enumerate() {
        b.sort_by(f64::total_cmp);
        let len = b.len();
        means.push(if len == 0 { Value::Null } else { json!(b.iter().sum::<f64>() / len as f64) });
        p95s.push(if len == 0 { Value::Null } else { json!(b[((0.95 * len as f64).ceil() as usize).clamp(1, len) - 1]) });
        let width = (bins[i] + req.run.bin_s).min(req.run.end_s) - bins[i];
        thr.push(if width > 0.0 { len as f64 / width } else { 0.0 });
    }
    let violations = budget.map(|b| v.iter().filter(|x| **x > req.slo_factor * b).count());
    json!({
        "count": n, "meanMs": mean, "p50Ms": pct(0.5), "p90Ms": pct(0.9), "p95Ms": pct(0.95), "p99Ms": pct(0.99),
        "maxMs": v.last().copied().unwrap_or(0.0), "budgetMs": budget, "violations": violations,
        "histogram": {"edgesMs": edges, "counts": counts},
        "overTime": {"t": bins, "meanMs": means, "p95Ms": p95s, "throughput": thr},
    })
}

/// Time-weighted mean over `[a, b]` of a step series (value 0 before its first tuple).
pub fn step_mean(rows: &[(f64, f64)], a: f64, b: f64) -> f64 {
    step_mean_by(rows, a, b, |v| v)
}

fn step_mean_by(rows: &[(f64, f64)], a: f64, b: f64, f: impl Fn(f64) -> f64) -> f64 {
    if !(b > a) {
        return 0.0;
    }
    // the value at a: the last tuple at or before a (rows are in time order)
    let start = rows.partition_point(|r| r.0 <= a);
    let mut cur = if start == 0 { 0.0 } else { f(rows[start - 1].1) };
    let mut t = a;
    let mut sum = 0.0;
    for &(tt, v) in &rows[start..] {
        if tt >= b {
            break;
        }
        sum += cur * (tt - t);
        t = tt;
        cur = f(v);
    }
    sum += cur * (b - t);
    sum / (b - a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_means() {
        let rows = [(1.0, 2.0), (3.0, 0.0), (3.0, 4.0), (5.0, 0.0)];
        assert_eq!(step_mean(&rows, 0.0, 6.0), (2.0 * 2.0 + 4.0 * 2.0) / 6.0);
        assert_eq!(step_mean(&rows, 2.0, 4.0), (2.0 + 4.0) / 2.0);
        assert_eq!(step_mean_by(&rows, 0.0, 6.0, |v| (v > 0.0) as u8 as f64), 4.0 / 6.0);
    }

    #[test]
    fn requests_from_json() {
        let r = DetailRequest::from_json(&json!({"durationS": 100, "profile": [{"timeS": 10, "factor": 2}], "budgetsMs": {"x": 50},
            "autoscaling": {"groups": [{"container": "C2", "min": 1, "max": 4, "target": 0.7}], "maxPods": 9}})).unwrap();
        assert_eq!(r.run.end_s, 100.0);
        assert_eq!(r.run.profile, vec![LoadStep { scenario: None, time: 10.0, factor: 2.0 }]);
        assert!(r.run.usage_streams);
        assert_eq!((r.run.groups.len(), r.run.max_pods), (1, 9));
        assert_eq!(r.budgets_ms["x"], 50.0);
        let g = DetailRequest::from_json(&json!({"groups": [{"container": "C2", "min": 1, "max": 4, "target": 0.7}], "percentiles": [99], "hpaSyncS": 5})).unwrap();
        assert_eq!(g.run.groups, r.run.groups);
        assert_eq!((g.run.percentiles.clone(), g.run.controller.sync_s), (vec![99.0], 5.0));
        assert!(DetailRequest::from_json(&json!({"durationS": 10, "warmUpS": 20})).is_err());
        assert!(DetailRequest::from_json(&json!({"bogus": 1})).is_err());
    }
}
