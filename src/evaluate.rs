//! `pcmEvaluate`: one simulation of the plain PCM of a model with SimOxide, and the raw
//! measurements of that run (`API-pcmEvaluate.md`). Nothing is interpreted here: cost, failure
//! models, budgets are the scripts'.
//!
//! - **Model.** The model's PCM with the processing resources overridden; per autoscaled
//!   container ([`ScalingGroup`]) an SPD elastic infrastructure with SimOxide's HPA, its
//!   assemblies behind load balancers (the model's, [`ServiceGroup`], or the plugin's,
//!   [`crate::lb`]); the model's monitors replaced by response-time monitors of every usage
//!   scenario, entry-level system call and system operation, and with `detailCalls` of every
//!   assembly operation (the design detail adds resource monitors, [`Monitors`]; monitors do not
//!   change the run).
//! - **Run** ([`crate::dynamic`]): SimOxide's fast mode with the request's seed, per-scenario
//!   load profile, usage streams, SimOxide's path monitoring (MEAS-13: the containers on each
//!   session's synchronous path, call counts) and busy-time hooks, read at the window's start,
//!   at every bin edge and at its end. A run that fails, stops at the pod cap, the event limit or
//!   the wall-clock limit is an evaluation with `failed` set, not an error.
//! - **Reduction** ([`Evaluation`]): response-time statistics (requested percentiles, a log
//!   histogram, per-bin series), utilisation and pods per processing resource (window and per
//!   bin), call counts, the container sets of the sessions' synchronous paths.
//!
//! Evaluations are deterministic and cached per (model digest, request): in the model's cache
//! ([`crate::pcm::PcmCache::evals`]) and in the process's bounded [`Evaluator`].

use crate::detail;
use crate::dynamic::{self, Controller, DynamicModel, PodGroup, RawRun, Reading};
use crate::graph::Graph;
use crate::pcm::PcmCache;
use serde_json::{Value, json};
use simoxide_sim::{Limits, LoadEntry, LoadProfile};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The element ids of a measuring-point key (`Kind[id|id|...]`): the measured elements.
pub fn key_elements(mp: &str) -> Vec<String> {
    let inner = mp.split_once('[').map_or("", |x| x.1).trim_end_matches(']');
    inner.split('|').filter(|s| !s.is_empty() && !s.contains('=') && *s != "null").map(String::from).collect()
}

/// The plugin build: date and git hash (`build.rs`).
pub const VERSION: &str = env!("PLUGIN_VERSION");

// ---------------------------------------------------------------------------------------------
// The request

/// One step of a load profile: from `time` (s) on, the arrival rate of `scenario` (every scenario
/// when `None`) is `factor` times the model's. Before a scenario's first step its factor is 1.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadStep {
    pub scenario: Option<String>,
    pub time: f64,
    pub factor: f64,
}

/// SimOxide's load profile of `steps` (times in s, `k` model units per s): one entry for the
/// steps without a scenario (every scenario), then one per named scenario (a later entry
/// overrides the earlier for its scenario), each sorted by time. `None` without steps.
pub fn load_profile(steps: &[LoadStep], k: f64) -> Result<Option<LoadProfile>, String> {
    if steps.is_empty() {
        return Ok(None);
    }
    let mut by: BTreeMap<Option<String>, Vec<(f64, f64)>> = BTreeMap::new();
    for s in steps {
        by.entry(s.scenario.clone()).or_default().push((s.time * k, s.factor));
    }
    let mut entries = Vec::new();
    for (sc, mut points) in by {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let Some(w) = points.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(format!("load profile: two steps at time {} s for {}", w[0].0 / k, sc.as_deref().unwrap_or("every scenario")));
        }
        entries.push(LoadEntry { scenarios: sc.into_iter().collect(), points });
    }
    let p = LoadProfile { entries };
    p.validate()?;
    Ok(Some(p))
}

/// One autoscaled container (an SPD elastic infrastructure with a HorizontalPodAutoscaler).
#[derive(Clone, Debug, PartialEq)]
pub struct ScalingGroup {
    /// The pod's ResourceContainer (its CPU: one pod's cores).
    pub container: String,
    pub min: u32,
    pub max: u32,
    /// Target CPU utilisation, 0..1.
    pub target: f64,
    /// Pods at the start (clamped to min..max).
    pub initial: u32,
    /// Start-up delay of a new pod (s).
    pub startup_s: f64,
    /// Scale-down stabilisation window (s); `None`: the controller's.
    pub scale_down_s: Option<f64>,
    /// The HPA's CPU request per pod, in the pod's cores; `None`: all its cores.
    pub cpu_request: Option<f64>,
    /// A Kubernetes `spec.behavior` object, passed to SimOxide's HPA as is.
    pub behavior: Option<Value>,
}

impl ScalingGroup {
    /// A group with Kubernetes-like defaults (1..10 pods, target 0.7, 30 s start-up).
    pub fn new(container: &str) -> ScalingGroup {
        ScalingGroup {
            container: container.to_string(),
            min: 1,
            max: 10,
            target: 0.7,
            initial: 1,
            startup_s: dynamic::DEFAULT_STARTUP_S,
            scale_down_s: None,
            cpu_request: None,
            behavior: None,
        }
    }
}

/// An assembly on a scaled container and its load balancer's assembly (an SPD service group).
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceGroup {
    pub container: String,
    pub assembly: String,
    pub load_balancer: String,
}

/// What one evaluation simulates (times in seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// (processing resource, replicas, rate).
    pub overrides: Vec<(String, i32, f64)>,
    pub groups: Vec<ScalingGroup>,
    pub services: Vec<ServiceGroup>,
    pub profile: Vec<LoadStep>,
    pub warm_up_s: f64,
    pub end_s: f64,
    pub bin_s: f64,
    /// Milliseconds per model time unit.
    pub ms_per_unit: f64,
    /// Percentiles (in percent) of the response-time statistics.
    pub percentiles: Vec<f64>,
    /// Response times per entry-level call and assembly operation too.
    pub detail_calls: bool,
    pub histogram_per_decade: usize,
    pub seed: i64,
    pub usage_streams: bool,
    pub fix_double_resume: bool,
    pub max_pods: u32,
    pub controller: Controller,
    pub max_path_sets: usize,
    /// Wall-clock limit of the simulation (s; ≤ 0: none).
    pub wall_s: f64,
    /// Event limit (0: the service's).
    pub max_events: u64,
    /// Path sets of asynchronous work (`asyncPaths`, revision 4) and the ForkActions whose
    /// asynchronous behaviours they do not follow (`asyncIgnoreForks`).
    pub async_paths: bool,
    pub async_ignore_forks: Vec<String>,
}

pub const DEFAULT_PERCENTILES: [f64; 4] = [50.0, 90.0, 95.0, 99.0];

impl Default for Request {
    fn default() -> Self {
        Request {
            overrides: Vec::new(),
            groups: Vec::new(),
            services: Vec::new(),
            profile: Vec::new(),
            warm_up_s: 0.0,
            end_s: 600.0,
            bin_s: 10.0,
            ms_per_unit: 1000.0,
            percentiles: DEFAULT_PERCENTILES.to_vec(),
            detail_calls: false,
            histogram_per_decade: 10,
            seed: 1,
            usage_streams: true,
            fix_double_resume: true,
            max_pods: dynamic::DEFAULT_MAX_PODS,
            controller: Controller::default(),
            max_path_sets: 10_000,
            wall_s: 60.0,
            max_events: 0,
            async_paths: false,
            async_ignore_forks: Vec::new(),
        }
    }
}

impl Request {
    /// The request with the caller's names replaced by canonical ones ([`crate::canon`]).
    pub fn canonical(&self, c: &crate::canon::Canonical) -> Request {
        let mut r = self.clone();
        for o in &mut r.overrides {
            o.0 = c.canon(&o.0);
        }
        for g in &mut r.groups {
            g.container = c.canon(&g.container);
        }
        for s in &mut r.services {
            s.container = c.canon(&s.container);
            s.assembly = c.canon(&s.assembly);
            s.load_balancer = c.canon(&s.load_balancer);
        }
        for p in &mut r.profile {
            p.scenario = p.scenario.as_deref().map(|s| c.canon(s));
        }
        for f in &mut r.async_ignore_forks {
            *f = c.canon(f);
        }
        r.async_ignore_forks.sort();
        r
    }

    /// The request as the JSON [`Request::from_json`] reads (the command line's and the design
    /// detail's request): `from_json(to_json(r)) == r`.
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "overrides": self.overrides.iter().map(|(r, n, rate)| json!({"resource": r, "replicas": n, "rate": rate})).collect::<Vec<_>>(),
            "groups": self.groups.iter().map(|g| {
                let mut x = json!({"container": g.container, "min": g.min, "max": g.max, "target": g.target, "initial": g.initial, "startupS": g.startup_s});
                if let Some(s) = g.scale_down_s { x["scaleDownS"] = json!(s); }
                if let Some(c) = g.cpu_request { x["cpuRequest"] = json!(c); }
                if let Some(b) = &g.behavior { x["behavior"] = b.clone(); }
                x
            }).collect::<Vec<_>>(),
            "services": self.services.iter().map(|s| json!({"container": s.container, "assembly": s.assembly, "loadBalancer": s.load_balancer})).collect::<Vec<_>>(),
            "profile": self.profile.iter().map(|p| {
                let mut x = json!({"timeS": p.time, "factor": p.factor});
                if let Some(s) = &p.scenario { x["scenario"] = json!(s); }
                x
            }).collect::<Vec<_>>(),
            "warmUpS": self.warm_up_s, "durationS": self.end_s, "binS": self.bin_s, "timeUnitMs": self.ms_per_unit,
            "percentiles": self.percentiles, "detailCalls": self.detail_calls, "histogramPerDecade": self.histogram_per_decade,
            "seed": self.seed, "usageStreams": self.usage_streams, "fixDoubleResume": self.fix_double_resume, "maxPods": self.max_pods,
            "hpaSyncS": self.controller.sync_s, "hpaTolerance": self.controller.tolerance,
            "hpaDownscaleStabilisationS": self.controller.downscale_stabilisation_s, "hpaCpuInitialisationS": self.controller.cpu_initialisation_s,
            "maxPathSets": self.max_path_sets, "wallSeconds": self.wall_s, "maxEvents": self.max_events,
        });
        if v["groups"].as_array().is_some_and(Vec::is_empty) {
            v.as_object_mut().unwrap().remove("groups");
        }
        // revision 4, only when asked for (requests without them keep revision 2's text)
        if self.async_paths {
            v["asyncPaths"] = json!(true);
        }
        if !self.async_ignore_forks.is_empty() {
            v["asyncIgnoreForks"] = json!(self.async_ignore_forks);
        }
        v
    }

    /// A canonical text of the request (cache key).
    pub fn key(&self) -> String {
        format!("{self:?}")
    }

    pub fn check(&self) -> Result<(), String> {
        if !(self.end_s > self.warm_up_s) || self.warm_up_s < 0.0 {
            return Err("needs end > warmUp ≥ 0".into());
        }
        if !(self.bin_s > 0.0) || !(self.ms_per_unit > 0.0) {
            return Err("needs binSeconds > 0 and msPerTimeUnit > 0".into());
        }
        if (self.end_s - self.warm_up_s) / self.bin_s > 1e6 {
            return Err("more than a million bins".into());
        }
        if let Some(p) = self.percentiles.iter().find(|p| !(**p >= 0.0 && **p <= 100.0)) {
            return Err(format!("percentile {p} is not in 0..100"));
        }
        if self.histogram_per_decade == 0 || self.histogram_per_decade > 1000 {
            return Err("histogramPerDecade must be 1..1000".into());
        }
        Ok(())
    }

    /// From the JSON of a detail request (`API-pcmEvaluate.md`, design detail).
    pub fn from_json(v: &Value) -> Result<Request, String> {
        let mut r = Request::default();
        let o = v.as_object().ok_or("a request is a JSON object")?;
        let num = |k: &str| o.get(k).and_then(Value::as_f64);
        if let Some(list) = o.get("overrides").and_then(Value::as_array) {
            for x in list {
                let res = x.get("resource").and_then(Value::as_str).ok_or("an override needs a resource")?;
                let n = x.get("replicas").and_then(Value::as_f64).ok_or("an override needs replicas")?;
                let rate = x.get("rate").and_then(Value::as_f64).ok_or("an override needs a rate")?;
                r.overrides.push((res.to_string(), n as i32, rate));
            }
        }
        if let Some(list) = o.get("profile").and_then(Value::as_array) {
            for x in list {
                r.profile.push(LoadStep {
                    scenario: x.get("scenario").and_then(Value::as_str).map(str::to_string),
                    time: x.get("timeS").and_then(Value::as_f64).ok_or("a load step needs timeS")?,
                    factor: x.get("factor").and_then(Value::as_f64).ok_or("a load step needs a factor")?,
                });
            }
        }
        if let Some(list) = o.get("percentiles").and_then(Value::as_array) {
            r.percentiles = list.iter().map(|x| x.as_f64().ok_or("percentiles holds numbers")).collect::<Result<_, _>>()?;
        }
        let flag = |k: &str| o.get(k).and_then(Value::as_bool);
        r.usage_streams = flag("usageStreams").unwrap_or(r.usage_streams);
        r.fix_double_resume = flag("fixDoubleResume").unwrap_or(r.fix_double_resume);
        r.detail_calls = flag("detailCalls").unwrap_or(r.detail_calls);
        r.async_paths = flag("asyncPaths").unwrap_or(r.async_paths);
        if let Some(list) = o.get("asyncIgnoreForks").and_then(Value::as_array) {
            r.async_ignore_forks = list.iter().map(|x| x.as_str().map(str::to_string).ok_or("asyncIgnoreForks holds ForkAction ids")).collect::<Result<_, _>>()?;
        }
        if let Some(s) = o.get("seed").and_then(Value::as_i64) {
            r.seed = s;
        }
        r.end_s = num("durationS").unwrap_or(r.end_s);
        r.warm_up_s = num("warmUpS").unwrap_or(r.warm_up_s);
        r.bin_s = num("binS").unwrap_or(r.bin_s);
        r.ms_per_unit = num("timeUnitMs").unwrap_or(r.ms_per_unit);
        r.wall_s = num("wallSeconds").unwrap_or(r.wall_s);
        r.max_events = num("maxEvents").map_or(r.max_events, |x| x.max(0.0) as u64);
        r.max_path_sets = num("maxPathSets").map_or(r.max_path_sets, |x| x.max(1.0) as usize);
        if let Some(h) = num("histogramPerDecade") {
            r.histogram_per_decade = h as usize;
        }
        let c = &mut r.controller;
        c.sync_s = num("hpaSyncS").unwrap_or(c.sync_s);
        c.tolerance = num("hpaTolerance").unwrap_or(c.tolerance);
        c.downscale_stabilisation_s = num("hpaDownscaleStabilisationS").unwrap_or(c.downscale_stabilisation_s);
        c.cpu_initialisation_s = num("hpaCpuInitialisationS").unwrap_or(c.cpu_initialisation_s);
        let a = o.get("autoscaling");
        let field = |k: &str| o.get(k).or_else(|| a.and_then(|a| a.get(k)));
        let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{k} is missing"));
        for g in field("groups").and_then(Value::as_array).into_iter().flatten() {
            let mut x = ScalingGroup::new(&s(g, "container")?);
            let n = |k: &str| g.get(k).and_then(Value::as_f64);
            x.min = n("min").map_or(x.min, |v| v as u32);
            x.max = n("max").map_or(x.max, |v| v as u32);
            x.target = n("target").unwrap_or(x.target);
            x.initial = n("initial").map_or(x.initial, |v| v as u32);
            x.startup_s = n("startupS").unwrap_or(x.startup_s);
            x.scale_down_s = n("scaleDownS").filter(|v| *v >= 0.0);
            x.cpu_request = n("cpuRequest").filter(|v| *v > 0.0);
            x.behavior = g.get("behavior").filter(|b| b.is_object()).cloned();
            r.groups.push(x);
        }
        for x in field("services").and_then(Value::as_array).into_iter().flatten() {
            r.services.push(ServiceGroup { container: s(x, "container")?, assembly: s(x, "assembly")?, load_balancer: s(x, "loadBalancer")? });
        }
        if let Some(m) = field("maxPods").and_then(Value::as_u64) {
            r.max_pods = m as u32;
        }
        r.check()?;
        Ok(r)
    }
}

/// The monitors of a run besides the evaluation's (scenarios, entry-level calls, system
/// operations, and with `detailCalls` assembly operations): the design detail's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Monitors {
    /// Response time of every assembly operation (except those that can call themselves).
    pub assembly_operations: bool,
    /// State of every core and the resource demand of every processing resource.
    pub resources: bool,
}

// ---------------------------------------------------------------------------------------------
// The result

/// Response-time statistics (ms) of one series over the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResponseStat {
    /// `systemOperation`, `scenario`, `call`, `assemblyOperation`.
    pub kind: &'static str,
    pub key: String,
    pub scenario: Option<String>,
    pub call: Option<String>,
    pub role: Option<String>,
    pub signature: Option<String>,
    pub assembly: Option<String>,
    pub count: u64,
    pub mean_ms: f64,
    pub max_ms: f64,
    pub percentiles_ms: Vec<f64>,
    pub histogram_edges_ms: Vec<f64>,
    pub histogram_counts: Vec<u64>,
    pub bin_counts: Vec<u64>,
    pub bin_mean_ms: Vec<f64>,
    /// Bin-major: bin `b`, percentile `j` at `b · P + j`.
    pub bin_percentiles_ms: Vec<f64>,
}

/// One processing resource of a container of the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceStat {
    pub container: String,
    pub resource: String,
    pub kind: String,
    pub autoscaled: bool,
    pub cores: f64,
    pub mean_utilisation: f64,
    pub bin_utilisation: Vec<f64>,
    pub max_bin_utilisation: f64,
    pub mean_ready_replicas: f64,
    pub peak_ready_replicas: f64,
    pub bin_ready_replicas: Vec<f64>,
    pub bin_replicas: Vec<f64>,
    pub replica_core_seconds: f64,
    pub ready_replica_core_seconds: f64,
    pub rescales: u64,
    pub created_pods: u64,
    pub reversals: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathSet {
    pub scenario: String,
    pub containers: Vec<String>,
    pub count: u64,
}

/// Completed units of asynchronous work with one key (`PcmAsyncPathSet`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsyncPathSet {
    pub scenario: Option<String>,
    pub assembly: String,
    pub seff: String,
    pub signature: String,
    pub context: Vec<String>,
    pub local_context: Vec<String>,
    pub containers: Vec<String>,
    pub count: u64,
}

/// What `pcmEvaluate` returns (`API-pcmEvaluate.md`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluation {
    pub failed: bool,
    pub capped: bool,
    pub timed_out: bool,
    pub message: String,
    pub window_s: f64,
    pub bin_s: f64,
    pub bins: usize,
    pub percentiles: Vec<f64>,
    pub simulated_s: f64,
    pub events: u64,
    pub wall_ms: f64,
    pub operations: Vec<ResponseStat>,
    pub sessions: Vec<ResponseStat>,
    pub calls: Vec<ResponseStat>,
    pub assembly_operations: Vec<ResponseStat>,
    pub resources: Vec<ResourceStat>,
    /// (ExternalCallAction or EntryLevelSystemCall, count in the window).
    pub call_counts: Vec<(String, u64)>,
    pub path_sets: Vec<PathSet>,
    pub path_sets_truncated: bool,
    /// `asyncPaths` was asked for (the three fields below are in the JSON).
    pub async_requested: bool,
    pub async_paths_measured: bool,
    pub async_path_sets: Vec<AsyncPathSet>,
    pub async_path_sets_truncated: bool,
    pub created_pods: u64,
    pub rescales: u64,
    pub peak_pods: f64,
    pub reversals: u64,
    pub warnings: Vec<String>,
}

fn response_json(s: &ResponseStat) -> Value {
    json!({
        "kind": s.kind, "key": s.key, "scenario": s.scenario, "call": s.call, "role": s.role, "signature": s.signature,
        "assembly": s.assembly, "count": s.count, "meanMs": s.mean_ms, "maxMs": s.max_ms, "percentilesMs": s.percentiles_ms,
        "histogramEdgesMs": s.histogram_edges_ms, "histogramCounts": s.histogram_counts, "binCounts": s.bin_counts,
        "binMeanMs": s.bin_mean_ms, "binPercentilesMs": s.bin_percentiles_ms,
    })
}

impl Evaluation {
    /// The evaluation with canonical names replaced by the caller's ([`crate::canon`]); lists
    /// keep their canonical order, so equal designs give equal lists.
    pub fn to_caller(&self, c: &crate::canon::Canonical) -> Evaluation {
        let mut e = self.clone();
        let o = |s: &mut String| *s = c.orig(s);
        let oo = |s: &mut Option<String>| {
            if let Some(x) = s {
                *x = c.orig(x);
            }
        };
        for v in [&mut e.operations, &mut e.sessions, &mut e.calls, &mut e.assembly_operations] {
            for s in v.iter_mut() {
                s.key = c.orig_text(&s.key, false);
                oo(&mut s.scenario);
                oo(&mut s.call);
                oo(&mut s.role);
                oo(&mut s.signature);
                oo(&mut s.assembly);
            }
        }
        for r in &mut e.resources {
            o(&mut r.container);
            o(&mut r.resource);
        }
        for (a, _) in &mut e.call_counts {
            o(a);
        }
        for p in &mut e.path_sets {
            o(&mut p.scenario);
            p.containers.iter_mut().for_each(o);
        }
        for p in &mut e.async_path_sets {
            oo(&mut p.scenario);
            o(&mut p.assembly);
            o(&mut p.seff);
            o(&mut p.signature);
            p.context.iter_mut().for_each(o);
            p.local_context.iter_mut().for_each(o);
            p.containers.iter_mut().for_each(o);
        }
        e.message = c.orig_text(&e.message, false);
        for w in &mut e.warnings {
            *w = c.orig_text(w, false);
        }
        e
    }

    pub fn failure(req: &Request, message: String) -> Evaluation {
        Evaluation {
            failed: true,
            message,
            async_requested: req.async_paths,
            window_s: req.end_s - req.warm_up_s,
            bin_s: req.bin_s,
            percentiles: req.percentiles.clone(),
            ..Evaluation::default()
        }
    }

    /// The evaluation as JSON (instances by id; the record fields of `PcmEvaluation`).
    pub fn to_json(&self) -> Value {
        let rs = |v: &[ResponseStat]| v.iter().map(response_json).collect::<Vec<_>>();
        let mut v = json!({
            "failed": self.failed, "capped": self.capped, "timedOut": self.timed_out, "message": self.message, "version": VERSION,
            "windowSeconds": self.window_s, "binSeconds": self.bin_s, "bins": self.bins, "percentiles": self.percentiles,
            "simulatedSeconds": self.simulated_s, "events": self.events, "wallMs": self.wall_ms,
            "operations": rs(&self.operations), "sessions": rs(&self.sessions), "calls": rs(&self.calls),
            "assemblyOperations": rs(&self.assembly_operations),
            "resources": self.resources.iter().map(|c| json!({
                "container": c.container, "resource": c.resource, "kind": c.kind, "autoscaled": c.autoscaled, "cores": c.cores,
                "meanUtilisation": c.mean_utilisation, "binUtilisation": c.bin_utilisation, "maxBinUtilisation": c.max_bin_utilisation,
                "meanReadyReplicas": c.mean_ready_replicas, "peakReadyReplicas": c.peak_ready_replicas,
                "binReadyReplicas": c.bin_ready_replicas, "binReplicas": c.bin_replicas,
                "replicaCoreSeconds": c.replica_core_seconds, "readyReplicaCoreSeconds": c.ready_replica_core_seconds,
                "rescales": c.rescales, "createdPods": c.created_pods, "reversals": c.reversals,
            })).collect::<Vec<_>>(),
            "callCounts": self.call_counts.iter().map(|(a, n)| json!({"action": a, "count": n})).collect::<Vec<_>>(),
            "pathSets": self.path_sets.iter().map(|s| json!({"scenario": s.scenario, "containers": s.containers, "count": s.count})).collect::<Vec<_>>(),
            "pathSetsTruncated": self.path_sets_truncated,
            "createdPods": self.created_pods, "rescales": self.rescales, "peakPods": self.peak_pods, "reversals": self.reversals,
            "warnings": self.warnings,
        });
        // revision 4: only when asked for (the JSON of other requests is revision 2's)
        if self.async_requested {
            let o = v.as_object_mut().unwrap();
            o.insert("asyncPathsMeasured".into(), json!(self.async_paths_measured));
            o.insert(
                "asyncPathSets".into(),
                json!(self.async_path_sets.iter().map(|s| json!({
                    "scenario": s.scenario, "assembly": s.assembly, "seff": s.seff, "signature": s.signature,
                    "context": s.context, "localContext": s.local_context, "containers": s.containers, "count": s.count,
                })).collect::<Vec<_>>()),
            );
            o.insert("asyncPathSetsTruncated".into(), json!(self.async_path_sets_truncated));
        }
        v
    }
}

// ---------------------------------------------------------------------------------------------
// The run

/// The simulated model of a request: the plain PCM with the overrides, load balancers for the
/// groups without service groups, the monitors; and its pod groups. `Err`: wrong arguments;
/// `Ok(Err)`: a model this design cannot be simulated with.
fn build(cache: &PcmCache, req: &Request, mon: Monitors) -> Result<Result<(DynamicModel, Graph), String>, String> {
    let mut wire = cache.with_overrides(&req.overrides);
    let g0 = Graph::new(&wire);
    for s in &req.groups {
        if g0.class(&s.container) != "ResourceContainer" {
            return Err(format!("{} is not a ResourceContainer", s.container));
        }
    }
    for x in &req.services {
        for a in [&x.assembly, &x.load_balancer] {
            if g0.class(a) != "AssemblyContext" {
                return Err(format!("{a} is not an AssemblyContext"));
            }
        }
    }
    let bare: Vec<String> = req.groups.iter().filter(|g| !req.services.iter().any(|s| s.container == g.container)).map(|g| g.container.clone()).collect();
    let mut services = req.services.clone();
    if !bare.is_empty() {
        match crate::lb::load_balanced(&wire, &bare) {
            Ok((w, added)) => {
                wire = w;
                services.extend(added.into_iter().map(|(container, assembly, load_balancer)| ServiceGroup { container, assembly, load_balancer }));
            }
            Err(e) => return Ok(Err(format!("the load balancers cannot be built: {e}"))),
        }
    }
    let mut g = Graph::new(&wire);
    detail::monitor(&mut g, Monitors { assembly_operations: mon.assembly_operations || req.detail_calls, resources: mon.resources });
    let k = 1000.0 / req.ms_per_unit;
    let mut groups = Vec::new();
    for s in &req.groups {
        let Some(cores) = g
            .refs(&s.container, "activeResourceSpecifications")
            .into_iter()
            .find(|r| g.str_attr(r, "activeResourceType").as_deref() == Some("CPU"))
            .and_then(|r| g.num_attr(&r, "numberOfReplicas"))
        else {
            return Err(format!("the scaled container {} has no CPU", g.entity(&s.container)));
        };
        let max = s.max.max(1);
        let min = s.min.clamp(1, max);
        groups.push(PodGroup {
            container: s.container.clone(),
            name: s.container.clone(),
            services: services.iter().filter(|x| x.container == s.container).map(|x| (x.assembly.clone(), x.load_balancer.clone())).collect(),
            target: s.target,
            min,
            max,
            initial: s.initial.clamp(min, max),
            startup_s: s.startup_s,
            scale_down_s: s.scale_down_s,
            cpu_request: s.cpu_request.unwrap_or(cores),
            pod_cores: cores,
            behavior: s.behavior.clone(),
        });
    }
    let profile = load_profile(&req.profile, k)?.unwrap_or_else(|| LoadProfile::uniform(vec![(0.0, 1.0)]));
    let first = |class: &str| g.all(class).into_iter().next().ok_or_else(|| format!("the model has no {class}"));
    let roots = match (first("Repository"), first("System"), first("Allocation"), first("ResourceEnvironment")) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => [a, b, c, d],
        (a, b, c, d) => return Ok(Err([a.err(), b.err(), c.err(), d.err()].into_iter().flatten().next().unwrap_or_default())),
    };
    let mut links = g.all("LinkingResource");
    links.sort();
    let dm = DynamicModel {
        pcm: g.to_wire(|_| true),
        groups,
        link: links.into_iter().next(),
        profile,
        warm_up_s: req.warm_up_s,
        end_s: req.end_s,
        ms_per_unit: req.ms_per_unit,
        roots,
        usage_streams: req.usage_streams,
        fix_double_resume: req.fix_double_resume,
        controller: req.controller.clone(),
        async_paths: req.async_paths,
        async_ignore_forks: req.async_ignore_forks.clone(),
    };
    Ok(Ok((dm, g)))
}

/// An evaluation and what the design detail reads besides it.
pub struct Outcome {
    pub eval: Evaluation,
    /// The simulated model (with the monitors) and its run (`None` if it could not run).
    pub graph: Option<Graph>,
    pub raw: Option<RawRun>,
    pub groups: Vec<PodGroup>,
}

/// The limits of a run: the request's wall-clock and event limits within the service's.
pub fn limits_of(req: &Request, service: Limits) -> Limits {
    let mut l = service;
    if req.wall_s > 0.0 {
        let d = Instant::now() + Duration::from_secs_f64(req.wall_s.min(1e7));
        l.deadline = Some(l.deadline.map_or(d, |x| x.min(d)));
    }
    if req.max_events > 0 {
        l.max_events = if l.max_events == 0 { req.max_events } else { l.max_events.min(req.max_events) };
    }
    l
}

/// Simulates `req` on the model of `cache` with the evaluation's monitors and `mon`, within
/// `service` limits and the request's. `Err` only for wrong arguments; a design that cannot be
/// simulated is an evaluation with `failed`.
pub fn run(cache: &PcmCache, req: &Request, mon: Monitors, service: Limits) -> Result<Outcome, String> {
    req.check()?;
    let limits = limits_of(req, service);
    let (dm, g) = match build(cache, req, mon)? {
        Ok(x) => x,
        Err(e) => return Ok(Outcome { eval: Evaluation::failure(req, e), graph: None, raw: None, groups: Vec::new() }),
    };
    let groups = dm.groups.clone();
    let k = 1000.0 / req.ms_per_unit;
    let edges = edges(req);
    let prepared = dynamic::prepare(Arc::new(dm), req.max_pods);
    let raw = match prepared.and_then(|p| dynamic::run_raw(&p, req.seed, limits, &edges.iter().map(|e| e * k).collect::<Vec<_>>())) {
        Ok(r) => r,
        Err(e) => return Ok(Outcome { eval: Evaluation::failure(req, e), graph: Some(g), raw: None, groups }),
    };
    let eval = reduce(cache, &g, req, &groups, &raw, edges.len());
    Ok(Outcome { eval, graph: Some(g), raw: Some(raw), groups })
}

/// [`run`] for a request in the caller's names, the evaluation in the caller's names.
pub fn run_caller(cache: &PcmCache, req: &Request, mon: Monitors, service: Limits) -> Result<Evaluation, String> {
    let c = &cache.canon;
    Ok(run(cache, &req.canonical(c), mon, service).map_err(|e| c.orig_text(&e, false))?.eval.to_caller(c))
}

/// The bin edges (s): warm-up, every bin width after it, the end.
fn edges(req: &Request) -> Vec<f64> {
    let mut v = Vec::new();
    let mut i = 0u64;
    loop {
        let t = req.warm_up_s + i as f64 * req.bin_s;
        if t >= req.end_s - 1e-9 * req.bin_s {
            break;
        }
        v.push(t);
        i += 1;
    }
    v.push(req.end_s);
    v
}

/// The nearest-rank percentile `p` (percent) of sorted `v`.
fn pct(v: &[f64], p: f64) -> f64 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    v[((p / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1]
}

/// Statistics of response-time tuples (model units) over the window, in ms.
fn response_stat(rows: &[(f64, f64)], req: &Request, nbins: usize) -> ResponseStat {
    let k = 1000.0 / req.ms_per_unit;
    let ms = req.ms_per_unit;
    let (w0, w1) = (req.warm_up_s, req.end_s);
    let mut v = Vec::new();
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); nbins];
    for &(t, x) in rows {
        let ts = t / k;
        if ts < w0 || ts > w1 {
            continue;
        }
        let x = x * ms;
        v.push(x);
        let b = (((ts - w0) / req.bin_s) as usize).min(nbins.saturating_sub(1));
        if let Some(p) = per.get_mut(b) {
            p.push(x);
        }
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let ps = &req.percentiles;
    // log-spaced histogram
    let (mut edges, mut counts) = (Vec::new(), Vec::new());
    let first_pos = v.partition_point(|x| *x <= 0.0);
    let positive = &v[first_pos..];
    if let (Some(lo), Some(hi)) = (positive.first(), positive.last()) {
        let d = req.histogram_per_decade as f64;
        let a = (lo.log10() * d).floor() as i64;
        let b = ((hi.log10() * d).floor() as i64) + 1;
        edges = (a..=b).map(|i| 10f64.powf(i as f64 / d)).collect();
        counts = vec![0u64; edges.len() - 1];
        for x in positive {
            let i = (((x.log10() * d).floor() as i64) - a).clamp(0, counts.len() as i64 - 1) as usize;
            counts[i] += 1;
        }
    }
    if first_pos > 0 {
        if edges.is_empty() {
            edges = vec![0.0, f64::MIN_POSITIVE];
            counts = vec![first_pos as u64];
        } else {
            edges.insert(0, 0.0);
            counts.insert(0, first_pos as u64);
        }
    }
    let mut bin_counts = Vec::with_capacity(nbins);
    let mut bin_mean = Vec::with_capacity(nbins);
    let mut bin_pct = Vec::with_capacity(nbins * ps.len());
    for b in per.iter_mut() {
        b.sort_by(f64::total_cmp);
        bin_counts.push(b.len() as u64);
        bin_mean.push(if b.is_empty() { 0.0 } else { b.iter().sum::<f64>() / b.len() as f64 });
        bin_pct.extend(ps.iter().map(|&p| pct(b, p)));
    }
    ResponseStat {
        count: n as u64,
        mean_ms: if n == 0 { 0.0 } else { v.iter().sum::<f64>() / n as f64 },
        max_ms: v.last().copied().unwrap_or(0.0),
        percentiles_ms: ps.iter().map(|&p| pct(&v, p)).collect(),
        histogram_edges_ms: edges,
        histogram_counts: counts,
        bin_counts,
        bin_mean_ms: bin_mean,
        bin_percentiles_ms: bin_pct,
        ..ResponseStat::default()
    }
}

/// The evaluation of a run (`nedges` bin edges requested).
fn reduce(cache: &PcmCache, g: &Graph, req: &Request, groups: &[PodGroup], raw: &RawRun, nedges: usize) -> Evaluation {
    let k = 1000.0 / req.ms_per_unit;
    let window_s = req.end_s - req.warm_up_s;
    let nbins = nedges - 1;
    let input = &cache.graph;
    let mut warnings = Vec::new();
    // ------------------------------------------------ response times
    let (mut operations, mut sessions, mut calls, mut asm_ops) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut elsc: HashMap<String, u64> = HashMap::new();
    let (w0, w1) = (req.warm_up_s * k, req.end_s * k);
    for (i, def) in raw.meas.series.iter().enumerate() {
        if !def.metric.starts_with("Response Time") {
            continue;
        }
        let mp: &str = &def.mp;
        let kind = mp.split('[').next().unwrap_or("");
        let ids = key_elements(mp);
        let rows = &raw.meas.rows[i];
        let known = |x: &str| input.has(x).then(|| x.to_string());
        match kind {
            "SystemOperationMeasuringPoint" if ids.len() >= 2 && input.has(&ids[0]) && input.has(&ids[1]) => {
                let mut s = response_stat(rows, req, nbins);
                s.kind = "systemOperation";
                s.key = format!("{}|{}", ids[0], ids[1]);
                s.role = known(&ids[0]);
                s.signature = known(&ids[1]);
                operations.push(s);
            }
            "UsageScenarioMeasuringPoint" if !ids.is_empty() && input.has(&ids[0]) => {
                let mut s = response_stat(rows, req, nbins);
                s.kind = "scenario";
                s.key = ids[0].clone();
                s.scenario = known(&ids[0]);
                sessions.push(s);
            }
            "EntryLevelSystemCallMeasuringPoint" if !ids.is_empty() && input.has(&ids[0]) => {
                elsc.insert(ids[0].clone(), rows.iter().filter(|r| r.0 >= w0 && r.0 <= w1).count() as u64);
                if req.detail_calls {
                    let mut s = response_stat(rows, req, nbins);
                    s.kind = "call";
                    s.key = ids[0].clone();
                    s.call = known(&ids[0]);
                    s.scenario = cache.scenario_of(&ids[0]);
                    s.role = input.ref1(&ids[0], "providedRole");
                    s.signature = input.ref1(&ids[0], "operationSignature");
                    calls.push(s);
                }
            }
            "AssemblyOperationMeasuringPoint" if req.detail_calls && ids.len() >= 3 && ids.iter().all(|x| input.has(x)) => {
                let mut s = response_stat(rows, req, nbins);
                s.kind = "assemblyOperation";
                s.key = format!("{}|{}|{}", ids[2], ids[0], ids[1]);
                s.role = known(&ids[0]);
                s.signature = known(&ids[1]);
                s.assembly = known(&ids[2]);
                asm_ops.push(s);
            }
            _ => {}
        }
    }
    // a scenario without completions has no series: listed with zeros
    for sc in input.all("UsageScenario") {
        if !sessions.iter().any(|s| s.key == sc) {
            let mut s = response_stat(&[], req, nbins);
            s.kind = "scenario";
            s.key = sc.clone();
            s.scenario = Some(sc);
            sessions.push(s);
        }
    }
    for v in [&mut operations, &mut sessions, &mut calls, &mut asm_ops] {
        v.sort_by(|a, b| a.key.cmp(&b.key));
    }
    // ------------------------------------------------ the readings
    let complete = raw.readings.len() == nedges && raw.failure.is_none();
    let first: Option<&Reading> = raw.readings.first();
    let last: &Reading = if complete { raw.readings.last().unwrap_or(&raw.last) } else { &raw.last };
    let hpa_at = |r: Option<&Reading>, name: &str| r.and_then(|r| r.hpa.iter().find(|h| h.name == name).cloned());
    // ------------------------------------------------ call counts
    let before: HashMap<&str, u64> = first.map(|r| r.calls.iter().map(|(a, n)| (a.as_str(), *n)).collect()).unwrap_or_default();
    let after: HashMap<&str, u64> = last.calls.iter().map(|(a, n)| (a.as_str(), *n)).collect();
    let mut call_counts = Vec::new();
    for a in input.all("ExternalCallAction") {
        let n = after.get(a.as_str()).copied().unwrap_or(0).saturating_sub(before.get(a.as_str()).copied().unwrap_or(0));
        call_counts.push((a, n));
    }
    for a in input.all("EntryLevelSystemCall") {
        let n = elsc.get(&a).copied().unwrap_or(0);
        call_counts.push((a, n));
    }
    // ------------------------------------------------ pods
    let mut ready: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
    let (mut created, mut rescales, mut reversals) = (0u64, 0u64, 0u64);
    for pg in groups {
        if let Some(e) = hpa_at(Some(last), &pg.name) {
            created += e.created;
            rescales += e.rescales;
        }
        reversals += raw.reversals(&pg.name, w0) as u64;
        ready.insert(pg.container.as_str(), raw.ready_series(&pg.name, pg.initial as i32, k));
    }
    let (ws0, ws1) = (req.warm_up_s, req.end_s);
    let peak_in = |s: &[(f64, f64)]| -> f64 {
        let at0 = s.iter().rev().find(|x| x.0 <= ws0).map_or(0.0, |x| x.1);
        s.iter().filter(|x| x.0 > ws0 && x.0 <= ws1).map(|x| x.1).fold(at0, f64::max)
    };
    // the most ready pods at once in the window, all groups
    let mut changes: Vec<(f64, usize, f64)> =
        groups.iter().enumerate().flat_map(|(i, pg)| ready[pg.container.as_str()].iter().map(move |&(t, v)| (t, i, v))).collect();
    changes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cur = vec![0.0; groups.len()];
    let mut peak_pods: f64 = 0.0;
    for (t, i, v) in changes {
        if t > ws1 {
            break;
        }
        if t > ws0 {
            peak_pods = peak_pods.max(cur.iter().sum());
        }
        cur[i] = v;
    }
    peak_pods = peak_pods.max(cur.iter().sum());
    // ------------------------------------------------ resources
    let readings: Vec<&Reading> = raw.readings.iter().collect();
    let busy_at = |r: &Reading, i: usize| r.busy.get(i).copied().flatten().map_or(0.0, |x| x.1);
    let mut resources = Vec::new();
    for c in input.all("ResourceContainer") {
        let group = groups.iter().find(|pg| pg.container == c);
        for spec in input.refs(&c, "activeResourceSpecifications") {
            let kind = g.str_attr(&spec, "activeResourceType").unwrap_or_else(|| "CPU".into());
            let delay = g.str_attr(&spec, "schedulingPolicy").as_deref() == Some("DELAY");
            let cores = g.num_attr(&spec, "numberOfReplicas").unwrap_or(1.0);
            // the run's resources of this spec: itself, or the pods' of its kind
            let mine: Vec<usize> = raw
                .resources
                .iter()
                .enumerate()
                .filter(|(_, r)| if group.is_some() { r.unit == c && r.kind == kind } else { r.spec == spec })
                .map(|(i, _)| i)
                .collect();
            let (mut bin_u, mut bin_ready, mut bin_all) = (Vec::new(), Vec::new(), Vec::new());
            let (mut busy_total, mut ready_total) = (0.0, 0.0);
            for w in readings.windows(2) {
                let (a, b) = (w[0], w[1]);
                let dt = (b.at - a.at).max(0.0);
                let busy: f64 = mine.iter().map(|&i| busy_at(b, i) - busy_at(a, i)).sum();
                // pod-time of the bin (model units): ready, and ready + starting
                let (rt, at) = match group {
                    Some(pg) => {
                        let (x, y) = (hpa_at(Some(a), &pg.name), hpa_at(Some(b), &pg.name));
                        let d = |f: fn(&simoxide_spd::hpa::HpaSummary) -> f64| y.as_ref().map_or(0.0, f) - x.as_ref().map_or(0.0, f);
                        (d(|h| h.ready_replica_seconds).max(0.0), d(|h| h.replica_seconds).max(0.0))
                    }
                    None => (dt, dt),
                };
                busy_total += busy;
                ready_total += rt;
                bin_u.push(if delay || rt <= 0.0 { 0.0 } else { busy / rt });
                bin_ready.push(if dt > 0.0 { rt / dt } else { 0.0 });
                bin_all.push(if dt > 0.0 { at / dt } else { 0.0 });
            }
            let mean_u = if delay || ready_total <= 0.0 { 0.0 } else { busy_total / ready_total };
            let (mean_ready, peak_ready, rcs, ready_rcs, re, cr, rev) = match group {
                Some(pg) => {
                    let (s, e) = (hpa_at(first, &pg.name), hpa_at(Some(last), &pg.name));
                    let d = |f: fn(&simoxide_spd::hpa::HpaSummary) -> f64| e.as_ref().map_or(0.0, f) - s.as_ref().map_or(0.0, f);
                    let rdy = d(|h| h.ready_replica_seconds) / k;
                    let all = (d(|h| h.replica_seconds) / k).max(rdy);
                    (
                        rdy / window_s,
                        peak_in(&ready[pg.container.as_str()]),
                        all * cores,
                        rdy * cores,
                        e.as_ref().map_or(0, |h| h.rescales),
                        e.as_ref().map_or(0, |h| h.created),
                        raw.reversals(&pg.name, w0) as u64,
                    )
                }
                None => (1.0, 1.0, cores * window_s, cores * window_s, 0, 0, 0),
            };
            resources.push(ResourceStat {
                container: c.clone(),
                resource: spec.clone(),
                kind,
                autoscaled: group.is_some(),
                cores,
                mean_utilisation: mean_u,
                max_bin_utilisation: bin_u.iter().copied().fold(0.0, f64::max),
                bin_utilisation: bin_u,
                mean_ready_replicas: mean_ready,
                peak_ready_replicas: peak_ready,
                bin_ready_replicas: bin_ready,
                bin_replicas: bin_all,
                replica_core_seconds: rcs,
                ready_replica_core_seconds: ready_rcs,
                rescales: re,
                created_pods: cr,
                reversals: rev,
            });
        }
    }
    // ------------------------------------------------ synchronous paths
    let mut sets: BTreeMap<(String, Vec<String>), i64> = BTreeMap::new();
    // a set of the model's containers (pods counted as their container once), in canonical order
    let of_model = |set: &[String]| -> Vec<String> {
        let mut v: Vec<String> = set.iter().filter(|c| input.class(c) == "ResourceContainer").cloned().collect();
        v.sort_by_key(|c| c.strip_prefix("_c").and_then(|d| d.parse::<u64>().ok()).unwrap_or(u64::MAX));
        v.dedup();
        v
    };
    for (sc, set, n) in &last.sessions {
        *sets.entry((sc.clone(), of_model(set))).or_default() += *n as i64;
    }
    if let Some(f) = first {
        for (sc, set, n) in &f.sessions {
            *sets.entry((sc.clone(), of_model(set))).or_default() -= *n as i64;
        }
    }
    let mut by_scenario: BTreeMap<String, Vec<PathSet>> = BTreeMap::new();
    for ((sc, containers), n) in sets {
        if n > 0 {
            by_scenario.entry(sc.clone()).or_default().push(PathSet { scenario: sc, containers, count: n as u64 });
        }
    }
    let mut path_sets = Vec::new();
    let mut truncated = false;
    for (_, mut v) in by_scenario {
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.containers.cmp(&b.containers)));
        if v.len() > req.max_path_sets {
            truncated = true;
            v.truncate(req.max_path_sets);
        }
        path_sets.extend(v);
    }
    if truncated {
        warnings.push(format!("more than {} distinct path sets in a scenario: the most frequent are kept", req.max_path_sets));
    }
    // ------------------------------------------------ asynchronous work (revision 4)
    let mut async_path_sets = Vec::new();
    let mut async_truncated = false;
    if let Some(units) = &raw.async_units {
        type K = (Option<String>, String, String, String, Vec<String>, Vec<String>, Vec<String>);
        let mut by: BTreeMap<(bool, Option<String>, String, String), Vec<AsyncPathSet>> = BTreeMap::new();
        let mut merged: BTreeMap<K, u64> = BTreeMap::new();
        for u in units {
            let k = (u.scenario.clone(), u.assembly.clone(), u.seff.clone(), u.signature.clone(), of_model(&u.context), of_model(&u.local_context), of_model(&u.containers));
            *merged.entry(k).or_default() += u.count;
        }
        for ((scenario, assembly, seff, signature, context, local_context, containers), count) in merged {
            by.entry((scenario.is_none(), scenario.clone(), assembly.clone(), seff.clone()))
                .or_default()
                .push(AsyncPathSet { scenario, assembly, seff, signature, context, local_context, containers, count });
        }
        for (_, mut v) in by {
            v.sort_by(|a, b| {
                b.count
                    .cmp(&a.count)
                    .then_with(|| a.context.cmp(&b.context))
                    .then_with(|| a.local_context.cmp(&b.local_context))
                    .then_with(|| a.containers.cmp(&b.containers))
            });
            if v.len() > req.max_path_sets {
                async_truncated = true;
                v.truncate(req.max_path_sets);
            }
            async_path_sets.extend(v);
        }
        if async_truncated {
            warnings.push(format!("more than {} distinct asynchronous path sets of a trigger: the most frequent are kept", req.max_path_sets));
        }
    }
    let mut eval = Evaluation {
        failed: raw.failure.is_some(),
        capped: raw.capped,
        timed_out: raw.timed_out,
        message: raw.failure.clone().unwrap_or_default(),
        window_s,
        bin_s: req.bin_s,
        bins: nbins,
        percentiles: req.percentiles.clone(),
        simulated_s: raw.end / k,
        events: raw.events,
        wall_ms: raw.wall_ms,
        operations,
        sessions,
        calls,
        assembly_operations: asm_ops,
        resources,
        call_counts,
        path_sets,
        path_sets_truncated: truncated,
        async_requested: req.async_paths,
        async_paths_measured: raw.async_units.is_some(),
        async_path_sets,
        async_path_sets_truncated: async_truncated,
        created_pods: created,
        rescales,
        peak_pods,
        reversals,
        warnings,
    };
    if !complete && raw.failure.is_none() {
        eval.failed = true;
        eval.message = format!("the run ended at {:.1} s, before the end of the window", raw.end / k);
    }
    eval
}

// ---------------------------------------------------------------------------------------------
// The process's evaluator: bounded concurrency, evaluations cached by content

type Cell = Arc<tokio::sync::OnceCell<Arc<Evaluation>>>;
type Key = ([u8; 32], String);

/// The cached evaluations: at most `SIM_RESULT_CACHE_SIZE` entries and `PCM_CACHE_MB` megabytes
/// (estimated), least recently used evicted first.
struct Cache {
    lru: lru::LruCache<Key, (Cell, usize)>,
    bytes: usize,
    max_bytes: usize,
}

impl Cache {
    fn cell(&mut self, k: &Key) -> Cell {
        self.lru.get_or_insert(k.clone(), || (Arc::new(tokio::sync::OnceCell::new()), 0)).0.clone()
    }
    /// Accounts the size of a finished entry and evicts beyond the bound.
    fn sized(&mut self, k: &Key, bytes: usize) {
        if let Some(e) = self.lru.peek_mut(k) {
            self.bytes += bytes.saturating_sub(e.1);
            e.1 = bytes;
        }
        while self.bytes > self.max_bytes && self.lru.len() > 1 {
            match self.lru.pop_lru() {
                Some((_, (_, b))) => self.bytes = self.bytes.saturating_sub(b),
                None => break,
            }
        }
    }
    fn remove(&mut self, k: &Key, cell: &Cell) {
        if self.lru.peek(k).is_some_and(|x| Arc::ptr_eq(&x.0, cell)) {
            if let Some((_, b)) = self.lru.pop(k) {
                self.bytes = self.bytes.saturating_sub(b);
            }
        }
    }
}

impl Evaluation {
    /// An estimate of the memory the evaluation holds (bytes).
    pub fn size(&self) -> usize {
        let rs = |v: &[ResponseStat]| -> usize {
            v.iter()
                .map(|s| {
                    200 + s.key.len()
                        + 8 * (s.percentiles_ms.len() + s.histogram_edges_ms.len() + s.histogram_counts.len() + s.bin_counts.len() + s.bin_mean_ms.len() + s.bin_percentiles_ms.len())
                })
                .sum()
        };
        let res: usize = self.resources.iter().map(|r| 200 + 8 * (r.bin_utilisation.len() + r.bin_ready_replicas.len() + r.bin_replicas.len())).sum();
        let paths: usize = self.path_sets.iter().map(|p| 64 + p.containers.iter().map(|c| 24 + c.len()).sum::<usize>()).sum();
        let calls: usize = self.call_counts.iter().map(|(a, _)| 40 + a.len()).sum();
        512 + rs(&self.operations) + rs(&self.sessions) + rs(&self.calls) + rs(&self.assembly_operations) + res + paths + calls + self.message.len()
    }
}

pub struct Evaluator {
    cells: Mutex<Cache>,
    pub permits: tokio::sync::Semaphore,
    timeout_s: u64,
    max_events: u64,
}

/// The process's evaluator, sized like the engine (`SIM_THREADS`: simulations at once,
/// `SIM_RESULT_CACHE_SIZE`: evaluations kept, `SIM_TIMEOUT_SECONDS`, `SIM_MAX_EVENTS`).
pub fn evaluator(config: &crate::config::Config) -> &'static Evaluator {
    static E: OnceLock<Evaluator> = OnceLock::new();
    let mb: usize = std::env::var("PCM_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    E.get_or_init(|| Evaluator {
        cells: Mutex::new(Cache {
            lru: lru::LruCache::new(std::num::NonZeroUsize::new(config.result_cache_size.max(1)).unwrap()),
            bytes: 0,
            max_bytes: mb.max(1) << 20,
        }),
        permits: tokio::sync::Semaphore::new(config.threads().max(1)),
        timeout_s: config.sim_timeout_seconds,
        max_events: config.sim_max_events,
    })
}

impl Evaluator {
    /// The service's limits (`SIM_TIMEOUT_SECONDS`, `SIM_MAX_EVENTS`).
    pub fn limits(&self) -> Limits {
        Limits {
            max_events: self.max_events,
            deadline: (self.timeout_s > 0).then(|| Instant::now() + Duration::from_secs(self.timeout_s)),
            ..Limits::default()
        }
    }

    /// The evaluation of `req` on `cache`'s model: from the model's cache, the process's, or
    /// simulated (at most `SIM_THREADS` at once).
    pub async fn evaluate(&'static self, cache: &Arc<PcmCache>, req: Request) -> Result<Arc<Evaluation>, String> {
        req.check()?;
        let key = req.key();
        if let Some(e) = cache.evals.lock().unwrap().get(&key) {
            return Ok(e.clone());
        }
        let c = cache.clone();
        let digest = tokio::task::spawn_blocking(move || c.digest()).await.map_err(|e| e.to_string())?;
        let ck = (digest, key.clone());
        let cell = self.cells.lock().unwrap().cell(&ck);
        let r = cell
            .get_or_try_init(|| async {
                let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
                // the limits start when the simulation does
                let (c, rq, svc) = (cache.clone(), req.clone(), self.limits());
                tokio::task::spawn_blocking(move || {
                    let fallback = rq.clone();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&c, &rq, Monitors::default(), svc).map(|o| o.eval)))
                        .unwrap_or_else(|_| Ok(Evaluation::failure(&fallback, "the simulation panicked".into())))
                })
                .await
                .map_err(|e| format!("the simulation was abandoned: {e}"))?
                .map(Arc::new)
            })
            .await
            .cloned();
        match &r {
            // a timed-out run depends on the machine: not kept
            Ok(e) if e.timed_out => self.cells.lock().unwrap().remove(&ck, &cell),
            Ok(e) => {
                self.cells.lock().unwrap().sized(&ck, e.size());
                let mut evals = cache.evals.lock().unwrap();
                if evals.len() >= 16 {
                    evals.clear();
                }
                evals.insert(key, e.clone());
            }
            Err(_) => {
                if !cell.initialized() {
                    self.cells.lock().unwrap().remove(&ck, &cell);
                }
            }
        }
        r
    }
}
