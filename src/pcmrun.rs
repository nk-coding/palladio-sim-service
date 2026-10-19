//! Simulations of plain PCM with what the engine's runs (`palladio_sim_service::engine`) do not
//! offer: a per-scenario load profile (SimOxide's `LoadProfile`, LP-1) and usage streams (common
//! random numbers across designs, `SimConfig::usage_streams`, RND-11, fast mode). The model is
//! compiled by the engine (its cache); the runs go through SimOxide directly, with the engine's
//! limits and the same statistics per series.
//!
//! The plugin knows only PCM: a load profile is a list of steps per usage scenario (or for every
//! scenario), passed by the script (`pcmRun(overrides, profile, usageStreams, ...)`). Where it
//! comes from (layer 2's `LoadProfile`, a session mix) is the script's business. Times are in the
//! model's time unit, as the simulation's.

use palladio_sim_service::config::Config;
use palladio_sim_service::engine::{Outcome, Prepared, RunParams, SeriesStats, key_elements};
use simoxide_sim::{CompiledModel, Limits, LoadEntry, LoadProfile, Measurements, Outputs, RunSpec, SimErrorKind};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One step of a load profile: from `time` on, the arrival rate of `scenario` (every scenario
/// when `None`) is `factor` times the model's. Before a scenario's first step its factor is 1.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadStep {
    pub scenario: Option<String>,
    pub time: f64,
    pub factor: f64,
}

/// What a run does besides the engine's parameters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOptions {
    pub profile: Vec<LoadStep>,
    pub usage_streams: bool,
}

impl RunOptions {
    /// A canonical text of the options (cache key).
    pub fn key(&self) -> String {
        let mut s = format!("streams={};", self.usage_streams);
        for st in &self.profile {
            s += &format!("{:?}@{:e}x{:e};", st.scenario, st.time, st.factor);
        }
        s
    }
}

/// SimOxide's load profile of `steps`: one entry for the steps without a scenario (every
/// scenario), then one per named scenario (a later entry overrides the earlier for its scenario),
/// each sorted by time. `None` without steps.
pub fn load_profile(steps: &[LoadStep]) -> Result<Option<LoadProfile>, String> {
    if steps.is_empty() {
        return Ok(None);
    }
    let mut by: BTreeMap<Option<String>, Vec<(f64, f64)>> = BTreeMap::new();
    for s in steps {
        by.entry(s.scenario.clone()).or_default().push((s.time, s.factor));
    }
    let mut entries = Vec::new();
    for (sc, mut points) in by {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let Some(w) = points.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(format!("load profile: two steps at time {} for {}", w[0].0, sc.as_deref().unwrap_or("every scenario")));
        }
        entries.push(LoadEntry { scenarios: sc.into_iter().collect(), points });
    }
    let p = LoadProfile { entries };
    p.validate()?;
    Ok(Some(p))
}

/// The outcome of a run: the engine's statistics, and with `keep` the measurements of every
/// replication (with its end time in model units).
pub struct RunOutcome {
    pub outcome: Outcome,
    pub runs: Vec<(Measurements, f64)>,
}

/// Runs `cm` with `params` and `opts` (blocking; replications in parallel threads).
pub fn run_compiled(cm: &CompiledModel, monitors: &[(String, Option<String>, Vec<String>)], params: &RunParams, opts: &RunOptions, limits: &Limits, keep: bool) -> Result<RunOutcome, String> {
    run_with(cm, monitors, params, opts, limits, keep, |_| {})
}

/// [`run_compiled`] with a last change of each replication's configuration.
#[allow(clippy::too_many_arguments)]
pub fn run_with(
    cm: &CompiledModel,
    monitors: &[(String, Option<String>, Vec<String>)],
    params: &RunParams,
    opts: &RunOptions,
    limits: &Limits,
    keep: bool,
    tweak: impl Fn(&mut simoxide_sim::SimConfig) + Sync,
) -> Result<RunOutcome, String> {
    if params.max_measurements <= 0 && params.max_sim_time <= 0 {
        return Err("a simulation needs a stop condition: set maxMeasurements or maxSimTime".into());
    }
    if params.replications == 0 {
        return Err("replications must be at least 1".into());
    }
    let profile = load_profile(&opts.profile)?.map(Arc::new);
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
            cfg.load_profile = profile.clone();
            cfg.usage_streams = opts.usage_streams;
            tweak(&mut cfg);
            cfg
        })
        .collect();
    let results: Vec<Result<simoxide_sim::RunResult, String>> = std::thread::scope(|s| {
        let hs: Vec<_> = configs
            .into_iter()
            .map(|cfg| {
                s.spawn(move || {
                    simoxide_sim::run(cm, cfg, Outputs::default()).map_err(|e| match e.kind {
                        SimErrorKind::Limit => format!("the simulation hit a limit: {e}"),
                        _ => format!("the simulation failed (as SimuLizar would): {e}"),
                    })
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the simulation panicked".into()))).collect()
    });
    let runs: Vec<simoxide_sim::RunResult> = results.into_iter().collect::<Result<_, _>>()?;
    let outcome = aggregate(monitors, &runs);
    let kept = if keep { runs.into_iter().map(|r| (r.measurements, r.end_ns as f64 / 1e9)).collect() } else { Vec::new() };
    Ok(RunOutcome { outcome, runs: kept })
}

// ---------------------------------------------------------------------------------------------
// The runner of the `pcmRun*` functions: bounded concurrency, outcomes cached by content

type Cell = Arc<tokio::sync::OnceCell<Arc<RunOutcome>>>;

pub struct Runner {
    cells: Mutex<lru::LruCache<([u8; 32], RunParams, String), Cell>>,
    permits: tokio::sync::Semaphore,
    timeout_s: u64,
    max_events: u64,
}

/// The process's runner, sized like the engine (`SIM_THREADS`, `SIM_RESULT_CACHE_SIZE`,
/// `SIM_TIMEOUT_SECONDS`, `SIM_MAX_EVENTS`).
pub fn runner(config: &Config) -> &'static Runner {
    static R: OnceLock<Runner> = OnceLock::new();
    R.get_or_init(|| Runner {
        cells: Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(config.result_cache_size.max(1)).unwrap())),
        permits: tokio::sync::Semaphore::new(config.threads().max(1)),
        timeout_s: config.sim_timeout_seconds,
        max_events: config.sim_max_events,
    })
}

impl Runner {
    /// One of the runner's permits (one concurrent simulation thread).
    pub async fn acquire(&'static self) -> Result<tokio::sync::SemaphorePermit<'static>, String> {
        self.permits.acquire().await.map_err(|e| e.to_string())
    }

    pub fn limits(&self) -> Limits {
        Limits {
            max_events: self.max_events,
            deadline: (self.timeout_s > 0).then(|| Instant::now() + Duration::from_secs(self.timeout_s)),
            ..Limits::default()
        }
    }

    /// The outcome of `prepared` with `params` and `opts` (runs are deterministic, so a cached
    /// outcome is what running again would give).
    pub async fn simulate(&'static self, prepared: Arc<Prepared>, params: RunParams, opts: RunOptions) -> Result<Arc<RunOutcome>, String> {
        let key = (prepared.digest, params.clone(), opts.key());
        let cell = {
            let mut c = self.cells.lock().unwrap();
            c.get_or_insert(key.clone(), || Arc::new(tokio::sync::OnceCell::new())).clone()
        };
        let r = cell
            .get_or_try_init(|| async {
                let permits = params.replications.clamp(1, self.permits.available_permits().max(1) as u32);
                let _permit = self.permits.acquire_many(permits).await.map_err(|e| e.to_string())?;
                let limits = self.limits();
                let p = prepared.clone();
                tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_compiled(&p.compiled, &p.monitors, &params, &opts, &limits, false)))
                        .unwrap_or_else(|_| Err("the simulation panicked".into()))
                })
                .await
                .map_err(|e| format!("the simulation was abandoned: {e}"))?
                .map(Arc::new)
            })
            .await
            .cloned();
        if r.is_err() {
            let mut c = self.cells.lock().unwrap();
            if c.peek(&key).is_some_and(|x| Arc::ptr_eq(x, &cell) && !x.initialized()) {
                c.pop(&key);
            }
        }
        r
    }
}

// ---------------------------------------------------------------------------------------------
// Statistics per series: the engine's arithmetic (`palladio_sim_service::engine::aggregate`)

fn aggregate(monitors: &[(String, Option<String>, Vec<String>)], runs: &[simoxide_sim::RunResult]) -> Outcome {
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
                let i = r.measurements.series.iter().position(|d| *d.mp == *mp && d.metric == metric)?;
                Some(r.measurements.rows[i].as_slice()).filter(|rows| !rows.is_empty())
            })
            .collect();
        let elements = key_elements(&mp);
        let monitor = monitors
            .iter()
            .find(|(_, _, t)| !t.is_empty() && elements.iter().all(|e| t.contains(e)) && t.iter().all(|x| elements.contains(x)));
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

fn stats(mp: String, elements: Vec<String>, monitor: Option<String>, metric: &str, per_run: &[&[(f64, f64)]]) -> SeriesStats {
    let rows: Vec<(f64, f64)> = per_run.iter().flat_map(|r| r.iter().copied()).collect();
    let n = rows.len() as f64;
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

fn ci_half_width(xs: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 {
        return None;
    }
    let m = xs.iter().sum::<f64>() / n as f64;
    let s = (xs.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n as f64 - 1.0)).sqrt();
    Some(t975(n - 1) * s / (n as f64).sqrt())
}

fn t975(dof: usize) -> f64 {
    const T: [f64; 30] = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145, 2.131, 2.120, 2.110, 2.101, 2.093,
        2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042,
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
