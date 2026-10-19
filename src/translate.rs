//! The completion: architecture decisions (layer 2, `architecture.mm`) → plain PCM (DESIGN.md
//! §4). The plugin knows PCM and layer 2 only: a domain layer (such as the HPA project's
//! microservices) is realised in the model by its own transformations, with PCM and the generic
//! completions of layer 2 (`Colocation`, `Work`, `Redirect`, `Notification`, `Replica`).
//!
//! Applied to a copy of the model, in this order:
//! 0. completions and calls that change hands: every `Colocation` allocates its assembly with its
//!    host, every `Work` becomes demands at its container's rates; an orchestrated `Workflow`
//!    moves its steps into the trigger's SEFF; across containers, a `Bypass` becomes the callee's
//!    own work and its direct calls, and a `ComposedCall` moves to its composer; a `Redirect`
//!    re-points its call; a `Notification` adds its CPU and an asynchronous fork with its
//!    deliveries after its action (a published one publishes once, its deliveries travel through
//!    the broker);
//! 1. mechanisms: `AsyncCall` wraps the call in an asynchronous fork (through the broker: the
//!    stub publishes on the caller's path and forks the rest), `CachedCall` in a probabilistic
//!    branch;
//! 2. remote calls: a stub (with the caller) and a skeleton (with the callee) on every assembly
//!    connector between two containers;
//! 3. replicas: n replicas of q cores become n·⌈q⌉ cores at speed q/⌈q⌉ (a quota: full speed
//!    when idle, slower with load); an autoscaled Deployable first gets the smallest n that keeps
//!    its offered load at its target utilisation (D50);
//! 4. nodes: the M/M/c slowdown of each node caps the speed of its CPUs.
//!
//! Every step works in a fixed order (decisions and connectors by name), and generated names
//! derive from the names of what they complete, so the result does not depend on the order of
//! the instances in the model.

use crate::graph::{Graph, is_pcm};
use crate::load::{self, Loads};
use palladio_sim_service::protocol::{WireModel, WireValue};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

mod quality;
pub use quality::{Quality, weighted};

/// The result of translating a model.
#[derive(Clone, Debug)]
pub struct Translation {
    /// The plain PCM model to simulate.
    pub pcm: WireModel,
    /// Offered loads of the translated model.
    pub loads: Loads,
    /// Expected calls per time unit between the original assembly contexts.
    pub calls: BTreeMap<(String, String), f64>,
    /// Container of every original assembly context (the placement).
    pub placement: BTreeMap<String, String>,
    pub nodes: Vec<NodeLoad>,
    pub cost: f64,
    pub modularity: f64,
    pub warnings: Vec<String>,
    /// Whether the model had any decision to translate.
    pub translated: bool,
    /// Per usage scenario: (arrival rate, no-load response time), both in model time units. The
    /// no-load time is the expected-value walk of D11 on the translated model without contention:
    /// replicas at their limit speed, a quota at full speed, no node slowdown.
    pub no_load: BTreeMap<String, (f64, f64)>,
    /// Replicas of every container with a Deployable, as simulated (fixed or autoscaled at the
    /// model's own workload, D50).
    pub replicas: BTreeMap<String, f64>,
    /// The analytic quality measures (D53–D55), as far as the model has their data.
    pub quality: Quality,
    pub ms_per_unit: f64,
}

#[derive(Clone, Debug)]
pub struct NodeLoad {
    pub name: String,
    pub cores: f64,
    /// Full-speed cores of work on the node, background included.
    pub load: f64,
    pub slowdown: f64,
}

impl Translation {
    /// The rate-weighted mean no-load response time over the usage scenarios, leaving out those
    /// `excluded` names (instance or entity names), in model time units.
    pub fn no_load_time(&self, excluded: &[String], entity: impl Fn(&str) -> String) -> f64 {
        let (mut sum, mut rate) = (0.0, 0.0);
        for (sc, (r, time)) in &self.no_load {
            if excluded.iter().any(|x| x == sc || *x == entity(sc)) {
                continue;
            }
            sum += r * time;
            rate += r;
        }
        if rate > 0.0 { sum / rate } else { 0.0 }
    }

    /// Utilisation of a container (the highest of its CPU and HDD resources), a node, or, for
    /// "" or "max", the highest of all.
    pub fn utilisation(&self, element: &str, entity: impl Fn(&str) -> String) -> Result<f64, String> {
        let node_u = |n: &NodeLoad| if n.cores > 0.0 { n.load / n.cores } else { 0.0 };
        if element.is_empty() || element == "max" {
            let r = self.loads.resources.iter().map(|r| r.utilisation()).fold(0.0, f64::max);
            return Ok(self.nodes.iter().map(node_u).fold(r, f64::max));
        }
        let name = element.strip_prefix("node:").unwrap_or(element);
        if let Some(n) = self.nodes.iter().find(|n| n.name == name) {
            return Ok(node_u(n));
        }
        let matching: Vec<f64> = self
            .loads
            .resources
            .iter()
            .filter(|r| r.container == element || entity(&r.container) == element || r.spec == element)
            .map(|r| r.utilisation())
            .collect();
        if matching.is_empty() {
            let mut known: Vec<String> = self.loads.resources.iter().map(|r| entity(&r.container)).collect();
            known.dedup();
            known.extend(self.nodes.iter().map(|n| format!("node:{}", n.name)));
            return Err(format!("no container or node '{element}'; known: {}", known.join(", ")));
        }
        Ok(matching.into_iter().fold(0.0, f64::max))
    }

    /// C1 (D55): Σ over the request types with a LatencyBudget of max(0, no-load time / budget − 1).
    pub fn latency_violation(&self) -> f64 {
        self.quality
            .budgets
            .iter()
            .map(|(sc, ms)| {
                let t = self.no_load.get(sc).map_or(0.0, |x| x.1) * self.ms_per_unit;
                if *ms > 0.0 { (t / ms - 1.0).max(0.0) } else { 0.0 }
            })
            .sum()
    }

    /// C2 (D55): the highest utilisation of the PeakCapacity containers at its load factor
    /// (offered loads scale linearly with the load), and its limit.
    pub fn peak_utilisation(&self) -> Option<(f64, f64)> {
        let (factor, limit, containers) = self.quality.peak.as_ref()?;
        let u = self
            .loads
            .resources
            .iter()
            .filter(|r| r.kind != "DELAY" && containers.contains(&r.container))
            .map(|r| r.utilisation() * factor)
            .fold(0.0, f64::max);
        Some((u, *limit))
    }

    /// Replicas of a container with a Deployable, or for "" the sum over the Deployables whose
    /// container hosts a component of the model.
    pub fn replicas_of(&self, element: &str, entity: impl Fn(&str) -> String) -> Result<f64, String> {
        if element.is_empty() {
            let used: BTreeSet<&String> = self.placement.values().collect();
            return Ok(self.replicas.iter().filter(|(c, _)| used.contains(c)).map(|(_, n)| n).sum());
        }
        self.replicas
            .iter()
            .find(|(c, _)| c.as_str() == element || entity(c) == element)
            .map(|(_, n)| *n)
            .ok_or_else(|| format!("no container with a Deployable named '{element}'"))
    }
}

// ---------------------------------------------------------------------------------------------
// Decisions

#[derive(Clone, Debug)]
enum Mechanism {
    Sync,
    Async,
    Cached { hit_ratio: f64, hit_cpu_ms: f64 },
    /// Across containers: made by the composer; `cache` = (hit ratio, hit CPU ms) at the composer.
    Composed { compose_cpu_ms: f64, cache: Option<(f64, f64)> },
    /// Across containers: the callee's own work and direct calls, made by the caller.
    Bypass { latency_ms: f64 },
    /// A mechanism class of another layer: the call as modelled (that layer realises the
    /// mechanism with the completions).
    Other,
}

#[derive(Clone, Debug)]
struct Deployable {
    container: String,
    /// Replicas at the model's own workload: fixed, or set by the autoscaler (D50).
    replicas: f64,
    cpu_per_replica: f64,
    sidecar_cores: f64,
    quota: bool,
    autoscale: Option<Autoscale>,
    /// (weight, replicas) per load level, for the cost: one level of weight 1 unless autoscaled.
    levels: Vec<(f64, f64)>,
}

/// An ideal autoscaler (D50).
#[derive(Clone, Debug)]
struct Autoscale {
    target: f64,
    max: Option<f64>,
    /// (factor, weight) per load level; the model's workload alone when none is given.
    levels: Vec<(f64, f64)>,
    /// The HPA settings of a dynamic run (`translate_dynamic`): minReplicas, the start-up delay
    /// of a new pod (s) and the scale-down stabilisation window (s); absent: the defaults.
    min: Option<f64>,
    startup_s: Option<f64>,
    scale_down_s: Option<f64>,
}

impl Autoscale {
    fn read(g: &Graph, a: &str) -> Result<Autoscale, String> {
        let target = num(g, a, "targetUtilisation")?;
        if !(target > 0.0 && target <= 1.0) {
            return Err(format!("{a}: targetUtilisation {target} must be in (0, 1]"));
        }
        let max = g.num_attr(a, "maxReplicas");
        if let Some(m) = max
            && m < 1.0
        {
            return Err(format!("{a}: maxReplicas must be at least 1"));
        }
        let mut levels = Vec::new();
        for l in sorted(g.refs(a, "levels")) {
            let (f, w) = (num(g, &l, "factor")?, num(g, &l, "weight")?);
            if !(f > 0.0) || !(w >= 0.0) {
                return Err(format!("{l}: needs a positive factor and a weight of at least 0"));
            }
            levels.push((f, w));
        }
        if levels.is_empty() {
            levels.push((1.0, 1.0));
        }
        if !(levels.iter().map(|x| x.1).sum::<f64>() > 0.0) {
            return Err(format!("{a}: the weights of its levels sum to 0"));
        }
        let min = g.num_attr(a, "minReplicas");
        if let Some(m) = min
            && !(m >= 1.0)
        {
            return Err(format!("{a}: minReplicas must be at least 1"));
        }
        let startup_s = g.num_attr(a, "startupSeconds");
        let scale_down_s = g.num_attr(a, "scaleDownSeconds");
        for (k, v) in [("startupSeconds", startup_s), ("scaleDownSeconds", scale_down_s)] {
            if let Some(v) = v
                && !(v >= 0.0)
            {
                return Err(format!("{a}: {k} must not be negative"));
            }
        }
        Ok(Autoscale { target, max: max.map(f64::floor), levels, min: min.map(f64::floor), startup_s, scale_down_s })
    }

    /// The smallest replica count n ≥ 1 with f·u/(n·q) ≤ target and f·u_s/(n·s) ≤ target, capped
    /// at maxReplicas; whether it was capped.
    fn count(&self, f: f64, u: f64, q: f64, us: f64, s: f64) -> (f64, bool) {
        let need = (f * u / (q * self.target)).max(f * us / (s * self.target));
        let n = (need - 1e-9).ceil().max(1.0);
        match self.max {
            Some(m) if n > m => (m, true),
            _ => (n, false),
        }
    }
}

#[derive(Clone, Debug)]
struct Workflow {
    name: String,
    trigger: String,
    steps: Vec<String>,
    /// `Some(stepCpuMs)` when orchestrated.
    orchestrated: Option<f64>,
}

/// A `Notification`: work after an action, in the background (layer 2, completions).
#[derive(Clone, Debug)]
struct Notification {
    name: String,
    after: String,
    cpu_ms: f64,
    published: bool,
    /// (provider assembly, signature), by the signature's entity name
    deliveries: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct RemoteCalls {
    caller_cpu_ms: f64,
    callee_cpu_ms: f64,
    latency_ms: f64,
    publish_cpu_ms: f64,
    deliver_cpu_ms: f64,
    sidecar: bool,
    broker: Option<String>,
    /// Containers reached without the middleware (no stub and skeleton).
    direct: BTreeSet<String>,
}

/// A time-varying load of the dynamic runs (`LoadProfile`, layer 2): steps of a factor on every
/// usage scenario's load, in seconds; objectives count from `warm_up` to `end`; a request
/// violates its service level above `slo_factor` times its latency budget.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadProfileSpec {
    pub steps: Vec<(f64, f64)>,
    pub warm_up: f64,
    pub end: f64,
    pub slo_factor: f64,
}

impl LoadProfileSpec {
    /// The factor at time `t` (s): the last step at or before it, 1 before the first.
    pub fn factor_at(&self, t: f64) -> f64 {
        self.steps.iter().take_while(|s| s.0 <= t).last().map_or(1.0, |s| s.1)
    }

    fn read(g: &Graph, p: &str) -> Result<LoadProfileSpec, String> {
        let mut steps = Vec::new();
        for st in g.refs(p, "steps") {
            steps.push((num(g, &st, "seconds")?, num(g, &st, "factor")?));
        }
        steps.sort_by(|a, b| a.0.total_cmp(&b.0));
        for w in steps.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(format!("{p}: two steps at {} s", w[0].0));
            }
        }
        if steps.iter().any(|s| !(s.0 >= 0.0) || !(s.1 > 0.0)) {
            return Err(format!("{p}: a step needs a time of at least 0 and a positive factor"));
        }
        let spec = LoadProfileSpec {
            steps,
            warm_up: num(g, p, "warmUpSeconds")?,
            end: num(g, p, "endSeconds")?,
            slo_factor: num(g, p, "sloFactor")?,
        };
        if !(spec.end > spec.warm_up && spec.warm_up >= 0.0 && spec.slo_factor > 0.0) {
            return Err(format!("{p}: needs 0 ≤ warmUpSeconds < endSeconds and a positive sloFactor"));
        }
        Ok(spec)
    }
}

struct Decisions {
    ms_per_unit: f64,
    /// The load profile of the dynamic runs, when the model has one.
    profile: Option<LoadProfileSpec>,
    sites: Vec<(String, String, Mechanism)>, // (call site, call, mechanism)
    /// call → the composer's call after which a ComposedCall makes it
    composers: BTreeMap<String, String>,
    remote: Option<RemoteCalls>,
    deployables: Vec<Deployable>,
    workflows: Vec<Workflow>,
    /// (assembly, host assembly)
    colocations: Vec<(String, String)>,
    /// (InternalAction, cpuMs, hddMs)
    work: Vec<(String, f64, f64)>,
    /// call → (provider assembly, signature)
    redirects: BTreeMap<String, (String, String)>,
    notifications: Vec<Notification>,
    nodes: Vec<(String, f64, f64, Vec<String>)>, // (name, cores, background, containers)
    cost: Option<[f64; 4]>,                  // perContainer, perCore, perReplicaOverhead, perCache
    prices: BTreeMap<String, f64>,           // container → fixed price (ContainerPrice)
    any: bool,
    /// The quality model (D53–D55): data, not decisions.
    q: QualityData,
}

/// The data of the quality model (layer 2 `Reliability`, `ChangeUnit`, ...).
#[derive(Clone, Debug, Default)]
struct QualityData {
    /// changeFailureRate, restoreHours, periodHours, processUnavailability,
    /// serverUnavailability, tenantShare
    reliability: Option<[f64; 6]>,
    /// (unit, changes, assemblies)
    units: Vec<(String, f64, Vec<String>)>,
    contract_break: f64,
    /// call → breakProbability of its site's mechanism
    breaks: BTreeMap<String, f64>,
    /// call → couplingWeight of its site's mechanism (modularity)
    coupling: BTreeMap<String, f64>,
    /// call → staleReads of its site's mechanism (inconsistency, across containers)
    stale: BTreeMap<String, f64>,
    /// (replica, source) of every Replica
    replicas: Vec<(String, String)>,
    /// calls whose site writes
    writes: BTreeSet<String>,
    /// syncWriteWeight, asyncWriteWeight
    consistency: Option<(f64, f64)>,
    /// (scenario, budget in ms)
    budgets: Vec<(String, f64)>,
    /// (factor, limit, containers)
    peak: Option<(f64, f64, Vec<String>)>,
}

fn num(g: &Graph, x: &str, key: &str) -> Result<f64, String> {
    g.num_attr(x, key).ok_or_else(|| format!("{x} ({}) has no {key}", g.class(x)))
}

fn one(g: &Graph, x: &str, feature: &str) -> Result<String, String> {
    g.ref1(x, feature).ok_or_else(|| format!("{x} ({}) has no {feature}", g.class(x)))
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// The caller's assembly and the providing assembly of a call, through the connector of its
/// role from the first assembly of the caller's component.
fn call_target(g: &Graph, call: &str) -> Option<(String, String)> {
    let mut comp = call.to_string();
    for _ in 0..64 {
        comp = g.container(&comp)?;
        if g.is_a(&comp, "BasicComponent") {
            break;
        }
    }
    let asm = sorted(g.referrers(&comp, "encapsulatedComponent")).into_iter().next()?;
    let role = g.ref1(call, "role")?;
    let conn = g
        .referrers(&asm, "requiringAssemblyContext")
        .into_iter()
        .find(|c| g.ref1(c, "requiredRole").as_deref() == Some(role.as_str()))?;
    Some((asm, g.ref1(&conn, "providingAssemblyContext")?))
}

/// The container an assembly is allocated to.
fn allocated(g: &Graph, asm: &str) -> Option<String> {
    g.referrers(asm, "assemblyContext")
        .into_iter()
        .find(|a| g.class(a) == "AllocationContext")
        .and_then(|a| g.ref1(&a, "resourceContainer"))
}

impl Decisions {
    fn read(g: &Graph, warnings: &mut Vec<String>) -> Result<Decisions, String> {
        let roots = g.all("ArchitectureDecisions");
        if roots.len() > 1 {
            return Err(format!("the model has {} ArchitectureDecisions; keep one", roots.len()));
        }
        let ms_per_unit = match roots.first() {
            Some(r) => num(g, r, "msPerTimeUnit")?,
            None => 1000.0,
        };
        if !(ms_per_unit > 0.0) {
            return Err("ArchitectureDecisions.msPerTimeUnit must be positive".into());
        }
        let mechanism = |m: &str| -> Result<Mechanism, String> {
            Ok(match g.class(m) {
                "SyncCall" => Mechanism::Sync,
                "AsyncCall" => Mechanism::Async,
                "CachedCall" => {
                    let h = num(g, m, "hitRatio")?;
                    if !(0.0..=1.0).contains(&h) {
                        return Err(format!("{m}: hitRatio {h} is not a probability"));
                    }
                    Mechanism::Cached { hit_ratio: h, hit_cpu_ms: num(g, m, "hitCpuMs")? }
                }
                "ComposedCall" => {
                    let cache = match g.num_attr(m, "hitRatio") {
                        Some(h) if h > 0.0 => {
                            if h > 1.0 {
                                return Err(format!("{m}: hitRatio {h} is not a probability"));
                            }
                            Some((h, g.num_attr(m, "hitCpuMs").unwrap_or(0.0)))
                        }
                        _ => None,
                    };
                    Mechanism::Composed { compose_cpu_ms: num(g, m, "composeCpuMs")?, cache }
                }
                "Bypass" => Mechanism::Bypass { latency_ms: num(g, m, "latencyMs")? },
                // a subclass of Mechanism from another layer: the call as modelled
                _ => Mechanism::Other,
            })
        };
        let mut q = QualityData::read(g)?;
        let mut sites = Vec::new();
        let mut composers = BTreeMap::new();
        let mut seen_calls = BTreeSet::new();
        for cs in sorted(g.all("CallSite")) {
            let call = one(g, &cs, "call")?;
            if !g.is_a(&call, "ExternalCallAction") {
                return Err(format!("{cs}.call is a {}, not an ExternalCallAction", g.class(&call)));
            }
            if !seen_calls.insert(call.clone()) {
                return Err(format!("{call} has two CallSites"));
            }
            let m = one(g, &cs, "mechanism")?;
            if !g.refs(&cs, "allowed").contains(&m) {
                warnings.push(format!("{cs}: mechanism {m} is not among the allowed ones"));
            }
            let mech = mechanism(&m)?;
            if let Some(b) = g.num_attr(&m, "breakProbability") {
                q.breaks.insert(call.clone(), b);
            }
            if let Some(w) = g.num_attr(&m, "couplingWeight") {
                q.coupling.insert(call.clone(), w);
            }
            if let Some(x) = g.num_attr(&m, "staleReads") {
                if !(0.0..=1.0).contains(&x) {
                    return Err(format!("{m}: staleReads {x} is not a share"));
                }
                q.stale.insert(call.clone(), x);
            }
            if g.bool_attr(&cs, "writes").unwrap_or(false) {
                q.writes.insert(call.clone());
            }
            match (g.ref1(&cs, "composer"), &mech) {
                (Some(c), _) => {
                    composers.insert(call.clone(), c);
                }
                (None, Mechanism::Composed { .. }) => {
                    warnings.push(format!("{cs}: a ComposedCall needs a composer; the call stays with its caller"));
                }
                _ => {}
            }
            sites.push((cs.clone(), call, mech));
        }
        let rcs = g.all("RemoteCalls");
        if rcs.len() > 1 {
            return Err("the model has more than one RemoteCalls".into());
        }
        let remote = match rcs.first() {
            None => None,
            Some(r) => Some(RemoteCalls {
                caller_cpu_ms: num(g, r, "callerCpuMs")?,
                callee_cpu_ms: num(g, r, "calleeCpuMs")?,
                latency_ms: num(g, r, "latencyMs")?,
                publish_cpu_ms: num(g, r, "publishCpuMs")?,
                deliver_cpu_ms: num(g, r, "deliverCpuMs")?,
                sidecar: g.bool_attr(r, "sidecar").unwrap_or(false),
                broker: g.ref1(r, "broker"),
                direct: g.refs(r, "direct").into_iter().collect(),
            }),
        };
        let mut deployables = Vec::new();
        let mut seen = BTreeSet::new();
        for d in sorted(g.all("Deployable")) {
            let c = one(g, &d, "container")?;
            if !seen.insert(c.clone()) {
                return Err(format!("container {c} has two Deployables"));
            }
            let autoscale = match g.ref1(&d, "autoscaling") {
                Some(a) => Some(Autoscale::read(g, &a)?),
                None => None,
            };
            let n = match (&autoscale, g.ref1(&d, "replicas")) {
                (Some(_), _) => 1.0,
                (None, Some(opt)) => {
                    if !g.refs(&d, "allowedReplicas").contains(&opt) {
                        warnings.push(format!("{d}: replica option {opt} is not among the allowed ones"));
                    }
                    num(g, &opt, "count")?
                }
                (None, None) => return Err(format!("{d}: needs replicas or an autoscaling")),
            };
            let q = num(g, &d, "cpuPerReplica")?;
            if n < 1.0 || !(q > 0.0) {
                return Err(format!("{d}: needs at least one replica and a positive cpuPerReplica"));
            }
            let sc = g.num_attr(&d, "sidecarCores").unwrap_or(1.0);
            if !(sc >= 1.0) {
                return Err(format!("{d}: sidecarCores must be at least 1"));
            }
            deployables.push(Deployable {
                container: c,
                replicas: n.floor(),
                cpu_per_replica: q,
                sidecar_cores: sc.floor(),
                quota: g.bool_attr(&d, "quota").unwrap_or(false),
                autoscale,
                levels: vec![(1.0, n.floor())],
            });
        }
        let mut workflows = Vec::new();
        for w in sorted(g.all("Workflow")) {
            let style = one(g, &w, "style")?;
            if !g.refs(&w, "allowed").contains(&style) {
                warnings.push(format!("{w}: style {style} is not among the allowed ones"));
            }
            let orchestrated = match g.class(&style) {
                "Orchestrated" => Some(num(g, &style, "stepCpuMs")?),
                "Choreographed" => None,
                other => return Err(format!("{style}: unknown coordination class {other}")),
            };
            let steps = sorted(g.refs(&w, "steps"));
            for s in &steps {
                if !g.is_a(s, "ExternalCallAction") {
                    return Err(format!("{w}: step {s} is a {}, not an ExternalCallAction", g.class(s)));
                }
            }
            workflows.push(Workflow { name: w.clone(), trigger: one(g, &w, "trigger")?, steps, orchestrated });
        }
        let mut colocations = Vec::new();
        for c in sorted(g.all("Colocation")) {
            colocations.push((one(g, &c, "assembly")?, one(g, &c, "host")?));
        }
        let mut work = Vec::new();
        for w in sorted(g.all("Work")) {
            let a = one(g, &w, "action")?;
            if !g.is_a(&a, "InternalAction") {
                return Err(format!("{w}.action is a {}, not an InternalAction", g.class(&a)));
            }
            let (cpu, hdd) = (g.num_attr(&w, "cpuMs").unwrap_or(0.0), g.num_attr(&w, "hddMs").unwrap_or(0.0));
            if !(cpu >= 0.0 && hdd >= 0.0) {
                return Err(format!("{w}: cpuMs and hddMs must not be negative"));
            }
            work.push((a, cpu, hdd));
        }
        let mut redirects = BTreeMap::new();
        for r in sorted(g.all("Redirect")) {
            let call = one(g, &r, "call")?;
            if !g.is_a(&call, "ExternalCallAction") {
                return Err(format!("{r}.call is a {}, not an ExternalCallAction", g.class(&call)));
            }
            if redirects.insert(call.clone(), (one(g, &r, "provider")?, one(g, &r, "service")?)).is_some() {
                return Err(format!("{call} has two Redirects"));
            }
        }
        let mut notifications = Vec::new();
        for n in sorted(g.all("Notification")) {
            let after = one(g, &n, "after")?;
            if !(g.is_a(&after, "ExternalCallAction") || g.is_a(&after, "InternalAction")) {
                return Err(format!("{n}.after is a {}, not an ExternalCallAction or InternalAction", g.class(&after)));
            }
            let mut deliveries = Vec::new();
            for d in sorted(g.refs(&n, "deliveries")) {
                deliveries.push((one(g, &d, "provider")?, one(g, &d, "service")?, d));
            }
            if deliveries.is_empty() {
                return Err(format!("{n}: a Notification needs at least one Delivery"));
            }
            deliveries.sort_by(|a, b| (g.entity(&a.1), &a.2).cmp(&(g.entity(&b.1), &b.2)));
            notifications.push(Notification {
                name: n.clone(),
                after,
                cpu_ms: g.num_attr(&n, "cpuMs").unwrap_or(0.0),
                published: g.bool_attr(&n, "published").unwrap_or(false),
                deliveries: deliveries.into_iter().map(|(p, s, _)| (p, s)).collect(),
            });
        }
        // published ones first: rule 0 inserts each right after its action, so a projection-like
        // one (CPU on the path) ends up before the forks of the published ones, as before H10
        notifications.sort_by_key(|n| !n.published);
        for r in sorted(g.all("Replica")) {
            q.replicas.push((one(g, &r, "replica")?, one(g, &r, "source")?));
        }
        let mut nodes = Vec::new();
        for n in sorted(g.all("Node")) {
            nodes.push((n.clone(), num(g, &n, "cores")?, g.num_attr(&n, "backgroundCores").unwrap_or(0.0), sorted(g.refs(&n, "containers"))));
        }
        let cost = match g.all("CostModel").first() {
            None => None,
            Some(c) => Some([
                num(g, c, "perContainer")?,
                num(g, c, "perCore")?,
                num(g, c, "perReplicaOverhead")?,
                num(g, c, "perCache")?,
            ]),
        };
        let mut prices = BTreeMap::new();
        for p in sorted(g.all("ContainerPrice")) {
            let price = num(g, &p, "price")?;
            for c in g.refs(&p, "containers") {
                if prices.insert(c.clone(), price).is_some() {
                    return Err(format!("container {c} has two ContainerPrices"));
                }
            }
        }
        for p in sorted(g.all("Placement")) {
            let a = one(g, &p, "allocation")?;
            if let Some(c) = g.ref1(&a, "resourceContainer")
                && !g.refs(&p, "candidates").contains(&c)
            {
                warnings.push(format!("{p}: the component sits on {c}, which is not a candidate"));
            }
        }
        let any = !sites.is_empty()
            || remote.is_some()
            || !deployables.is_empty()
            || !nodes.is_empty()
            || !workflows.is_empty()
            || !colocations.is_empty()
            || !work.is_empty()
            || !redirects.is_empty()
            || !notifications.is_empty();
        let profile = match g.all("LoadProfile").as_slice() {
            [] => None,
            [p] => Some(LoadProfileSpec::read(g, p)?),
            more => return Err(format!("the model has {} LoadProfiles; keep one", more.len())),
        };
        Ok(Decisions {
            ms_per_unit,
            profile,
            sites,
            composers,
            remote,
            deployables,
            workflows,
            colocations,
            work,
            redirects,
            notifications,
            nodes,
            cost,
            prices,
            any,
            q,
        })
    }
}

impl QualityData {
    fn read(g: &Graph) -> Result<QualityData, String> {
        let first = |class: &str| g.all(class).into_iter().next();
        let reliability = match first("Reliability") {
            None => None,
            Some(r) => Some([
                num(g, &r, "changeFailureRate")?,
                num(g, &r, "restoreHours")?,
                num(g, &r, "periodHours")?,
                num(g, &r, "processUnavailability")?,
                num(g, &r, "serverUnavailability")?,
                num(g, &r, "tenantShare")?,
            ]),
        };
        if let Some(r) = &reliability
            && !(r[2] > 0.0)
        {
            return Err("Reliability.periodHours must be positive".into());
        }
        let mut units = Vec::new();
        for u in sorted(g.all("ChangeUnit")) {
            units.push((u.clone(), num(g, &u, "changes")?, sorted(g.refs(&u, "components"))));
        }
        let contract_break = match first("ChangeCoupling") {
            Some(c) => num(g, &c, "contractBreak")?,
            None => 0.15,
        };
        let consistency = match first("Consistency") {
            Some(c) => Some((num(g, &c, "syncWriteWeight")?, num(g, &c, "asyncWriteWeight")?)),
            None => None,
        };
        let mut budgets = Vec::new();
        for b in sorted(g.all("LatencyBudget")) {
            budgets.push((one(g, &b, "scenario")?, num(g, &b, "ms")?));
        }
        let peak = match first("PeakCapacity") {
            Some(c) => Some((num(g, &c, "factor")?, num(g, &c, "limit")?, sorted(g.refs(&c, "containers")))),
            None => None,
        };
        Ok(QualityData {
            reliability,
            units,
            contract_break,
            breaks: BTreeMap::new(),
            coupling: BTreeMap::new(),
            stale: BTreeMap::new(),
            replicas: Vec::new(),
            writes: BTreeSet::new(),
            consistency,
            budgets,
            peak,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Building PCM

struct Builder {
    g: Graph,
    ms: f64,
    repo: String,
    system: String,
    allocation: String,
    resenv: String,
    /// Full-speed CPU and HDD rate of every original container (before rules 3 and 4).
    base_cpu: HashMap<String, f64>,
    base_hdd: HashMap<String, f64>,
    sidecars: BTreeMap<String, String>,
    replicas: HashMap<String, f64>,
    sidecar_cores: HashMap<String, f64>,
    /// Required roles whose calls are messages, and how (rule 2 builds their stubs).
    roles: BTreeMap<String, RoleKind>,
    /// Roles created by `role_to`, by (component, interface, provider, kind).
    wired: BTreeMap<(String, String, String, Option<RoleKind>), String>,
    /// Assemblies that run on their caller's sidecar (publishers): connectors into them get no
    /// stub.
    local_assemblies: BTreeSet<String>,
    /// Publisher assembly per container.
    publishers: BTreeMap<String, (String, String)>,
    /// Calls an orchestrator makes (rule 1 leaves them alone).
    orchestrated: BTreeSet<String>,
    /// Quota containers: (container, replicas, cpu per replica).
    quota: Vec<(String, f64, f64)>,
    /// Offered load of every autoscaled container and of its sidecar (full-speed cores, rule 3a).
    offered: BTreeMap<String, (f64, f64)>,
    warnings: Vec<String>,
}

/// How the calls through a role travel as messages (rule 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RoleKind {
    /// An asynchronous call: publish on the caller's path, the rest in the background.
    Async,
    /// A delivery of a published Notification: published once by the sender's publisher, then
    /// broker, latency, call.
    Published,
    /// An orchestrated asynchronous step: request and reply through the broker, the caller waits.
    Reply,
}

fn s(v: impl Into<String>) -> WireValue {
    WireValue::String(v.into())
}

/// A number as a StoEx literal.
fn lit(x: f64) -> String {
    let t = format!("{x}");
    if t.contains('.') || t.contains('e') { t } else { format!("{t}.0") }
}

impl Builder {
    fn named(&mut self, base: &str, class: &str, entity: Option<&str>) -> String {
        let n = self.g.create(base, class);
        if let Some(e) = entity {
            self.g.set_attr(&n, "entityName", s(e));
        }
        n
    }

    fn container_of(&self, asm: &str) -> Option<String> {
        self.g
            .referrers(asm, "assemblyContext")
            .into_iter()
            .find(|a| self.g.class(a) == "AllocationContext")
            .and_then(|a| self.g.ref1(&a, "resourceContainer"))
    }

    fn assemblies_of(&self, comp: &str) -> Vec<String> {
        sorted(self.g.referrers(comp, "encapsulatedComponent"))
    }

    /// The BasicComponent whose SEFF contains `action`.
    fn component_of(&self, action: &str) -> Option<String> {
        let mut cur = action.to_string();
        for _ in 0..64 {
            let up = self.g.container(&cur)?;
            if self.g.is_a(&up, "BasicComponent") {
                return Some(up);
            }
            cur = up;
        }
        None
    }

    fn cpu_units(&self, container: &str, ms: f64) -> Result<f64, String> {
        let rate = self.base_cpu.get(container).ok_or_else(|| format!("container {} has no CPU", self.g.entity(container)))?;
        Ok(ms / self.ms * rate)
    }

    fn hdd_units(&self, container: &str, ms: f64) -> Result<f64, String> {
        let rate = self.base_hdd.get(container).ok_or_else(|| format!("container {} has no HDD", self.g.entity(container)))?;
        Ok(ms / self.ms * rate)
    }

    fn demand(&mut self, action: &str, kind: &str, units: f64, exponential: bool) {
        let d = self.named(&format!("{action}_{}", kind.to_lowercase()), "ParametricResourceDemand", None);
        let spec = if exponential { format!("Exp({})", lit(1.0 / units)) } else { lit(units) };
        self.g.set_attr(&d, "specification", s(spec));
        self.g.set_attr(&d, "requiredResource", s(kind));
        self.g.link(action, "resourceDemand", &d);
    }

    /// An InternalAction with exponential CPU/HDD demands and a constant DELAY.
    fn internal(&mut self, prefix: &str, local: &str, demands: &[(&str, f64)]) -> String {
        let a = self.named(&format!("{prefix}_{local}"), "InternalAction", Some(local));
        for &(kind, units) in demands {
            if units > 0.0 {
                self.demand(&a, kind, units, kind != "DELAY");
            }
        }
        a
    }

    /// Puts `actions` into behaviour `beh` between a new start and stop action.
    fn fill(&mut self, beh: &str, actions: &[String]) {
        let start = self.named(&format!("{beh}_start"), "StartAction", None);
        let stop = self.named(&format!("{beh}_stop"), "StopAction", None);
        let mut chain = vec![start];
        chain.extend(actions.iter().cloned());
        chain.push(stop);
        for a in &chain {
            self.g.link(beh, "steps", a);
        }
        for w in chain.windows(2) {
            self.g.link(&w[0], "successor", &w[1]);
        }
    }

    /// Puts `new` in the place of `old` in its behaviour's chain; `old` leaves the behaviour.
    fn replace_in_chain(&mut self, old: &str, new: &str) -> Result<(), String> {
        let beh = self.g.container(old).ok_or_else(|| format!("{old} is in no behaviour"))?;
        let pred = self.g.ref1(old, "predecessor");
        let succ = self.g.ref1(old, "successor");
        if let Some(p) = &pred {
            self.g.unlink(p, "successor", old);
        }
        if let Some(n) = &succ {
            self.g.unlink(old, "successor", n);
        }
        self.g.unlink(&beh, "steps", old);
        self.g.link(&beh, "steps", new);
        if let Some(p) = &pred {
            self.g.link(p, "successor", new);
        }
        if let Some(n) = &succ {
            self.g.link(new, "successor", n);
        }
        Ok(())
    }

    /// Inserts `new` right after `after` in its behaviour.
    fn insert_after(&mut self, after: &str, new: &str) -> Result<(), String> {
        let beh = self.g.container(after).ok_or_else(|| format!("{after} is in no behaviour"))?;
        let succ = self.g.ref1(after, "successor");
        if let Some(n) = &succ {
            self.g.unlink(after, "successor", n);
            self.g.link(new, "successor", n);
        }
        self.g.link(&beh, "steps", new);
        self.g.link(after, "successor", new);
        Ok(())
    }

    /// The SEFF that `action` belongs to (through loops, branches and forks).
    fn seff_of(&self, action: &str) -> Option<String> {
        let mut cur = action.to_string();
        for _ in 0..64 {
            let up = self.g.container(&cur)?;
            if self.g.class(&up) == "ResourceDemandingSEFF" {
                return Some(up);
            }
            cur = up;
        }
        None
    }

    /// The action of its SEFF's own chain that holds `action`: itself, or the loop, branch or fork
    /// around it.
    fn top_level(&self, action: &str) -> Option<String> {
        let seff = self.seff_of(action)?;
        let mut cur = action.to_string();
        for _ in 0..64 {
            let up = self.g.container(&cur)?;
            if up == seff {
                return Some(cur);
            }
            cur = up;
        }
        None
    }

    /// The position of `action` in its behaviour's chain (0 = the start action).
    fn chain_index(&self, action: &str) -> usize {
        let mut n = 0;
        let mut cur = action.to_string();
        while let Some(p) = self.g.ref1(&cur, "predecessor") {
            n += 1;
            cur = p;
            if n > 100_000 {
                break;
            }
        }
        n
    }

    /// Takes `action` out of its behaviour; its predecessor and successor are joined.
    fn detach(&mut self, action: &str) -> Result<(), String> {
        let beh = self.g.container(action).ok_or_else(|| format!("{action} is in no behaviour"))?;
        let pred = self.g.ref1(action, "predecessor");
        let succ = self.g.ref1(action, "successor");
        if let Some(p) = &pred {
            self.g.unlink(p, "successor", action);
        }
        if let Some(n) = &succ {
            self.g.unlink(action, "successor", n);
        }
        self.g.unlink(&beh, "steps", action);
        if let (Some(p), Some(n)) = (&pred, &succ) {
            self.g.link(p, "successor", n);
        }
        Ok(())
    }

    /// Inserts `actions` one after the other, right after `after`.
    fn insert_all_after(&mut self, after: &str, actions: &[String]) -> Result<(), String> {
        let mut prev = after.to_string();
        for a in actions {
            self.insert_after(&prev, a)?;
            prev = a.clone();
        }
        Ok(())
    }

    /// The caller's assembly and the providing assembly of a call (through its role's connector).
    fn target_of(&self, call: &str) -> Option<(String, String)> {
        call_target(&self.g, call)
    }

    /// The interface a call's role requires.
    fn interface_of(&self, call: &str) -> Result<String, String> {
        let role = one(&self.g, call, "role")?;
        one(&self.g, &role, "requiredInterface")
    }

    /// The role of `asm`'s component that provides `iface`.
    fn provided_role(&self, asm: &str, iface: &str) -> Option<String> {
        let comp = self.g.ref1(asm, "encapsulatedComponent")?;
        sorted(self.g.refs(&comp, "providedRoles"))
            .into_iter()
            .find(|r| self.g.ref1(r, "providedInterface").as_deref() == Some(iface))
    }

    /// A required role of `comp` for `iface`, wired from every assembly of `comp` to `provider`:
    /// an existing plain one when `kind` is None, else a role of its own for messages of `kind`.
    fn role_to(&mut self, comp: &str, iface: &str, provider: &str, kind: Option<RoleKind>) -> Result<String, String> {
        let asms = self.assemblies_of(comp);
        if kind.is_none()
            && let Some(asm) = asms.first()
        {
            for conn in sorted(self.g.referrers(asm, "requiringAssemblyContext")) {
                if self.g.ref1(&conn, "providingAssemblyContext").as_deref() != Some(provider) {
                    continue;
                }
                let Some(r) = self.g.ref1(&conn, "requiredRole") else { continue };
                if !self.roles.contains_key(&r) && self.g.ref1(&r, "requiredInterface").as_deref() == Some(iface) {
                    return Ok(r);
                }
            }
        }
        let key = (comp.to_string(), iface.to_string(), provider.to_string(), kind);
        if let Some(r) = self.wired.get(&key) {
            return Ok(r.clone());
        }
        let prole = self
            .provided_role(provider, iface)
            .ok_or_else(|| format!("{} does not provide {}", self.g.entity(provider), self.g.entity(iface)))?;
        let r = self.role(comp, iface, false);
        if let Some(k) = kind {
            let label = match k {
                RoleKind::Async => "async",
                RoleKind::Published => "published",
                RoleKind::Reply => "reply",
            };
            self.g.set_attr(&r, "entityName", s(format!("{}_{label}", self.g.entity(&r))));
            self.roles.insert(r.clone(), k);
        }
        for asm in asms {
            self.connector(&asm, &r, provider, &prole);
        }
        self.wired.insert(key, r.clone());
        Ok(r)
    }

    /// An asynchronous fork that runs `actions` in the background.
    fn async_fork(&mut self, base: &str, actions: &[String]) -> String {
        let fork = self.named(base, "ForkAction", None);
        let fb = self.named(&format!("{fork}_behaviour"), "ForkedBehaviour", None);
        self.g.link(&fork, "asynchronousForkedBehaviours", &fb);
        self.fill(&fb, actions);
        fork
    }

    /// A fork that runs every sequence of `branches` at once and waits for all of them.
    fn parallel(&mut self, base: &str, branches: &[Vec<String>]) -> String {
        let fork = self.named(base, "ForkAction", None);
        let sp = self.named(&format!("{fork}_join"), "SynchronisationPoint", None);
        self.g.link(&fork, "synchronisingBehaviours", &sp);
        for (i, br) in branches.iter().enumerate() {
            let fb = self.named(&format!("{fork}_branch{}", i + 1), "ForkedBehaviour", None);
            self.g.link(&sp, "synchronousForkedBehaviours", &fb);
            self.fill(&fb, br);
        }
        fork
    }

    /// A cache in front of `call` (not in any behaviour): hit = `hit_units` of CPU, miss = the call.
    fn cache_branch(&mut self, call: &str, hit_ratio: f64, hit_units: f64) -> String {
        let branch = self.named(&format!("{call}_cache"), "BranchAction", Some("cache"));
        let hit = self.named(&format!("{branch}_hit"), "ProbabilisticBranchTransition", Some("hit"));
        let miss = self.named(&format!("{branch}_miss"), "ProbabilisticBranchTransition", Some("miss"));
        self.g.set_attr(&hit, "branchProbability", WireValue::Double(hit_ratio));
        self.g.set_attr(&miss, "branchProbability", WireValue::Double(1.0 - hit_ratio));
        self.g.link(&branch, "branches", &hit);
        self.g.link(&branch, "branches", &miss);
        let hb = self.named(&format!("{hit}_body"), "ResourceDemandingBehaviour", None);
        let mb = self.named(&format!("{miss}_body"), "ResourceDemandingBehaviour", None);
        self.g.link(&hit, "branchBehaviour", &hb);
        self.g.link(&miss, "branchBehaviour", &mb);
        let work = self.internal(&hit, "cacheHit", &[("CPU", hit_units)]);
        self.fill(&hb, &[work]);
        self.fill(&mb, &[call.to_string()]);
        branch
    }

    /// The container of a component's first assembly.
    fn container_of_component(&self, comp: &str) -> Result<String, String> {
        let asm = self.assemblies_of(comp).into_iter().next().ok_or_else(|| format!("{} is not in the system", self.g.entity(comp)))?;
        self.container_of(&asm).ok_or_else(|| format!("{asm} is not allocated"))
    }

    /// The publisher of a container: a component on its sidecar (or on the container itself)
    /// whose one operation is the publish CPU of a message. Returns (assembly, interface).
    fn publisher(&mut self, container: &str, rc: &RemoteCalls) -> Result<(String, String), String> {
        if let Some(p) = self.publishers.get(container) {
            return Ok(p.clone());
        }
        let host = if rc.sidecar { self.sidecar(container)? } else { container.to_string() };
        let label = self.g.entity(container);
        let iface = self.named(&format!("publishing_{label}"), "OperationInterface", Some(&format!("publishing_{label}")));
        let repo = self.repo.clone();
        self.g.link(&repo, "interfaces", &iface);
        let sig = self.named(&format!("{iface}_publish"), "OperationSignature", Some("publish"));
        self.g.link(&iface, "signatures", &sig);
        let comp = self.component(&format!("publisher_{label}"));
        self.role(&comp, &iface, true);
        let seff = self.seff(&comp, &sig);
        let units = self.cpu_units(&host, rc.publish_cpu_ms)?;
        let a = self.internal(&seff, "publish", &[("CPU", units)]);
        self.fill(&seff, &[a]);
        let asm = self.assembly(&comp, &host);
        self.local_assemblies.insert(asm.clone());
        self.publishers.insert(container.to_string(), (asm.clone(), iface.clone()));
        Ok((asm, iface))
    }

    fn component(&mut self, base: &str) -> String {
        let c = self.named(base, "BasicComponent", Some(base));
        let repo = self.repo.clone();
        self.g.link(&repo, "components", &c);
        c
    }

    fn role(&mut self, comp: &str, iface: &str, provided: bool) -> String {
        let (class, feature, iface_feature, kind) = if provided {
            ("OperationProvidedRole", "providedRoles", "providedInterface", "provides")
        } else {
            ("OperationRequiredRole", "requiredRoles", "requiredInterface", "requires")
        };
        let r = self.named(&format!("{comp}_{kind}_{iface}"), class, None);
        self.g.link(comp, feature, &r);
        self.g.link(&r, iface_feature, iface);
        r
    }

    fn seff(&mut self, comp: &str, sig: &str) -> String {
        let name = format!("{comp}_{}", self.g.entity(sig));
        let x = self.named(&name, "ResourceDemandingSEFF", None);
        self.g.link(comp, "serviceEffectSpecifications", &x);
        self.g.link(&x, "describedService", sig);
        x
    }

    fn assembly(&mut self, comp: &str, container: &str) -> String {
        let a = self.named(&format!("Assembly_{comp}"), "AssemblyContext", Some(&format!("Assembly_{comp}")));
        let system = self.system.clone();
        self.g.link(&system, "assemblyContexts", &a);
        self.g.link(&a, "encapsulatedComponent", comp);
        let ac = self.named(&format!("alloc_{a}"), "AllocationContext", None);
        let alloc = self.allocation.clone();
        self.g.link(&alloc, "allocationContexts", &ac);
        self.g.link(&ac, "assemblyContext", &a);
        self.g.link(&ac, "resourceContainer", container);
        a
    }

    fn connector(&mut self, req: &str, req_role: &str, prov: &str, prov_role: &str) -> String {
        let c = self.named(&format!("{req}_to_{prov}"), "AssemblyConnector", None);
        let system = self.system.clone();
        self.g.link(&system, "connectors", &c);
        self.g.link(&c, "requiringAssemblyContext", req);
        self.g.link(&c, "requiredRole", req_role);
        self.g.link(&c, "providingAssemblyContext", prov);
        self.g.link(&c, "providedRole", prov_role);
        c
    }

    fn external(&mut self, base: &str, role: &str, sig: &str) -> String {
        let a = self.named(base, "ExternalCallAction", None);
        self.g.link(&a, "role", role);
        self.g.link(&a, "calledService", sig);
        a
    }

    /// A VariableUsage `name` with one characterisation `ty` = `spec`.
    fn usage(&mut self, owner: &str, feature: &str, name: &str, ty: &str, spec: &str) {
        let u = self.named(&format!("{owner}_{name}"), "VariableUsage", None);
        self.g.set_attr(&u, "namedReference", s(name));
        let ch = self.named(&format!("{u}_{}", ty.to_lowercase()), "VariableCharacterisation", None);
        self.g.set_attr(&ch, "type", s(ty));
        self.g.set_attr(&ch, "specification", s(spec));
        self.g.link(&u, "variableCharacterisation", &ch);
        self.g.link(owner, feature, &u);
    }

    fn ensure_delay(&mut self, container: &str) {
        let has = self
            .g
            .refs(container, "activeResourceSpecifications")
            .iter()
            .any(|r| self.g.str_attr(r, "activeResourceType").as_deref() == Some("DELAY"));
        if !has {
            let r = self.named(&format!("{container}_delay"), "ProcessingResourceSpecification", None);
            self.g.set_attr(&r, "schedulingPolicy", s("DELAY"));
            self.g.set_attr(&r, "activeResourceType", s("DELAY"));
            self.g.set_attr(&r, "processingRate", s("1"));
            self.g.link(container, "activeResourceSpecifications", &r);
        }
    }

    /// The DELAY rate of a container (units per time unit).
    fn delay_rate(&self, container: &str) -> f64 {
        self.g
            .refs(container, "activeResourceSpecifications")
            .iter()
            .find(|r| self.g.str_attr(r, "activeResourceType").as_deref() == Some("DELAY"))
            .and_then(|r| self.g.str_attr(r, "processingRate"))
            .and_then(|r| load::mean_of(&r))
            .unwrap_or(1.0)
    }

    fn sidecar(&mut self, container: &str) -> Result<String, String> {
        if let Some(s) = self.sidecars.get(container) {
            return Ok(s.clone());
        }
        let rate = *self.base_cpu.get(container).ok_or_else(|| format!("container {} has no CPU", self.g.entity(container)))?;
        let entity = format!("{}_sidecar", self.g.entity(container));
        let sc = self.named(&format!("{container}_sidecar"), "ResourceContainer", Some(&entity));
        let resenv = self.resenv.clone();
        self.g.link(&resenv, "resourceContainer", &sc);
        let cpu = self.named(&format!("{sc}_cpu"), "ProcessingResourceSpecification", None);
        self.g.set_attr(&cpu, "schedulingPolicy", s("PROCESSOR_SHARING"));
        self.g.set_attr(&cpu, "activeResourceType", s("CPU"));
        self.g.set_attr(&cpu, "processingRate", s(lit(rate)));
        let n = self.replicas.get(container).copied().unwrap_or(1.0) * self.sidecar_cores.get(container).copied().unwrap_or(1.0);
        self.g.set_attr(&cpu, "numberOfReplicas", WireValue::Int(n as i32));
        self.g.link(&sc, "activeResourceSpecifications", &cpu);
        self.base_cpu.insert(sc.clone(), rate);
        for l in self.g.all("LinkingResource") {
            if self.g.refs(&l, "connectedResourceContainers").iter().any(|c| c == container) {
                self.g.link(&l, "connectedResourceContainers", &sc);
            }
        }
        self.sidecars.insert(container.to_string(), sc.clone());
        Ok(sc)
    }

    /// What callers of `sig` through `role` of `comp` pass and read back: input
    /// characterisations (name, type) and RETURN characterisation types.
    fn forwarding(&self, comp: &str, role: &str, sig: &str) -> (BTreeSet<(String, String)>, BTreeSet<String>) {
        let mut inputs = BTreeSet::new();
        let mut returns = BTreeSet::new();
        for call in self.g.referrers(role, "role") {
            if self.g.ref1(&call, "calledService").as_deref() != Some(sig) || self.component_of(&call).as_deref() != Some(comp) {
                continue;
            }
            for u in self.g.refs(&call, "inputVariableUsages") {
                let Some(n) = self.g.str_attr(&u, "namedReference") else { continue };
                for ch in self.g.refs(&u, "variableCharacterisation") {
                    inputs.insert((n.clone(), self.g.str_attr(&ch, "type").unwrap_or_else(|| "VALUE".into())));
                }
            }
            for u in self.g.refs(&call, "returnVariableUsage") {
                for ch in self.g.refs(&u, "variableCharacterisation") {
                    let spec = self.g.str_attr(&ch, "specification").unwrap_or_default();
                    for ty in ["VALUE", "BYTESIZE", "NUMBER_OF_ELEMENTS", "TYPE", "STRUCTURE"] {
                        if spec.contains(&format!("RETURN.{ty}")) {
                            returns.insert(ty.to_string());
                        }
                    }
                }
            }
        }
        (inputs, returns)
    }

    /// A forwarding call: passes `inputs` on and hands `returns` back.
    fn forward_call(
        &mut self,
        base: &str,
        role: &str,
        sig: &str,
        inputs: &BTreeSet<(String, String)>,
        returns: &BTreeSet<String>,
    ) -> Vec<String> {
        let call = self.external(base, role, sig);
        for (n, ty) in inputs {
            self.usage(&call, "inputVariableUsages", n, ty, &format!("{n}.{ty}"));
        }
        let mut out = vec![call.clone()];
        if !returns.is_empty() {
            let set = self.named(&format!("{base}_return"), "SetVariableAction", None);
            for ty in returns {
                self.usage(&call, "returnVariableUsage", "RETURN", ty, &format!("RETURN.{ty}"));
                self.usage(&set, "localVariableUsages", "RETURN", ty, &format!("RETURN.{ty}"));
            }
            out.push(set);
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The rules

/// Node slowdown: the mean slowdown of an M/M/c processor-sharing station with offered load u
/// cores, 1 + C(c, u)/(c − u) (msa-sim-service, DESIGN E25). ∞ for u ≥ c.
pub fn node_slowdown(c: f64, u: f64) -> f64 {
    let cores = c.floor().max(0.0) as u32;
    if cores == 0 || !(u > 0.0) {
        return 1.0;
    }
    let cf = cores as f64;
    if u >= cf {
        return f64::INFINITY;
    }
    1.0 + erlang_c(cores, u) / (cf - u)
}

/// Erlang's C formula: the probability that an arrival at an M/M/c station waits.
pub fn erlang_c(c: u32, a: f64) -> f64 {
    if !(a > 0.0) {
        return 0.0;
    }
    let cf = c as f64;
    if c == 0 || a >= cf {
        return 1.0;
    }
    let mut b = 1.0;
    for k in 1..=c {
        b = a * b / (k as f64 + a * b);
    }
    cf * b / (cf - a * (1.0 - b))
}

/// The highest slowdown a node applies; beyond it the node is reported as saturated.
const MAX_SLOWDOWN: f64 = 1000.0;

pub fn translate(model: &WireModel) -> Result<Translation, String> {
    translate_impl(model, false).map(|(t, _)| t)
}

/// [`translate`], and the model of a dynamic run with Kubernetes-style autoscaling (rule 5,
/// [`DynamicModel`]): every autoscaled Deployable as one pod behind load balancers, for
/// SimOxide's HPA (`crate::dynamic`).
pub fn translate_dynamic(model: &WireModel) -> Result<(Translation, DynamicModel), String> {
    let (t, d) = translate_impl(model, true)?;
    Ok((t, d.ok_or("a dynamic run needs at least one autoscaled Deployable")?))
}

fn translate_impl(model: &WireModel, dynamic: bool) -> Result<(Translation, Option<DynamicModel>), String> {
    let g = Graph::new(model);
    let mut warnings = Vec::new();
    let mut d = Decisions::read(&g, &mut warnings)?;
    // Calls that do not run their callee: redirected calls, and bypasses across containers.
    let mut skipped: BTreeSet<String> = d.redirects.keys().cloned().collect();
    for (_, call, m) in &d.sites {
        match m {
            Mechanism::Bypass { .. } => {
                if let Some((a, p)) = call_target(&g, call)
                    && allocated(&g, &a) != allocated(&g, &p)
                {
                    skipped.insert(call.clone());
                }
            }
            _ => {}
        }
    }
    let original = load::analyse_skipping(&g, &skipped);
    warnings.extend(original.warnings.iter().cloned());
    let mut placement = BTreeMap::new();
    for a in g.all("AllocationContext") {
        if let (Some(asm), Some(c)) = (g.ref1(&a, "assemblyContext"), g.ref1(&a, "resourceContainer")) {
            placement.insert(asm, c);
        }
    }
    let modularity = modularity(&g, &d, &original, &placement);
    let mut quality = quality::before(&g, &d, &placement, &mut warnings);

    if !d.any {
        let cost = cost(&g, &d, &placement, &mut warnings);
        let pcm = g.to_wire(|x| is_pcm(&x.class_name));
        let no_load = original.arrivals.iter().map(|(sc, r)| (sc.clone(), (*r, original.times.get(sc).copied().unwrap_or(0.0)))).collect();
        return Ok((Translation {
            pcm,
            loads: original.clone(),
            calls: original.calls,
            placement,
            nodes: Vec::new(),
            cost,
            modularity,
            warnings,
            translated: false,
            no_load,
            replicas: BTreeMap::new(),
            quality,
            ms_per_unit: d.ms_per_unit,
        }, None));
    }

    let first = |class: &str| g.all(class).into_iter().next().ok_or_else(|| format!("the model has no {class}"));
    let mut b = Builder {
        repo: first("Repository")?,
        system: first("System")?,
        allocation: first("Allocation")?,
        resenv: first("ResourceEnvironment")?,
        ms: d.ms_per_unit,
        base_cpu: HashMap::new(),
        base_hdd: HashMap::new(),
        sidecars: BTreeMap::new(),
        replicas: d.deployables.iter().map(|x| (x.container.clone(), x.replicas)).collect(),
        sidecar_cores: d.deployables.iter().map(|x| (x.container.clone(), x.sidecar_cores)).collect(),
        roles: BTreeMap::new(),
        wired: BTreeMap::new(),
        local_assemblies: BTreeSet::new(),
        publishers: BTreeMap::new(),
        orchestrated: BTreeSet::new(),
        quota: Vec::new(),
        offered: BTreeMap::new(),
        warnings,
        g,
    };
    for c in b.g.all("ResourceContainer") {
        for r in b.g.refs(&c, "activeResourceSpecifications") {
            let rate = b.g.str_attr(&r, "processingRate").and_then(|x| load::mean_of(&x));
            let ty = b.g.str_attr(&r, "activeResourceType").unwrap_or_default();
            if let Some(rate) = rate {
                match ty.as_str() {
                    "CPU" => b.base_cpu.entry(c.clone()).or_insert(rate),
                    "HDD" => b.base_hdd.entry(c.clone()).or_insert(rate),
                    _ => continue,
                };
            }
        }
    }
    colocations(&mut b, &d)?;
    work(&mut b, &d)?;
    workflows(&mut b, &d)?;
    bypasses(&mut b, &d)?;
    composed_calls(&mut b, &d)?;
    redirects(&mut b, &d)?;
    notifications(&mut b, &d)?;
    mechanisms(&mut b, &d)?;
    if let Some(rc) = &d.remote {
        remote_calls(&mut b, rc)?;
    }
    autoscale(&mut b, &mut d);
    let cost = cost(&b.g, &d, &placement, &mut b.warnings);
    replicas(&mut b, &d)?;
    let (nodes, idle) = contention(&mut b, &d);
    let no_load = idle.arrivals.iter().map(|(sc, r)| (sc.clone(), (*r, idle.times.get(sc).copied().unwrap_or(0.0)))).collect();
    let mut loads = load::analyse(&b.g);
    // A quota container's utilisation is on its capacity, n·q cores.
    for (c, n, q) in &b.quota {
        let full: f64 = idle.resources.iter().filter(|r| &r.container == c && r.kind == "CPU").map(|r| r.busy).sum();
        for r in loads.resources.iter_mut().filter(|r| &r.container == c && r.kind == "CPU") {
            r.capacity_basis = Some(full / (n * q));
        }
    }
    let mut warnings = std::mem::take(&mut b.warnings);
    for w in &loads.warnings {
        if !warnings.contains(w) {
            warnings.push(w.clone());
        }
    }
    let pcm = b.g.to_wire(|x| is_pcm(&x.class_name));
    let replicas = d.deployables.iter().map(|x| (x.container.clone(), x.replicas)).collect();
    quality::after(&mut quality, &b.g, &d, &placement, &b.sidecars, &idle, &mut warnings);
    let dynamic = if dynamic { Some(pods(&mut b, &d, &placement, &quality)?) } else { None };
    Ok((Translation {
        pcm,
        loads,
        calls: original.calls,
        placement,
        nodes,
        cost,
        modularity,
        warnings,
        translated: true,
        no_load,
        replicas,
        quality,
        ms_per_unit: d.ms_per_unit,
    }, dynamic))
}

/// Rule 0: orchestrated workflows. The orchestrator (the trigger's component) makes every step
/// at the end of the trigger's SEFF, as msa-sim-service does (DESIGN E8 there): per step its
/// coordination CPU, then the step (the call with its own loop or branch); a step after the
/// steps it follows; parallel where the steps form a forest, else level by level; all of it in
/// the background when the first step is an asynchronous call to another container.
fn workflows(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let mech: HashMap<&String, &Mechanism> = d.sites.iter().map(|(_, c, m)| (c, m)).collect();
    for w in &d.workflows {
        let Some(step_cpu) = w.orchestrated else { continue };
        let orch = b.g.container(&w.trigger).ok_or_else(|| format!("{}: the trigger is in no component", w.name))?;
        let orch_c = b.container_of_component(&orch)?;
        struct Step {
            call: String,
            seff: String,
            unit: String,
            provider: String,
            callee: Option<String>,
            remote: bool,
            asynchronous: bool,
            order: (u8, usize, String),
        }
        let mut steps = Vec::new();
        for call in &w.steps {
            let seff = b.seff_of(call).ok_or_else(|| format!("{}: step {call} is in no SEFF", w.name))?;
            let unit = b.top_level(call).ok_or_else(|| format!("{}: step {call} is in no SEFF", w.name))?;
            let (_, provider) = b.target_of(call).ok_or_else(|| format!("{}: step {call} has no connector", w.name))?;
            let sig = one(&b.g, call, "calledService")?;
            let pcomp = one(&b.g, &provider, "encapsulatedComponent")?;
            let callee = b.g.refs(&pcomp, "serviceEffectSpecifications").into_iter().find(|x| b.g.ref1(x, "describedService").as_deref() == Some(sig.as_str()));
            let remote = b.container_of(&provider).as_deref() != Some(orch_c.as_str());
            let asynchronous = matches!(mech.get(call), Some(Mechanism::Async));
            let order = (u8::from(seff != w.trigger), b.chain_index(&unit), call.clone());
            steps.push(Step { call: call.clone(), seff, unit, provider, callee, remote, asynchronous, order });
        }
        steps.sort_by(|x, y| x.order.cmp(&y.order));
        let m = steps.len();
        // b follows a when b's call sits in the SEFF that a calls
        let pred: Vec<Vec<usize>> =
            (0..m).map(|i| (0..m).filter(|&j| j != i && steps[j].callee.as_deref() == Some(steps[i].seff.as_str())).collect()).collect();
        let mut succ = vec![Vec::new(); m];
        for (i, ps) in pred.iter().enumerate() {
            for &j in ps {
                succ[j].push(i);
            }
        }
        let mut level = vec![usize::MAX; m];
        let mut indeg: Vec<usize> = pred.iter().map(Vec::len).collect();
        let mut queue: Vec<usize> = (0..m).filter(|&i| indeg[i] == 0).collect();
        for &q in &queue {
            level[q] = 0;
        }
        let mut qi = 0;
        while qi < queue.len() {
            let x = queue[qi];
            qi += 1;
            for &y in &succ[x] {
                level[y] = if level[y] == usize::MAX { level[x] + 1 } else { level[y].max(level[x] + 1) };
                indeg[y] -= 1;
                if indeg[y] == 0 {
                    queue.push(y);
                }
            }
        }
        let max_level = level.iter().filter(|&&l| l != usize::MAX).max().copied().unwrap_or(0);
        let cyclic = level.iter().any(|&l| l == usize::MAX);
        for l in &mut level {
            if *l == usize::MAX {
                *l = max_level + 1;
            }
        }
        let forest = pred.iter().all(|p| p.len() <= 1) && !cyclic;
        // take every step out of its SEFF; the orchestrator calls it
        let units = b.cpu_units(&orch_c, step_cpu)?;
        let mut nodes: Vec<Vec<String>> = Vec::new();
        for st in &steps {
            b.detach(&st.unit)?;
            let iface = b.interface_of(&st.call)?;
            let kind = if st.asynchronous && st.remote { Some(RoleKind::Reply) } else { None };
            let r = b.role_to(&orch, &iface, &st.provider, kind)?;
            b.g.set_ref(&st.call, "role", &r);
            b.orchestrated.insert(st.call.clone());
            let coord = b.internal(&st.unit, "coordinate", &[("CPU", units)]);
            nodes.push(vec![coord, st.unit.clone()]);
        }
        let name = format!("{}_orchestration", b.g.entity(&w.trigger));
        let roots: Vec<usize> = (0..m).filter(|&i| level[i] == 0).collect();
        let body: Vec<String> = if forest {
            fn sub(b: &mut Builder, i: usize, succ: &[Vec<usize>], nodes: &[Vec<String>], name: &str) -> Vec<String> {
                let mut out = nodes[i].clone();
                let kids: Vec<Vec<String>> = succ[i].iter().map(|&j| sub(b, j, succ, nodes, name)).collect();
                match kids.len() {
                    0 => {}
                    1 => out.extend(kids.into_iter().next().unwrap()),
                    _ => out.push(b.parallel(&format!("{name}_after_{}", b.g.entity(&nodes[i][1])), &kids)),
                }
                out
            }
            let kids: Vec<Vec<String>> = roots.iter().map(|&r| sub(b, r, &succ, &nodes, &name)).collect();
            if kids.len() == 1 { kids.into_iter().next().unwrap() } else { vec![b.parallel(&format!("{name}_roots"), &kids)] }
        } else {
            let mut out = Vec::new();
            for l in 0..=max_level + 1 {
                let group: Vec<Vec<String>> = (0..m).filter(|&i| level[i] == l).map(|i| nodes[i].clone()).collect();
                match group.len() {
                    0 => {}
                    1 => out.extend(group.into_iter().next().unwrap()),
                    _ => out.push(b.parallel(&format!("{name}_level{l}"), &group)),
                }
            }
            out
        };
        let in_background = roots.first().is_some_and(|&r| steps[r].asynchronous && steps[r].remote);
        let actions = if in_background { vec![b.async_fork(&name, &body)] } else { body };
        let stop = b
            .g
            .refs(&w.trigger, "steps")
            .into_iter()
            .find(|x| b.g.is_a(x, "StopAction"))
            .ok_or_else(|| format!("{}: the trigger has no stop action", w.name))?;
        let last = one(&b.g, &stop, "predecessor")?;
        b.insert_all_after(&last, &actions)?;
    }
    Ok(())
}

/// Rule 0: bypasses. Across containers the caller does the callee's own CPU work, waits the
/// latency, and makes the callee's calls into direct containers itself (msa SHARED_DB).
fn bypasses(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let direct: BTreeSet<String> = d.remote.as_ref().map(|r| r.direct.clone()).unwrap_or_default();
    for (cs, call, m) in &d.sites {
        let Mechanism::Bypass { latency_ms } = m else { continue };
        let (caller_asm, provider) = b.target_of(call).ok_or_else(|| format!("{cs}: {call} has no connector"))?;
        let (Some(cc), Some(pc)) = (b.container_of(&caller_asm), b.container_of(&provider)) else { continue };
        if cc == pc {
            continue;
        }
        let caller = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
        let sig = one(&b.g, call, "calledService")?;
        let pcomp = one(&b.g, &provider, "encapsulatedComponent")?;
        let seff = b
            .g
            .refs(&pcomp, "serviceEffectSpecifications")
            .into_iter()
            .find(|x| b.g.ref1(x, "describedService").as_deref() == Some(sig.as_str()))
            .ok_or_else(|| format!("{cs}: {} has no SEFF for {}", b.g.entity(&pcomp), b.g.entity(&sig)))?;
        let ratio = match (b.base_cpu.get(&cc), b.base_cpu.get(&pc)) {
            (Some(x), Some(y)) if *y > 0.0 => x / y,
            _ => 1.0,
        };
        let mut work: BTreeMap<String, f64> = BTreeMap::new();
        let mut directs = Vec::new();
        let mut cur = b.g.refs(&seff, "steps").into_iter().find(|x| b.g.is_a(x, "StartAction"));
        while let Some(a) = cur {
            match b.g.class(&a) {
                "InternalAction" => {
                    for dm in b.g.refs(&a, "resourceDemand") {
                        let ty = b.g.str_attr(&dm, "requiredResource").unwrap_or_else(|| "CPU".into());
                        match b.g.str_attr(&dm, "specification").and_then(|x| load::mean_of(&x)) {
                            Some(u) => *work.entry(ty.clone()).or_default() += if ty == "CPU" { u * ratio } else { u },
                            None => b.warnings.push(format!("{cs}: a demand of {} depends on parameters; the bypass leaves it out", b.g.entity(&a))),
                        }
                    }
                }
                "ExternalCallAction" => {
                    if let Some((_, t)) = b.target_of(&a)
                        && b.container_of(&t).is_some_and(|c| direct.contains(&c))
                    {
                        directs.push((a.clone(), t));
                    }
                }
                _ => {}
            }
            cur = b.g.ref1(&a, "successor");
        }
        let demands: Vec<(&str, f64)> = work.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let mut actions = vec![b.internal(call, "bypass", &demands)];
        if *latency_ms > 0.0 {
            b.ensure_delay(&cc);
            let units = latency_ms / b.ms * b.delay_rate(&cc);
            actions.push(b.internal(call, "bypassLatency", &[("DELAY", units)]));
        }
        for (a, t) in directs {
            let iface = b.interface_of(&a)?;
            let r = b.role_to(&caller, &iface, &t, None)?;
            let s_ = one(&b.g, &a, "calledService")?;
            actions.push(b.external(&format!("{call}_{}", b.g.entity(&a)), &r, &s_));
        }
        b.replace_in_chain(call, &actions[0])?;
        b.insert_all_after(&actions[0].clone(), &actions[1..])?;
        b.g.remove(call);
    }
    Ok(())
}

/// Rule 0: composed calls. Across containers the composer makes the call right after its own
/// call into the caller's operation: composeCpuMs, then (with a cache) the hit branch or the
/// call. Several composed calls after one composer call run in parallel (msa GATEWAY_JOIN).
fn composed_calls(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let mut groups: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for (cs, call, m) in &d.sites {
        let Mechanism::Composed { compose_cpu_ms, cache } = m else { continue };
        let Some(after) = d.composers.get(call) else { continue };
        let (caller_asm, provider) = b.target_of(call).ok_or_else(|| format!("{cs}: {call} has no connector"))?;
        if b.container_of(&caller_asm) == b.container_of(&provider) {
            continue;
        }
        let composer = b.component_of(after).ok_or_else(|| format!("{cs}: the composer {after} is in no component"))?;
        let cc = b.container_of_component(&composer)?;
        let unit = b.top_level(call).ok_or_else(|| format!("{cs}: {call} is in no SEFF"))?;
        let pred = one(&b.g, call, "predecessor")?;
        b.detach(call)?;
        let iface = b.interface_of(call)?;
        let r = b.role_to(&composer, &iface, &provider, None)?;
        b.g.set_ref(call, "role", &r);
        let compose = b.internal(call, "compose", &[("CPU", b.cpu_units(&cc, *compose_cpu_ms)?)]);
        let access = match cache {
            Some((h, hit_ms)) => {
                let units = b.cpu_units(&cc, *hit_ms)?;
                b.cache_branch(call, *h, units)
            }
            None => call.clone(),
        };
        let seq = if &unit == call {
            vec![compose, access]
        } else {
            b.insert_all_after(&pred, &[compose, access])?;
            b.detach(&unit)?;
            vec![unit]
        };
        groups.entry(after.clone()).or_default().push(seq);
    }
    for (after, seqs) in groups {
        if seqs.len() == 1 {
            b.insert_all_after(&after, &seqs[0])?;
        } else {
            let fork = b.parallel(&format!("{after}_compose"), &seqs);
            b.insert_after(&after, &fork)?;
        }
    }
    Ok(())
}

/// Rule 0, completions: every `Colocation` gives its assembly an AllocationContext in the
/// container of its host (after the walks of the model before translation, which do not see it).
fn colocations(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    for (asm, host) in &d.colocations {
        if b.container_of(asm).is_some() {
            return Err(format!("{asm} has a Colocation and an AllocationContext of its own"));
        }
        let c = b.container_of(host).ok_or_else(|| format!("{asm} sits with {host}, which is not allocated"))?;
        let ac = b.named(&format!("alloc_{asm}"), "AllocationContext", None);
        let alloc = b.allocation.clone();
        b.g.link(&alloc, "allocationContexts", &ac);
        b.g.link(&ac, "assemblyContext", asm);
        b.g.link(&ac, "resourceContainer", &c);
    }
    Ok(())
}

/// Rule 0, completions: every `Work` becomes exponential demands of its InternalAction, CPU then
/// HDD, at the rates of the container of its component's first assembly. A component without an
/// assembly never runs: its Work is left out.
fn work(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    for (action, cpu_ms, hdd_ms) in &d.work {
        let comp = b.component_of(action).ok_or_else(|| format!("{action} is in no component"))?;
        let Some(c) = b.assemblies_of(&comp).first().and_then(|a| b.container_of(a)) else { continue };
        if *cpu_ms > 0.0 {
            let units = b.cpu_units(&c, *cpu_ms)?;
            b.demand(action, "CPU", units, true);
        }
        if *hdd_ms > 0.0 {
            let units = b.hdd_units(&c, *hdd_ms)?;
            b.demand(action, "HDD", units, true);
        }
    }
    Ok(())
}

/// Rule 0, completions: every `Redirect` re-points its call to its service on its provider,
/// through a role of the caller's component (wired from every assembly of the caller), and drops
/// the call's parameter and return usages.
fn redirects(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    for (call, (provider, service)) in &d.redirects {
        let caller = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
        let iface = b.g.container(service).ok_or_else(|| format!("{service} is in no interface"))?;
        let role = b.role_to(&caller, &iface, provider, None)?;
        b.g.set_ref(call, "role", &role);
        b.g.set_ref(call, "calledService", service);
        for u in b.g.refs(call, "inputVariableUsages").into_iter().chain(b.g.refs(call, "returnVariableUsage")) {
            for ch in b.g.refs(&u, "variableCharacterisation") {
                b.g.remove(&ch);
            }
            b.g.remove(&u);
        }
    }
    Ok(())
}

/// Rule 0, completions: every `Notification` puts its CPU (`cpuMs` on the CPU of its action's
/// container) and an asynchronous fork with its deliveries right after its action. A published
/// one, with a broker, first publishes once through the publisher of the action's container (on
/// its sidecar); its deliveries are messages whose stubs only travel (broker, latency, call:
/// rule 2). Otherwise each delivery is a plain call.
fn notifications(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let broker = d.remote.as_ref().filter(|r| r.broker.is_some());
    for n in &d.notifications {
        let w = &n.after;
        let owner = b.component_of(w).ok_or_else(|| format!("{}: {w} is in no component", n.name))?;
        let oc = b.container_of_component(&owner)?;
        let message = n.published && broker.is_some();
        let mut sends = Vec::new();
        if message && let Some(rc) = broker {
            let (pasm, piface) = b.publisher(&oc, rc)?;
            let sig = b.g.refs(&piface, "signatures").into_iter().next().ok_or("a publisher without its operation")?;
            let r = b.role_to(&owner, &piface, &pasm, None)?;
            sends.push(b.external(&format!("{w}_publish"), &r, &sig));
        }
        for (provider, service) in &n.deliveries {
            let iface = b.g.container(service).ok_or_else(|| format!("{service} is in no interface"))?;
            let kind = if message { Some(RoleKind::Published) } else { None };
            let role = b.role_to(&owner, &iface, provider, kind)?;
            let label = b.g.entity(service);
            sends.push(b.external(&format!("{w}_{label}"), &role, service));
        }
        let mut actions = Vec::new();
        if n.cpu_ms > 0.0 {
            actions.push(b.internal(w, "notify", &[("CPU", b.cpu_units(&oc, n.cpu_ms)?)]));
        }
        actions.push(b.async_fork(&format!("{w}_notification"), &sends));
        b.insert_all_after(w, &actions)?;
    }
    Ok(())
}

/// Rule 1: asynchronous and cached calls.
fn mechanisms(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let broker = d.remote.as_ref().and_then(|r| r.broker.clone());
    let direct: BTreeSet<String> = d.remote.as_ref().map(|r| r.direct.clone()).unwrap_or_default();
    for (_, call, m) in &d.sites {
        if b.orchestrated.contains(call) {
            continue;
        }
        match m {
            Mechanism::Sync | Mechanism::Other | Mechanism::Composed { .. } | Mechanism::Bypass { .. } => {}
            Mechanism::Async => {
                // Through the broker to another container: the stub publishes on the caller's
                // path and forks the rest (rule 2), as msa-sim-service does (D6 there: E5).
                let remote = b.target_of(call).and_then(|(a, p)| {
                    let (ca, cp) = (b.container_of(&a)?, b.container_of(&p)?);
                    Some(ca != cp && !direct.contains(&cp)).filter(|x| *x).map(|_| p)
                });
                if broker.is_some()
                    && let Some(provider) = remote
                {
                    let comp = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
                    let iface = b.interface_of(call)?;
                    let r = b.role_to(&comp, &iface, &provider, Some(RoleKind::Async))?;
                    b.g.set_ref(call, "role", &r);
                    continue;
                }
                let fork = b.named(&format!("{call}_async"), "ForkAction", None);
                let fb = b.named(&format!("{call}_async_behaviour"), "ForkedBehaviour", None);
                b.g.link(&fork, "asynchronousForkedBehaviours", &fb);
                b.replace_in_chain(call, &fork)?;
                b.fill(&fb, std::slice::from_ref(call));
            }
            Mechanism::Cached { hit_ratio, hit_cpu_ms } => {
                let comp = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
                let c = b.container_of_component(&comp)?;
                let units = b.cpu_units(&c, *hit_cpu_ms)?;
                let pred = one(&b.g, call, "predecessor")?;
                b.detach(call)?;
                let branch = b.cache_branch(call, *hit_ratio, units);
                b.insert_after(&pred, &branch)?;
            }
        }
    }
    Ok(())
}

/// Rule 2: a stub and a skeleton on every assembly connector between two containers.
fn remote_calls(b: &mut Builder, rc: &RemoteCalls) -> Result<(), String> {
    let broker = match &rc.broker {
        None => None,
        Some(asm) => {
            let comp = one(&b.g, asm, "encapsulatedComponent")?;
            let prov = b
                .g
                .refs(&comp, "providedRoles")
                .into_iter()
                .find(|r| b.g.class(r) == "OperationProvidedRole")
                .ok_or_else(|| format!("the broker {asm} provides no operation interface"))?;
            let iface = one(&b.g, &prov, "providedInterface")?;
            let sig = b.g.refs(&iface, "signatures").into_iter().next().ok_or_else(|| format!("the broker's interface {iface} has no signature"))?;
            Some((asm.clone(), prov, iface, sig))
        }
    };
    let connectors = sorted(b.g.all("AssemblyConnector"));
    for conn in connectors {
        let (Some(req), Some(prov), Some(role), Some(prole)) = (
            b.g.ref1(&conn, "requiringAssemblyContext"),
            b.g.ref1(&conn, "providingAssemblyContext"),
            b.g.ref1(&conn, "requiredRole"),
            b.g.ref1(&conn, "providedRole"),
        ) else {
            continue;
        };
        let (Some(cr), Some(cp)) = (b.container_of(&req), b.container_of(&prov)) else { continue };
        // co-located, reached directly, a publisher on the caller's sidecar, or a role no call uses
        if cr == cp || rc.direct.contains(&cp) || b.local_assemblies.contains(&prov) || b.g.referrers(&role, "role").is_empty() {
            continue;
        }
        let kind = b.roles.get(&role).copied();
        let iface = one(&b.g, &prole, "providedInterface")?;
        let caller_comp = one(&b.g, &req, "encapsulatedComponent")?;
        let (stub_c, skel_c) = if rc.sidecar { (b.sidecar(&cr)?, b.sidecar(&cp)?) } else { (cr.clone(), cp.clone()) };
        let callee_comp = one(&b.g, &prov, "encapsulatedComponent")?;
        let pair = format!("{}_{}", b.g.entity(&caller_comp), b.g.entity(&callee_comp));
        let stub = b.component(&format!("stub_{pair}"));
        let s_p = b.role(&stub, &iface, true);
        let s_r = b.role(&stub, &iface, false);
        let skel = b.component(&format!("skeleton_{pair}"));
        let k_p = b.role(&skel, &iface, true);
        let k_r = b.role(&skel, &iface, false);
        let s_b = match (&broker, kind) {
            (Some((_, _, biface, _)), Some(_)) => Some(b.role(&stub, biface, false)),
            _ => None,
        };
        let cpu = |b: &Builder, c: &str, ms: f64| b.cpu_units(c, ms);
        let caller_units = cpu(b, &stub_c, rc.caller_cpu_ms)?;
        let callee_units = cpu(b, &skel_c, rc.callee_cpu_ms)?;
        let publish_out = cpu(b, &stub_c, rc.publish_cpu_ms)?;
        let deliver_back = cpu(b, &stub_c, rc.deliver_cpu_ms)?;
        let deliver_in = cpu(b, &skel_c, rc.deliver_cpu_ms)?;
        let publish_back = cpu(b, &skel_c, rc.publish_cpu_ms)?;
        if rc.latency_ms > 0.0 {
            b.ensure_delay(&stub_c);
        }
        let latency_units = rc.latency_ms / b.ms * b.delay_rate(&stub_c);
        for sig in b.g.refs(&iface, "signatures") {
            let (inputs, returns) = b.forwarding(&caller_comp, &role, &sig);
            let seff = b.seff(&stub, &sig);
            // the trip: (broker), latency, call
            let mut trip = Vec::new();
            if let (Some(sb), Some((_, _, _, bsig))) = (&s_b, &broker) {
                trip.push(b.external(&format!("{seff}_broker"), sb, bsig));
            }
            if latency_units > 0.0 {
                trip.push(b.internal(&seff, "latency", &[("DELAY", latency_units)]));
            }
            trip.extend(b.forward_call(&format!("{seff}_call"), &s_r, &sig, &inputs, &returns));
            let steps = match kind {
                // caller CPU, the trip
                None => {
                    let mut v = vec![b.internal(&seff, "marshal", &[("CPU", caller_units)])];
                    v.extend(trip);
                    v
                }
                // publish on the caller's path, the trip in the background
                Some(RoleKind::Async) => {
                    let p = b.internal(&seff, "publish", &[("CPU", publish_out)]);
                    let f = b.async_fork(&format!("{seff}_send"), &trip);
                    vec![p, f]
                }
                // published once by the sender's publisher: the trip only
                Some(RoleKind::Published) => trip,
                // request and reply: publish, the trip, then the reply's broker, latency, deliver
                Some(RoleKind::Reply) => {
                    let mut v = vec![b.internal(&seff, "publish", &[("CPU", publish_out)])];
                    v.extend(trip);
                    if let (Some(sb), Some((_, _, _, bsig))) = (&s_b, &broker) {
                        v.push(b.external(&format!("{seff}_replyBroker"), sb, bsig));
                    }
                    if latency_units > 0.0 {
                        v.push(b.internal(&seff, "replyLatency", &[("DELAY", latency_units)]));
                    }
                    v.push(b.internal(&seff, "replyDeliver", &[("CPU", deliver_back)]));
                    v
                }
            };
            b.fill(&seff, &steps);
            // skeleton: callee CPU (or deliver), call the component (a reply publishes after it)
            let seff = b.seff(&skel, &sig);
            let mut steps = vec![match kind {
                None => b.internal(&seff, "unmarshal", &[("CPU", callee_units)]),
                Some(_) => b.internal(&seff, "deliver", &[("CPU", deliver_in)]),
            }];
            steps.extend(b.forward_call(&format!("{seff}_call"), &k_r, &sig, &inputs, &returns));
            if kind == Some(RoleKind::Reply) {
                steps.push(b.internal(&seff, "replyPublish", &[("CPU", publish_back)]));
            }
            b.fill(&seff, &steps);
        }
        let a_s = b.assembly(&stub, &stub_c);
        let a_k = b.assembly(&skel, &skel_c);
        b.g.set_ref(&conn, "providingAssemblyContext", &a_s);
        b.g.set_ref(&conn, "providedRole", &s_p);
        b.connector(&a_s, &s_r, &a_k, &k_p);
        b.connector(&a_k, &k_r, &prov, &prole);
        if let (Some(sb), Some((basm, bprov, _, _))) = (&s_b, &broker) {
            b.connector(&a_s, sb, basm, bprov);
        }
    }
    Ok(())
}

/// Rule 3a (D50): the replica count of every autoscaled Deployable, from the offered load of the
/// model as rules 0 to 2 left it (full-speed cores, so the speeds of rules 3 and 4 do not
/// matter): the smallest n ≥ 1 that keeps its CPU, and its sidecar's, at most at the target
/// utilisation, at the model's workload (simulated) and at every load level (cost). Sidecars,
/// created by rule 2 with one replica, get their cores for n replicas.
fn autoscale(b: &mut Builder, d: &mut Decisions) {
    if !d.deployables.iter().any(|x| x.autoscale.is_some()) {
        return;
    }
    let loads = load::analyse(&b.g);
    let work = |c: &str| -> f64 {
        let Some(base) = b.base_cpu.get(c).copied() else { return 0.0 };
        loads.resources.iter().filter(|r| r.container == c && r.kind == "CPU").map(|r| r.busy * r.rate / base).sum()
    };
    let mut updates = Vec::new();
    for x in d.deployables.iter_mut() {
        let Some(a) = &x.autoscale else { continue };
        let u = work(&x.container);
        let sidecar = b.sidecars.get(&x.container).cloned();
        let us = sidecar.as_deref().map(work).unwrap_or(0.0);
        b.offered.insert(x.container.clone(), (u, us));
        let (n, capped) = a.count(1.0, u, x.cpu_per_replica, us, x.sidecar_cores);
        if capped {
            b.warnings.push(format!(
                "{}: the autoscaler would need more than {n} replicas (maxReplicas) for {u:.3} cores of load",
                b.g.entity(&x.container)
            ));
        }
        x.replicas = n;
        x.levels = a.levels.iter().map(|&(f, w)| (w, a.count(f, u, x.cpu_per_replica, us, x.sidecar_cores).0)).collect();
        updates.push((x.container.clone(), n, sidecar, x.sidecar_cores));
    }
    for (c, n, sidecar, cores) in updates {
        b.replicas.insert(c, n);
        let Some(sc) = sidecar else { continue };
        for r in b.g.refs(&sc, "activeResourceSpecifications") {
            if b.g.str_attr(&r, "activeResourceType").as_deref() == Some("CPU") {
                b.g.set_attr(&r, "numberOfReplicas", WireValue::Int((n * cores) as i32));
            }
        }
    }
}

/// Rule 3: replicas with a CPU limit each. A quota container stays at full speed here; rule 4
/// sets its speed from its load.
fn replicas(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    for x in &d.deployables {
        let c = &x.container;
        let base = *b.base_cpu.get(c).ok_or_else(|| format!("container {} has no CPU", b.g.entity(c)))?;
        let (n, q) = (x.replicas, x.cpu_per_replica);
        let per = q.ceil();
        let rate = if x.quota { base } else { base * q / per };
        for r in b.g.refs(c, "activeResourceSpecifications") {
            if b.g.str_attr(&r, "activeResourceType").as_deref() != Some("CPU") {
                continue;
            }
            b.g.set_attr(&r, "numberOfReplicas", WireValue::Int((n * per) as i32));
            b.g.set_attr(&r, "processingRate", s(lit(rate)));
        }
        if x.quota {
            b.quota.push((c.clone(), n, q));
        }
    }
    Ok(())
}

/// Rule 4: the load-dependent speeds. One load analysis of the model as rules 0 to 3 left it
/// (replicas at their limit speed, quota containers at full speed: the no-load state, which it
/// returns); from it the quota speed of each quota container (msa-sim-service's `quota` rule)
/// and the slowdown of each node, which caps the speed of the node's CPUs.
fn contention(b: &mut Builder, d: &Decisions) -> (Vec<NodeLoad>, Loads) {
    let loads = load::analyse(&b.g);
    let mut out = Vec::new();
    let mut slowdowns: Vec<(Vec<String>, f64)> = Vec::new();
    for (name, cores, background, containers) in &d.nodes {
        let mut members: Vec<String> = containers.clone();
        for c in containers {
            if let Some(s) = b.sidecars.get(c) {
                members.push(s.clone());
            }
        }
        let mut u = *background;
        for r in &loads.resources {
            if r.kind == "CPU"
                && members.contains(&r.container)
                && let Some(base) = b.base_cpu.get(&r.container)
            {
                u += r.busy * r.rate / base;
            }
        }
        let mut slow = node_slowdown(*cores, u);
        if slow > MAX_SLOWDOWN {
            b.warnings.push(format!("node {name} is saturated: {u:.3} cores of work on {cores} cores"));
            slow = MAX_SLOWDOWN;
        }
        slowdowns.push((members, slow));
        out.push(NodeLoad { name: name.clone(), cores: *cores, load: u, slowdown: slow });
    }
    // quota: full speed when idle, max(q', 1 − u/(n⌈q⌉) · (1 − q')/q') of it under load u
    for (c, n, q) in b.quota.clone() {
        let Some(base) = b.base_cpu.get(&c).copied() else { continue };
        let u: f64 = loads.resources.iter().filter(|r| r.container == c && r.kind == "CPU").map(|r| r.busy * r.rate / base).sum();
        let per = q.ceil();
        let qp = q / per;
        let speed = if qp >= 1.0 { 1.0 } else { qp.max(1.0 - u / (n * per) * (1.0 - qp) / qp) };
        for r in b.g.refs(&c, "activeResourceSpecifications") {
            if b.g.str_attr(&r, "activeResourceType").as_deref() == Some("CPU") {
                b.g.set_attr(&r, "processingRate", s(lit(base * speed)));
            }
        }
    }
    for (members, slow) in slowdowns {
        if slow <= 1.0 {
            continue;
        }
        for c in &members {
            let Some(base) = b.base_cpu.get(c).copied() else { continue };
            for r in b.g.refs(c, "activeResourceSpecifications") {
                if b.g.str_attr(&r, "activeResourceType").as_deref() != Some("CPU") {
                    continue;
                }
                let rate = b.g.str_attr(&r, "processingRate").and_then(|x| load::mean_of(&x)).unwrap_or(base);
                if base / slow < rate {
                    b.g.set_attr(&r, "processingRate", s(lit(base / slow)));
                }
            }
        }
    }
    (out, loads)
}

// ---------------------------------------------------------------------------------------------
// Objectives

fn cost(g: &Graph, d: &Decisions, placement: &BTreeMap<String, String>, warnings: &mut Vec<String>) -> f64 {
    let [per_container, per_core, per_replica, per_cache] = d.cost.unwrap_or_else(|| {
        if d.any {
            warnings.push("the model has no CostModel: cost counts the containers in use".into());
        }
        [1.0, 0.0, 0.0, 0.0]
    });
    let used: BTreeSet<&String> = placement.values().collect();
    let mut total = 0.0;
    for c in used {
        let deployable = d.deployables.iter().find(|x| &x.container == c);
        // replicas: the weighted mean over the load levels (D50); 1 without a Deployable
        let mean_replicas = deployable.map(|x| {
            let w: f64 = x.levels.iter().map(|l| l.0).sum();
            x.levels.iter().map(|(wi, n)| wi * n).sum::<f64>() / w
        });
        if let Some(p) = d.prices.get(c) {
            total += p * mean_replicas.unwrap_or(1.0);
            continue;
        }
        let (cores, replicas) = match deployable {
            Some(x) => {
                let n = mean_replicas.unwrap_or(x.replicas);
                (n * x.cpu_per_replica, n)
            }
            None => {
                let cores = g
                    .refs(c, "activeResourceSpecifications")
                    .iter()
                    .filter(|r| g.str_attr(r, "activeResourceType").as_deref() == Some("CPU"))
                    .map(|r| g.num_attr(r, "numberOfReplicas").unwrap_or(1.0))
                    .sum::<f64>();
                (cores, 1.0)
            }
        };
        total += per_container + per_core * cores + per_replica * replicas;
    }
    let caches = d
        .sites
        .iter()
        .filter(|(_, _, m)| matches!(m, Mechanism::Cached { .. } | Mechanism::Composed { cache: Some(_), .. }))
        .count();
    total + per_cache * caches as f64
}

/// How much a call between two containers couples them, by mechanism (DESIGN §12 D32, D44, as
/// msa's coupling weights): the mechanism's `couplingWeight`, else by class: a bypass 2, a
/// synchronous or cached call 1, a composed call 0.5, an asynchronous call 0.3, a mechanism of
/// another layer 1. A call inside one container counts 1 whatever its mechanism.
fn coupling_weight(m: &Mechanism, weight: Option<f64>) -> f64 {
    if let Some(w) = weight {
        return w;
    }
    match m {
        Mechanism::Bypass { .. } => 2.0,
        Mechanism::Sync | Mechanism::Cached { .. } | Mechanism::Other => 1.0,
        Mechanism::Composed { .. } => 0.5,
        Mechanism::Async => 0.3,
    }
}

/// Cohesive MQ (DESIGN §12 D32): TurboMQ (Mancoridis et al.) of the call graph between the
/// movable components, with containers as clusters, where every component's own work counts as
/// cohesion of its container.
///
/// - Modules are the assemblies with a `Placement` that has a candidate outside
///   `RemoteCalls.direct`. Fixed components (no Placement) and components that can only sit in
///   direct containers (database servers) are infrastructure: they form no cluster, and calls into
///   them count neither way.
/// - A module's own work is its executions per time unit (calls into its SEFFs); it is a
///   self-loop of its container.
/// - A call between two modules weighs its expected calls per time unit, times the coupling
///   weight of its mechanism when the two sit in different containers. Calls of a module to
///   itself are part of its own work.
/// - Per container: CF = intra / (intra + inter / 2); MQ = Σ CF over containers with a module.
///
/// A loosely coupled one-component container scores close to 1, a single container scores 1,
/// and putting unrelated components together lowers the value.
fn modularity(g: &Graph, d: &Decisions, loads: &Loads, placement: &BTreeMap<String, String>) -> f64 {
    let direct: BTreeSet<String> = d.remote.as_ref().map(|r| r.direct.clone()).unwrap_or_default();
    let mut modules: BTreeSet<String> = BTreeSet::new();
    for p in g.all("Placement") {
        let Some(asm) = g.ref1(&p, "allocation").and_then(|a| g.ref1(&a, "assemblyContext")) else { continue };
        if g.refs(&p, "candidates").iter().any(|c| !direct.contains(c)) {
            modules.insert(asm);
        }
    }
    let weight: HashMap<&String, f64> =
        d.sites.iter().map(|(_, call, m)| (call, coupling_weight(m, d.q.coupling.get(call).copied()))).collect();
    let mut intra: BTreeMap<&String, f64> = BTreeMap::new();
    let mut inter: BTreeMap<&String, f64> = BTreeMap::new();
    for m in &modules {
        let Some(c) = placement.get(m) else { continue };
        *intra.entry(c).or_default() += loads.executions.get(m).copied().unwrap_or(0.0);
    }
    for ((a, b, action), w) in &loads.action_calls {
        if a == b || !modules.contains(a) || !modules.contains(b) {
            continue;
        }
        let (Some(ca), Some(cb)) = (placement.get(a), placement.get(b)) else { continue };
        if ca == cb {
            *intra.entry(ca).or_default() += w;
        } else {
            let w = w * weight.get(action).copied().unwrap_or(1.0);
            *inter.entry(ca).or_default() += w;
            *inter.entry(cb).or_default() += w;
        }
    }
    let clusters: BTreeSet<&String> = modules.iter().filter_map(|m| placement.get(m)).collect();
    clusters
        .into_iter()
        .map(|c| {
            let mu = intra.get(c).copied().unwrap_or(0.0);
            let eps = inter.get(c).copied().unwrap_or(0.0);
            let cf = if mu + eps > 0.0 { mu / (mu + eps / 2.0) } else { 0.0 };
            if std::env::var_os("ARCH_DEBUG_MQ").is_some() {
                eprintln!("MQ {} intra {mu:.3} inter {eps:.3} cf {cf:.4}", g.entity(c));
            }
            cf
        })
        .sum()
}

/// Is `class` part of a decision layer (not Palladio)?
pub fn is_decision(class: &str) -> bool {
    !is_pcm(class)
}

// ---------------------------------------------------------------------------------------------
// Rule 5: the model of a dynamic run (Kubernetes-style autoscaling, SimOxide's HPA)

/// The model of a dynamic run (rule 5, `translate_dynamic`): the translated model (rules 0 to
/// 4) with every autoscaled Deployable as **one pod** behind load balancers, the structure
/// SimOxide's HPA scales (SPD elastic infrastructure, Slingshot's adjustments; SimOxide
/// `DESIGN-SPD.md` §5.10):
///
/// - the pod is the Deployable's container with one replica's CPU: ⌈q⌉ cores at the per-core
///   speed rules 3 and 4 gave it (a quota's speed and a node's slowdown at the static replica
///   count stay fixed); its HPA request is q cores of full speed;
/// - every assembly on the pod gets a load balancer (a component whose SEFFs are one branch with
///   one call per pod, on the container `LoadBalancerServer`, which has no work): the callers'
///   connectors point at the load balancer, the load balancer at the assembly;
/// - the sidecar is not scaled: it gets the cores of `max` pods, so it never limits (H3);
/// - the SPD's target groups and the semantic configuration are written by `crate::dynamic`.
#[derive(Clone, Debug)]
pub struct DynamicModel {
    pub pcm: WireModel,
    pub groups: Vec<PodGroup>,
    /// The linking resource the pods join (the first of the resource environment), if any.
    pub link: Option<String>,
    pub profile: Option<LoadProfileSpec>,
    /// Cost of everything but the autoscaled replicas (D13, D22): containers, fixed replicas,
    /// caches.
    pub fixed_cost: f64,
    /// Milliseconds per model time unit.
    pub ms_per_unit: f64,
    /// Latency budget per usage scenario (instance name), ms.
    pub budgets: BTreeMap<String, f64>,
    /// Instance names of the repository, system, allocation and resource environment.
    pub roots: [String; 4],
    /// A per-scenario load profile (model time units) in place of `profile`'s steps (the
    /// generic `pcm*` dynamic run; `None` for the translation's runs).
    pub scenario_profile: Option<simoxide_sim::LoadProfile>,
    /// SimOxide's usage streams (common random numbers; off for the translation's runs).
    pub usage_streams: bool,
}

/// One autoscaled Deployable of a [`DynamicModel`].
#[derive(Clone, Debug)]
pub struct PodGroup {
    /// The pod's container (the target group's unit) and its entity name (the target group's
    /// and the autoscaler's name).
    pub container: String,
    pub name: String,
    /// (assembly on the pod, its load balancer's assembly)
    pub services: Vec<(String, String)>,
    pub target: f64,
    pub min: u32,
    pub max: u32,
    /// Pods at the start: the ideal autoscaler's count at the profile's first factor.
    pub initial: u32,
    /// Start-up delay of a new pod (s) and the scale-down stabilisation window (s, absent:
    /// Kubernetes' default).
    pub startup_s: f64,
    pub scale_down_s: Option<f64>,
    /// The HPA's request per pod, in the pod's cores (q of full speed).
    pub cpu_request: f64,
    pub pod_cores: f64,
    pub price_per_replica: f64,
    /// Offered load of the container and of its sidecar at the model's workload (full-speed
    /// cores), q and the sidecar's cores per replica: the ideal autoscaler's inputs.
    pub offered: (f64, f64),
    pub cpu_per_replica: f64,
    pub sidecar_cores: f64,
    /// `maxReplicas` of the Autoscaling, if given.
    pub max_ideal: Option<f64>,
}

impl PodGroup {
    /// The ideal autoscaler's replicas (D50) at `factor` times the model's workload.
    pub fn ideal(&self, factor: f64) -> f64 {
        let (u, us) = self.offered;
        let need = (factor * u / (self.cpu_per_replica * self.target)).max(factor * us / (self.sidecar_cores * self.target));
        let n = (need - 1e-9).ceil().max(1.0);
        match self.max_ideal {
            Some(m) if n > m => m,
            _ => n,
        }
    }
}

/// Default start-up delay of a pod (s): container start and readiness of a JVM service.
pub const DEFAULT_STARTUP_S: f64 = 30.0;

fn pods(b: &mut Builder, d: &Decisions, placement: &BTreeMap<String, String>, quality: &Quality) -> Result<DynamicModel, String> {
    let profile = d.profile.clone();
    let (f0, fmax) = match &profile {
        Some(p) => (p.factor_at(0.0), p.steps.iter().map(|s| s.1).fold(p.factor_at(0.0), f64::max)),
        None => (1.0, 1.0),
    };
    let (fixed_cost, per_replica) = cost_parts(&b.g, d, placement);
    // the load balancers' container: no work runs there
    let host = b.named("LoadBalancerServer", "ResourceContainer", Some("LoadBalancerServer"));
    let resenv = b.resenv.clone();
    b.g.link(&resenv, "resourceContainer", &host);
    let cpu = b.named(&format!("{host}_cpu"), "ProcessingResourceSpecification", None);
    b.g.set_attr(&cpu, "schedulingPolicy", s("PROCESSOR_SHARING"));
    b.g.set_attr(&cpu, "activeResourceType", s("CPU"));
    b.g.set_attr(&cpu, "processingRate", s("1000000.0"));
    b.g.set_attr(&cpu, "numberOfReplicas", WireValue::Int(1));
    b.g.link(&host, "activeResourceSpecifications", &cpu);
    if std::env::var_os("ARCH_NO_PRUNE").is_none() {
        prune_unreachable(b);
    }
    let link = sorted(b.g.all("LinkingResource")).into_iter().next();
    if let Some(l) = &link {
        b.g.link(l, "connectedResourceContainers", &host);
    }
    let mut groups = Vec::new();
    let mut scaled: Vec<&Deployable> = d.deployables.iter().filter(|x| x.autoscale.is_some()).collect();
    scaled.sort_by(|a, b| a.container.cmp(&b.container));
    for x in scaled {
        let a = x.autoscale.as_ref().expect("autoscaled");
        let c = x.container.clone();
        if !placement.values().any(|p| p == &c) {
            continue; // an empty container is not deployed
        }
        let mut group = PodGroup {
            container: c.clone(),
            name: b.g.entity(&c),
            services: Vec::new(),
            target: a.target,
            min: a.min.unwrap_or(1.0) as u32,
            max: 0,
            initial: 1,
            startup_s: a.startup_s.unwrap_or(DEFAULT_STARTUP_S),
            scale_down_s: a.scale_down_s,
            cpu_request: 0.0,
            pod_cores: x.cpu_per_replica.ceil(),
            price_per_replica: per_replica.get(&c).copied().unwrap_or(0.0),
            offered: b.offered.get(&c).copied().unwrap_or((0.0, 0.0)),
            cpu_per_replica: x.cpu_per_replica,
            sidecar_cores: x.sidecar_cores,
            max_ideal: a.max,
        };
        let peak = group.ideal(fmax);
        group.max = match a.max {
            Some(m) => m as u32,
            None => (2.0 * peak).max(4.0) as u32,
        };
        group.min = group.min.min(group.max).max(1);
        group.initial = (group.ideal(f0) as u32).clamp(group.min, group.max);
        // the pod: one replica, at the per-core speed of rules 3 and 4
        let base = *b.base_cpu.get(&c).ok_or_else(|| format!("container {} has no CPU", b.g.entity(&c)))?;
        let mut rate = base;
        for r in b.g.refs(&c, "activeResourceSpecifications") {
            if b.g.str_attr(&r, "activeResourceType").as_deref() != Some("CPU") {
                continue;
            }
            rate = b.g.str_attr(&r, "processingRate").and_then(|x| load::mean_of(&x)).unwrap_or(base);
            b.g.set_attr(&r, "numberOfReplicas", WireValue::Int(group.pod_cores as i32));
        }
        // utilisation relative to q full-speed cores: busy cores · rate / base / q
        group.cpu_request = x.cpu_per_replica * base / rate;
        // the sidecar: not scaled, with the cores of `max` pods
        if let Some(sc) = b.sidecars.get(&c).cloned() {
            for r in b.g.refs(&sc, "activeResourceSpecifications") {
                if b.g.str_attr(&r, "activeResourceType").as_deref() == Some("CPU") {
                    b.g.set_attr(&r, "numberOfReplicas", WireValue::Int((group.max as f64 * x.sidecar_cores) as i32));
                }
            }
        }
        let mut on_pod: Vec<String> = b
            .g
            .referrers(&c, "resourceContainer")
            .into_iter()
            .filter(|ac| b.g.class(ac) == "AllocationContext")
            .filter_map(|ac| b.g.ref1(&ac, "assemblyContext"))
            .collect();
        on_pod.sort();
        for asm in on_pod {
            let lb = load_balancer(b, &asm, &host)?;
            group.services.push((asm, lb));
        }
        groups.push(group);
    }
    let pcm = b.g.to_wire(|x| is_pcm(&x.class_name));
    Ok(DynamicModel {
        pcm,
        groups,
        link,
        profile,
        fixed_cost,
        ms_per_unit: d.ms_per_unit,
        budgets: quality.budgets.iter().cloned().collect(),
        roots: [b.repo.clone(), b.system.clone(), b.allocation.clone(), b.resenv.clone()],
        scenario_profile: None,
        usage_streams: false,
    })
}

/// A load balancer in front of assembly `asm`, on container `host`: a component that provides
/// and requires the assembly's one interface, with one SEFF per signature (a branch with one
/// transition that calls the signature, Slingshot's load-balancer pattern); the callers'
/// connectors now point at it. Returns its assembly.
fn load_balancer(b: &mut Builder, asm: &str, host: &str) -> Result<String, String> {
    let comp = one(&b.g, asm, "encapsulatedComponent")?;
    let provs: Vec<String> = b.g.refs(&comp, "providedRoles").into_iter().filter(|r| b.g.class(r) == "OperationProvidedRole").collect();
    let [prole] = provs.as_slice() else {
        return Err(format!("{} provides {} interfaces; a load balancer serves one", b.g.entity(&comp), provs.len()));
    };
    let prole = prole.clone();
    let iface = one(&b.g, &prole, "providedInterface")?;
    let conns: Vec<String> = sorted(b.g.referrers(asm, "providingAssemblyContext").into_iter().filter(|c| b.g.class(c) == "AssemblyConnector").collect());
    let delegs: Vec<String> = sorted(b.g.referrers(asm, "assemblyContext").into_iter().filter(|c| b.g.class(c) == "ProvidedDelegationConnector").collect());
    let label = b.g.entity(asm);
    let label = label.strip_prefix("Assembly_").unwrap_or(&label).to_string();
    let lb = b.component(&format!("LoadBalancer_{label}"));
    let lp = b.role(&lb, &iface, true);
    let lr = b.role(&lb, &iface, false);
    // the signatures the assembly still serves (rule 5 keeps the reachable SEFFs only)
    let served: BTreeSet<String> =
        b.g.refs(&comp, "serviceEffectSpecifications").iter().filter_map(|x| b.g.ref1(x, "describedService")).collect();
    for sig in b.g.refs(&iface, "signatures").into_iter().filter(|x| served.contains(x)) {
        // what the callers pass
        let mut inputs = BTreeSet::new();
        for conn in &conns {
            let (Some(req), Some(role)) = (b.g.ref1(conn, "requiringAssemblyContext"), b.g.ref1(conn, "requiredRole")) else { continue };
            let caller = one(&b.g, &req, "encapsulatedComponent")?;
            let (i, r) = b.forwarding(&caller, &role, &sig);
            if !r.is_empty() {
                return Err(format!(
                    "{}: a caller reads the RETURN of {}; Slingshot's load balancers hand nothing back",
                    b.g.entity(&comp),
                    b.g.entity(&sig)
                ));
            }
            inputs.extend(i);
        }
        let seff = b.seff(&lb, &sig);
        let branch = b.named(&format!("{seff}_balance"), "BranchAction", Some("balance"));
        let t = b.named(&format!("{branch}_1"), "ProbabilisticBranchTransition", Some("pod 1"));
        b.g.set_attr(&t, "branchProbability", WireValue::Double(1.0));
        b.g.link(&branch, "branches", &t);
        let body = b.named(&format!("{t}_body"), "ResourceDemandingBehaviour", None);
        b.g.link(&t, "branchBehaviour", &body);
        let call = b.external(&format!("{seff}_call"), &lr, &sig);
        for (n, ty) in &inputs {
            b.usage(&call, "inputVariableUsages", n, ty, &format!("{n}.{ty}"));
        }
        b.fill(&body, &[call]);
        b.fill(&seff, &[branch]);
    }
    let lb_asm = b.assembly(&lb, host);
    for conn in &conns {
        b.g.set_ref(conn, "providingAssemblyContext", &lb_asm);
        b.g.set_ref(conn, "providedRole", &lp);
    }
    for dc in &delegs {
        b.g.set_ref(dc, "assemblyContext", &lb_asm);
        b.g.set_ref(dc, "innerProvidedRole", &lp);
    }
    b.connector(&lb_asm, &lr, asm, &prole);
    Ok(lb_asm)
}

/// Slingshot's load balancers on plain PCM, as rule 5 builds them, for the generic dynamic runs
/// (`crate::pcmhpa`): a container `LoadBalancerServer` without work (joined to the first linking
/// resource) and a load balancer in front of every assembly allocated on each of `containers`.
/// Returns the model and (container, assembly, load balancer assembly) per assembly.
pub fn load_balanced(model: &WireModel, containers: &[String]) -> Result<(WireModel, Vec<(String, String, String)>), String> {
    let g = Graph::new(model);
    let first = |class: &str| g.all(class).into_iter().next().ok_or_else(|| format!("the model has no {class}"));
    let mut b = Builder {
        repo: first("Repository")?,
        system: first("System")?,
        allocation: first("Allocation")?,
        resenv: first("ResourceEnvironment")?,
        ms: 1.0,
        base_cpu: HashMap::new(),
        base_hdd: HashMap::new(),
        sidecars: BTreeMap::new(),
        replicas: HashMap::new(),
        sidecar_cores: HashMap::new(),
        roles: BTreeMap::new(),
        wired: BTreeMap::new(),
        local_assemblies: BTreeSet::new(),
        publishers: BTreeMap::new(),
        orchestrated: BTreeSet::new(),
        quota: Vec::new(),
        offered: BTreeMap::new(),
        warnings: Vec::new(),
        g,
    };
    let host = b.named("LoadBalancerServer", "ResourceContainer", Some("LoadBalancerServer"));
    let resenv = b.resenv.clone();
    b.g.link(&resenv, "resourceContainer", &host);
    let cpu = b.named(&format!("{host}_cpu"), "ProcessingResourceSpecification", None);
    b.g.set_attr(&cpu, "schedulingPolicy", s("PROCESSOR_SHARING"));
    b.g.set_attr(&cpu, "activeResourceType", s("CPU"));
    b.g.set_attr(&cpu, "processingRate", s("1000000.0"));
    b.g.set_attr(&cpu, "numberOfReplicas", WireValue::Int(1));
    b.g.link(&host, "activeResourceSpecifications", &cpu);
    if let Some(l) = sorted(b.g.all("LinkingResource")).into_iter().next() {
        b.g.link(&l, "connectedResourceContainers", &host);
    }
    let mut out = Vec::new();
    for c in containers {
        let mut on_pod: Vec<String> = b
            .g
            .referrers(c, "resourceContainer")
            .into_iter()
            .filter(|ac| b.g.class(ac) == "AllocationContext")
            .filter_map(|ac| b.g.ref1(&ac, "assemblyContext"))
            .collect();
        on_pod.sort();
        for asm in on_pod {
            let lb = load_balancer(&mut b, &asm, &host)?;
            out.push((c.clone(), asm, lb));
        }
    }
    Ok((b.g.to_wire(|_| true), out))
}

/// The cost of `cost` split into the part that does not depend on the autoscaled replicas and
/// the cost of one replica of each autoscaled container in use.
fn cost_parts(g: &Graph, d: &Decisions, placement: &BTreeMap<String, String>) -> (f64, BTreeMap<String, f64>) {
    let mut warnings = Vec::new();
    let total = cost(g, d, placement, &mut warnings);
    let [_, per_core, per_replica, _] = d.cost.unwrap_or([1.0, 0.0, 0.0, 0.0]);
    let used: BTreeSet<&String> = placement.values().collect();
    let mut per = BTreeMap::new();
    let mut scaled = 0.0;
    for x in &d.deployables {
        if x.autoscale.is_none() || !used.contains(&x.container) {
            continue;
        }
        let p = match d.prices.get(&x.container) {
            Some(p) => *p,
            None => per_core * x.cpu_per_replica + per_replica,
        };
        let w: f64 = x.levels.iter().map(|l| l.0).sum();
        let mean = x.levels.iter().map(|(wi, n)| wi * n).sum::<f64>() / w;
        scaled += p * mean;
        per.insert(x.container.clone(), p);
    }
    (total - scaled, per)
}

/// Rule 5's reduction: the SEFFs no request reaches go, with their contents. A request starts at
/// an entry-level call of the usage model, enters the system through its delegation, and runs a
/// SEFF per (assembly, signature); every external call anywhere in a SEFF continues over its
/// connector. The middleware of rule 2 has one SEFF per signature of an interface, and most of
/// them are never called through their connector: in today's cut they are half of the model,
/// and every scaling step rebuilds the whole model. Nothing a run executes changes.
fn prune_unreachable(b: &mut Builder) {
    let g = &b.g;
    let mut conn: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut deleg: HashMap<String, Vec<String>> = HashMap::new();
    for c in g.all("AssemblyConnector") {
        if let (Some(req), Some(role), Some(prov)) =
            (g.ref1(&c, "requiringAssemblyContext"), g.ref1(&c, "requiredRole"), g.ref1(&c, "providingAssemblyContext"))
        {
            conn.entry((req, role)).or_default().push(prov);
        }
    }
    for c in g.all("ProvidedDelegationConnector") {
        if let (Some(outer), Some(asm)) = (g.ref1(&c, "outerProvidedRole"), g.ref1(&c, "assemblyContext")) {
            deleg.entry(outer).or_default().push(asm);
        }
    }
    let mut seff_of: HashMap<(String, String), String> = HashMap::new();
    let mut all_seffs = Vec::new();
    for comp in g.all("BasicComponent") {
        for x in g.refs(&comp, "serviceEffectSpecifications") {
            if let Some(sig) = g.ref1(&x, "describedService") {
                seff_of.insert((comp.clone(), sig), x.clone());
            }
            all_seffs.push(x);
        }
    }
    let mut work: Vec<(String, String)> = Vec::new();
    for call in g.all("EntryLevelSystemCall") {
        if let (Some(role), Some(sig)) = (g.ref1(&call, "providedRole"), g.ref1(&call, "operationSignature")) {
            for asm in deleg.get(&role).into_iter().flatten() {
                work.push((asm.clone(), sig.clone()));
            }
        }
    }
    let mut seen = HashSet::new();
    let mut keep = HashSet::new();
    while let Some((asm, sig)) = work.pop() {
        if !seen.insert((asm.clone(), sig.clone())) {
            continue;
        }
        let Some(comp) = g.ref1(&asm, "encapsulatedComponent") else { continue };
        let Some(seff) = seff_of.get(&(comp, sig)) else { continue };
        keep.insert(seff.clone());
        for x in g.subtree(seff) {
            if g.class(&x) != "ExternalCallAction" {
                continue;
            }
            let (Some(role), Some(csig)) = (g.ref1(&x, "role"), g.ref1(&x, "calledService")) else { continue };
            for prov in conn.get(&(asm.clone(), role)).into_iter().flatten() {
                work.push((prov.clone(), csig.clone()));
            }
        }
    }
    let mut gone = HashSet::new();
    for x in all_seffs {
        if !keep.contains(&x) {
            gone.extend(b.g.subtree(&x));
        }
    }
    b.g.remove_all(&gone);
}
