//! Dynamic runs with Kubernetes-style autoscaling: the model of rule 5
//! ([`crate::translate::DynamicModel`]) simulated over its load profile with SimOxide's HPA
//! (`simoxide_spd::hpa`; SimOxide `DESIGN-SPD.md` §5.10, `docs/spec/hpa.md`), and the dynamic
//! objectives from the run.
//!
//! - **Structure.** One SPD elastic infrastructure per autoscaled Deployable (unit: its pod), a
//!   service group per assembly on the pod with its load balancer, one `HorizontalPodAutoscaler`
//!   per group: CPU utilisation target `round(100 · targetUtilisation)`, `minReplicas`,
//!   `maxReplicas`, start-up delay; rescales are Slingshot's absolute adjustments
//!   (`MainTransformation`), run by `QvtoAdjuster::spd`. Each group starts with the ideal
//!   autoscaler's count at the profile's first factor (an enforced absolute adjustment at t = 0).
//! - **Load.** The model's `LoadProfile` as SimOxide's load profile (every usage scenario).
//! - **Objectives** over `[warmUp, end]`: cost (the fixed cost plus each pod's price times its
//!   ready pods, time-weighted), the share of requests over `sloFactor` × their latency budget,
//!   overall and in the two minutes after each load increase, scaling lag, flapping, peak pods.
//! - **Bounds.** A run that creates more than `max_pods` pods stops (SimOxide's
//!   `maxCreatedPods`), and so does one past the engine's limits (`SIM_TIMEOUT_SECONDS`,
//!   `SIM_MAX_EVENTS`); it is reported as capped and penalised (every pod at its maximum, every
//!   request over budget), so an evaluation never grows without bound.

use crate::translate::{DynamicModel, PodGroup};
use palladio_sim_service::config::Config;
use palladio_sim_service::{schema, xmi};
use sha2::{Digest, Sha256};
use simoxide_reconf::{QvtoAdjuster, QvtoCore, SpdAdjust};
use simoxide_sim::{CompiledModel, Fast, Limits, LoadProfile, Mode, Outputs, RunSpec, Simulation};
use simoxide_spd::Quirks;
use simoxide_spd::hpa::{self, Hpa, HpaConfig};
use simoxide_spd::log::{Log, Record};
use simoxide_spd::sim::{SpdRun, spd_of};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What a dynamic run is asked to do.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HpaParams {
    pub seed: i64,
    /// Usage scenarios (entity names) left out of the response-time measures.
    pub exclude: Vec<String>,
}

/// The window after a load increase in which SLO violations are counted separately (s).
pub const STEP_WINDOW_S: f64 = 120.0;

/// The most pods one evaluation may create, all groups together (beyond: capped).
pub const DEFAULT_MAX_PODS: u32 = 400;

/// One autoscaler's part of a dynamic run.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupOutcome {
    pub name: String,
    /// Time-weighted ready pods over the window, and their ideal-autoscaler counterpart.
    pub mean_replicas: f64,
    pub ideal_replicas: f64,
    pub max_replicas: i32,
    pub rescales: u64,
    pub created: u64,
    /// Mean scaling lag over the load increases that needed more pods (s).
    pub lag: f64,
    /// Direction reversals of its rescales.
    pub reversals: u32,
}

/// The dynamic objectives of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct HpaOutcome {
    /// Mean cost per time unit of the window (the CostModel's units, as `archCost`).
    pub cost: f64,
    /// The same with the ideal autoscaler's counts at each step's factor (no lag, no tolerance).
    pub ideal_cost: f64,
    pub ready_replica_seconds: f64,
    pub replica_seconds: f64,
    /// Share of the measured requests over their budget, in the window and in the two minutes
    /// after each load increase.
    pub slo: f64,
    pub slo_steps: f64,
    pub requests: u64,
    /// Mean and 95th percentile of the measured response times (ms).
    pub mean_ms: f64,
    pub p95_ms: f64,
    /// Scaling lag (s): mean and largest over the (increase, group) pairs that needed pods.
    pub lag: f64,
    pub lag_max: f64,
    /// Direction reversals of rescales per hour of the window, all groups.
    pub flapping: f64,
    /// The most ready pods at once, all groups (at a sync).
    pub peak_replicas: f64,
    pub rescales: u64,
    pub created: u64,
    pub adjustments: u64,
    /// The run stopped at `max_pods` created pods: the objectives are penalised.
    pub capped: bool,
    pub events: u64,
    pub wall_ms: f64,
    /// Of `wall_ms`: the adjustments (Slingshot's transformation and the model changes).
    pub adjust_ms: f64,
    pub groups: Vec<GroupOutcome>,
}

/// Times the adjustments of the run's adjuster.
struct Timed {
    inner: QvtoAdjuster,
    total: Duration,
}

impl simoxide_spd::sim::Adjuster<simoxide_sim::Fast, simoxide_sim::SimuLizar> for Timed {
    fn adjust(
        &mut self,
        sim: &mut Simulation<'_, Fast>,
        spd: &simoxide_spd::Spd,
        state: &mut simoxide_spd::SpdState,
        req: &simoxide_spd::AdjustmentRequest,
    ) -> Result<simoxide_spd::AdjustOutcome, simoxide_sim::SimError> {
        let t0 = Instant::now();
        let r = self.inner.adjust_on(sim, spd, state, req);
        self.total += t0.elapsed();
        r
    }
}

// ---------------------------------------------------------------------------------------------
// The engine: one per process, a bounded cache of outcomes and a limit on concurrent runs

/// A cached outcome, computed once (single flight).
type Cell = Arc<tokio::sync::OnceCell<Arc<HpaOutcome>>>;

pub struct DynEngine {
    outcomes: Mutex<lru::LruCache<([u8; 32], HpaParams), Cell>>,
    permits: tokio::sync::Semaphore,
    timeout_s: u64,
    max_events: u64,
    pub max_pods: u32,
    pub runs: std::sync::atomic::AtomicU64,
}

/// The process's dynamic engine, sized like the simulation engine (`SIM_THREADS`,
/// `SIM_RESULT_CACHE_SIZE`, `SIM_TIMEOUT_SECONDS`, `SIM_MAX_EVENTS`; `HPA_MAX_PODS`).
pub fn engine(config: &Config) -> &'static DynEngine {
    static E: OnceLock<DynEngine> = OnceLock::new();
    E.get_or_init(|| DynEngine {
        outcomes: Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(config.result_cache_size.max(1)).unwrap())),
        permits: tokio::sync::Semaphore::new(config.threads().max(1)),
        timeout_s: config.sim_timeout_seconds,
        max_events: config.sim_max_events,
        max_pods: std::env::var("HPA_MAX_PODS").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_PODS),
        runs: Default::default(),
    })
}

/// A dynamic model ready to run: its files and their digest.
pub struct Prepared {
    pub model: Arc<DynamicModel>,
    pub files: Vec<(String, String)>,
    pub digest: [u8; 32],
}

/// The files of a dynamic run: the PCM files of the model, its SPD and semantic configuration,
/// and the HPA configuration (in the digest; passed in memory).
pub fn prepare(dm: Arc<DynamicModel>, max_pods: u32) -> Result<Prepared, String> {
    let pcm = xmi::to_xmi(schema::schema(), &dm.pcm)?;
    let mut files = pcm.files;
    let (spd, sem) = spd_files(&dm, &files)?;
    files.push(("dynamic.spd".into(), spd));
    files.push(("dynamic.semanticspd".into(), sem));
    files.push(("hpa.json".into(), hpa_json(&dm, max_pods)));
    let mut h = Sha256::new();
    for (name, text) in &files {
        h.update((name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update((text.len() as u64).to_le_bytes());
        h.update(text.as_bytes());
    }
    if let Some(p) = &dm.profile {
        h.update(format!("{:?}", p).as_bytes());
    }
    Ok(Prepared { model: dm, files, digest: h.finalize().into() })
}

impl DynEngine {
    /// The outcome of running `p` with `params`, cached by content (runs are deterministic).
    pub async fn simulate(&'static self, p: Arc<Prepared>, params: HpaParams) -> Result<Arc<HpaOutcome>, String> {
        let key = (p.digest, params.clone());
        let cell = {
            let mut c = self.outcomes.lock().unwrap();
            c.get_or_insert(key.clone(), || Arc::new(tokio::sync::OnceCell::new())).clone()
        };
        let r = cell
            .get_or_try_init(|| async {
                let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
                let limits = Limits {
                    max_events: self.max_events,
                    deadline: (self.timeout_s > 0).then(|| Instant::now() + Duration::from_secs(self.timeout_s)),
                    ..Limits::default()
                };
                self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let p2 = p.clone();
                tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&p2, &params, limits)))
                        .unwrap_or_else(|_| Err("the dynamic run panicked".into()))
                })
                .await
                .map_err(|e| format!("the dynamic run was abandoned: {e}"))?
                .map(Arc::new)
            })
            .await
            .cloned();
        if r.is_err() {
            let mut c = self.outcomes.lock().unwrap();
            if c.peek(&key).is_some_and(|x| Arc::ptr_eq(x, &cell) && !x.initialized()) {
                c.pop(&key);
            }
        }
        r
    }
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
            if let Some(w) = g.scale_down_s {
                spec["behavior"] = serde_json::json!({"scaleDown": {"stabilizationWindowSeconds": (w * k).round() as i64}});
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
            "syncPeriod": 15.0 * k,
            "downscaleStabilization": 300.0 * k,
            "cpuInitializationPeriod": 300.0 * k,
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

/// Runs a prepared dynamic model.
pub fn run(p: &Prepared, params: &HpaParams, limits: Limits) -> Result<HpaOutcome, String> {
    let t0 = Instant::now();
    let dm = &p.model;
    let profile = dm.profile.clone().ok_or("a dynamic run needs a LoadProfile in the model")?;
    let k = units_per_s(dm);
    let usage = file_with(&p.files, ".usagemodel")?;
    let spec = RunSpec { usagemodel: Some(usage), ..RunSpec::default() };
    let model_files: Vec<&(String, String)> = p.files.iter().filter(|(n, _)| n != "hpa.json").collect();
    let mut model = spec.load_model_memory(&model_files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect::<Vec<_>>())?;
    let hpa_text = &p.files.iter().find(|(n, _)| n == "hpa.json").ok_or("no HPA configuration")?.1;
    let cfg = HpaConfig::from_json(hpa_text)?;
    hpa::attach(&mut model, &cfg)?;
    let spd = spd_of(&model, Quirks::fixed()).map_err(|e| e.to_string())?.ok_or("the dynamic model has no SPD")?;
    let hpa = Hpa::new(&cfg, &spd)?;
    let cm = CompiledModel::compile(model).map_err(|e| format!("the dynamic model cannot be simulated: {e}"))?;
    let mut sc = spec.sim_config("hpa");
    sc.seed = params.seed;
    sc.max_sim_time = (profile.end * k).ceil() as i64;
    sc.max_measurements = -1;
    sc.mode = Mode::Fast;
    // O(log n) processor sharing: an overloaded pod holds thousands of jobs (same semantics,
    // other float rounding)
    sc.ps_algorithm = simoxide_sim::PsAlgorithm::VirtualTime;
    sc.limits = limits;
    sc.load_profile = Some(Arc::new(LoadProfile::uniform(profile.steps.iter().map(|&(t, f)| (t * k, f)).collect())));
    let sim = Simulation::<Fast>::create(&cm, sc, Outputs::default()).map_err(|e| e.message)?;
    let mut run = SpdRun::new(sim, &spd).map_err(|e| e.message)?.with_hpa(&hpa).map_err(|e| e.message)?;
    let init: Vec<String> = dm.groups.iter().filter(|g| g.initial > 1).map(init_policy).collect();
    run.enforce_policies(&init.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    let mut adj = Timed { inner: QvtoAdjuster::spd(core()?, &SpdAdjust::new(&Quirks::fixed())).map_err(|e| e.to_string())?, total: Duration::ZERO };
    let mut log = HpaLog::default();
    let warm_ns = (profile.warm_up * k * 1e9) as i64;
    let mut capped = false;
    let mut at_warm = Vec::new();
    let r1 = run.run_until(&mut adj, &mut log, Some(warm_ns));
    let r2 = match r1 {
        Ok(_) => {
            at_warm = run.hpa_summary();
            run.run(&mut adj, &mut log)
        }
        Err(e) => Err(e),
    };
    if let Err(e) = r2 {
        // the pod cap, or a limit of the run (deadline, events): penalised, not an error
        if e.message.contains("maxCreatedPods") || e.kind == simoxide_sim::SimErrorKind::Limit {
            capped = true;
        } else {
            return Err(format!("the dynamic run failed: {}", e.message));
        }
    }
    let at_end = run.hpa_summary();
    let events = run.sim.state().events();
    let measurements = run.sim.state().measurements().clone();
    drop(run);
    let mut o = outcome(dm, &profile, params, &cm, &measurements, &log, &at_warm, &at_end, capped, events, t0)?;
    o.adjust_ms = adj.total.as_secs_f64() * 1e3;
    Ok(o)
}

#[allow(clippy::too_many_arguments)]
fn outcome(
    dm: &DynamicModel,
    profile: &crate::translate::LoadProfileSpec,
    params: &HpaParams,
    cm: &CompiledModel,
    meas: &simoxide_sim::Measurements,
    log: &HpaLog,
    at_warm: &[simoxide_spd::hpa::HpaSummary],
    at_end: &[simoxide_spd::hpa::HpaSummary],
    capped: bool,
    events: u64,
    t0: Instant,
) -> Result<HpaOutcome, String> {
    let k = units_per_s(dm);
    let (w0, w1) = (profile.warm_up, profile.end);
    let window = w1 - w0;
    let increases: Vec<(f64, f64, f64)> = {
        // (time, old factor, new factor) of every increase inside the window
        let mut v = Vec::new();
        let mut f = profile.factor_at(0.0);
        for &(t, nf) in &profile.steps {
            if t > 0.0 && nf > f && t < w1 {
                v.push((t, f, nf));
            }
            f = nf;
        }
        v
    };
    // ------------------------------------------------ cost and pods
    let mut cost = dm.fixed_cost;
    let mut ideal_cost = dm.fixed_cost;
    let mut ready_rs = 0.0;
    let mut all_rs = 0.0;
    let mut groups = Vec::new();
    let mut lags = Vec::new();
    let mut reversals_total = 0u32;
    let (mut rescales, mut created, mut adjustments) = (0, 0, 0);
    // ideal counts, time-weighted over the window
    let ideal_mean = |g: &PodGroup| -> f64 {
        let mut pts: Vec<f64> = vec![w0];
        pts.extend(profile.steps.iter().map(|s| s.0).filter(|&t| t > w0 && t < w1));
        pts.push(w1);
        pts.windows(2).map(|w| g.ideal(profile.factor_at(w[0])) * (w[1] - w[0])).sum::<f64>() / window
    };
    for g in &dm.groups {
        let end = at_end.iter().find(|s| s.name == g.name);
        let start = at_warm.iter().find(|s| s.name == g.name);
        let (ready, total) = match (end, start) {
            (Some(e), Some(s)) => (e.ready_replica_seconds - s.ready_replica_seconds, e.replica_seconds - s.replica_seconds),
            (Some(e), None) => (e.ready_replica_seconds, e.replica_seconds),
            _ => (0.0, 0.0),
        };
        let mean = if capped { g.max as f64 } else { ready / (window * k) };
        let ideal = ideal_mean(g);
        cost += g.price_per_replica * mean;
        ideal_cost += g.price_per_replica * ideal;
        ready_rs += ready / k;
        all_rs += total / k;
        // the ready pods over time: syncs and readiness records
        let mut series: Vec<(f64, i32)> = log
            .syncs
            .iter()
            .filter(|r| *r.1 == *g.name)
            .map(|r| (r.0 / k, r.2))
            .chain(log.ready.iter().filter(|r| *r.1 == *g.name).map(|r| (r.0 / k, r.2)))
            .collect();
        series.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ready_at = |t: f64| -> i32 { series.iter().take_while(|x| x.0 <= t).last().map_or(g.initial as i32, |x| x.1) };
        let mut glags = Vec::new();
        for (i, &(t, _, nf)) in increases.iter().enumerate() {
            let need = (g.ideal(nf) / 1.1).ceil() as i32;
            if ready_at(t) >= need {
                continue;
            }
            let until = increases.get(i + 1).map_or(w1, |x| x.0).min(profile.steps.iter().map(|s| s.0).find(|&s| s > t).unwrap_or(w1));
            let reached = series.iter().find(|x| x.0 >= t && x.1 >= need).map(|x| x.0);
            let lag = reached.filter(|&r| r <= until).map_or(until - t, |r| r - t);
            glags.push(lag);
        }
        // rescale directions
        let mut dirs: Vec<i8> = Vec::new();
        for r in log.syncs.iter().filter(|r| *r.1 == *g.name && r.0 / k >= w0) {
            let d = (r.4 - r.3).signum() as i8;
            if d != 0 && dirs.last() != Some(&d) {
                dirs.push(d);
            }
        }
        let reversals = dirs.len().saturating_sub(1) as u32;
        reversals_total += reversals;
        let (re, cr, ad, mx) = end.map_or((0, 0, 0, 0), |e| (e.rescales, e.created, e.adjustments, e.max_replicas));
        rescales += re;
        created += cr;
        adjustments += ad;
        let lag = if glags.is_empty() { 0.0 } else { glags.iter().sum::<f64>() / glags.len() as f64 };
        lags.extend(glags);
        groups.push(GroupOutcome {
            name: g.name.clone(),
            mean_replicas: mean,
            ideal_replicas: ideal,
            max_replicas: mx,
            rescales: re,
            created: cr,
            lag,
            reversals,
        });
    }
    // peak: the most ready pods at once over the syncs
    let mut by_time: BTreeMap<i64, BTreeMap<&str, i32>> = BTreeMap::new();
    let mut last: BTreeMap<&str, i32> = dm.groups.iter().map(|g| (g.name.as_str(), g.initial as i32)).collect();
    let mut all_records: Vec<(f64, &str, i32)> =
        log.syncs.iter().map(|r| (r.0, &*r.1, r.2)).chain(log.ready.iter().map(|r| (r.0, &*r.1, r.2))).collect();
    all_records.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut peak = last.values().sum::<i32>() as f64;
    for (t, name, ready) in all_records {
        last.insert(name, ready);
        by_time.entry((t * 1e3) as i64).or_default();
        peak = peak.max(last.values().sum::<i32>() as f64);
    }
    // ------------------------------------------------ service levels
    let excluded: Vec<&str> = params.exclude.iter().map(String::as_str).collect();
    let mut total = 0u64;
    let mut over = 0u64;
    let mut step_total = 0u64;
    let mut step_over = 0u64;
    let mut rts: Vec<f64> = Vec::new();
    for (i, def) in meas.series.iter().enumerate() {
        if !def.mp.starts_with("UsageScenarioMeasuringPoint[") || !def.metric.starts_with("Response Time") {
            continue;
        }
        let id = def.mp.trim_start_matches("UsageScenarioMeasuringPoint[").trim_end_matches(']');
        let Some(sc) = cm.scenarios.iter().map(|s| &cm.model.usage_scenarios[s.id]).find(|s| &*s.id == id) else { continue };
        if excluded.contains(&&*sc.name) {
            continue;
        }
        let Some(budget_ms) = dm.budgets.get(id) else { continue };
        let limit = profile.slo_factor * budget_ms / dm.ms_per_unit;
        for &(t, v) in &meas.rows[i] {
            let ts = t / k;
            if ts < w0 || ts > w1 {
                continue;
            }
            total += 1;
            let bad = v > limit;
            over += bad as u64;
            rts.push(v * dm.ms_per_unit);
            if increases.iter().any(|&(s, _, _)| ts >= s && ts < s + STEP_WINDOW_S) {
                step_total += 1;
                step_over += bad as u64;
            }
        }
    }
    if let Some(dir) = std::env::var_os("HPA_TRACE_DIR") {
        trace(std::path::Path::new(&dir), dm, cm, meas, log, k);
    }
    rts.sort_by(f64::total_cmp);
    let mean_ms = if rts.is_empty() { 0.0 } else { rts.iter().sum::<f64>() / rts.len() as f64 };
    let p95_ms = if rts.is_empty() { 0.0 } else { rts[((0.95 * rts.len() as f64).ceil() as usize).clamp(1, rts.len()) - 1] };
    let share = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    Ok(HpaOutcome {
        cost,
        ideal_cost,
        ready_replica_seconds: ready_rs,
        replica_seconds: all_rs,
        slo: if capped { 1.0 } else { share(over, total) },
        slo_steps: if capped { 1.0 } else { share(step_over, step_total) },
        requests: total,
        mean_ms,
        p95_ms,
        lag: if lags.is_empty() { 0.0 } else { lags.iter().sum::<f64>() / lags.len() as f64 },
        lag_max: lags.iter().copied().fold(0.0, f64::max),
        flapping: reversals_total as f64 / (window / 3600.0),
        peak_replicas: peak,
        rescales,
        created,
        adjustments,
        capped,
        events,
        wall_ms: t0.elapsed().as_secs_f64() * 1e3,
        adjust_ms: 0.0,
        groups,
    })
}

/// Writes the time series of a run (`HPA_TRACE_DIR`): per 10 s the mean response time, the
/// requests and the share over 2, 3, 5 and 10 times their latency budget (`rt.csv`), and the
/// ready pods of every group at its syncs (`pods.csv`).
fn trace(dir: &std::path::Path, dm: &DynamicModel, cm: &CompiledModel, meas: &simoxide_sim::Measurements, log: &HpaLog, k: f64) {
    const F: [f64; 4] = [2.0, 3.0, 5.0, 10.0];
    let _ = std::fs::create_dir_all(dir);
    let mut bins: BTreeMap<i64, (f64, u64, [u64; 4])> = BTreeMap::new();
    for (i, def) in meas.series.iter().enumerate() {
        if !def.mp.starts_with("UsageScenarioMeasuringPoint[") || !def.metric.starts_with("Response Time") {
            continue;
        }
        let id = def.mp.trim_start_matches("UsageScenarioMeasuringPoint[").trim_end_matches(']');
        if cm.scenarios.iter().map(|s| &cm.model.usage_scenarios[s.id]).any(|s| &*s.id == id && &*s.name == "login") {
            continue;
        }
        let budget = dm.budgets.get(id).copied().unwrap_or(f64::INFINITY);
        for &(t, v) in &meas.rows[i] {
            let e = bins.entry((t / k / 10.0).floor() as i64).or_default();
            let ms = v * dm.ms_per_unit;
            e.0 += ms;
            e.1 += 1;
            for (j, f) in F.iter().enumerate() {
                e.2[j] += (ms > f * budget) as u64;
            }
        }
    }
    let mut rt = String::from("t,mean_ms,requests,over2,over3,over5,over10\n");
    for (b, (sum, n, o)) in bins {
        let sh = |x: u64| x as f64 / n as f64;
        rt += &format!("{},{:.3},{},{:.4},{:.4},{:.4},{:.4}\n", b * 10, sum / n as f64, n, sh(o[0]), sh(o[1]), sh(o[2]), sh(o[3]));
    }
    let _ = std::fs::write(dir.join("rt.csv"), rt);
    let mut pods = String::from("t,group,ready,current,desired\n");
    for r in &log.syncs {
        pods += &format!("{:.1},{},{},{},{}\n", r.0 / k, r.1, r.2, r.3, r.4);
    }
    let _ = std::fs::write(dir.join("pods.csv"), pods);
}
