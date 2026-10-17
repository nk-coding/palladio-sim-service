//! The simulation engine: turns models into compiled PCM models and runs them, in SimOxide's
//! default mode (fast; SimuLizar-exact with the feature `exact`), on one thread pool shared by
//! every session.
//!
//! - **One pool.** Runs are CPU-bound, so they go to a rayon pool of `SIM_THREADS` threads, never
//!   to the async runtime. Replications of one call run in parallel on it, and so do the calls of
//!   all sessions: an optimizer with many workers keeps every thread busy, and one script asking
//!   for many replications does as well.
//! - **Content-addressed caches.** A model is identified by the digest of its PCM files. Compiling
//!   is cached by that digest, and outcomes by the digest and the run parameters. Both modes are
//!   deterministic, so a cached outcome is exactly what running again would give; it saves the
//!   work when an optimizer meets a model again, or when several objectives of one solution ask
//!   for the same simulation.
//! - **Single flight.** Concurrent requests for the same compilation or outcome share one
//!   computation, also across sessions.

use crate::config::Config;
use crate::protocol::WireModel;
use crate::schema::Schema;
use crate::xmi;
use sha2::{Digest, Sha256};
use simoxide_sim::{CompiledModel, Limits, Outputs, RunSpec, SimErrorKind};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

/// What a run is asked to do.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RunParams {
    pub seed: i64,
    pub max_measurements: i64,
    pub max_sim_time: i64,
    pub replications: u32,
    pub simulate_linking_resources: bool,
}

/// Statistics of one series (measuring point and metric), over all replications.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesStats {
    pub measuring_point: String,
    /// The ids inside the measuring point key: the instance names of the measured elements.
    pub elements: Vec<String>,
    /// The monitor that measures it, when one names this measuring point.
    pub monitor: Option<String>,
    /// The metric, e.g. `Response Time` (the key's metric without ` Tuple`).
    pub metric: String,
    pub count: u64,
    pub mean: f64,
    pub std_dev: f64,
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    /// Mean of the value as a step function of time (state and utilisation series); averaged
    /// over the replications that define it.
    pub time_weighted_mean: Option<f64>,
    /// Half-width of the 95 % confidence interval of the mean, from the replication means
    /// (Student's t); needs two replications.
    pub ci_half_width: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub series: Vec<SeriesStats>,
    pub replications: u32,
    pub events: u64,
    pub measurements: u64,
    /// Mean simulated time at the end of the runs, in seconds.
    pub simulated_time: f64,
}

/// A model ready to run.
pub struct Prepared {
    pub digest: [u8; 32],
    pub compiled: CompiledModel,
    /// Monitors: (instance name, entity name, names its measuring point refers to).
    pub monitors: Vec<(String, Option<String>, Vec<String>)>,
    /// Entity names by instance name.
    pub entity_names: HashMap<String, String>,
    pub usage_model: String,
}

pub struct Engine {
    pub config: Config,
    pool: rayon::ThreadPool,
    compiled: Flights<[u8; 32], Arc<Prepared>>,
    outcomes: Flights<([u8; 32], RunParams), Arc<Outcome>>,
    pub stats: EngineStats,
}

#[derive(Default)]
pub struct EngineStats {
    pub runs: std::sync::atomic::AtomicU64,
    pub outcome_hits: std::sync::atomic::AtomicU64,
    pub compiles: std::sync::atomic::AtomicU64,
}

impl Engine {
    pub fn new(config: Config) -> Engine {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(config.threads())
            .thread_name(|i| format!("sim-{i}"))
            // StoEx evaluation recurses (capped); give it room.
            .stack_size(8 << 20)
            .build()
            .expect("simulation thread pool");
        Engine {
            compiled: Flights::new(config.model_cache_size),
            outcomes: Flights::new(config.result_cache_size),
            pool,
            config,
            stats: EngineStats::default(),
        }
    }

    pub fn threads(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// Runs `f` on the simulation pool and waits for it without blocking the async runtime. A
    /// panic fails the call instead of the service.
    async fn on_pool<T: Send + 'static>(&self, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pool.spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .unwrap_or_else(|p| Err(format!("the simulator failed: {}", panic_message(&p))));
            let _ = tx.send(r);
        });
        rx.await.map_err(|_| "the simulation was abandoned".to_string())?
    }

    /// The model as PCM, compiled; shared by every model with the same content.
    pub async fn prepare(self: &Arc<Self>, schema: &'static Schema, model: Arc<WireModel>) -> Result<Arc<Prepared>, String> {
        if let Some(dir) = &self.config.dump_dir {
            dump(dir, schema, &model);
        }
        // Writing the XMI is cheap next to compiling; the digest of it identifies the model.
        let files = xmi::to_xmi(schema, &model)?;
        let mut h = Sha256::new();
        for (name, text) in &files.files {
            h.update((name.len() as u64).to_le_bytes());
            h.update(name.as_bytes());
            h.update((text.len() as u64).to_le_bytes());
            h.update(text.as_bytes());
        }
        let digest: [u8; 32] = h.finalize().into();
        let this = self.clone();
        self.compiled
            .get(digest, async move {
                this.stats.compiles.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                this.on_pool(move || compile(schema, &model, files, digest)).await.map(Arc::new)
            })
            .await
    }

    /// The outcome of running `prepared` with `params`.
    pub async fn simulate(self: &Arc<Self>, prepared: Arc<Prepared>, params: RunParams) -> Result<Arc<Outcome>, String> {
        if params.max_measurements <= 0 && params.max_sim_time <= 0 {
            return Err("a simulation needs a stop condition: set maxMeasurements or maxSimTime".into());
        }
        if params.replications == 0 || params.replications > self.config.max_replications {
            return Err(format!("replications must be between 1 and {}", self.config.max_replications));
        }
        let key = (prepared.digest, params.clone());
        let this = self.clone();
        let computed = std::sync::atomic::AtomicBool::new(false);
        let out = self
            .outcomes
            .get(key, async {
                computed.store(true, std::sync::atomic::Ordering::Relaxed);
                let limits = this.limits();
                let this2 = this.clone();
                this.on_pool(move || run_replications(&this2, &prepared, &params, limits)).await.map(Arc::new)
            })
            .await;
        if !computed.load(std::sync::atomic::Ordering::Relaxed) {
            self.stats.outcome_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        out
    }

    fn limits(&self) -> Limits {
        Limits {
            max_events: self.config.sim_max_events,
            deadline: (self.config.sim_timeout_seconds > 0)
                .then(|| Instant::now() + Duration::from_secs(self.config.sim_timeout_seconds)),
            ..Limits::default()
        }
    }
}

/// Writes a received model to `dir/<n>/`: `model.m`, and its PCM files when it converts.
fn dump(dir: &std::path::Path, schema: &Schema, model: &WireModel) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = dir.join(format!("{:06}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = std::fs::create_dir_all(&d);
    match crate::mtext::write(schema, model, &model.metamodel_path) {
        Ok(text) => {
            let _ = std::fs::write(d.join("model.m"), text);
        }
        Err(e) => {
            let _ = std::fs::write(d.join("model.error.txt"), e);
        }
    }
    let _ = std::fs::write(d.join("model.debug.txt"), format!("{model:#?}"));
    match xmi::to_xmi(schema, model) {
        Ok(files) => {
            for (name, text) in files.files {
                let _ = std::fs::write(d.join(name), text);
            }
        }
        Err(e) => {
            let _ = std::fs::write(d.join("xmi.error.txt"), e);
        }
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

fn compile(schema: &Schema, model: &WireModel, files: xmi::PcmFiles, digest: [u8; 32]) -> Result<Prepared, String> {
    let usage: Vec<&(String, String)> = files.files.iter().filter(|(n, _)| n.ends_with(".usagemodel")).collect();
    let usage_model = match usage.as_slice() {
        [one] => one.0.clone(),
        [] => return Err("the model has no UsageModel: there is nothing to simulate".into()),
        more => {
            return Err(format!(
                "the model has {} UsageModels ({}); keep exactly one",
                more.len(),
                more.iter().map(|(n, _)| n.trim_end_matches(".usagemodel")).collect::<Vec<_>>().join(", ")
            ));
        }
    };
    if !files.files.iter().any(|(n, _)| n.ends_with(".monitorrepository")) {
        return Err("the model has no MonitorRepository: nothing would be measured".into());
    }
    let spec = RunSpec { usagemodel: Some(usage_model.clone()), ..RunSpec::default() };
    let pcm = spec.load_model_memory(&files.files).map_err(|e| format!("the PCM model is invalid: {e}"))?;
    let compiled = CompiledModel::compile(pcm).map_err(|e| format!("the model cannot be simulated: {e}"))?;
    let entity_names = entity_names(model);
    Ok(Prepared { digest, compiled, monitors: monitors(schema, model), entity_names, usage_model })
}

/// Entity names by instance name. A processing resource specification has none of its own; it is
/// named after its container and resource type, `ServerNode.CPU`.
fn entity_names(model: &WireModel) -> HashMap<String, String> {
    use crate::protocol::WireValue;
    let string = |i: &crate::protocol::WireInstance, attr: &str| -> Option<String> {
        match i.attributes.iter().find(|(k, _)| k == attr)?.1.first()? {
            WireValue::String(s) => Some(s.clone()),
            _ => None,
        }
    };
    let mut names: HashMap<String, String> =
        model.instances.iter().filter_map(|i| Some((i.name.clone(), string(i, "entityName")?))).collect();
    let by_name: HashMap<&str, &crate::protocol::WireInstance> =
        model.instances.iter().map(|i| (i.name.as_str(), i)).collect();
    for container in &model.instances {
        let Some((_, specs)) = container.references.iter().find(|(k, _)| k == "activeResourceSpecifications") else {
            continue;
        };
        let container_name = names.get(&container.name).cloned().unwrap_or_else(|| container.name.clone());
        for spec in specs {
            if let Some(i) = by_name.get(spec.as_str()) {
                let ty = string(i, "activeResourceType").unwrap_or_else(|| "resource".into());
                names.insert(spec.clone(), format!("{container_name}.{ty}"));
            }
        }
    }
    names
}

/// Every monitor with the names its measuring point refers to.
fn monitors(schema: &Schema, model: &WireModel) -> Vec<(String, Option<String>, Vec<String>)> {
    let by_name: HashMap<&str, &crate::protocol::WireInstance> =
        model.instances.iter().map(|i| (i.name.as_str(), i)).collect();
    let _ = schema;
    let mut out = Vec::new();
    for inst in &model.instances {
        if inst.class_name != "Monitor" {
            continue;
        }
        let entity = inst.attributes.iter().find(|(k, _)| k == "entityName").and_then(|(_, v)| match v.first() {
            Some(crate::protocol::WireValue::String(s)) => Some(s.clone()),
            _ => None,
        });
        let mp = inst.references.iter().find(|(k, _)| k == "measuringPoint").and_then(|(_, v)| v.first());
        let targets: Vec<String> = mp
            .and_then(|m| by_name.get(m.as_str()))
            .map(|m| {
                m.references
                    .iter()
                    .filter(|(k, _)| k != "measuringPointRepository")
                    .flat_map(|(_, v)| v.iter().cloned())
                    .collect()
            })
            .unwrap_or_default();
        out.push((inst.name.clone(), entity, targets));
    }
    out
}

fn run_replications(engine: &Engine, prepared: &Prepared, params: &RunParams, limits: Limits) -> Result<Outcome, String> {
    use rayon::prelude::*;
    let configs: Vec<simoxide_sim::SimConfig> = (0..params.replications)
        .map(|r| {
            let spec = RunSpec {
                seed: params.seed.wrapping_add(r as i64),
                max_measurements: params.max_measurements,
                max_sim_time: params.max_sim_time,
                simulate_linking_resources: params.simulate_linking_resources,
                ..RunSpec::default()
            };
            let mut cfg = spec.sim_config("");
            cfg.limits = limits.clone();
            cfg
        })
        .collect();
    let results: Vec<Result<simoxide_sim::RunResult, String>> = configs
        .into_par_iter()
        .map(|cfg| {
            engine.stats.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            simoxide_sim::run(&prepared.compiled, cfg, Outputs::default()).map_err(|e| match e.kind {
                SimErrorKind::Limit => format!("the simulation hit a limit: {e}"),
                _ => format!("the simulation failed (as SimuLizar would): {e}"),
            })
        })
        .collect();
    let runs: Vec<simoxide_sim::RunResult> = results.into_iter().collect::<Result<_, _>>()?;
    Ok(aggregate(prepared, &runs))
}

/// Pools the tuples of every replication per series. With one replication the statistics are
/// exactly SimOxide's `Measurements::summaries`.
fn aggregate(prepared: &Prepared, runs: &[simoxide_sim::RunResult]) -> Outcome {
    // Series keys in the output order of the first run that has them.
    let mut keys: Vec<(String, &'static str)> = Vec::new();
    let mut seen: HashSet<(String, &'static str)> = HashSet::new();
    for r in runs {
        for i in r.measurements.sorted_series() {
            let d = &r.measurements.series[i];
            let k = (d.mp.to_string(), d.metric);
            if !r.measurements.rows[i].is_empty() && seen.insert(k.clone()) {
                keys.push(k);
            }
        }
    }
    let mut series = Vec::with_capacity(keys.len());
    for (mp, metric) in keys {
        let per_run: Vec<&[(f64, f64)]> = runs
            .iter()
            .filter_map(|r| {
                let i = r.measurements.series.iter().position(|d| &*d.mp == mp && d.metric == metric)?;
                Some(r.measurements.rows[i].as_slice()).filter(|rows| !rows.is_empty())
            })
            .collect();
        let elements = key_elements(&mp);
        let monitor = prepared.monitors.iter().find(|(_, _, t)| {
            !t.is_empty() && elements.iter().all(|e| t.contains(e)) && t.iter().all(|x| elements.contains(x))
        });
        series.push(stats(mp, elements, monitor.map(|m| m.0.clone()), metric, &per_run));
    }
    let n = runs.len().max(1) as f64;
    Outcome {
        series,
        replications: runs.len() as u32,
        events: runs.iter().map(|r| r.events).sum(),
        measurements: runs.iter().map(|r| r.measurements.count).sum(),
        simulated_time: runs.iter().map(|r| r.end_ns as f64 / 1e9).sum::<f64>() / n,
    }
}

/// `SystemOperationMeasuringPoint[role|signature|system]` → `[role, signature, system]`.
pub fn key_elements(mp: &str) -> Vec<String> {
    let inner = mp.split_once('[').map_or("", |x| x.1).trim_end_matches(']');
    inner.split('|').filter(|s| !s.is_empty() && !s.contains('=') && *s != "null").map(String::from).collect()
}

fn stats(mp: String, elements: Vec<String>, monitor: Option<String>, metric: &str, per_run: &[&[(f64, f64)]]) -> SeriesStats {
    let rows: Vec<(f64, f64)> = per_run.iter().flat_map(|r| r.iter().copied()).collect();
    let n = rows.len() as f64;
    // The same arithmetic as `Measurements::summaries`, so one replication gives its numbers.
    let mean = rows.iter().map(|r| r.1).sum::<f64>() / n;
    let var = if rows.len() > 1 { rows.iter().map(|r| (r.1 - mean) * (r.1 - mean)).sum::<f64>() / (n - 1.0) } else { 0.0 };
    let mut v: Vec<f64> = rows.iter().map(|r| r.1).collect();
    v.sort_by(f64::total_cmp);
    let pct = |p: f64| v[((p * n).ceil() as usize).clamp(1, v.len()) - 1];
    let twm: Vec<f64> = per_run.iter().filter_map(|r| time_weighted_mean(r)).collect();
    let rep_means: Vec<f64> = per_run.iter().map(|r| r.iter().map(|x| x.1).sum::<f64>() / r.len() as f64).collect();
    SeriesStats {
        measuring_point: mp,
        elements,
        monitor,
        metric: metric.strip_suffix(" Tuple").unwrap_or(metric).to_string(),
        count: rows.len() as u64,
        mean,
        std_dev: var.sqrt(),
        min: v[0],
        max: v[v.len() - 1],
        p50: pct(0.5),
        p90: pct(0.9),
        p95: pct(0.95),
        p99: pct(0.99),
        time_weighted_mean: (!twm.is_empty()).then(|| twm.iter().sum::<f64>() / twm.len() as f64),
        ci_half_width: ci_half_width(&rep_means),
    }
}

fn time_weighted_mean(rows: &[(f64, f64)]) -> Option<f64> {
    let (t0, t1) = (rows.first()?.0, rows.last()?.0);
    (t1 > t0).then(|| rows.windows(2).map(|w| w[0].1 * (w[1].0 - w[0].0)).sum::<f64>() / (t1 - t0))
}

/// 95 % confidence half-width of the mean of `xs` (Student's t, n - 1 degrees of freedom).
fn ci_half_width(xs: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 {
        return None;
    }
    let m = xs.iter().sum::<f64>() / n as f64;
    let s = (xs.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n as f64 - 1.0)).sqrt();
    Some(t975(n - 1) * s / (n as f64).sqrt())
}

/// The 0.975 quantile of Student's t distribution.
fn t975(dof: usize) -> f64 {
    const T: [f64; 30] = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145, 2.131,
        2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042,
    ];
    match dof {
        0 => f64::NAN,
        1..=30 => T[dof - 1],
        31..=40 => 2.021,
        41..=60 => 2.000,
        61..=120 => 1.980,
        _ => 1.960,
    }
}

// ---------------------------------------------------------------------------------------------

/// A bounded, content-addressed cache whose entries are computed once: concurrent requests for a
/// key wait for the one computation. A failed computation is not kept.
struct Flights<K, V> {
    cells: Mutex<lru::LruCache<K, Arc<OnceCell<V>>>>,
}

impl<K: Hash + Eq + Clone, V: Clone> Flights<K, V> {
    fn new(capacity: usize) -> Self {
        Flights { cells: Mutex::new(lru::LruCache::new(NonZeroUsize::new(capacity.max(1)).unwrap())) }
    }

    async fn get(&self, key: K, compute: impl std::future::Future<Output = Result<V, String>>) -> Result<V, String> {
        let cell = {
            let mut cells = self.cells.lock().unwrap();
            cells.get_or_insert(key.clone(), || Arc::new(OnceCell::new())).clone()
        };
        let r = cell.get_or_try_init(|| compute).await.cloned();
        if r.is_err() {
            let mut cells = self.cells.lock().unwrap();
            if cells.peek(&key).is_some_and(|c| Arc::ptr_eq(c, &cell) && !c.initialized()) {
                cells.pop(&key);
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measuring_point_elements() {
        assert_eq!(key_elements("UsageScenarioMeasuringPoint[_us1]"), vec!["_us1"]);
        assert_eq!(key_elements("ActiveResourceMeasuringPoint[_prs1|replicaID=0]"), vec!["_prs1"]);
        assert_eq!(key_elements("SystemOperationMeasuringPoint[r|s|sys]"), vec!["r", "s", "sys"]);
        assert_eq!(key_elements("ResourceURIMeasuringPoint[null|Reconfigurations]"), vec!["Reconfigurations"]);
    }

    #[test]
    fn t_quantiles() {
        assert_eq!(t975(1), 12.706);
        assert_eq!(t975(1000), 1.96);
        assert!(ci_half_width(&[1.0]).is_none());
        let h = ci_half_width(&[1.0, 2.0, 3.0]).unwrap();
        assert!((h - 4.303 * 1.0 / 3f64.sqrt()).abs() < 1e-12);
    }
}
