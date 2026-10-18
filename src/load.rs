//! Offered load and call frequencies of a plain PCM model, without simulating it.
//!
//! The analysis walks the usage model and the SEFFs it reaches, as the simulator would, and adds
//! up the expected resource demand per processing resource and the expected number of calls
//! between assembly contexts, per time unit:
//! - demands, loop counts and branch probabilities contribute their expected values; a
//!   variable-free StoEx has its mean computed once (exactly for constants and `Exp(x)`, else
//!   from 50 000 draws with a fixed seed);
//! - parameters (input, return and component parameter characterisations) are evaluated as the
//!   simulator evaluates them. When a walk depends on a random parameter value (a guarded
//!   branch on a `BoolPMF` argument, a demand of `file.BYTESIZE`), the scenario is walked 4 000
//!   times with sampled values and the results averaged; otherwise one walk is exact;
//! - an open workload arrives at 1/E[inter-arrival time]; a closed one at
//!   population / (E[think time] + the no-load response time).
//!
//! Utilisation is busy cores over cores: Σ arrival rate × expected units / processing rate,
//! divided by the resource's number of replicas.

use crate::graph::Graph;
use simoxide_random::UniformSource;
use simoxide_stoex::{Env, EvalError, Program, Value, VariableMode};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Load of one processing resource.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceLoad {
    /// The ProcessingResourceSpecification.
    pub spec: String,
    pub container: String,
    /// `CPU`, `HDD` or `DELAY`.
    pub kind: String,
    /// Processing rate of one replica (units per time unit).
    pub rate: f64,
    pub replicas: f64,
    /// Expected busy replicas (Σ arrival rate × units / rate).
    pub busy: f64,
    /// The utilisation on another basis than busy / replicas (a CPU quota: on its capacity).
    pub capacity_basis: Option<f64>,
}

impl ResourceLoad {
    pub fn utilisation(&self) -> f64 {
        if let Some(u) = self.capacity_basis {
            return u;
        }
        if self.kind == "DELAY" || self.replicas <= 0.0 { 0.0 } else { self.busy / self.replicas }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Loads {
    pub resources: Vec<ResourceLoad>,
    /// Expected calls per time unit between assembly contexts (caller, callee).
    pub calls: BTreeMap<(String, String), f64>,
    /// Expected calls per time unit per external call action: (caller, callee, action).
    pub action_calls: BTreeMap<(String, String, String), f64>,
    /// Expected executions per time unit of every assembly context (calls into its SEFFs, from
    /// the usage model and from other assemblies, itself included).
    pub executions: BTreeMap<String, f64>,
    /// Arrival rate per usage scenario.
    pub arrivals: BTreeMap<String, f64>,
    /// Expected response time per usage scenario without contention (the no-load time, in model
    /// time units): every demand at its resource's rate, the longest branch of a synchronous
    /// fork, nothing for asynchronous work.
    pub times: BTreeMap<String, f64>,
    /// Per usage scenario, expected calls per request of every external call action: (caller,
    /// callee, action). Only filled by `analyse_with(.., per_scenario = true)`.
    pub scenario_calls: BTreeMap<String, BTreeMap<(String, String, String), f64>>,
    pub warnings: Vec<String>,
}

impl Loads {
    pub fn of_container(&self, container: &str) -> impl Iterator<Item = &ResourceLoad> {
        self.resources.iter().filter(move |r| r.container == container)
    }
}

// ---------------------------------------------------------------------------------------------
// StoEx evaluation

struct Rng(u64);

impl UniformSource for Rng {
    fn next_uniform(&mut self) -> f64 {
        // SplitMix64, mapped to (0, 1).
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
}

#[derive(Default, Clone)]
struct Frame(HashMap<u32, Value>);

impl Env for Frame {
    fn lookup<R: UniformSource + ?Sized>(&self, slot: u32, _: &mut R) -> Result<Option<Value>, EvalError> {
        Ok(self.0.get(&slot).cloned())
    }
}

struct Exprs {
    ids: HashMap<String, u32>,
    programs: HashMap<String, Option<Arc<Program>>>,
    means: HashMap<String, Option<f64>>,
    warnings: Vec<String>,
}

impl Exprs {
    fn slot(&mut self, id: &str) -> u32 {
        let n = self.ids.len() as u32;
        *self.ids.entry(id.to_string()).or_insert(n)
    }

    fn program(&mut self, src: &str) -> Option<Arc<Program>> {
        if let Some(p) = self.programs.get(src) {
            return p.clone();
        }
        let mut ids = std::mem::take(&mut self.ids);
        let p = Program::from_str(src, |v| {
            let n = ids.len() as u32;
            *ids.entry(v.id().to_string()).or_insert(n)
        });
        self.ids = ids;
        let p = match p {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                self.warnings.push(format!("cannot read the expression {src:?}: {e:?}"));
                None
            }
        };
        self.programs.insert(src.to_string(), p.clone());
        p
    }

    /// The mean of a variable-free expression, or None if it has variables.
    fn mean(&mut self, src: &str) -> Option<f64> {
        if let Some(m) = self.means.get(src) {
            return *m;
        }
        let m = self.compute_mean(src);
        self.means.insert(src.to_string(), m);
        m
    }

    fn compute_mean(&mut self, src: &str) -> Option<f64> {
        let p = self.program(src)?;
        if !p.vars().is_empty() {
            return None;
        }
        if let Some(c) = p.constant() {
            return c.to_f64().ok();
        }
        let t = src.trim();
        if let Some(inner) = t.strip_prefix("Exp(").and_then(|x| x.strip_suffix(')'))
            && let Ok(rate) = inner.trim().parse::<f64>()
            && rate > 0.0
        {
            return Some(1.0 / rate);
        }
        let mut rng = Rng(0x5EED);
        let n = 50_000;
        let mut sum = 0.0;
        for _ in 0..n {
            match p.eval(&Frame::default(), &mut rng).and_then(|v| v.to_f64()) {
                Ok(v) => sum += v,
                Err(e) => {
                    self.warnings.push(format!("cannot evaluate {src:?}: {e:?}"));
                    return Some(0.0);
                }
            }
        }
        Some(sum / n as f64)
    }
}

// ---------------------------------------------------------------------------------------------
// The model, indexed

struct Res {
    spec: String,
    rate: f64,
}

struct Index<'a> {
    g: &'a Graph,
    /// (assembly, required role) → providing assembly
    connectors: HashMap<(String, String), String>,
    /// system provided role → (assembly)
    delegations: HashMap<String, String>,
    container_of: HashMap<String, String>,
    /// (container, resource type) → resource
    resources: HashMap<(String, String), Res>,
    /// (component, signature) → SEFF
    seffs: HashMap<(String, String), String>,
}

impl<'a> Index<'a> {
    fn new(g: &'a Graph, ex: &mut Exprs) -> Index<'a> {
        let mut connectors = HashMap::new();
        for c in g.all("AssemblyConnector") {
            if let (Some(a), Some(r), Some(p)) =
                (g.ref1(&c, "requiringAssemblyContext"), g.ref1(&c, "requiredRole"), g.ref1(&c, "providingAssemblyContext"))
            {
                connectors.insert((a, r), p);
            }
        }
        let mut delegations = HashMap::new();
        for c in g.all("ProvidedDelegationConnector") {
            if let (Some(o), Some(a)) = (g.ref1(&c, "outerProvidedRole"), g.ref1(&c, "assemblyContext")) {
                delegations.insert(o, a);
            }
        }
        let mut container_of = HashMap::new();
        for a in g.all("AllocationContext") {
            if let (Some(asm), Some(c)) = (g.ref1(&a, "assemblyContext"), g.ref1(&a, "resourceContainer")) {
                container_of.insert(asm, c);
            }
        }
        let mut resources = HashMap::new();
        for c in g.all("ResourceContainer") {
            for s in g.refs(&c, "activeResourceSpecifications") {
                let ty = g.str_attr(&s, "activeResourceType").unwrap_or_else(|| "CPU".into());
                let rate = g.str_attr(&s, "processingRate").and_then(|r| ex.mean(&r)).unwrap_or(1.0);
                resources.entry((c.clone(), ty)).or_insert(Res { spec: s.clone(), rate });
            }
        }
        let mut seffs = HashMap::new();
        for comp in g.all("BasicComponent") {
            for s in g.refs(&comp, "serviceEffectSpecifications") {
                if let Some(sig) = g.ref1(&s, "describedService") {
                    seffs.insert((comp.clone(), sig), s);
                }
            }
        }
        Index { g, connectors, delegations, container_of, resources, seffs }
    }
}

// ---------------------------------------------------------------------------------------------
// The walk

struct Walk<'a, 'b> {
    ix: &'b Index<'a>,
    ex: &'b mut Exprs,
    rng: Rng,
    /// units per resource spec
    demand: HashMap<String, f64>,
    calls: HashMap<(String, String), f64>,
    action_calls: HashMap<(String, String, String), f64>,
    executions: HashMap<String, f64>,
    /// Calls that do not reach their callee for `executions` and `action_calls` (a redirected call).
    skip: &'b BTreeSet<String>,
    /// Calls whose callee runs with this share of the caller's weight (a cache miss: 1 − hit).
    scale: &'b HashMap<String, f64>,
    /// > 0 inside such a call.
    skipping: usize,
    random: bool,
    depth: usize,
    warned: std::collections::HashSet<String>,
}

const ID_TYPES: &[(&str, &str)] =
    &[("VALUE", "VALUE"), ("BYTESIZE", "BYTESIZE"), ("NUMBER_OF_ELEMENTS", "NUMBER_OF_ELEMENTS"), ("TYPE", "TYPE"), ("STRUCTURE", "STRUCTURE")];

impl Walk<'_, '_> {
    fn warn(&mut self, w: String) {
        if self.warned.insert(w.clone()) {
            self.ex.warnings.push(w);
        }
    }

    /// Evaluates `src` in `frame` (a value, not a mean).
    fn value(&mut self, src: &str, frame: &Frame) -> Option<Value> {
        let p = self.ex.program(src)?;
        if p.constant().is_none() {
            self.random = true;
        }
        match p.eval_mode(frame, &mut self.rng, VariableMode::ReturnDefaultOnNotFound) {
            Ok(v) => Some(v),
            Err(e) => {
                self.warn(format!("cannot evaluate {src:?}: {e:?}"));
                None
            }
        }
    }

    /// The expected value of `src` in `frame`.
    fn expect(&mut self, src: &str, frame: &Frame) -> f64 {
        if let Some(m) = self.ex.mean(src) {
            return m;
        }
        self.value(src, frame).and_then(|v| v.to_f64().ok()).unwrap_or(0.0)
    }

    /// Binds the characterisations of `usages` (VariableUsages) evaluated in `from` into `to`.
    fn bind(&mut self, usages: &[String], from: &Frame, to: &mut Frame) {
        let g = self.ix.g;
        for u in usages {
            let Some(name) = g.str_attr(u, "namedReference") else { continue };
            for ch in g.refs(u, "variableCharacterisation") {
                let ty = g.str_attr(&ch, "type").unwrap_or_else(|| "VALUE".into());
                let ty = ID_TYPES.iter().find(|(k, _)| *k == ty).map_or("VALUE", |(_, v)| v);
                let Some(spec) = g.str_attr(&ch, "specification") else { continue };
                if let Some(v) = self.value(&spec, from) {
                    let slot = self.ex.slot(&format!("{name}.{ty}"));
                    to.0.insert(slot, v);
                }
            }
        }
    }

    fn component_frame(&mut self, asm: &str) -> Frame {
        let g = self.ix.g;
        let mut f = Frame::default();
        if let Some(comp) = g.ref1(asm, "encapsulatedComponent") {
            let usages = g.refs(&comp, "componentParameterUsage");
            self.bind(&usages, &Frame::default(), &mut f);
        }
        let usages = g.refs(asm, "configParameterUsages");
        self.bind(&usages, &Frame::default(), &mut f);
        f
    }

    /// Calls `sig` on assembly `asm` with weight `w`; returns the result frame and the time.
    fn call(&mut self, asm: &str, sig: &str, mut frame: Frame, w: f64) -> (Frame, f64) {
        let g = self.ix.g;
        if self.depth > 64 {
            self.warn(format!("calls nested deeper than 64 at {asm}: cut"));
            return (Frame::default(), 0.0);
        }
        let Some(comp) = g.ref1(asm, "encapsulatedComponent") else { return (Frame::default(), 0.0) };
        let Some(seff) = self.ix.seffs.get(&(comp.clone(), sig.to_string())).cloned() else {
            self.warn(format!("{} has no SEFF for {}", g.entity(&comp), g.entity(sig)));
            return (Frame::default(), 0.0);
        };
        if self.skipping == 0 {
            *self.executions.entry(asm.to_string()).or_default() += w;
        }
        for (k, v) in self.component_frame(asm).0 {
            frame.0.entry(k).or_insert(v);
        }
        self.depth += 1;
        let mut result = Frame::default();
        let t = self.behaviour(asm, &seff, &mut frame, &mut result, w);
        self.depth -= 1;
        (result, t)
    }

    /// Walks a behaviour from its start action; returns the expected time.
    fn behaviour(&mut self, asm: &str, beh: &str, frame: &mut Frame, result: &mut Frame, w: f64) -> f64 {
        let g = self.ix.g;
        let steps = g.refs(beh, "steps");
        let mut cur = steps.iter().find(|s| g.is_a(s, "StartAction")).cloned();
        let mut time = 0.0;
        let mut n = 0;
        while let Some(a) = cur {
            n += 1;
            if n > 10_000 {
                break;
            }
            time += self.action(asm, &a, frame, result, w);
            cur = g.ref1(&a, "successor");
        }
        time
    }

    fn action(&mut self, asm: &str, a: &str, frame: &mut Frame, result: &mut Frame, w: f64) -> f64 {
        let g = self.ix.g;
        let class = g.class(a).to_string();
        let mut time = 0.0;
        for d in g.refs(a, "resourceDemand") {
            let ty = g.str_attr(&d, "requiredResource").unwrap_or_else(|| "CPU".into());
            let units = g.str_attr(&d, "specification").map_or(0.0, |s| self.expect(&s, frame));
            let Some(container) = self.ix.container_of.get(asm) else { continue };
            match self.ix.resources.get(&(container.clone(), ty.clone())) {
                Some(r) => {
                    *self.demand.entry(r.spec.clone()).or_default() += w * units;
                    if r.rate > 0.0 {
                        time += units / r.rate;
                    }
                }
                None => self.warn(format!("{} has no {ty} resource for {}", g.entity(container), g.entity(a))),
            }
        }
        match class.as_str() {
            "ExternalCallAction" => {
                let (Some(role), Some(sig)) = (g.ref1(a, "role"), g.ref1(a, "calledService")) else { return time };
                let Some(target) = self.ix.connectors.get(&(asm.to_string(), role.clone())).cloned() else {
                    return time; // a call out of the system
                };
                *self.calls.entry((asm.to_string(), target.clone())).or_default() += w;
                if self.skipping == 0 {
                    *self.action_calls.entry((asm.to_string(), target.clone(), a.to_string())).or_default() += w;
                }
                let skipped = self.skip.contains(a);
                self.skipping += skipped as usize;
                let mut callee = Frame::default();
                let inputs = g.refs(a, "inputVariableUsages");
                self.bind(&inputs, frame, &mut callee);
                let wc = w * self.scale.get(a).copied().unwrap_or(1.0);
                let (ret, t) = self.call(&target, &sig, callee, wc);
                self.skipping -= skipped as usize;
                // return values: evaluated against the callee's result (RETURN.*)
                let mut ret_env = ret.clone();
                for (k, v) in &frame.0 {
                    ret_env.0.entry(*k).or_insert(v.clone());
                }
                let outs = g.refs(a, "returnVariableUsage");
                let mut back = Frame::default();
                self.bind(&outs, &ret_env, &mut back);
                frame.0.extend(back.0);
                time += t;
            }
            "SetVariableAction" => {
                let usages = g.refs(a, "localVariableUsages");
                let mut out = Frame::default();
                self.bind(&usages, frame, &mut out);
                result.0.extend(out.0.clone());
                frame.0.extend(out.0);
            }
            "BranchAction" => {
                let ts = g.refs(a, "branches");
                let mut guarded_done = false;
                let mut t_sum = 0.0;
                for t in ts {
                    let Some(body) = g.ref1(&t, "branchBehaviour") else { continue };
                    if g.class(&t) == "ProbabilisticBranchTransition" {
                        let p = g.num_attr(&t, "branchProbability").unwrap_or(0.0);
                        if p > 0.0 {
                            t_sum += p * self.behaviour(asm, &body, frame, result, w * p);
                        }
                    } else if !guarded_done {
                        let cond = g.str_attr(&t, "branchCondition").unwrap_or_else(|| "true".into());
                        let yes = self.value(&cond, frame).and_then(|v| v.to_bool().ok()).unwrap_or(false);
                        if yes {
                            guarded_done = true;
                            t_sum += self.behaviour(asm, &body, frame, result, w);
                        }
                    }
                }
                time += t_sum;
            }
            "LoopAction" => {
                let k = g.str_attr(a, "iterationCount").map_or(1.0, |s| self.expect(&s, frame));
                if let Some(body) = g.ref1(a, "bodyBehaviour")
                    && k > 0.0
                {
                    time += k * self.behaviour(asm, &body, frame, result, w * k);
                }
            }
            "CollectionIteratorAction" => {
                let k = g
                    .ref1(a, "parameter")
                    .and_then(|p| g.str_attr(&p, "parameterName"))
                    .map_or(1.0, |p| self.expect(&format!("{p}.NUMBER_OF_ELEMENTS"), frame));
                if let Some(body) = g.ref1(a, "bodyBehaviour")
                    && k > 0.0
                {
                    time += k * self.behaviour(asm, &body, frame, result, w * k);
                }
            }
            "ForkAction" => {
                for fb in g.refs(a, "asynchronousForkedBehaviours") {
                    let mut f = frame.clone();
                    self.behaviour(asm, &fb, &mut f, &mut Frame::default(), w);
                }
                let mut longest: f64 = 0.0;
                for sp in g.refs(a, "synchronisingBehaviours") {
                    for fb in g.refs(&sp, "synchronousForkedBehaviours") {
                        let mut f = frame.clone();
                        longest = longest.max(self.behaviour(asm, &fb, &mut f, &mut Frame::default(), w));
                    }
                }
                time += longest;
            }
            "InternalCallAction" => {
                if let Some(b) = g.ref1(a, "calledResourceDemandingInternalBehaviour") {
                    time += self.behaviour(asm, &b, frame, result, w);
                }
            }
            _ => {}
        }
        time
    }

    /// A usage scenario behaviour; returns the time.
    fn scenario(&mut self, beh: &str, w: f64) -> f64 {
        let g = self.ix.g;
        let actions = g.refs(beh, "actions");
        let mut cur = actions.iter().find(|s| g.is_a(s, "Start")).cloned();
        let mut time = 0.0;
        let mut n = 0;
        while let Some(a) = cur {
            n += 1;
            if n > 10_000 {
                break;
            }
            let frame = Frame::default();
            match g.class(&a) {
                "EntryLevelSystemCall" => {
                    if let (Some(role), Some(sig)) = (g.ref1(&a, "providedRole"), g.ref1(&a, "operationSignature"))
                        && let Some(asm) = self.ix.delegations.get(&role).cloned()
                    {
                        let mut callee = Frame::default();
                        let inputs = g.refs(&a, "inputParameterUsages");
                        self.bind(&inputs, &frame, &mut callee);
                        time += self.call(&asm, &sig, callee, w).1;
                    }
                }
                "Branch" => {
                    for t in g.refs(&a, "branchTransitions") {
                        let p = g.num_attr(&t, "branchProbability").unwrap_or(0.0);
                        if let Some(b) = g.ref1(&t, "branchedBehaviour")
                            && p > 0.0
                        {
                            time += p * self.scenario(&b, w * p);
                        }
                    }
                }
                "Loop" => {
                    let k = g.str_attr(&a, "loopIteration").map_or(1.0, |s| self.expect(&s, &frame));
                    if let Some(b) = g.ref1(&a, "bodyBehaviour")
                        && k > 0.0
                    {
                        time += k * self.scenario(&b, w * k);
                    }
                }
                "Delay" => {
                    time += g.str_attr(&a, "timeSpecification").map_or(0.0, |s| self.expect(&s, &frame));
                }
                _ => {}
            }
            cur = g.ref1(&a, "successor");
        }
        time
    }
}

/// The offered load of every processing resource of `g` (plain PCM; other instances are
/// ignored) and the call frequencies between its assembly contexts.
pub fn analyse(g: &Graph) -> Loads {
    analyse_skipping(g, &BTreeSet::new())
}

/// As `analyse`, but the calls of `skip` (external call actions) and everything below them do
/// not count in `executions` and `action_calls` (the call itself does): a redirected call does not
/// run its callee.
pub fn analyse_skipping(g: &Graph, skip: &BTreeSet<String>) -> Loads {
    analyse_with(g, skip, &HashMap::new(), false)
}

/// As `analyse_skipping`; besides, the callee of a call in `scale` runs with that share of the
/// caller's weight (everything below it too), and with `per_scenario` the expected calls per
/// request of each scenario are kept (`scenario_calls`).
pub fn analyse_with(g: &Graph, skip: &BTreeSet<String>, scale: &HashMap<String, f64>, per_scenario: bool) -> Loads {
    let mut ex = Exprs { ids: HashMap::new(), programs: HashMap::new(), means: HashMap::new(), warnings: Vec::new() };
    let ix = Index::new(g, &mut ex);
    let mut busy: HashMap<String, f64> = HashMap::new();
    let mut calls: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut action_calls: BTreeMap<(String, String, String), f64> = BTreeMap::new();
    let mut executions: BTreeMap<String, f64> = BTreeMap::new();
    let mut arrivals = BTreeMap::new();
    let mut times = BTreeMap::new();
    let mut scenario_calls = BTreeMap::new();
    for sc in g.all("UsageScenario") {
        let Some(beh) = g.ref1(&sc, "scenarioBehaviour") else { continue };
        let mut walk = Walk {
            ix: &ix,
            ex: &mut ex,
            rng: Rng(0xC0FFEE),
            demand: HashMap::new(),
            calls: HashMap::new(),
            action_calls: HashMap::new(),
            executions: HashMap::new(),
            skip,
            scale,
            skipping: 0,
            random: false,
            depth: 0,
            warned: Default::default(),
        };
        let mut time = walk.scenario(&beh, 1.0);
        let mut runs = 1;
        if walk.random {
            walk.demand.clear();
            walk.calls.clear();
            walk.action_calls.clear();
            walk.executions.clear();
            runs = 4000;
            time = 0.0;
            for _ in 0..runs {
                time += walk.scenario(&beh, 1.0);
            }
        }
        let n = runs as f64;
        let time = time / n;
        let demand: Vec<(String, f64)> = walk.demand.drain().map(|(k, v)| (k, v / n)).collect();
        let sc_calls: Vec<((String, String), f64)> = walk.calls.drain().map(|(k, v)| (k, v / n)).collect();
        let sc_action_calls: Vec<((String, String, String), f64)> = walk.action_calls.drain().map(|(k, v)| (k, v / n)).collect();
        let sc_executions: Vec<(String, f64)> = walk.executions.drain().map(|(k, v)| (k, v / n)).collect();
        let wl = g.ref1(&sc, "workload");
        let rate = match wl.as_deref().map(|w| g.class(w)) {
            Some("OpenWorkload") => {
                let ia = g.str_attr(wl.as_ref().unwrap(), "interArrivalTime").and_then(|s| ex.mean(&s)).unwrap_or(0.0);
                if ia > 0.0 { 1.0 / ia } else { 0.0 }
            }
            Some("ClosedWorkload") => {
                let w = wl.as_ref().unwrap();
                let pop = g.num_attr(w, "population").unwrap_or(1.0);
                let think = g.str_attr(w, "thinkTime").and_then(|s| ex.mean(&s)).unwrap_or(0.0);
                if think + time > 0.0 { pop / (think + time) } else { 0.0 }
            }
            _ => 0.0,
        };
        arrivals.insert(sc.clone(), rate);
        times.insert(sc.clone(), time);
        for (spec, units) in demand {
            *busy.entry(spec).or_default() += rate * units;
        }
        for (k, v) in sc_calls {
            *calls.entry(k).or_default() += rate * v;
        }
        if per_scenario {
            scenario_calls.insert(sc.clone(), sc_action_calls.iter().cloned().collect::<BTreeMap<_, _>>());
        }
        for (k, v) in sc_action_calls {
            *action_calls.entry(k).or_default() += rate * v;
        }
        for (k, v) in sc_executions {
            *executions.entry(k).or_default() += rate * v;
        }
    }
    let mut resources = Vec::new();
    for c in g.all("ResourceContainer") {
        for s in g.refs(&c, "activeResourceSpecifications") {
            let kind = g.str_attr(&s, "activeResourceType").unwrap_or_else(|| "CPU".into());
            let rate = g.str_attr(&s, "processingRate").and_then(|r| ex.mean(&r)).unwrap_or(1.0);
            let replicas = g.num_attr(&s, "numberOfReplicas").unwrap_or(1.0).max(1.0);
            let units = busy.get(&s).copied().unwrap_or(0.0);
            resources.push(ResourceLoad {
                spec: s.clone(),
                container: c.clone(),
                kind,
                rate,
                replicas,
                busy: if rate > 0.0 { units / rate } else { 0.0 },
                capacity_basis: None,
            });
        }
    }
    Loads { resources, calls, action_calls, executions, arrivals, times, scenario_calls, warnings: ex.warnings }
}

/// Per usage scenario: the probability that one request needs each container on its
/// synchronous path (DESIGN.md D53, PCM reliability semantics after Brosch et al.). A SEFF marks
/// its container; a probabilistic branch scales by its probability; a loop with at least one
/// iteration counts once; asynchronous forked behaviours are not followed (their work is
/// queued); synchronous forks are. A container already on the current call chain is not marked
/// again (the callee runs only if its caller does); marks from separate calls combine as
/// independent events, 1 − Π(1 − p). A guarded branch takes each of its n transitions with 1/n, with a warning.
/// `fate` maps a container to the one whose fate it shares (a sidecar to its container).
pub fn sync_dependencies(g: &Graph, fate: &HashMap<String, String>) -> (BTreeMap<String, BTreeMap<String, f64>>, Vec<String>) {
    let mut ex = Exprs { ids: HashMap::new(), programs: HashMap::new(), means: HashMap::new(), warnings: Vec::new() };
    let ix = Index::new(g, &mut ex);
    let mut out = BTreeMap::new();
    let mut d = Deps { ix: &ix, ex: &mut ex, fate, depth: 0, chain: Vec::new(), warned: Default::default() };
    for sc in g.all("UsageScenario") {
        let Some(beh) = g.ref1(&sc, "scenarioBehaviour") else { continue };
        let mut acc = BTreeMap::new();
        d.scenario(&beh, 1.0, &mut acc);
        out.insert(sc.clone(), acc);
    }
    (out, ex.warnings)
}

struct Deps<'a, 'b> {
    ix: &'b Index<'a>,
    ex: &'b mut Exprs,
    fate: &'b HashMap<String, String>,
    depth: usize,
    /// Containers of the calls on the current chain.
    chain: Vec<String>,
    warned: std::collections::HashSet<String>,
}

impl Deps<'_, '_> {
    fn warn(&mut self, w: String) {
        if self.warned.insert(w.clone()) {
            self.ex.warnings.push(w);
        }
    }

    fn mark(acc: &mut BTreeMap<String, f64>, c: &str, p: f64) {
        let e = acc.entry(c.to_string()).or_insert(0.0);
        *e = 1.0 - (1.0 - *e) * (1.0 - p.clamp(0.0, 1.0));
    }

    /// The probability factor of a loop body: once if it runs at least once on average.
    fn once(&mut self, src: Option<String>) -> f64 {
        match src.map(|s| self.ex.mean(&s)) {
            Some(Some(k)) => k.clamp(0.0, 1.0),
            Some(None) => {
                self.warn("a loop count depends on parameters: its body counts once".into());
                1.0
            }
            None => 1.0,
        }
    }

    fn call(&mut self, asm: &str, sig: &str, p: f64, acc: &mut BTreeMap<String, f64>) {
        let g = self.ix.g;
        if self.depth > 64 || p <= 0.0 {
            return;
        }
        let c = self.ix.container_of.get(asm).cloned().unwrap_or_default();
        let c = self.fate.get(&c).cloned().unwrap_or(c);
        if !self.chain.contains(&c) {
            Self::mark(acc, &c, p);
        }
        let Some(comp) = g.ref1(asm, "encapsulatedComponent") else { return };
        let Some(seff) = self.ix.seffs.get(&(comp, sig.to_string())).cloned() else { return };
        self.depth += 1;
        self.chain.push(c);
        self.behaviour(asm, &seff, p, acc);
        self.chain.pop();
        self.depth -= 1;
    }

    fn behaviour(&mut self, asm: &str, beh: &str, p: f64, acc: &mut BTreeMap<String, f64>) {
        let g = self.ix.g;
        let steps = g.refs(beh, "steps");
        let mut cur = steps.iter().find(|s| g.is_a(s, "StartAction")).cloned();
        let mut n = 0;
        while let Some(a) = cur {
            n += 1;
            if n > 10_000 {
                break;
            }
            match g.class(&a) {
                "ExternalCallAction" => {
                    if let (Some(role), Some(sig)) = (g.ref1(&a, "role"), g.ref1(&a, "calledService"))
                        && let Some(target) = self.ix.connectors.get(&(asm.to_string(), role)).cloned()
                    {
                        self.call(&target, &sig, p, acc);
                    }
                }
                "BranchAction" => {
                    let ts = g.refs(&a, "branches");
                    let k = ts.len().max(1) as f64;
                    for t in &ts {
                        let Some(body) = g.ref1(t, "branchBehaviour") else { continue };
                        let q = if g.class(t) == "ProbabilisticBranchTransition" {
                            g.num_attr(t, "branchProbability").unwrap_or(0.0)
                        } else {
                            self.warn(format!("{}: a guarded branch counts each transition with 1/{k}", g.entity(&a)));
                            1.0 / k
                        };
                        self.behaviour(asm, &body, p * q, acc);
                    }
                }
                "LoopAction" => {
                    let q = self.once(g.str_attr(&a, "iterationCount"));
                    if let Some(body) = g.ref1(&a, "bodyBehaviour") {
                        self.behaviour(asm, &body, p * q, acc);
                    }
                }
                "CollectionIteratorAction" => {
                    if let Some(body) = g.ref1(&a, "bodyBehaviour") {
                        self.behaviour(asm, &body, p, acc);
                    }
                }
                "ForkAction" => {
                    for sp in g.refs(&a, "synchronisingBehaviours") {
                        for fb in g.refs(&sp, "synchronousForkedBehaviours") {
                            self.behaviour(asm, &fb, p, acc);
                        }
                    }
                }
                "InternalCallAction" => {
                    if let Some(b) = g.ref1(&a, "calledResourceDemandingInternalBehaviour") {
                        self.behaviour(asm, &b, p, acc);
                    }
                }
                _ => {}
            }
            cur = g.ref1(&a, "successor");
        }
    }

    fn scenario(&mut self, beh: &str, p: f64, acc: &mut BTreeMap<String, f64>) {
        let g = self.ix.g;
        let actions = g.refs(beh, "actions");
        let mut cur = actions.iter().find(|s| g.is_a(s, "Start")).cloned();
        let mut n = 0;
        while let Some(a) = cur {
            n += 1;
            if n > 10_000 {
                break;
            }
            match g.class(&a) {
                "EntryLevelSystemCall" => {
                    if let (Some(role), Some(sig)) = (g.ref1(&a, "providedRole"), g.ref1(&a, "operationSignature"))
                        && let Some(asm) = self.ix.delegations.get(&role).cloned()
                    {
                        self.call(&asm, &sig, p, acc);
                    }
                }
                "Branch" => {
                    for t in g.refs(&a, "branchTransitions") {
                        let q = g.num_attr(&t, "branchProbability").unwrap_or(0.0);
                        if let Some(b) = g.ref1(&t, "branchedBehaviour") {
                            self.scenario(&b, p * q, acc);
                        }
                    }
                }
                "Loop" => {
                    let q = self.once(g.str_attr(&a, "loopIteration"));
                    if let Some(b) = g.ref1(&a, "bodyBehaviour") {
                        self.scenario(&b, p * q, acc);
                    }
                }
                _ => {}
            }
            cur = g.ref1(&a, "successor");
        }
    }
}

/// The mean of a variable-free StoEx (e.g. a processing rate), if it has one.
pub fn mean_of(src: &str) -> Option<f64> {
    let mut ex = Exprs { ids: HashMap::new(), programs: HashMap::new(), means: HashMap::new(), warnings: Vec::new() };
    ex.mean(src)
}
