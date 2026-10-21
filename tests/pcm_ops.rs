//! The plugin functions of work package H (domain project, DESIGN.md §H): per-operation
//! analyses, runs with a per-scenario load profile and usage streams, the design detail.
//!
//! Fixture: `two-tier.m` (Frontend 5 ms CPU on C1 calls Backend 10 ms CPU on C2, one-call
//! scenario `scenario` at 20/s) plus a session scenario `session` at 10/s: a call, a think time
//! of 2 s, then a loop of 3 calls.

use palladio_arch::detail::{self, DetailRequest};
use palladio_arch::pcm::PcmCache;
use palladio_arch::pcmhpa::{self, HpaRequest, ScalingGroup, ServiceGroup};
use palladio_arch::pcmrun::{self, LoadStep, RunOptions};
use palladio_arch::{mfile, translate};
use palladio_sim_service::config::Config;
use palladio_sim_service::engine::{Engine, RunParams};
use palladio_sim_service::protocol::WireModel;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const SESSION: &str = r#"
usage.usageScenario -- session
session : UsageScenario { entityName = "session" }
session.scenarioBehaviour -- session_b
session.workload -- session_wl
session_wl : OpenWorkload { interArrivalTime = "Exp(10.0)" }
session_b : ScenarioBehaviour { }
session_b.actions -- s_start
session_b.actions -- s_call
session_b.actions -- s_think
session_b.actions -- s_loop
session_b.actions -- s_stop
s_start : Start { }
s_call : EntryLevelSystemCall { }
s_call.providedRole -- system_provides
s_call.operationSignature -- handle
s_think : Delay { timeSpecification = "2.0" }
s_loop : Loop { loopIteration = "3" }
s_loop.bodyBehaviour -- s_body
s_stop : Stop { }
s_start.successor -- s_call
s_call.successor -- s_think
s_think.successor -- s_loop
s_loop.successor -- s_stop
s_body : ScenarioBehaviour { }
s_body.actions -- b_start
s_body.actions -- b_call
s_body.actions -- b_stop
b_start : Start { }
b_call : EntryLevelSystemCall { }
b_call.providedRole -- system_provides
b_call.operationSignature -- handle
b_stop : Stop { }
b_start.successor -- b_call
b_call.successor -- b_stop
"#;

fn model() -> WireModel {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    let text = text.replace("RATE", "20.0").replace("BACKEND_CONTAINER", "C2");
    mfile::parse(&format!("{text}\n{SESSION}")).unwrap()
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs()).max(1e-300)
}

#[test]
fn operations_per_call() {
    let cache = PcmCache::new(&model());
    let ops = cache.operations();
    let by: HashMap<&str, _> = ops.iter().map(|o| (o.call.as_str(), o)).collect();
    assert_eq!(ops.len(), 3);
    assert!(close(by["scenario_call"].rate, 20.0, 1e-12));
    assert!(close(by["s_call"].rate, 10.0, 1e-12));
    assert!(close(by["b_call"].rate, 30.0, 1e-12));
    assert_eq!(by["b_call"].scenario.as_deref(), Some("session"));
    assert_eq!(by["b_call"].signature.as_deref(), Some("handle"));
    for o in &ops {
        // 5 ms on the frontend + 10 ms on the backend, per call
        assert!(close(o.time, 0.015, 1e-12), "{}: {}", o.call, o.time);
        // pcmOperations' no-load time per call (scenario walk, rate-weighted) is the same
        let t = cache.loads.entry_times[&o.call] / o.rate;
        assert!(close(t, o.time, 1e-12), "{}: {t} vs {}", o.call, o.time);
    }
    // visits: the backend's CPU action once per call; linear in the rates
    let backend_cpu = cache
        .graph
        .all("InternalAction")
        .into_iter()
        .find(|a| a.starts_with("Backend"))
        .expect("the backend's internal action");
    let w = vec![(backend_cpu.clone(), 2.0)];
    let per: HashMap<String, f64> = cache.operation_visits(&w).into_iter().collect();
    for o in &ops {
        assert!(close(per[&o.call], 2.0, 1e-12));
    }
    let total: f64 = ops.iter().map(|o| o.rate * per[&o.call]).sum();
    let rate: f64 = cache.loads.arrivals.values().sum();
    assert!(close(total, cache.weighted_visits(&w, &[]) * rate, 1e-12));
    // the call itself counts once per call
    let per_call: HashMap<String, f64> = cache.operation_visits(&[("b_call".into(), 1.0)]).into_iter().collect();
    assert_eq!(per_call["b_call"], 1.0);
    assert_eq!(per_call["s_call"], 0.0);
    // failure per call: needs both containers
    let unavailable: HashMap<String, f64> = [("C1".to_string(), 0.02), ("C2".to_string(), 0.01)].into_iter().collect();
    let (f, _) = cache.operation_failures(&unavailable, &HashSet::new());
    for (call, p) in &f {
        assert!(close(*p, 1.0 - 0.98 * 0.99, 1e-12), "{call}: {p}");
    }
    // the backend as transparent: only the frontend's container counts
    let backend: HashSet<String> = ["AssemblyBackend".to_string()].into_iter().collect();
    let (f, _) = cache.operation_failures(&unavailable, &backend);
    assert!(f.iter().all(|(_, p)| close(*p, 0.02, 1e-12)));
}

const TODAY: &str = "../palladio-architecture-optimization-domain/models/misarch-today.m";

#[test]
fn per_call_failure_and_time_match_the_scenarios_on_misarch() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(TODAY);
    if !path.exists() {
        eprintln!("skipped: {} is missing", path.display());
        return;
    }
    let t = translate::translate(&mfile::read(&path).unwrap()).unwrap();
    let cache = PcmCache::new(&t.pcm);
    let unavailable: HashMap<String, f64> = t.quality.unavailability.clone().into_iter().collect();
    // one call per scenario: the rate-weighted failure of the calls is the scenarios' failure
    let (per, _) = cache.operation_failures(&unavailable, &HashSet::new());
    let per: HashMap<String, f64> = per.into_iter().collect();
    let ops = cache.operations();
    let login: Vec<&str> = ops.iter().filter(|o| o.scenario.as_ref().is_some_and(|s| cache.graph.entity(s) == "login")).map(|o| o.call.as_str()).collect();
    let (mut sum, mut rate) = (0.0, 0.0);
    for o in &ops {
        if login.contains(&o.call.as_str()) {
            continue;
        }
        sum += o.rate * per[&o.call];
        rate += o.rate;
        let t = cache.loads.entry_times[&o.call] / o.rate;
        assert!(close(t, o.time, 1e-9), "{}: {t} vs {}", o.call, o.time);
    }
    let (f, _) = cache.failure(&unavailable, &HashSet::new(), &["login".into()]);
    assert!(close(sum / rate, f, 1e-12), "{} vs {f}", sum / rate);
}

#[test]
fn load_profiles() {
    assert!(pcmrun::load_profile(&[]).unwrap().is_none());
    let steps = vec![
        LoadStep { scenario: Some("session".into()), time: 50.0, factor: 2.0 },
        LoadStep { scenario: None, time: 10.0, factor: 0.5 },
        LoadStep { scenario: Some("session".into()), time: 0.0, factor: 1.5 },
    ];
    let p = pcmrun::load_profile(&steps).unwrap().unwrap();
    assert_eq!(p.entries.len(), 2);
    assert!(p.entries[0].scenarios.is_empty());
    assert_eq!(p.entries[0].points, vec![(10.0, 0.5)]);
    assert_eq!(p.entries[1].scenarios, vec!["session".to_string()]);
    assert_eq!(p.entries[1].points, vec![(0.0, 1.5), (50.0, 2.0)]);
    let dup = vec![LoadStep { scenario: None, time: 1.0, factor: 1.0 }, LoadStep { scenario: None, time: 1.0, factor: 2.0 }];
    assert!(pcmrun::load_profile(&dup).is_err());
    assert!(pcmrun::load_profile(&[LoadStep { scenario: None, time: 1.0, factor: 0.0 }]).is_err());
}

fn count(o: &palladio_sim_service::engine::Outcome, mp_prefix: &str, id: &str) -> u64 {
    o.series.iter().find(|s| s.measuring_point.starts_with(mp_prefix) && s.elements.iter().any(|e| e == id)).map_or(0, |s| s.count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_with_profiles_and_usage_streams() {
    let engine = Arc::new(Engine::new(Config::from_env().unwrap()));
    let m = model();
    // monitors for both scenarios
    let mut g = palladio_arch::graph::Graph::new(&m);
    detail::monitor_everything(&mut g, false);
    let p = engine.prepare(palladio_sim_service::schema::schema(), Arc::new(g.to_wire(|_| true))).await.unwrap();
    let params = RunParams { seed: 7, max_measurements: -1, max_sim_time: 300, replications: 1, simulate_linking_resources: false };
    let limits = simoxide_sim::Limits::default();
    let plain = pcmrun::run_compiled(&p.compiled, &p.monitors, &params, &RunOptions::default(), &limits, false).unwrap();
    // without options, the statistics are the engine's
    let e = engine.simulate(p.clone(), params.clone()).await.unwrap();
    assert_eq!(plain.outcome.series, e.series);
    assert_eq!(plain.outcome.events, e.events);
    // a factor of 2 on the sessions (from t = 0) doubles them, the other scenario stays
    let opts = RunOptions { profile: vec![LoadStep { scenario: Some("session".into()), time: 0.0, factor: 2.0 }], usage_streams: true };
    let twice = pcmrun::run_compiled(&p.compiled, &p.monitors, &params, &opts, &limits, false).unwrap();
    let base_opts = RunOptions { profile: vec![], usage_streams: true };
    let base = pcmrun::run_compiled(&p.compiled, &p.monitors, &params, &base_opts, &limits, false).unwrap();
    let (s1, s2) = (count(&base.outcome, "UsageScenario", "session") as f64, count(&twice.outcome, "UsageScenario", "session") as f64);
    assert!((s2 / s1 - 2.0).abs() < 0.15, "sessions {s1} → {s2}");
    // a factor of 1 draws nothing: the same run as without a profile
    let one = RunOptions { profile: vec![LoadStep { scenario: Some("session".into()), time: 0.0, factor: 1.0 }], usage_streams: true };
    let b1 = pcmrun::run_compiled(&p.compiled, &p.monitors, &params, &one, &limits, false).unwrap();
    assert_eq!(b1.outcome.series, base.outcome.series);
    // deterministic, and different from the single stream
    let again = pcmrun::run_compiled(&p.compiled, &p.monitors, &params, &base_opts, &limits, false).unwrap();
    assert_eq!(again.outcome.series, base.outcome.series);
    assert_ne!(base.outcome.series, plain.outcome.series);
    // common random numbers across designs: a faster backend sees the same users
    let mut g2 = palladio_arch::graph::Graph::new(&m);
    g2.set_attr("C2_cpu", "processingRate", palladio_sim_service::protocol::WireValue::String("1500.0".into()));
    detail::monitor_everything(&mut g2, false);
    let p2 = engine.prepare(palladio_sim_service::schema::schema(), Arc::new(g2.to_wire(|_| true))).await.unwrap();
    let fast = pcmrun::run_compiled(&p2.compiled, &p2.monitors, &params, &base_opts, &limits, false).unwrap();
    for sc in ["scenario", "session"] {
        let (a, b) = (count(&base.outcome, "UsageScenario", sc), count(&fast.outcome, "UsageScenario", sc));
        assert!(a.abs_diff(b) <= 3, "{sc}: {a} vs {b}");
    }
    // the runner (cached, as the pcmRun functions use it)
    let runner = pcmrun::runner(&engine.config);
    let r1 = runner.simulate(p.clone(), params.clone(), base_opts.clone()).await.unwrap();
    let r2 = runner.simulate(p.clone(), params.clone(), base_opts.clone()).await.unwrap();
    assert!(Arc::ptr_eq(&r1, &r2));
    assert_eq!(r1.outcome.series, base.outcome.series);
}

#[test]
fn design_detail() {
    let m = model();
    let mut budgets = std::collections::BTreeMap::new();
    budgets.insert("handle".to_string(), 30.0);
    let req = DetailRequest { duration_s: 200.0, warm_up_s: 20.0, bin_s: 20.0, budgets_ms: budgets, slo_factor: 1.0, ..DetailRequest::default() };
    let v = detail::detail(&m, &req, simoxide_sim::Limits::default()).unwrap();
    let arr = |k: &str| v[k].as_array().unwrap().clone();
    assert_eq!(arr("scenarios").len(), 2);
    assert_eq!(arr("calls").len(), 3);
    assert_eq!(arr("systemOperations").len(), 1);
    assert_eq!(v["t"].as_array().unwrap().len(), 10);
    let mut slo_requests = 0;
    for g in ["scenarios", "calls", "systemOperations", "assemblyOperations"] {
        for s in arr(g) {
            let n = s["count"].as_u64().unwrap();
            assert!(n > 0, "{g} {}", s["key"]);
            let hist: u64 = s["histogram"]["counts"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).sum();
            assert_eq!(hist, n, "{g} {}", s["key"]);
            let (p50, p95, max) = (s["p50Ms"].as_f64().unwrap(), s["p95Ms"].as_f64().unwrap(), s["maxMs"].as_f64().unwrap());
            assert!(p50 <= p95 && p95 <= max);
            assert_eq!(s["overTime"]["meanMs"].as_array().unwrap().len(), 10);
            if !s["budgetMs"].is_null() {
                slo_requests += n;
            }
        }
    }
    // the system operation (named by its signature) carries the budget
    let so = &arr("systemOperations")[0];
    assert_eq!(so["budgetMs"].as_f64(), Some(30.0));
    assert_eq!(v["slo"]["total"]["requests"].as_u64(), Some(slo_requests));
    // at least the backend's operation is called through a connector
    assert!(arr("assemblyOperations").iter().any(|s| s["assembly"] == "AssemblyBackend"));
    // utilisation: as the analysis says (C1 0.3, C2 0.6)
    for r in arr("resources") {
        let (u, a) = (r["utilisation"].as_f64().unwrap(), r["analyticUtilisation"].as_f64().unwrap());
        assert!((u - a).abs() < 0.06, "{}: {u} vs {a}", r["resource"]);
        assert!(r["queueLength"].as_f64().unwrap() >= u - 1e-9);
        assert!(r["waitMs"].as_f64().unwrap() >= 0.0);
        assert_eq!(r["queueOverTime"].as_array().unwrap().len(), 10);
    }
    assert_eq!(arr("containers").len(), 2);
    // mean session time: 4 calls of ~15 ms (more under load) and 2 s thinking
    let session = arr("scenarios").into_iter().find(|s| s["scenario"] == "session").unwrap();
    let mean = session["meanMs"].as_f64().unwrap();
    assert!(mean > 2060.0 && mean < 2300.0, "{mean}");
}

/// A workload cardinality as a PMF input of the call, used by a demand: the `pcm*` analysis
/// enumerates its values (exact, one walk per value) instead of sampling 4 000 walks.
#[test]
fn pmf_inputs_are_enumerated() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    let text = text.replace("RATE", "20.0").replace("BACKEND_CONTAINER", "C2").replace(
        "specification    = \"Exp(0.2)\"",
        "specification    = \"items.VALUE * 5 + (mode.VALUE == \\\"FAST\\\" ? 0 : 10)\"",
    );
    let extra = r#"
items_usage : VariableUsage { namedReference = "items" }
items_char : VariableCharacterisation {
    type          = VariableCharacterisationType.VALUE
    specification = "IntPMF[(1;0.5)(3;0.5)]"
}
items_usage.variableCharacterisation -- items_char
scenario_call.inputParameterUsages -- items_usage
mode_usage : VariableUsage { namedReference = "mode" }
mode_char : VariableCharacterisation {
    type          = VariableCharacterisationType.VALUE
    specification = "EnumPMF[(\"FAST\";0.25)(\"SLOW\";0.75)]"
}
mode_usage.variableCharacterisation -- mode_char
scenario_call.inputParameterUsages -- mode_usage
"#;
    let m = mfile::parse(&format!("{text}\n{extra}")).unwrap();
    let cache = PcmCache::new(&m);
    // frontend units: E[5 items] + 0.75 · 10 = 10 + 7.5 = 17.5 → 17.5 ms; backend 10 ms
    let ops = cache.operations();
    assert_eq!(ops.len(), 1);
    assert!(close(ops[0].time, 0.0275, 1e-12), "{}", ops[0].time);
    assert!(close(cache.loads.times["scenario"], 0.0275, 1e-12));
    let c1 = cache.loads.resources.iter().find(|r| r.container == "C1").unwrap();
    assert!(close(c1.busy, 20.0 * 0.0175, 1e-12), "{}", c1.busy);
    assert!(cache.loads.warnings.is_empty(), "{:?}", cache.loads.warnings);
    // the translation's path (no exact means) still samples: close, not exact
    let g = palladio_arch::graph::Graph::new(&cache.plain);
    let l = palladio_arch::load::analyse(&g);
    assert!((l.times["scenario"] - 0.0275).abs() < 0.0275 * 0.02, "{}", l.times["scenario"]);
}

// ---------------------------------------------------------------------------------------------
// Dynamic runs on plain PCM (SPD + HPA): Backend's container C2 (one core, 10 ms per request)
// autoscaled at a target utilisation of 0.7

/// The fixture at `rate` requests per second.
fn hpa_model(rate: f64) -> WireModel {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    mfile::parse(&text.replace("RATE", &format!("{rate:?}")).replace("BACKEND_CONTAINER", "C2")).unwrap()
}

/// C2 autoscaled (up to `max` pods, one at the start) over the load profile `steps` (`(s,
/// factor)`), from `warm_up` to `end` (s); a budget of 30 ms on the scenario.
fn hpa_request(steps: &[(f64, f64)], warm_up: f64, end: f64, max: u32, services: Vec<ServiceGroup>) -> HpaRequest {
    let mut budgets = std::collections::BTreeMap::new();
    budgets.insert("scenario".to_string(), 30.0);
    let run = DetailRequest {
        profile: steps.iter().map(|&(t, f)| LoadStep { scenario: None, time: t, factor: f }).collect(),
        usage_streams: false,
        seed: 3,
        duration_s: end,
        warm_up_s: warm_up,
        bin_s: 30.0,
        time_unit_ms: 1000.0,
        budgets_ms: budgets,
        slo_factor: 2.0,
        assembly_operations: false,
        ..DetailRequest::default()
    };
    let groups = vec![ScalingGroup { container: "C2".into(), min: 1, max, target: 0.7, initial: 1, startup_s: 30.0, scale_down_s: None, cpu_request: None }];
    HpaRequest { groups, services, run, max_pods: 400 }
}

/// The model with load balancers in front of C2's assemblies, and them as service groups.
fn load_balanced(m: &WireModel) -> (WireModel, Vec<ServiceGroup>) {
    let (w, added) = translate::load_balanced(m, &["C2".to_string()]).unwrap();
    (w, added.into_iter().map(|(container, assembly, load_balancer)| ServiceGroup { container, assembly, load_balancer }).collect())
}

/// Without service groups the plugin adds the load balancers itself: the same run as with them
/// in the model. The pods follow the load.
#[test]
fn hpa_with_load_balancers_added_by_the_plugin() {
    let steps = [(0.0, 0.3), (200.0, 1.0), (400.0, 0.5)];
    let m = hpa_model(100.0);
    let (r, detail) = pcmhpa::run_model(&m, &hpa_request(&steps, 60.0, 600.0, 6, vec![]), Default::default()).unwrap();
    let (with_lb, services) = load_balanced(&m);
    let (r2, _) = pcmhpa::run_model(&with_lb, &hpa_request(&steps, 60.0, 600.0, 6, services), Default::default()).unwrap();
    assert_eq!(r.groups, r2.groups);
    assert_eq!((r.ready_replica_seconds, r.replica_seconds, r.peak_replicas), (r2.ready_replica_seconds, r2.replica_seconds, r2.peak_replicas));
    assert_eq!((r.slo, r.requests, r.mean_ms, r.events), (r2.slo, r2.requests, r2.mean_ms, r2.events));
    // one pod at 0.3 cores of load, two at 1.0 (the scale-in after 0.5 falls after the end)
    assert!(!r.capped && r.groups[0].rescales >= 1 && r.groups[0].max_replicas == 2, "{r:?}");
    // the scaled container's pods over time: more at the step to 1.0
    let c2 = detail["containers"].as_array().unwrap().iter().find(|c| c["container"] == "C2").unwrap().clone();
    assert_eq!(c2["autoscaled"], true);
    let pods: Vec<f64> = c2["replicas"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    assert!(pods[12] > pods[5], "{pods:?}");
    // with usage streams and a per-scenario profile it runs as well
    let mut req = hpa_request(&steps, 60.0, 600.0, 6, vec![]);
    req.run.usage_streams = true;
    req.run.profile = vec![LoadStep { scenario: Some("scenario".into()), time: 0.0, factor: 0.5 }];
    let (r3, _) = pcmhpa::run_model(&m, &req, Default::default()).unwrap();
    assert!(r3.groups[0].mean_replicas < r.groups[0].mean_replicas + 1.0);
}

/// At a constant load the HPA's pods settle at the ideal count, ⌈load / target⌉ (within
/// Kubernetes' 10 % tolerance band: at most one pod off), from one pod.
#[test]
fn at_constant_load_the_hpa_settles_at_the_ideal_count() {
    // Backend's load is rate · 10 ms of a core (Frontend's core on C1 is not scaled: 150/s is
    // 0.75 of it); a tenth of the load at the start, the load from 60 s on, measured from 360 s
    for (rate, want) in [(50.0, 1.0), (100.0, 2.0), (150.0, 3.0)] {
        let req = hpa_request(&[(0.0, 0.1), (60.0, 1.0)], 360.0, 1200.0, 20, vec![]);
        let (r, _) = pcmhpa::run_model(&hpa_model(rate), &req, Default::default()).unwrap();
        assert!(!r.capped);
        assert!((r.groups[0].mean_replicas - want).abs() <= 1.0, "λ = {rate}: HPA {} pods, ideal {want}", r.groups[0].mean_replicas);
    }
}

/// An evaluation never grows without bound: past `max_pods` created pods the run stops and is
/// reported as capped.
#[test]
fn pod_creation_is_capped() {
    let mut steps = Vec::new();
    for i in 0..40 {
        steps.push((60.0 + 60.0 * i as f64, if i % 2 == 0 { 2.0 } else { 0.1 }));
    }
    let mut req = hpa_request(&steps, 60.0, 2460.0, 20, vec![]);
    req.groups[0].scale_down_s = Some(0.0);
    req.groups[0].startup_s = 5.0;
    req.max_pods = 20;
    let (r, _) = pcmhpa::run_model(&hpa_model(100.0), &req, Default::default()).unwrap();
    assert!(r.capped, "{r:?}");
    assert!(r.created <= 21, "{}", r.created);
}

/// The script functions, called as the platform calls them (records and lists of instances).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_functions() {
    use palladio_arch::functions;
    use palladio_sim_service::protocol::{HeapObject, WireValue};
    let engine = Arc::new(Engine::new(Config::from_env().unwrap()));
    let rec = |c: &str, f: Vec<(&str, WireValue)>| WireValue::Record { class_name: c.into(), fields: f.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
    let inst = |n: &str| WireValue::Instance(n.into());
    let mut n = 1000;
    let mut next = || {
        n += 1;
        n
    };
    async fn call(
        engine: &Arc<Engine>,
        op: &str,
        args: Vec<WireValue>,
        lists: &HashMap<i64, Vec<WireValue>>,
        cache: &Arc<PcmCache>,
        next: &mut impl FnMut() -> i64,
    ) -> (WireValue, Vec<HeapObject>) {
        let mut prepared = None;
        let a = functions::call(engine, op, &args, lists, cache, &mut prepared, next).await.unwrap_or_else(|e| panic!("{op}: {e}"));
        (a.value, a.objects)
    }
    let field = |v: &WireValue, k: &str| -> WireValue {
        let WireValue::Record { fields, .. } = v else { panic!("not a record: {v:?}") };
        fields.iter().find(|(n, _)| n == k).unwrap().1.clone()
    };
    // per operation
    let cache = Arc::new(PcmCache::new(&model()));
    let backend_cpu = cache.graph.all("InternalAction").into_iter().find(|a| a.starts_with("Backend")).unwrap();
    let mut lists: HashMap<i64, Vec<WireValue>> = HashMap::new();
    lists.insert(1, vec![inst(&backend_cpu)]);
    lists.insert(2, vec![WireValue::Double(3.0)]);
    let (v, objs) = call(&engine, "pcmOperationVisits", vec![WireValue::Ref(1), WireValue::Ref(2)], &lists, &cache, &mut next).await;
    let WireValue::Ref(id) = v else { panic!() };
    let items = objs.iter().find(|o| o.id == id).unwrap().elements.clone().unwrap();
    assert_eq!(items.len(), 3);
    for it in &items {
        assert_eq!(field(it, "value"), WireValue::Double(3.0));
    }
    lists.insert(3, vec![inst("C1")]);
    lists.insert(4, vec![WireValue::Double(0.1)]);
    lists.insert(5, vec![]);
    let (v, objs) = call(&engine, "pcmOperationFailure", vec![WireValue::Ref(3), WireValue::Ref(4), WireValue::Ref(5)], &lists, &cache, &mut next).await;
    let WireValue::Ref(id) = v else { panic!() };
    for it in objs.iter().find(|o| o.id == id).unwrap().elements.clone().unwrap() {
        let WireValue::Double(x) = field(&it, "value") else { panic!() };
        assert!(close(x, 0.1, 1e-12));
    }
    // pcmRun / pcmRunStat with a profile (the fixture's own monitor: scenario `scenario`)
    lists.insert(6, vec![]);
    lists.insert(7, vec![rec("PcmLoadStep", vec![("scenario", inst("scenario")), ("time", WireValue::Double(0.0)), ("factor", WireValue::Double(2.0))])]);
    let run = |extra: Vec<WireValue>| {
        let mut a = vec![WireValue::Ref(6), WireValue::Ref(7), WireValue::Bool(true)];
        a.extend(extra);
        a.extend([WireValue::Int(1), WireValue::Int(-1), WireValue::Int(200), WireValue::Int(1), WireValue::Bool(false)]);
        a
    };
    let (v, _) = call(&engine, "pcmRunStat", run(vec![WireValue::String("scenario".into()), WireValue::String("Response Time".into()), WireValue::String("count".into())]), &lists, &cache, &mut next).await;
    let WireValue::Double(cnt) = v else { panic!() };
    assert!((cnt / (40.0 * 200.0) - 1.0).abs() < 0.05, "{cnt}");
    let (v, _) = call(&engine, "pcmRun", run(vec![]), &lists, &cache, &mut next).await;
    assert!(matches!(field(&v, "events"), WireValue::Long(e) if e > 0));
    // pcmHpaRun on a model with load balancers, as pcmhpa runs it
    let steps = [(0.0, 0.3), (200.0, 1.0), (400.0, 0.5)];
    let (with_lb, services) = load_balanced(&hpa_model(100.0));
    let cache = Arc::new(PcmCache::new(&with_lb));
    let mut lists: HashMap<i64, Vec<WireValue>> = HashMap::new();
    lists.insert(
        1,
        vec![rec(
            "PcmScalingGroup",
            vec![
                ("container", inst("C2")),
                ("minReplicas", WireValue::Int(1)),
                ("maxReplicas", WireValue::Int(6)),
                ("targetUtilisation", WireValue::Double(0.7)),
                ("initialReplicas", WireValue::Int(1)),
                ("startup", WireValue::Double(30.0)),
                ("scaleDown", WireValue::Double(-1.0)),
                ("cpuRequest", WireValue::Double(1.0)),
            ],
        )],
    );
    lists.insert(2, services.iter().map(|x| rec("PcmServiceGroup", vec![("container", inst(&x.container)), ("assembly", inst(&x.assembly)), ("loadBalancer", inst(&x.load_balancer))])).collect());
    lists.insert(3, steps.iter().map(|&(t, f)| rec("PcmLoadStep", vec![("scenario", WireValue::Null), ("time", WireValue::Double(t)), ("factor", WireValue::Double(f))])).collect());
    lists.insert(4, vec![inst("scenario")]);
    lists.insert(5, vec![WireValue::Double(30.0)]);
    lists.insert(9, vec![]);
    let args = vec![
        WireValue::Ref(9),
        WireValue::Ref(1),
        WireValue::Ref(2),
        WireValue::Ref(3),
        WireValue::Ref(4),
        WireValue::Ref(5),
        WireValue::Double(60.0),
        WireValue::Double(600.0),
        WireValue::Double(1000.0),
        WireValue::Double(2.0),
        WireValue::Bool(false),
        WireValue::Int(3),
        WireValue::Int(400),
    ];
    let (v, _) = call(&engine, "pcmHpaRun", args.clone(), &lists, &cache, &mut next).await;
    let mut req = hpa_request(&steps, 60.0, 600.0, 6, services);
    req.run.bin_s = 10.0;
    let (r, _) = pcmhpa::run_model(&with_lb, &req, Default::default()).unwrap();
    assert_eq!(field(&v, "readyReplicaSeconds"), WireValue::Double(r.ready_replica_seconds));
    assert_eq!(field(&v, "slo"), WireValue::Double(r.slo));
    // cached per model and request
    let (v2, _) = call(&engine, "pcmHpaRun", args, &lists, &cache, &mut next).await;
    assert_eq!(field(&v2, "wallMs"), field(&v, "wallMs"));
}
