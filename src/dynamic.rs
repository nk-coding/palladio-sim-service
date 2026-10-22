//! The simulation of an evaluation: a [`DynamicModel`] (plain PCM, with pod groups behind load
//! balancers when it autoscales; made by `crate::evaluate`) simulated over its load profile,
//! with SimOxide's HPA (`simoxide_spd::hpa`; SimOxide `DESIGN-SPD.md` §5.10, `docs/spec/hpa.md`)
//! when it has pod groups. SimOxide's path monitoring (synchronous paths per session, call
//! counts; MEAS-13) and busy-time hooks are on; the run pauses at the bin edges to read them.
//!
//! - **Structure.** One SPD elastic infrastructure per pod group (unit: its pod container), a
//!   service group per assembly on the pod with its load balancer, one `HorizontalPodAutoscaler`
//!   per group: CPU utilisation target `round(100 · target)`, `minReplicas`, `maxReplicas`,
//!   start-up delay; rescales are Slingshot's absolute adjustments (`MainTransformation`), run by
//!   `QvtoAdjuster::spd`. Each group starts with its initial pods (an enforced absolute
//!   adjustment at t = 0).
//! - **Load.** The model's per-scenario load profile and, optionally, usage streams.
//! - **Bounds.** A run that creates more than `max_pods` pods stops (SimOxide's
//!   `maxCreatedPods`), and so does one past the engine's limits (`SIM_TIMEOUT_SECONDS`,
//!   `SIM_MAX_EVENTS`); it is reported as capped, so an evaluation never grows without bound.

use palladio_sim_service::protocol::WireModel;
use palladio_sim_service::{schema, xmi};
use simoxide_reconf::{QvtoAdjuster, QvtoCore, SpdAdjust};
use simoxide_sim::{CompiledModel, Fast, Limits, LoadProfile, Mode, Outputs, Pause, RunSpec, Simulation};
use simoxide_spd::Quirks;
use simoxide_spd::hpa::{self, Hpa, HpaConfig};
use simoxide_spd::log::{Log, Record};
use simoxide_spd::sim::{SpdRun, spd_of};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// The most pods one evaluation may create, all groups together (beyond: capped).
pub const DEFAULT_MAX_PODS: u32 = 400;

/// Default start-up delay of a pod (s): container start and readiness of a JVM service.
pub const DEFAULT_STARTUP_S: f64 = 30.0;

/// The model of a dynamic run: plain PCM in which every autoscaled container is **one pod**
/// (one replica's CPU) behind load balancers (a component per assembly on the pod whose SEFFs
/// branch to the pods, on a container without work; the callers' connectors point at it), the
/// structure SimOxide's HPA scales (SPD elastic infrastructure, Slingshot's adjustments; SimOxide
/// `DESIGN-SPD.md` §5.10). The SPD's target groups and the semantic configuration are written by
/// [`prepare`].
#[derive(Clone, Debug)]
pub struct DynamicModel {
    pub pcm: WireModel,
    pub groups: Vec<PodGroup>,
    /// The linking resource the pods join (the first of the resource environment), if any.
    pub link: Option<String>,
    /// The load over time per usage scenario (model time units).
    pub profile: LoadProfile,
    /// The window the statistics cover (s): from `warm_up_s` to the end of the run, `end_s`.
    pub warm_up_s: f64,
    pub end_s: f64,
    /// Milliseconds per model time unit.
    pub ms_per_unit: f64,
    /// Instance names of the repository, system, allocation and resource environment.
    pub roots: [String; 4],
    /// SimOxide's usage streams (common random numbers).
    pub usage_streams: bool,
    /// SimOxide's fix of SimuLizar's double resume at a fork join (REF-2).
    pub fix_double_resume: bool,
    /// The HPA controller's settings.
    pub controller: Controller,
}

/// Settings of the HPA controller (Kubernetes' `kube-controller-manager` flags), in s.
#[derive(Clone, Debug, PartialEq)]
pub struct Controller {
    pub sync_s: f64,
    pub tolerance: f64,
    pub downscale_stabilisation_s: f64,
    pub cpu_initialisation_s: f64,
}

impl Default for Controller {
    fn default() -> Self {
        Controller { sync_s: 15.0, tolerance: 0.1, downscale_stabilisation_s: 300.0, cpu_initialisation_s: 300.0 }
    }
}

/// One autoscaled container of a [`DynamicModel`].
#[derive(Clone, Debug)]
pub struct PodGroup {
    /// The pod's container (the target group's unit) and the target group's and autoscaler's
    /// name.
    pub container: String,
    pub name: String,
    /// (assembly on the pod, its load balancer's assembly)
    pub services: Vec<(String, String)>,
    pub target: f64,
    pub min: u32,
    pub max: u32,
    /// Pods at the start.
    pub initial: u32,
    /// Start-up delay of a new pod (s) and the scale-down stabilisation window (s, absent:
    /// Kubernetes' default).
    pub startup_s: f64,
    pub scale_down_s: Option<f64>,
    /// The HPA's request per pod, in the pod's cores.
    pub cpu_request: f64,
    pub pod_cores: f64,
    /// A Kubernetes `spec.behavior` object, passed as is.
    pub behavior: Option<serde_json::Value>,
}

/// A dynamic model ready to run: its files.
pub struct Prepared {
    pub model: Arc<DynamicModel>,
    pub files: Vec<(String, String)>,
}

/// The files of a dynamic run: the PCM files of the model, its SPD and semantic configuration,
/// and the HPA configuration (passed in memory).
pub fn prepare(dm: Arc<DynamicModel>, max_pods: u32) -> Result<Prepared, String> {
    let pcm = xmi::to_xmi(schema::schema(), &dm.pcm)?;
    let mut files = pcm.files;
    if dm.groups.is_empty() {
        return Ok(Prepared { model: dm, files });
    }
    let (spd, sem) = spd_files(&dm, &files)?;
    files.push(("dynamic.spd".into(), spd));
    files.push(("dynamic.semanticspd".into(), sem));
    files.push(("hpa.json".into(), hpa_json(&dm, max_pods)));
    Ok(Prepared { model: dm, files })
}

// ---------------------------------------------------------------------------------------------
// Configuration files

fn file_with(files: &[(String, String)], ext: &str) -> Result<String, String> {
    files
        .iter()
        .find(|(n, _)| n.ends_with(ext))
        .map(|(n, _)| n.clone())
        .ok_or_else(|| format!("the translated model has no {ext} file"))
}

fn tg(g: &PodGroup) -> String {
    format!("tg_{}", g.container)
}

fn init_policy(g: &PodGroup) -> String {
    format!("init_{}", g.container)
}

/// The SPD (a target group per pod container, an inactive initial absolute adjustment each) and
/// the semantic configuration (the elastic infrastructures and service groups).
fn spd_files(dm: &DynamicModel, files: &[(String, String)]) -> Result<(String, String), String> {
    let resenv = file_with(files, ".resourceenvironment")?;
    let system = file_with(files, ".system")?;
    let allocation = file_with(files, ".allocation")?;
    let repository = file_with(files, ".repository")?;
    let [repo_id, system_id, alloc_id, _] = &dm.roots;
    let xmi = r#"xmi:version="2.0" xmlns:xmi="http://www.omg.org/XMI" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;
    let mut spd = String::new();
    let mut sem = String::new();
    for g in &dm.groups {
        let c = &g.container;
        spd += &format!(
            "  <scalingPolicies id=\"{}\" entityName=\"initial pods of {}\" active=\"false\" targetGroup=\"{}\">\n    <adjustmentType xsi:type=\"adjustments:AbsoluteAdjustment\" goalValue=\"{}\"/>\n  </scalingPolicies>\n",
            init_policy(g),
            g.name,
            tg(g),
            g.initial
        );
        spd += &format!(
            "  <targetGroups xsi:type=\"targets:ElasticInfrastructure\" id=\"{}\" entityName=\"{}\">\n    <unit href=\"{resenv}#{c}\"/>\n  </targetGroups>\n",
            tg(g),
            g.name
        );
        let link = match &dm.link {
            Some(l) => format!("    <linkingResource href=\"{resenv}#{l}\"/>\n"),
            None => String::new(),
        };
        sem += &format!(
            "  <targetCfgs xsi:type=\"semanticspd:ElasticInfrastructureCfg\">\n    <unit href=\"{resenv}#{c}\"/>\n    <elements href=\"{resenv}#{c}\"/>\n{link}    <resourceEnvironment href=\"{resenv}#/\"/>\n  </targetCfgs>\n"
        );
        for (asm, lb) in &g.services {
            sem += &format!(
                "  <targetCfgs xsi:type=\"semanticspd:ServiceGroupCfg\">\n    <unit href=\"{system}#{asm}\"/>\n    <elements href=\"{system}#{asm}\"/>\n    <loadBalancingAssembly href=\"{system}#{lb}\"/>\n  </targetCfgs>\n"
            );
        }
    }
    let spd = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<spd:SPD {xmi} xmlns:adjustments=\"http://palladiosimulator.org/ScalingPolicyDefinition/Adjustments/1.0\" xmlns:spd=\"http://palladiosimulator.org/ScalingPolicyDefinition/1.0\" xmlns:targets=\"http://palladiosimulator.org/ScalingPolicyDefinition/Targets/1.0\" id=\"_dynamic_spd\" entityName=\"autoscaling\">\n{spd}</spd:SPD>\n"
    );
    let sem = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<semanticspd:Configuration {xmi} xmlns:semanticspd=\"http://palladiosimulator.org/ScalingPolicyDefinitionSemantic/1.0\">\n  <spd href=\"dynamic.spd#_dynamic_spd\"/>\n{sem}  <allocation href=\"{allocation}#{alloc_id}\"/>\n  <system href=\"{system}#{system_id}\"/>\n  <resourceEnvironment href=\"{resenv}#/\"/>\n  <repository href=\"{repository}#{repo_id}\"/>\n</semanticspd:Configuration>\n"
    );
    Ok((spd, sem))
}

/// Model time units per second.
fn units_per_s(dm: &DynamicModel) -> f64 {
    1000.0 / dm.ms_per_unit
}

/// One `HorizontalPodAutoscaler` per group (Kubernetes' defaults where the model sets nothing:
/// sync 15 s, tolerance 0.1, the legacy rate limits and 5 min stabilisation).
fn hpa_json(dm: &DynamicModel, max_pods: u32) -> String {
    let k = units_per_s(dm);
    let items: Vec<serde_json::Value> = dm
        .groups
        .iter()
        .map(|g| {
            let mut spec = serde_json::json!({
                "scaleTargetRef": {"kind": "ElasticInfrastructure", "name": g.name},
                "minReplicas": g.min,
                "maxReplicas": g.max,
                "metrics": [{"type": "Resource", "resource": {"name": "cpu",
                    "target": {"type": "Utilization", "averageUtilization": (100.0 * g.target).round() as i64}}}]
            });
            if let Some(b) = &g.behavior {
                spec["behavior"] = b.clone();
            }
            if let Some(w) = g.scale_down_s
                && spec["behavior"].get("scaleDown").is_none()
            {
                if !spec["behavior"].is_object() {
                    spec["behavior"] = serde_json::json!({});
                }
                spec["behavior"]["scaleDown"] = serde_json::json!({"stabilizationWindowSeconds": (w * k).round() as i64});
            }
            let mut sx = serde_json::json!({"startupDelay": g.startup_s * k});
            if (g.cpu_request - g.pod_cores).abs() > 1e-9 {
                sx["cpuRequest"] = serde_json::json!(g.cpu_request);
            }
            serde_json::json!({
                "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
                "metadata": {"name": g.name},
                "spec": spec,
                "simoxide": sx,
            })
        })
        .collect();
    serde_json::json!({
        "controller": {
            "syncPeriod": dm.controller.sync_s * k,
            "tolerance": dm.controller.tolerance,
            "downscaleStabilization": dm.controller.downscale_stabilisation_s * k,
            "cpuInitializationPeriod": dm.controller.cpu_initialisation_s * k,
            "maxCreatedPods": max_pods,
        },
        "items": items,
    })
    .to_string()
}

// ---------------------------------------------------------------------------------------------
// The run

/// The HPA records of a run's decision log (the rest is dropped).
#[derive(Default)]
struct HpaLog {
    /// (time, autoscaler, ready, current, desired)
    syncs: Vec<(f64, Box<str>, i32, i32, i32)>,
    /// (time, autoscaler, ready)
    ready: Vec<(f64, Box<str>, i32)>,
}

impl Log for HpaLog {
    fn enabled(&self) -> bool {
        true
    }
    fn all(&self) -> bool {
        false
    }
    fn record(&mut self, r: Record) {
        match r {
            Record::Hpa(h) => self.syncs.push((h.t, h.hpa, h.ready, h.current, h.desired)),
            Record::HpaReady { t, hpa, ready, .. } => self.ready.push((t, hpa, ready)),
            _ => {}
        }
    }
}

fn core() -> Result<&'static QvtoCore, String> {
    static CORE: OnceLock<Result<QvtoCore, String>> = OnceLock::new();
    CORE.get_or_init(|| QvtoCore::new().map_err(|e| format!("QVTo set-up: {e}"))).as_ref().map_err(Clone::clone)
}

/// A processing resource of the run (of the model, or of a pod created by a scale-out).
#[derive(Clone, Debug, PartialEq)]
pub struct ResInfo {
    /// Its container's id, and the scaled container's when the container is a pod of one.
    pub container: String,
    pub unit: String,
    /// The processing resource specification's id and its resource type's name.
    pub spec: String,
    pub kind: String,
}

/// What the run had done at one instant (a bin edge).
#[derive(Clone, Debug, Default)]
pub struct Reading {
    /// Model time units.
    pub at: f64,
    /// Busy-time integral (model units) per [`RawRun::resources`] index, tracked since
    /// `since` (model units); `None`: not tracked yet.
    pub busy: Vec<Option<(f64, f64)>>,
    /// Finished sessions: (scenario id, container ids, sessions).
    pub sessions: Vec<(String, Vec<String>, u64)>,
    /// External calls so far: (action id, count).
    pub calls: Vec<(String, u64)>,
    pub hpa: Vec<simoxide_spd::hpa::HpaSummary>,
}

/// What a run leaves: the measurements, the HPA log, the readings at the bin edges.
pub struct RawRun {
    pub meas: simoxide_sim::Measurements,
    log: HpaLog,
    pub resources: Vec<ResInfo>,
    /// One per edge reached, in order (the first at the window's start).
    pub readings: Vec<Reading>,
    /// The final reading (at the end of the run).
    pub last: Reading,
    /// The run stopped at the pod cap or a limit of the engine (`capped`), or failed: why.
    pub capped: bool,
    /// The run hit the wall-clock limit.
    pub timed_out: bool,
    pub failure: Option<String>,
    pub events: u64,
    /// Simulated time at the end (model units).
    pub end: f64,
    pub wall_ms: f64,
}

impl RawRun {
    /// The ready pods of autoscaler `name` over time (s): (time, ready) at its syncs and
    /// readiness changes, in time order, starting with `initial` at 0.
    pub fn ready_series(&self, name: &str, initial: i32, units_per_s: f64) -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = vec![(0.0, initial as f64)];
        let mut rest: Vec<(f64, f64)> = self
            .log
            .syncs
            .iter()
            .filter(|r| *r.1 == *name)
            .map(|r| (r.0 / units_per_s, r.2 as f64))
            .chain(self.log.ready.iter().filter(|r| *r.1 == *name).map(|r| (r.0 / units_per_s, r.2 as f64)))
            .collect();
        rest.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.extend(rest);
        v
    }

    /// Direction reversals of the rescales of autoscaler `name` from `from` (model units) on.
    pub fn reversals(&self, name: &str, from: f64) -> u32 {
        let mut dirs: Vec<i8> = Vec::new();
        for r in self.log.syncs.iter().filter(|r| *r.1 == *name && r.0 >= from) {
            let d = (r.4 - r.3).signum() as i8;
            if d != 0 && dirs.last() != Some(&d) {
                dirs.push(d);
            }
        }
        dirs.len().saturating_sub(1) as u32
    }
}

/// Tracks the resources the run has that `res` does not list yet, and returns the reading at
/// `at_ns` (the run paused before it).
fn read(
    sim: &mut Simulation<'_, Fast>,
    units: &[(simoxide_model::ContainerId, simoxide_model::ContainerId)],
    res: &mut Vec<ResInfo>,
    idx: &mut Vec<u32>,
    at_ns: i64,
    hpa: Vec<simoxide_spd::hpa::HpaSummary>,
) -> Reading {
    let cm = sim.compiled();
    let m = &cm.model;
    let mut new = Vec::new();
    for (ri, r) in cm.resources.iter().enumerate().skip(idx.last().map_or(0, |&i| i as usize + 1)) {
        let simoxide_sim::ir::ResKind::Processing(pid) = r.kind else { continue };
        let pr = &m.processing_resources[pid];
        let Some(c) = pr.container else { continue };
        let unit = units.iter().find(|x| x.0 == c).map_or(c, |x| x.1);
        res.push(ResInfo {
            container: m.containers[c].id.to_string(),
            unit: m.containers[unit].id.to_string(),
            spec: pr.id.to_string(),
            kind: pr.resource_type.map(|t| m.resource_types[t].name.to_string()).unwrap_or_default(),
        });
        idx.push(ri as u32);
        new.push(ri as u32);
    }
    let st = sim.state();
    let sessions = st
        .sync_paths(cm)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let ids = s
                .containers
                .iter()
                .map(|&c| {
                    let u = units.iter().find(|x| x.0 == c).map_or(c, |x| x.1);
                    m.containers[u].id.to_string()
                })
                .collect();
            (s.scenario_id, ids, s.sessions)
        })
        .collect();
    let calls = st
        .call_counts()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(a, n)| (cm.action_ids[a].to_string(), *n))
        .collect();
    let busy = idx.iter().map(|&ri| sim.busy_integral_at(ri, at_ns)).collect();
    for ri in new {
        sim.track_busy(ri);
    }
    Reading { at: at_ns as f64 / 1e9, busy, sessions, calls, hpa }
}

/// Runs a prepared model with `seed`, pausing at `edges` (model time units, increasing) to read
/// what it did. An error before the run (an invalid model) is an `Err`; a run that fails, hits
/// the pod cap or a limit returns what it had done (`failure`, `capped`).
pub fn run_raw(p: &Prepared, seed: i64, limits: Limits, edges: &[f64]) -> Result<RawRun, String> {
    let t0 = Instant::now();
    let dm = &p.model;
    let k = units_per_s(dm);
    let usage = file_with(&p.files, ".usagemodel")?;
    let spec = RunSpec { usagemodel: Some(usage), ..RunSpec::default() };
    let model_files: Vec<&(String, String)> = p.files.iter().filter(|(n, _)| n != "hpa.json").collect();
    let mut model = spec
        .load_model_memory(&model_files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect::<Vec<_>>())
        .map_err(|e| format!("the PCM model is invalid: {e}"))?;
    let hpa_cfg = match p.files.iter().find(|(n, _)| n == "hpa.json") {
        Some((_, text)) => {
            let cfg = HpaConfig::from_json(text)?;
            hpa::attach(&mut model, &cfg)?;
            Some(cfg)
        }
        None => None,
    };
    let spd = match &hpa_cfg {
        Some(_) => Some(spd_of(&model, Quirks::fixed()).map_err(|e| e.to_string())?.ok_or("the dynamic model has no SPD")?),
        None => None,
    };
    let hpa = match (&hpa_cfg, &spd) {
        (Some(c), Some(s)) => Some(Hpa::new(c, s)?),
        _ => None,
    };
    let cm = CompiledModel::compile(model).map_err(|e| format!("the model cannot be simulated: {e}"))?;
    let mut sc = spec.sim_config("evaluate");
    sc.seed = seed;
    sc.max_sim_time = (dm.end_s * k).ceil() as i64;
    sc.max_measurements = -1;
    sc.mode = Mode::Fast;
    // O(log n) processor sharing: an overloaded pod holds thousands of jobs (same semantics,
    // other float rounding)
    sc.ps_algorithm = simoxide_sim::PsAlgorithm::VirtualTime;
    sc.limits = limits;
    sc.load_profile = Some(Arc::new(dm.profile.clone()));
    sc.usage_streams = dm.usage_streams;
    sc.sync_paths = true;
    sc.call_counts = true;
    // SimuLizar's double resume at a synchronous fork join (REF-2) fixed
    sc.fix_double_resume = dm.fix_double_resume;
    let mut sim = Simulation::<Fast>::create(&cm, sc, Outputs::default()).map_err(|e| e.message)?;
    let (mut res, mut idx) = (Vec::new(), Vec::new());
    let _ = read(&mut sim, &[], &mut res, &mut idx, 0, Vec::new());
    let mut readings = Vec::new();
    let mut error: Option<simoxide_sim::SimError> = None;
    let end_ns = |sim: &Simulation<'_, Fast>| sim.state().now_ns();
    let (meas, events, end, last, log) = match (&spd, &hpa) {
        (Some(spd), Some(hpa)) => {
            let mut run = match SpdRun::new(sim, spd).and_then(|r| r.with_hpa(hpa)) {
                Ok(r) => r,
                Err(e) => return Err(format!("the dynamic model cannot be simulated: {}", e.message)),
            };
            let init: Vec<String> = dm.groups.iter().filter(|g| g.initial > 1).map(init_policy).collect();
            run.enforce_policies(&init.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
            let mut adj = QvtoAdjuster::spd(core()?, &SpdAdjust::new(&Quirks::fixed())).map_err(|e| e.to_string())?;
            let mut log = HpaLog::default();
            for &e in edges {
                let ns = (e * 1e9).round() as i64;
                match run.run_until(&mut adj, &mut log, Some(ns)) {
                    Ok(simoxide_sim::Status::Paused) => {
                        let units = run.container_units().to_vec();
                        let h = run.hpa_summary();
                        readings.push(read(&mut run.sim, &units, &mut res, &mut idx, ns, h));
                    }
                    Ok(simoxide_sim::Status::Finished) => break,
                    Err(x) => {
                        error = Some(x);
                        break;
                    }
                }
            }
            if error.is_none()
                && let Err(x) = run.run(&mut adj, &mut log)
            {
                error = Some(x);
            }
            let units = run.container_units().to_vec();
            let h = run.hpa_summary();
            let now = end_ns(&run.sim);
            let last = read(&mut run.sim, &units, &mut res, &mut idx, now, h);
            let st = run.sim.state();
            (st.measurements().clone(), st.events(), now as f64 / 1e9, last, log)
        }
        _ => {
            for &e in edges {
                let ns = (e * 1e9).round() as i64;
                match sim.run_until(Pause::Before(ns)) {
                    Ok(simoxide_sim::Status::Paused) => readings.push(read(&mut sim, &[], &mut res, &mut idx, ns, Vec::new())),
                    Ok(simoxide_sim::Status::Finished) => break,
                    Err(x) => {
                        error = Some(x);
                        break;
                    }
                }
            }
            if error.is_none()
                && let Err(x) = sim.run_until(Pause::Never)
            {
                error = Some(x);
            }
            let now = end_ns(&sim);
            let last = read(&mut sim, &[], &mut res, &mut idx, now, Vec::new());
            let st = sim.state();
            (st.measurements().clone(), st.events(), now as f64 / 1e9, last, HpaLog::default())
        }
    };
    let (mut capped, mut timed_out, mut failure) = (false, false, None);
    if let Some(e) = error {
        // the pod cap or the event limit: capped; the deadline: timed out
        timed_out = e.kind == simoxide_sim::SimErrorKind::Cancelled;
        capped = e.message.contains("maxCreatedPods") || e.kind == simoxide_sim::SimErrorKind::Limit;
        failure = Some(if e.message.contains("maxCreatedPods") {
            format!("the run created more pods than allowed (maxPods): {}", e.message)
        } else if timed_out {
            format!("the simulation hit its wall-clock limit (wallSeconds): {e}")
        } else {
            format!("the simulation failed: {e}")
        });
    }
    Ok(RawRun { meas, log, resources: res, readings, last, capped, timed_out, failure, events, end, wall_ms: t0.elapsed().as_secs_f64() * 1e3 })
}
