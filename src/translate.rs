//! The completion: architecture decisions (layers 2 and 3) → plain PCM (DESIGN.md §4, §5).
//!
//! Applied to a copy of the model, in this order:
//! 0. layer 3: a call with a `LocalCopy` mechanism reads the copy; every feeding write sends the
//!    copy an asynchronous message;
//! 1. mechanisms: `AsyncCall` wraps the call in an asynchronous fork, `CachedCall` in a
//!    probabilistic branch;
//! 2. remote calls: a stub (with the caller) and a skeleton (with the callee) on every assembly
//!    connector between two containers;
//! 3. replicas: n replicas of q cores become n·⌈q⌉ cores at speed q/⌈q⌉;
//! 4. nodes: the M/M/c slowdown of each node caps the speed of its CPUs.
//!
//! Every step works in a fixed order (decisions and connectors by name), and generated names
//! derive from the names of what they complete, so the result does not depend on the order of
//! the instances in the model.

use crate::graph::{Graph, is_pcm};
use crate::load::{self, Loads};
use palladio_sim_service::protocol::{WireModel, WireValue};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
}

// ---------------------------------------------------------------------------------------------
// Decisions

#[derive(Clone, Debug)]
enum Mechanism {
    Sync,
    Async,
    Cached { hit_ratio: f64, hit_cpu_ms: f64 },
    LocalCopy { read_db_ms: f64, write_cpu_ms: f64, write_db_ms: f64 },
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
}

struct Decisions {
    ms_per_unit: f64,
    sites: Vec<(String, String, Mechanism)>, // (call site, call, mechanism)
    remote: Option<RemoteCalls>,
    deployables: Vec<(String, f64, f64)>, // (container, replicas, cpu per replica)
    nodes: Vec<(String, f64, f64, Vec<String>)>, // (name, cores, background, containers)
    cost: Option<[f64; 4]>,                  // perContainer, perCore, perReplicaOverhead, perCache
    feeds: Vec<(String, String, Vec<String>, String)>, // (feed, site, writers, store)
    any: bool,
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
                "LocalCopy" => Mechanism::LocalCopy {
                    read_db_ms: num(g, m, "readDbMs")?,
                    write_cpu_ms: num(g, m, "writeCpuMs")?,
                    write_db_ms: num(g, m, "writeDbMs")?,
                },
                other => return Err(format!("{m}: unknown mechanism class {other}")),
            })
        };
        let mut sites = Vec::new();
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
            sites.push((cs.clone(), call, mechanism(&m)?));
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
            }),
        };
        let mut deployables = Vec::new();
        let mut seen = BTreeSet::new();
        for d in sorted(g.all("Deployable")) {
            let c = one(g, &d, "container")?;
            if !seen.insert(c.clone()) {
                return Err(format!("container {c} has two Deployables"));
            }
            let opt = one(g, &d, "replicas")?;
            if !g.refs(&d, "allowedReplicas").contains(&opt) {
                warnings.push(format!("{d}: replica option {opt} is not among the allowed ones"));
            }
            let n = num(g, &opt, "count")?;
            let q = num(g, &d, "cpuPerReplica")?;
            if n < 1.0 || !(q > 0.0) {
                return Err(format!("{d}: needs at least one replica and a positive cpuPerReplica"));
            }
            deployables.push((c, n.floor(), q));
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
        let mut feeds = Vec::new();
        for f in sorted(g.all("CopyFeed")) {
            feeds.push((f.clone(), one(g, &f, "site")?, sorted(g.refs(&f, "writers")), one(g, &f, "store")?));
        }
        for p in sorted(g.all("Placement")) {
            let a = one(g, &p, "allocation")?;
            if let Some(c) = g.ref1(&a, "resourceContainer")
                && !g.refs(&p, "candidates").contains(&c)
            {
                warnings.push(format!("{p}: the component sits on {c}, which is not a candidate"));
            }
        }
        let any = !sites.is_empty() || remote.is_some() || !deployables.is_empty() || !nodes.is_empty() || !feeds.is_empty();
        Ok(Decisions { ms_per_unit, sites, remote, deployables, nodes, cost, feeds, any })
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
    async_roles: BTreeSet<String>,
    warnings: Vec<String>,
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
        let n = self.replicas.get(container).copied().unwrap_or(1.0);
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
    let g = Graph::new(model);
    let mut warnings = Vec::new();
    let d = Decisions::read(&g, &mut warnings)?;
    let original = load::analyse(&g);
    warnings.extend(original.warnings.iter().cloned());
    let mut placement = BTreeMap::new();
    for a in g.all("AllocationContext") {
        if let (Some(asm), Some(c)) = (g.ref1(&a, "assemblyContext"), g.ref1(&a, "resourceContainer")) {
            placement.insert(asm, c);
        }
    }
    let modularity = modularity(&original.calls, &placement);
    let cost = cost(&g, &d, &placement, &mut warnings);

    if !d.any {
        let pcm = g.to_wire(|x| is_pcm(&x.class_name));
        return Ok(Translation {
            pcm,
            loads: original.clone(),
            calls: original.calls,
            placement,
            nodes: Vec::new(),
            cost,
            modularity,
            warnings,
            translated: false,
        });
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
        replicas: d.deployables.iter().map(|(c, n, _)| (c.clone(), *n)).collect(),
        async_roles: BTreeSet::new(),
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
    local_copies(&mut b, &d)?;
    mechanisms(&mut b, &d)?;
    if let Some(rc) = &d.remote {
        remote_calls(&mut b, rc)?;
    }
    replicas(&mut b, &d)?;
    let nodes = nodes(&mut b, &d);
    let loads = load::analyse(&b.g);
    let mut warnings = b.warnings;
    for w in &loads.warnings {
        if !warnings.contains(w) {
            warnings.push(w.clone());
        }
    }
    let pcm = b.g.to_wire(|x| is_pcm(&x.class_name));
    Ok(Translation { pcm, loads, calls: original.calls, placement, nodes, cost, modularity, warnings, translated: true })
}

/// Layer 3: local copies.
fn local_copies(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    // store → (interface, component, provided role, assembly)
    let mut stores: BTreeMap<String, (String, String, String, String)> = BTreeMap::new();
    // (component, store) → required role
    let mut roles: BTreeMap<(String, String), String> = BTreeMap::new();
    // writer → forked behaviour that sends its messages
    let mut writer_forks: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for (cs, call, m) in &d.sites {
        let Mechanism::LocalCopy { read_db_ms, write_cpu_ms, write_db_ms } = m else { continue };
        let Some((_, _, writers, store)) = d.feeds.iter().find(|f| &f.1 == cs) else {
            return Err(format!("{cs} reads a LocalCopy, but no CopyFeed feeds it"));
        };
        let store_c = b.container_of(store).ok_or_else(|| format!("the copy store {store} is not allocated"))?;
        if !stores.contains_key(store) {
            let iface = b.named(&format!("{store}_copies"), "OperationInterface", Some(&format!("{store}_copies")));
            let repo = b.repo.clone();
            b.g.link(&repo, "interfaces", &iface);
            let comp = b.component(&format!("{store}_copyStore"));
            let prov = b.role(&comp, &iface, true);
            let asm = b.assembly(&comp, &store_c);
            stores.insert(store.clone(), (iface, comp, prov, asm));
        }
        let (iface, comp, prov, asm) = stores[store].clone();
        let read = b.named(&format!("{iface}_read_{cs}"), "OperationSignature", Some(&format!("read_{cs}")));
        let write = b.named(&format!("{iface}_write_{cs}"), "OperationSignature", Some(&format!("write_{cs}")));
        b.g.link(&iface, "signatures", &read);
        b.g.link(&iface, "signatures", &write);
        let rs = b.seff(&comp, &read);
        let units = b.hdd_units(&store_c, *read_db_ms)?;
        let a = b.internal(&rs, "readCopy", &[("HDD", units)]);
        b.fill(&rs, &[a]);
        let ws = b.seff(&comp, &write);
        let (cu, hu) = (b.cpu_units(&store_c, *write_cpu_ms)?, b.hdd_units(&store_c, *write_db_ms)?);
        let a = b.internal(&ws, "writeCopy", &[("CPU", cu), ("HDD", hu)]);
        b.fill(&ws, &[a]);

        // the reader: the call reads the copy
        let caller = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
        let role = match roles.get(&(caller.clone(), store.clone())) {
            Some(r) => r.clone(),
            None => {
                let r = b.role(&caller, &iface, false);
                for asm_c in b.assemblies_of(&caller) {
                    b.connector(&asm_c, &r, &asm, &prov);
                }
                roles.insert((caller.clone(), store.clone()), r.clone());
                r
            }
        };
        b.g.set_ref(call, "role", &role);
        b.g.set_ref(call, "calledService", &read);
        for u in b.g.refs(call, "inputVariableUsages").into_iter().chain(b.g.refs(call, "returnVariableUsage")) {
            for ch in b.g.refs(&u, "variableCharacterisation") {
                b.g.remove(&ch);
            }
            b.g.remove(&u);
        }

        // the writers: an asynchronous message to the copy after each feeding write
        for w in writers {
            if !(b.g.is_a(w, "ExternalCallAction") || b.g.is_a(w, "InternalAction")) {
                return Err(format!("CopyFeed writer {w} is a {}, not an ExternalCallAction or InternalAction", b.g.class(w)));
            }
            let owner = b.component_of(w).ok_or_else(|| format!("{w} is in no component"))?;
            let role = match roles.get(&(owner.clone(), store.clone())) {
                Some(r) if b.async_roles.contains(r) => r.clone(),
                _ => {
                    let r = b.role(&owner, &iface, false);
                    for asm_o in b.assemblies_of(&owner) {
                        b.connector(&asm_o, &r, &asm, &prov);
                    }
                    b.async_roles.insert(r.clone());
                    roles.insert((owner.clone(), store.clone()), r.clone());
                    r
                }
            };
            let send = b.external(&format!("{w}_feed_{cs}"), &role, &write);
            writer_forks.entry(w.clone()).or_insert_with(|| (owner.clone(), Vec::new())).1.push(send);
        }
    }
    for (w, (_, sends)) in writer_forks {
        let fork = b.named(&format!("{w}_feed"), "ForkAction", None);
        let fb = b.named(&format!("{w}_feed_async"), "ForkedBehaviour", None);
        b.g.link(&fork, "asynchronousForkedBehaviours", &fb);
        b.fill(&fb, &sends);
        b.insert_after(&w, &fork)?;
    }
    Ok(())
}

/// Rule 1: asynchronous and cached calls.
fn mechanisms(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    let broker = d.remote.as_ref().and_then(|r| r.broker.clone());
    let mut async_roles: BTreeMap<(String, String), String> = BTreeMap::new();
    for (_, call, m) in &d.sites {
        match m {
            Mechanism::Sync | Mechanism::LocalCopy { .. } => {}
            Mechanism::Async => {
                let fork = b.named(&format!("{call}_async"), "ForkAction", None);
                let fb = b.named(&format!("{call}_async_behaviour"), "ForkedBehaviour", None);
                b.g.link(&fork, "asynchronousForkedBehaviours", &fb);
                b.replace_in_chain(call, &fork)?;
                b.fill(&fb, std::slice::from_ref(call));
                // Through the broker, an asynchronous call needs a role of its own.
                if broker.is_some()
                    && let Some(role) = b.g.ref1(call, "role")
                    && let Some(comp) = b.component_of(call)
                {
                    let key = (comp.clone(), role.clone());
                    let r = match async_roles.get(&key) {
                        Some(r) => r.clone(),
                        None => {
                            let iface = one(&b.g, &role, "requiredInterface")?;
                            let r = b.role(&comp, &iface, false);
                            b.g.set_attr(&r, "entityName", s(format!("{}_async", b.g.entity(&role))));
                            for asm in b.assemblies_of(&comp) {
                                let conn = b
                                    .g
                                    .referrers(&asm, "requiringAssemblyContext")
                                    .into_iter()
                                    .find(|c| b.g.ref1(c, "requiredRole").as_deref() == Some(role.as_str()));
                                if let Some(conn) = conn {
                                    let p = one(&b.g, &conn, "providingAssemblyContext")?;
                                    let pr = one(&b.g, &conn, "providedRole")?;
                                    b.connector(&asm, &r, &p, &pr);
                                }
                            }
                            b.async_roles.insert(r.clone());
                            async_roles.insert(key, r.clone());
                            r
                        }
                    };
                    b.g.set_ref(call, "role", &r);
                }
            }
            Mechanism::Cached { hit_ratio, hit_cpu_ms } => {
                let comp = b.component_of(call).ok_or_else(|| format!("{call} is in no component"))?;
                let asm = b.assemblies_of(&comp).into_iter().next().ok_or_else(|| format!("{} is not in the system", b.g.entity(&comp)))?;
                let c = b.container_of(&asm).ok_or_else(|| format!("{asm} is not allocated"))?;
                let units = b.cpu_units(&c, *hit_cpu_ms)?;
                let branch = b.named(&format!("{call}_cache"), "BranchAction", Some("cache"));
                let hit = b.named(&format!("{branch}_hit"), "ProbabilisticBranchTransition", Some("hit"));
                let miss = b.named(&format!("{branch}_miss"), "ProbabilisticBranchTransition", Some("miss"));
                b.g.set_attr(&hit, "branchProbability", WireValue::Double(*hit_ratio));
                b.g.set_attr(&miss, "branchProbability", WireValue::Double(1.0 - hit_ratio));
                b.g.link(&branch, "branches", &hit);
                b.g.link(&branch, "branches", &miss);
                let hb = b.named(&format!("{hit}_body"), "ResourceDemandingBehaviour", None);
                let mb = b.named(&format!("{miss}_body"), "ResourceDemandingBehaviour", None);
                b.g.link(&hit, "branchBehaviour", &hb);
                b.g.link(&miss, "branchBehaviour", &mb);
                b.replace_in_chain(call, &branch)?;
                let work = b.internal(&hit, "cacheHit", &[("CPU", units)]);
                b.fill(&hb, &[work]);
                b.fill(&mb, std::slice::from_ref(call));
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
        // co-located, or a role no call uses (any more)
        if cr == cp || b.g.referrers(&role, "role").is_empty() {
            continue;
        }
        let via_broker = b.async_roles.contains(&role) && broker.is_some();
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
        let s_b = match (&broker, via_broker) {
            (Some((_, _, biface, _)), true) => Some(b.role(&stub, biface, false)),
            _ => None,
        };
        let (out_ms, in_ms) = if via_broker { (rc.publish_cpu_ms, rc.deliver_cpu_ms) } else { (rc.caller_cpu_ms, rc.callee_cpu_ms) };
        let out_units = b.cpu_units(&stub_c, out_ms)?;
        let in_units = b.cpu_units(&skel_c, in_ms)?;
        if rc.latency_ms > 0.0 {
            b.ensure_delay(&stub_c);
        }
        let latency_units = rc.latency_ms / b.ms * b.delay_rate(&stub_c);
        for sig in b.g.refs(&iface, "signatures") {
            let (inputs, returns) = b.forwarding(&caller_comp, &role, &sig);
            // stub: caller CPU, (broker), latency, call on
            let seff = b.seff(&stub, &sig);
            let mut steps = vec![b.internal(&seff, if via_broker { "publish" } else { "marshal" }, &[("CPU", out_units)])];
            if let (Some(sb), Some((_, _, _, bsig))) = (&s_b, &broker) {
                steps.push(b.external(&format!("{seff}_broker"), sb, bsig));
            }
            if latency_units > 0.0 {
                steps.push(b.internal(&seff, "latency", &[("DELAY", latency_units)]));
            }
            steps.extend(b.forward_call(&format!("{seff}_call"), &s_r, &sig, &inputs, &returns));
            b.fill(&seff, &steps);
            // skeleton: callee CPU, call the component
            let seff = b.seff(&skel, &sig);
            let mut steps = vec![b.internal(&seff, if via_broker { "deliver" } else { "unmarshal" }, &[("CPU", in_units)])];
            steps.extend(b.forward_call(&format!("{seff}_call"), &k_r, &sig, &inputs, &returns));
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

/// Rule 3: replicas with a CPU limit each.
fn replicas(b: &mut Builder, d: &Decisions) -> Result<(), String> {
    for (c, n, q) in &d.deployables {
        let base = *b.base_cpu.get(c).ok_or_else(|| format!("container {} has no CPU", b.g.entity(c)))?;
        let per = q.ceil();
        for r in b.g.refs(c, "activeResourceSpecifications") {
            if b.g.str_attr(&r, "activeResourceType").as_deref() != Some("CPU") {
                continue;
            }
            b.g.set_attr(&r, "numberOfReplicas", WireValue::Int((n * per) as i32));
            b.g.set_attr(&r, "processingRate", s(lit(base * q / per)));
        }
    }
    Ok(())
}

/// Rule 4: node contention.
fn nodes(b: &mut Builder, d: &Decisions) -> Vec<NodeLoad> {
    if d.nodes.is_empty() {
        return Vec::new();
    }
    let loads = load::analyse(&b.g);
    let mut out = Vec::new();
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
        if slow > 1.0 {
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
        out.push(NodeLoad { name: name.clone(), cores: *cores, load: u, slowdown: slow });
    }
    out
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
        let (cores, replicas) = match d.deployables.iter().find(|(x, _, _)| x == c) {
            Some((_, n, q)) => (n * q, *n),
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
    let caches = d.sites.iter().filter(|(_, _, m)| matches!(m, Mechanism::Cached { .. })).count();
    total + per_cache * caches as f64
}

/// MQ (Mancoridis et al.) of the call graph with containers as clusters: the sum over clusters
/// of 2μ/(2μ + ε), μ the calls inside the cluster and ε the calls across its border, all
/// weighted by expected calls per time unit.
pub fn modularity(calls: &BTreeMap<(String, String), f64>, placement: &BTreeMap<String, String>) -> f64 {
    let mut intra: BTreeMap<&String, f64> = BTreeMap::new();
    let mut inter: BTreeMap<&String, f64> = BTreeMap::new();
    for ((a, b), w) in calls {
        let (Some(ca), Some(cb)) = (placement.get(a), placement.get(b)) else { continue };
        if a == b {
            continue;
        }
        if ca == cb {
            *intra.entry(ca).or_default() += w;
        } else {
            *inter.entry(ca).or_default() += w;
            *inter.entry(cb).or_default() += w;
        }
    }
    let clusters: BTreeSet<&String> = placement.values().collect();
    clusters
        .into_iter()
        .map(|c| {
            let mu = intra.get(c).copied().unwrap_or(0.0);
            let eps = inter.get(c).copied().unwrap_or(0.0);
            if mu > 0.0 { 2.0 * mu / (2.0 * mu + eps) } else { 0.0 }
        })
        .sum()
}

/// Is `class` part of a decision layer (not Palladio)?
pub fn is_decision(class: &str) -> bool {
    !is_pcm(class)
}
