//! The analytic quality measures (DESIGN.md D53–D55; METRICS.md §1 of the project). All come
//! from expected-value walks, none from a simulation:
//! - failure (O2): the share of requests that fail, from the containers each request needs on
//!   its synchronous path (walk of the translated model) and their unavailability (shared fate);
//! - change coupling (O3): containers touched per change of a change unit (static, from the
//!   calls of the model before translation);
//! - inconsistency exposure (O4): stale reads and writes to other containers per request (walk
//!   of the model before translation, with the decisions' effect on who runs);
//! - the latency budgets (C1) and the capacity of unscaled containers at a peak (C2) are read
//!   here and evaluated by the functions.
//! Cost under the autoscaler at a peak (O1) is `archCost` with an `Autoscaling` load level.

use super::{Decisions, Mechanism, allocated, call_target, sorted};
use crate::graph::Graph;
use crate::load::{self, Loads};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Debug, Default)]
pub struct Quality {
    /// O2 per usage scenario: (arrival rate, probability that one request fails).
    pub failure: Option<BTreeMap<String, (f64, f64)>>,
    /// O2: unavailability of every container in use that has one.
    pub unavailability: BTreeMap<String, f64>,
    /// O3: expected containers touched per change.
    pub change_coupling: Option<f64>,
    /// O4 per usage scenario: (arrival rate, expected stale reads + weighted writes to other
    /// containers per request).
    pub inconsistency: Option<BTreeMap<String, (f64, f64)>>,
    /// C1: (scenario, budget in ms).
    pub budgets: Vec<(String, f64)>,
    /// C2: (factor, limit, containers).
    pub peak: Option<(f64, f64, Vec<String>)>,
}

/// Rate-weighted mean of per-scenario values, leaving out `excluded` scenarios (instance or
/// entity names).
pub fn weighted(per: &BTreeMap<String, (f64, f64)>, excluded: &[String], entity: impl Fn(&str) -> String) -> f64 {
    let (mut sum, mut rate) = (0.0, 0.0);
    for (sc, (r, v)) in per {
        if excluded.iter().any(|x| x == sc || *x == entity(sc)) {
            continue;
        }
        sum += r * v;
        rate += r;
    }
    if rate > 0.0 { sum / rate } else { 0.0 }
}

/// Assemblies with a Placement that may sit outside the direct containers (as the modularity's
/// modules, D32): the movable application components.
fn modules(g: &Graph, d: &Decisions) -> BTreeSet<String> {
    let direct: BTreeSet<String> = d.remote.as_ref().map(|r| r.direct.clone()).unwrap_or_default();
    let mut out = BTreeSet::new();
    for p in g.all("Placement") {
        let Some(asm) = g.ref1(&p, "allocation").and_then(|a| g.ref1(&a, "assemblyContext")) else { continue };
        if g.refs(&p, "candidates").iter().any(|c| !direct.contains(c)) {
            out.insert(asm);
        }
    }
    out
}

fn direct(d: &Decisions) -> BTreeSet<String> {
    d.remote.as_ref().map(|r| r.direct.clone()).unwrap_or_default()
}

/// What the model before translation gives: O3, O4, and the data of C1 and C2.
pub(super) fn before(g: &Graph, d: &Decisions, placement: &BTreeMap<String, String>, warnings: &mut Vec<String>) -> Quality {
    let mut q = Quality { budgets: d.q.budgets.clone(), peak: d.q.peak.clone(), ..Default::default() };
    if !d.q.units.is_empty() {
        q.change_coupling = Some(change_coupling(g, d, placement));
    }
    if let Some(weights) = d.q.consistency {
        let (per, w) = inconsistency(g, d, placement, weights);
        q.inconsistency = Some(per);
        for x in w {
            if !warnings.contains(&x) {
                warnings.push(x);
            }
        }
    }
    q
}

/// What the translated model gives: O2.
pub(super) fn after(
    q: &mut Quality,
    tg: &Graph,
    d: &Decisions,
    placement: &BTreeMap<String, String>,
    sidecars: &BTreeMap<String, String>,
    idle: &Loads,
    warnings: &mut Vec<String>,
) {
    let Some([cfr, restore, period, u_proc, u_db, kappa]) = d.q.reliability else { return };
    let direct = direct(d);
    let mods = modules(tg, d);
    // unavailability per change unit, and the units on each container
    let mut on: BTreeMap<&String, f64> = BTreeMap::new();
    for (_, changes, asms) in &d.q.units {
        let u = changes * cfr * restore / period;
        let hosts: BTreeSet<&String> = asms.iter().filter_map(|a| placement.get(a)).collect();
        for c in hosts {
            *on.entry(c).or_default() += u;
        }
    }
    let used: BTreeSet<&String> = placement.values().collect();
    let hosts_module: BTreeSet<&String> = mods.iter().filter_map(|m| placement.get(m)).collect();
    let mut unavail = BTreeMap::new();
    for c in used {
        let units = on.get(c).copied().unwrap_or(0.0);
        let u = if direct.contains(c) {
            u_db + kappa * units
        } else if hosts_module.contains(c) {
            u_proc + units
        } else {
            continue;
        };
        unavail.insert(c.clone(), u);
    }
    // a sidecar shares its container's fate
    let owner: HashMap<String, String> = sidecars.iter().map(|(c, s)| (s.clone(), c.clone())).collect();
    let (deps, w) = load::sync_dependencies(tg, &owner);
    for x in w {
        if !warnings.contains(&x) {
            warnings.push(x);
        }
    }
    let mut per = BTreeMap::new();
    for (sc, acc) in deps {
        let ok: f64 = acc.iter().map(|(c, p)| 1.0 - p * unavail.get(c).copied().unwrap_or(0.0)).product();
        per.insert(sc.clone(), (idle.arrivals.get(&sc).copied().unwrap_or(0.0), 1.0 - ok));
    }
    q.failure = Some(per);
    q.unavailability = unavail;
}

/// The assembly of the component that owns a SEFF.
fn assembly_of_seff(g: &Graph, seff: &str) -> Option<String> {
    let comp = g.container(seff)?;
    sorted(g.referrers(&comp, "encapsulatedComponent")).into_iter().next()
}

/// O3: Σ_u f_u · (containers of u + Σ over other containers that call into u of the largest
/// break probability of their calls) / Σ_u f_u. Only containers with a movable component count
/// (deployables); calls inside one container break nothing. An orchestrated workflow's steps are
/// calls of its trigger's component.
fn change_coupling(g: &Graph, d: &Decisions, placement: &BTreeMap<String, String>) -> f64 {
    let direct = direct(d);
    let deployables: BTreeSet<&String> =
        modules(g, d).iter().filter_map(|m| placement.get(m)).filter(|c| !direct.contains(*c)).collect();
    let mut step_caller: BTreeMap<&String, String> = BTreeMap::new();
    for w in &d.workflows {
        if w.orchestrated.is_some()
            && let Some(asm) = assembly_of_seff(g, &w.trigger)
        {
            for s in &w.steps {
                step_caller.insert(s, asm.clone());
            }
        }
    }
    // callee assembly → (caller container → largest break probability)
    let mut consumers: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for a in sorted(g.all("ExternalCallAction")) {
        let Some((caller, callee)) = call_target(g, &a) else { continue };
        let caller = step_caller.get(&a).cloned().unwrap_or(caller);
        let (Some(cc), Some(ck)) = (placement.get(&caller), placement.get(&callee)) else { continue };
        if cc == ck || !deployables.contains(cc) {
            continue;
        }
        let p = d.q.breaks.get(&a).copied().unwrap_or(d.q.contract_break);
        let e = consumers.entry(callee).or_default().entry(cc.clone()).or_insert(0.0);
        *e = e.max(p);
    }
    let (mut sum, mut total) = (0.0, 0.0);
    for (_, f, asms) in &d.q.units {
        let own: BTreeSet<&String> = asms.iter().filter_map(|a| placement.get(a)).filter(|c| deployables.contains(c)).collect();
        let mut others: BTreeMap<&String, f64> = BTreeMap::new();
        for a in asms {
            for (k, p) in consumers.get(a).into_iter().flatten() {
                if !own.contains(k) {
                    let e = others.entry(k).or_insert(0.0);
                    *e = e.max(*p);
                }
            }
        }
        sum += f * (own.len().max(1) as f64 + others.values().sum::<f64>());
        total += f;
    }
    if total > 0.0 { sum / total } else { 0.0 }
}

/// O4 per scenario: per request, the expected stale reads (cache hits; across containers, the
/// share `staleReads` of a call's mechanism; calls into a `Replica` that sits apart from its
/// source) plus the writes to another container (CallSite.writes), weighted by `weights`
/// (synchronous, asynchronous). The walk runs on the model before translation; a cache hit, a
/// redirected call and a bypass across containers do not run the callee.
fn inconsistency(
    g: &Graph,
    d: &Decisions,
    placement: &BTreeMap<String, String>,
    (sync_w, async_w): (f64, f64),
) -> (BTreeMap<String, (f64, f64)>, Vec<String>) {
    let mut scale: HashMap<String, f64> = HashMap::new();
    let mut per_visit: HashMap<String, f64> = HashMap::new();
    for (_, call, m) in &d.sites {
        let across = call_target(g, call).is_some_and(|(a, p)| allocated(g, &a) != allocated(g, &p));
        let mut stale = 0.0;
        match m {
            Mechanism::Cached { hit_ratio, .. } => {
                scale.insert(call.clone(), 1.0 - hit_ratio);
                stale = *hit_ratio;
            }
            Mechanism::Composed { cache: Some((h, _)), .. } if across => {
                scale.insert(call.clone(), 1.0 - h);
                stale = *h;
            }
            Mechanism::Bypass { .. } if across => {
                scale.insert(call.clone(), 0.0);
            }
            _ => {}
        }
        if d.redirects.contains_key(call) {
            scale.insert(call.clone(), 0.0);
        }
        if across && let Some(x) = d.q.stale.get(call) {
            stale += x;
        }
        let write = if across && d.q.writes.contains(call) {
            if matches!(m, Mechanism::Async) { async_w } else { sync_w }
        } else {
            0.0
        };
        if stale + write > 0.0 {
            per_visit.insert(call.clone(), stale + write);
        }
    }
    let stale_callees: BTreeSet<&String> =
        d.q.replicas.iter().filter(|(r, s)| placement.get(r) != placement.get(s)).map(|(r, _)| r).collect();
    let loads = load::analyse_with(g, &BTreeSet::new(), &scale, true);
    let mut out = BTreeMap::new();
    for (sc, calls) in &loads.scenario_calls {
        let mut x = 0.0;
        for ((_, callee, action), n) in calls {
            x += n * per_visit.get(action).copied().unwrap_or(0.0);
            if stale_callees.contains(callee) {
                x += n;
            }
        }
        out.insert(sc.clone(), (loads.arrivals.get(sc).copied().unwrap_or(0.0), x));
    }
    (out, loads.warnings)
}
