//! The generic Palladio functions (`pcm*`): plain PCM in, numbers out. They know nothing above
//! PCM: the model's instances of other classes (decision layers, traces) are ignored, and every
//! parameter that says how to read the model (resource overrides, branch probabilities, assemblies
//! that are part of the communication, unavailabilities) comes from the script. The architecture
//! layers above PCM are realised in the model by model transformations (the domain project's
//! DESIGN.md §B); the `arch*` functions with their own translation stay as they were.
//!
//! - `pcmResources()`: offered load of every processing resource;
//! - `pcmScenarios()`: arrival rate and no-load response time of every usage scenario;
//! - `pcmWeightedVisits(actions, weights, exclude)`: the rate-weighted mean over the scenarios of
//!   Σ expected executions of an action per request × its weight;
//! - `pcmCallRates(transitions, probabilities)`: calls per time unit of every external and
//!   entry-level call, with the given probabilistic branch transitions overridden;
//! - `pcmFailure(containers, unavailabilities, transparent, exclude)`: the rate-weighted share of
//!   requests that need an unavailable container on their synchronous path;
//! - `pcmSimulate(overrides, ...)`, `pcmStat(overrides, ...)`: a simulation with the processing
//!   resources' replicas and rates overridden;
//! - `pcmMean(expr)`: the mean of a variable-free StoEx.

use crate::graph::{Graph, is_pcm};
use crate::load::{self, Loads};
use palladio_sim_service::protocol::{WireModel, WireValue};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The plain PCM of a model and its analysis, computed once per model.
pub struct PcmCache {
    pub plain: WireModel,
    pub graph: Graph,
    pub loads: Loads,
}

impl PcmCache {
    pub fn new(model: &WireModel) -> PcmCache {
        let plain = Graph::new(model).to_wire(|x| is_pcm(&x.class_name));
        let graph = Graph::new(&plain);
        let loads = load::analyse_pcm(&graph, true);
        PcmCache { plain, graph, loads }
    }

    fn entity(&self, name: &str) -> String {
        self.graph.entity(name)
    }

    fn excluded(&self, sc: &str, exclude: &[String]) -> bool {
        exclude.iter().any(|x| x == sc || *x == self.entity(sc))
    }

    /// Σ_o λ_o · Σ_a n_o(a) w(a) / Σ_o λ_o over the scenarios not excluded.
    pub fn weighted_visits(&self, weights: &[(String, f64)], exclude: &[String]) -> f64 {
        let (mut sum, mut rate) = (0.0, 0.0);
        for (sc, visits) in &self.loads.scenario_visits {
            if self.excluded(sc, exclude) {
                continue;
            }
            let r = self.loads.arrivals.get(sc).copied().unwrap_or(0.0);
            let mut x = 0.0;
            for (a, w) in weights {
                x += visits.get(a).copied().unwrap_or(0.0) * w;
            }
            sum += r * x;
            rate += r;
        }
        if rate > 0.0 { sum / rate } else { 0.0 }
    }

    /// The share of failed requests (D53's semantics on plain PCM): per scenario the probability
    /// p_o(k) that a request needs container k on its synchronous path, then
    /// Σ_o λ_o (1 − Π_k (1 − p_o(k) u_k)) / Σ_o λ_o.
    pub fn failure(&self, unavailable: &HashMap<String, f64>, transparent: &HashSet<String>, exclude: &[String]) -> (f64, Vec<String>) {
        let (deps, warnings) = load::sync_dependencies_pcm(&self.graph, transparent);
        let (mut sum, mut rate) = (0.0, 0.0);
        for (sc, acc) in deps {
            if self.excluded(&sc, exclude) {
                continue;
            }
            let ok: f64 = acc.iter().map(|(c, p)| 1.0 - p * unavailable.get(c).copied().unwrap_or(0.0)).product();
            let r = self.loads.arrivals.get(&sc).copied().unwrap_or(0.0);
            sum += r * (1.0 - ok);
            rate += r;
        }
        (if rate > 0.0 { sum / rate } else { 0.0 }, warnings)
    }

    /// Calls per time unit of every external call (caller, callee, action) and entry-level call
    /// (callee, call), with probabilistic branch transitions overridden.
    pub fn call_rates(&self, overrides: &[(String, f64)]) -> Loads {
        if overrides.is_empty() {
            return self.loads.clone();
        }
        let mut g = self.graph.clone();
        for (t, p) in overrides {
            if g.has(t) {
                g.set_attr(t, "branchProbability", WireValue::Double(*p));
            }
        }
        // with the per-action tracking, which also counts the entry-level calls
        load::analyse_pcm(&g, true)
    }

    /// The plain model with the replicas and rates of processing resources overridden:
    /// (resource spec, replicas, rate).
    pub fn with_overrides(&self, overrides: &[(String, i32, f64)]) -> WireModel {
        if overrides.is_empty() {
            return self.plain.clone();
        }
        let mut g = self.graph.clone();
        for (spec, n, rate) in overrides {
            if !g.has(spec) {
                continue;
            }
            g.set_attr(spec, "numberOfReplicas", WireValue::Int(*n));
            g.set_attr(spec, "processingRate", WireValue::String(lit(*rate)));
        }
        g.to_wire(|_| true)
    }
}

/// A number as a StoEx literal (as the translation writes them).
pub fn lit(x: f64) -> String {
    let t = format!("{x}");
    if t.contains('.') || t.contains('e') || t.contains("inf") || t.contains("NaN") { t } else { format!("{t}.0") }
}

/// Sorted keys, for deterministic output.
pub fn sorted_keys<V>(m: &BTreeMap<String, V>) -> Vec<&String> {
    m.keys().collect()
}
