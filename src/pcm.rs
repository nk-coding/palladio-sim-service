//! The plain PCM part of a model, held once per model: what `pcmEvaluate` simulates. It knows
//! nothing above PCM: the model's instances of other classes (decision layers, traces) are
//! ignored, and everything that says how to run the model (resource overrides, autoscaled
//! containers, load profile) comes from the script. The architecture layers
//! above PCM are realised in the model by model transformations (the domain project's
//! DESIGN.md §B).
//!
//! `pcmEvaluate(...)` ([`crate::evaluate`]) runs one simulation of it with SimOxide (with SPD
//! and the HPA when containers autoscale) and returns what it measured.

use crate::graph::{Graph, is_pcm};
use palladio_sim_service::protocol::{WireModel, WireValue};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The plain PCM of a model, computed once per model, and the evaluations run on it.
pub struct PcmCache {
    pub plain: WireModel,
    pub graph: Graph,
    /// A digest of `plain` (computed on first use): the key of the evaluations shared across
    /// models with the same PCM ([`crate::evaluate::Evaluator`]).
    digest: OnceLock<[u8; 32]>,
    /// The evaluations of this model, by request key.
    pub evals: Mutex<HashMap<String, Arc<crate::evaluate::Evaluation>>>,
}

impl PcmCache {
    pub fn new(model: &WireModel) -> PcmCache {
        let plain = Graph::new(model).to_wire(|x| is_pcm(&x.class_name));
        let graph = Graph::new(&plain);
        PcmCache { plain, graph, digest: OnceLock::new(), evals: Mutex::default() }
    }

    pub fn digest(&self) -> [u8; 32] {
        *self.digest.get_or_init(|| {
            let mut h = Sha256::new();
            h.update(format!("{:?}", self.plain).as_bytes());
            h.finalize().into()
        })
    }

    /// The usage scenario an action of the usage model belongs to.
    pub fn scenario_of(&self, action: &str) -> Option<String> {
        let mut cur = self.graph.container(action);
        while let Some(c) = cur {
            if self.graph.class(&c) == "UsageScenario" {
                return Some(c);
            }
            cur = self.graph.container(&c);
        }
        None
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

/// A number as a StoEx literal.
pub fn lit(x: f64) -> String {
    let t = format!("{x}");
    if t.contains('.') || t.contains('e') || t.contains("inf") || t.contains("NaN") { t } else { format!("{t}.0") }
}
