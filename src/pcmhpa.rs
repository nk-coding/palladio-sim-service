//! Dynamic runs of plain PCM with Kubernetes-style autoscaling, through SPD (Palladio's scaling
//! policy definitions) and SimOxide's HPA: the generic counterpart of `archHpaSimulate`
//! (`crate::dynamic`), for models whose layer 2 is realised in the model (domain project,
//! DESIGN.md §B5/§H).
//!
//! Per autoscaled container the model holds a pod (the container, its CPU with the cores of one
//! pod). The script says which containers scale and how (`PcmScalingGroup`: min, max, target
//! CPU utilisation, initial pods, start-up delay, scale-down window, CPU request). The load
//! balancers that SPD's service groups need (Slingshot's pattern: a component per assembly whose
//! SEFFs branch to the pods, the callers' connectors pointing at it) are either in the model,
//! named by `PcmServiceGroup`s (assembly, load balancer), or, for a group without any, added by
//! the plugin for the run as rule 5 adds them (`translate::load_balanced`): they are SPD's
//! realisation of a service group, Palladio, not a layer above it. The plugin writes the SPD
//! (an elastic infrastructure per group, a service group per pair), the semantic configuration and
//! the HPA configuration, and runs them with a per-scenario load profile. Nothing above PCM: cost
//! (price × replica-seconds) and the objectives are the script's.

use crate::detail::{self, DetailRequest};
use crate::dynamic::{self, HpaParams};
use crate::graph::Graph;
use crate::pcm::PcmCache;
use crate::pcmrun::LoadStep;
use crate::translate::{DynamicModel, LoadProfileSpec, PodGroup};
use palladio_sim_service::protocol::WireModel;
use serde_json::{Value, json};
use simoxide_sim::Limits;
use std::collections::BTreeMap;
use std::sync::Arc;

/// One autoscaled container (an SPD elastic infrastructure with a HorizontalPodAutoscaler).
#[derive(Clone, Debug, PartialEq)]
pub struct ScalingGroup {
    /// The pod's ResourceContainer.
    pub container: String,
    pub min: u32,
    pub max: u32,
    /// Target CPU utilisation, 0..1.
    pub target: f64,
    /// Pods at the start (clamped to min..max).
    pub initial: u32,
    /// Start-up delay of a new pod (s).
    pub startup_s: f64,
    /// Scale-down stabilisation window (s); `None`: Kubernetes' 300 s.
    pub scale_down_s: Option<f64>,
    /// The HPA's CPU request per pod, in the pod's cores; `None`: all its cores.
    pub cpu_request: Option<f64>,
}

/// An assembly on a scaled container and its load balancer's assembly (an SPD service group).
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceGroup {
    pub container: String,
    pub assembly: String,
    pub load_balancer: String,
}

/// A dynamic run: the groups, and the run's settings as a detail run's (profile and times in
/// seconds, budgets per series, usage streams, seed).
#[derive(Clone, Debug)]
pub struct HpaRequest {
    pub groups: Vec<ScalingGroup>,
    pub services: Vec<ServiceGroup>,
    pub run: DetailRequest,
    pub max_pods: u32,
}

/// One group's part of a dynamic run.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupResult {
    pub container: String,
    /// Time-weighted ready pods over the window.
    pub mean_replicas: f64,
    pub ready_replica_seconds: f64,
    pub replica_seconds: f64,
    pub max_replicas: i32,
    pub rescales: u64,
    pub created: u64,
    pub reversals: u32,
}

/// The generic outcome of a dynamic run (window `[warmUpS, durationS]`).
#[derive(Clone, Debug, PartialEq)]
pub struct HpaResult {
    pub ready_replica_seconds: f64,
    pub replica_seconds: f64,
    /// The most ready pods at once, all groups.
    pub peak_replicas: f64,
    pub rescales: u64,
    pub created: u64,
    /// Rescale direction reversals per hour of the window, all groups.
    pub flapping: f64,
    /// Share of the requests of the budgeted series over `sloFactor` × budget, and their count.
    pub slo: f64,
    pub requests: u64,
    /// Mean and p95 response time of the entry-level calls (ms).
    pub mean_ms: f64,
    pub p95_ms: f64,
    /// The run stopped at the pod cap or a limit (the numbers cover the part that ran).
    pub capped: bool,
    pub events: u64,
    pub wall_ms: f64,
    pub groups: Vec<GroupResult>,
}

fn first(g: &Graph, class: &str) -> Result<String, String> {
    g.all(class).into_iter().next().ok_or_else(|| format!("the model has no {class}"))
}

/// The dynamic model of `model`'s PCM with full monitoring (as a detail run's).
pub fn dynamic_model(cache: &PcmCache, req: &HpaRequest) -> Result<(DynamicModel, Graph), String> {
    let mut wire = cache.with_overrides(&req.run.overrides);
    // groups without service groups: the plugin puts the load balancers in (rule 5's pattern)
    let bare: Vec<String> = req.groups.iter().filter(|g| !req.services.iter().any(|s| s.container == g.container)).map(|g| g.container.clone()).collect();
    let mut services = req.services.clone();
    if !bare.is_empty() {
        let (w, added) = crate::translate::load_balanced(&wire, &bare)?;
        wire = w;
        services.extend(added.into_iter().map(|(container, assembly, load_balancer)| ServiceGroup { container, assembly, load_balancer }));
    }
    let mut g = Graph::new(&wire);
    detail::monitor_everything(&mut g, req.run.assembly_operations);
    let k = 1000.0 / req.run.time_unit_ms;
    let mut groups = Vec::new();
    for s in &req.groups {
        if g.class(&s.container) != "ResourceContainer" {
            return Err(format!("{} is not a ResourceContainer", s.container));
        }
        let cores = g
            .refs(&s.container, "activeResourceSpecifications")
            .into_iter()
            .find(|r| g.str_attr(r, "activeResourceType").as_deref() == Some("CPU"))
            .and_then(|r| g.num_attr(&r, "numberOfReplicas"))
            .ok_or_else(|| format!("{} has no CPU", g.entity(&s.container)))?;
        let max = s.max.max(1);
        let min = s.min.clamp(1, max);
        let services: Vec<(String, String)> =
            services.iter().filter(|x| x.container == s.container).map(|x| (x.assembly.clone(), x.load_balancer.clone())).collect();
        for (a, lb) in &services {
            for x in [a, lb] {
                if g.class(x) != "AssemblyContext" {
                    return Err(format!("{x} is not an AssemblyContext"));
                }
            }
        }
        groups.push(PodGroup {
            container: s.container.clone(),
            name: s.container.clone(),
            services,
            target: s.target,
            min,
            max,
            initial: s.initial.clamp(min, max),
            startup_s: s.startup_s,
            scale_down_s: s.scale_down_s,
            cpu_request: s.cpu_request.unwrap_or(cores),
            pod_cores: cores,
            price_per_replica: 0.0,
            offered: (0.0, 0.0),
            cpu_per_replica: cores,
            sidecar_cores: 1.0,
            max_ideal: None,
        });
    }
    let profile = crate::pcmrun::load_profile(
        &req.run.profile.iter().map(|s| LoadStep { scenario: s.scenario.clone(), time: s.time * k, factor: s.factor }).collect::<Vec<_>>(),
    )?
    .unwrap_or_else(|| simoxide_sim::LoadProfile::uniform(vec![(0.0, 1.0)]));
    let mut links = g.all("LinkingResource");
    links.sort();
    let dm = DynamicModel {
        pcm: g.to_wire(|_| true),
        groups,
        link: links.into_iter().next(),
        // no uniform steps: the per-scenario profile drives the load
        profile: Some(LoadProfileSpec { steps: Vec::new(), warm_up: req.run.warm_up_s, end: req.run.duration_s, slo_factor: req.run.slo_factor }),
        fixed_cost: 0.0,
        ms_per_unit: req.run.time_unit_ms,
        budgets: BTreeMap::new(),
        roots: [first(&g, "Repository")?, first(&g, "System")?, first(&g, "Allocation")?, first(&g, "ResourceEnvironment")?],
        scenario_profile: Some(profile),
        usage_streams: req.run.usage_streams,
    };
    Ok((dm, g))
}

/// [`run`] on a model (its PCM part).
pub fn run_model(model: &WireModel, req: &HpaRequest, limits: Limits) -> Result<(HpaResult, Value), String> {
    run(&PcmCache::new(model), req, limits)
}

/// Runs a dynamic model of plain PCM: the generic outcome and the detail (as
/// [`detail::detail`]'s, with each scaled container's ready pods over time).
pub fn run(cache: &PcmCache, req: &HpaRequest, limits: Limits) -> Result<(HpaResult, Value), String> {
    let (dm, g) = dynamic_model(cache, req)?;
    let p = dynamic::prepare(Arc::new(dm), req.max_pods)?;
    let raw = dynamic::run_raw(&p, &HpaParams { seed: req.run.seed, exclude: Vec::new() }, limits)?;
    let dm = &p.model;
    let k = 1000.0 / req.run.time_unit_ms;
    let (w0, w1) = (req.run.warm_up_s, req.run.duration_s);
    let window = (w1 - w0).max(f64::MIN_POSITIVE);
    let mut detail = detail::reduce(&g, cache, &req.run, &raw.meas, raw.end);
    // groups
    let mut groups = Vec::new();
    let (mut ready_rs, mut all_rs, mut rescales, mut created, mut reversals) = (0.0, 0.0, 0, 0, 0);
    let bins: Vec<f64> = detail["t"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let mut series_all = Vec::new();
    for pg in &dm.groups {
        let end = raw.at_end.iter().find(|s| s.name == pg.name);
        let start = raw.at_warm.iter().find(|s| s.name == pg.name);
        let (ready, total) = match (end, start) {
            (Some(e), Some(s)) => (e.ready_replica_seconds - s.ready_replica_seconds, e.replica_seconds - s.replica_seconds),
            (Some(e), None) => (e.ready_replica_seconds, e.replica_seconds),
            _ => (0.0, 0.0),
        };
        let (re, cr, mx) = end.map_or((0, 0, 0), |e| (e.rescales, e.created, e.max_replicas));
        let rev = raw.reversals(&pg.name, w0 * k);
        let series = raw.ready_series(&pg.name, pg.initial as i32, k);
        // ready pods per bin (time-weighted), into the container's timeline
        let per_bin: Vec<f64> = bins.iter().map(|&b| detail::step_mean(&series, b, (b + req.run.bin_s).min(w1))).collect();
        if let Some(cs) = detail["containers"].as_array_mut() {
            for c in cs.iter_mut().filter(|c| c["container"] == pg.container.as_str()) {
                c["replicas"] = json!(per_bin);
                c["autoscaled"] = json!(true);
            }
        }
        series_all.push(series);
        ready_rs += ready / k;
        all_rs += total / k;
        rescales += re;
        created += cr;
        reversals += rev;
        groups.push(GroupResult {
            container: pg.container.clone(),
            mean_replicas: ready / (window * k),
            ready_replica_seconds: ready / k,
            replica_seconds: total / k,
            max_replicas: mx,
            rescales: re,
            created: cr,
            reversals: rev,
        });
    }
    // peak: the most ready pods at once
    let mut events: Vec<(f64, usize, f64)> = series_all.iter().enumerate().flat_map(|(i, s)| s.iter().map(move |&(t, v)| (t, i, v))).collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cur = vec![0.0; series_all.len()];
    let mut peak: f64 = 0.0;
    for (_, i, v) in events {
        cur[i] = v;
        peak = peak.max(cur.iter().sum());
    }
    // responses: the calls
    let mut rts: Vec<f64> = Vec::new();
    for (i, def) in raw.meas.series.iter().enumerate() {
        if def.mp.starts_with("EntryLevelSystemCallMeasuringPoint[") && def.metric.starts_with("Response Time") {
            rts.extend(raw.meas.rows[i].iter().filter(|r| r.0 / k >= w0 && r.0 / k <= w1).map(|r| r.1 * req.run.time_unit_ms));
        }
    }
    rts.sort_by(f64::total_cmp);
    let n = rts.len();
    let slo = &detail["slo"]["total"];
    let result = HpaResult {
        ready_replica_seconds: ready_rs,
        replica_seconds: all_rs,
        peak_replicas: peak,
        rescales,
        created,
        flapping: reversals as f64 / (window / 3600.0),
        slo: slo["share"].as_f64().unwrap_or(0.0),
        requests: slo["requests"].as_u64().unwrap_or(0),
        mean_ms: if n == 0 { 0.0 } else { rts.iter().sum::<f64>() / n as f64 },
        p95_ms: if n == 0 { 0.0 } else { rts[((0.95 * n as f64).ceil() as usize).clamp(1, n) - 1] },
        capped: raw.capped,
        events: raw.events,
        wall_ms: raw.t0.elapsed().as_secs_f64() * 1e3,
        groups,
    };
    detail["simulator"] = json!({
        "name": "SimOxide", "mode": "fast", "seed": req.run.seed, "usageStreams": req.run.usage_streams,
        "durationS": req.run.duration_s, "warmUpS": req.run.warm_up_s, "simulatedS": raw.end / k,
        "events": raw.events, "autoscaling": "SPD + HPA", "capped": raw.capped, "computeMs": result.wall_ms,
    });
    Ok((result, detail))
}
