//! `pcmEvaluate` (`palladio_sim_service::evaluate`) on small fixtures where the answers are known.
//!
//! - `two-tier.m`: Frontend (C1, 5 ms CPU) calls Backend (C2, 10 ms CPU), scenario `scenario`
//!   at RATE/s; time unit s, CPUs at 1000 units/s (one unit = 1 ms of CPU);
//! - `paths.m`: Frontend (C1) calls Backend (C2) with probability 0.3 and forks Audit (C3)
//!   asynchronously: a session's synchronous path holds C1 always, C2 in 30 %, C3 never.

use palladio_sim_service::detail::{self, DetailRequest};
use palladio_sim_service::evaluate::{self, Evaluation, LoadStep, Monitors, Request, ScalingGroup};
use palladio_sim_service::mfile;
use palladio_sim_service::pcm::PcmCache;
use palladio_sim_service::config::Config;
use palladio_sim_service::protocol::{HeapObject, WireModel, WireValue};
use std::collections::HashMap;
use std::sync::Arc;

fn fixture(name: &str, rate: f64) -> WireModel {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    mfile::parse(&text.replace("RATE", &format!("{rate:?}")).replace("BACKEND_CONTAINER", "C2")).unwrap()
}

fn eval(m: &WireModel, req: &Request) -> Evaluation {
    evaluate::run_caller(&PcmCache::new(m), req, Monitors::default(), Default::default()).unwrap()
}

fn near(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs())
}

fn steady(warm: f64, end: f64) -> Request {
    Request { warm_up_s: warm, end_s: end, bin_s: 30.0, ms_per_unit: 1000.0, seed: 3, ..Request::default() }
}

/// Counts, response-time statistics (percentiles, histogram, bins), utilisation and core-seconds
/// of a fixed two-tier system.
#[test]
fn counts_response_times_and_core_seconds() {
    let req = Request { percentiles: vec![50.0, 95.0, 99.9], ..steady(60.0, 360.0) };
    let e = eval(&fixture("two-tier.m", 20.0), &req);
    assert!(!e.failed, "{}", e.message);
    assert_eq!((e.window_s, e.bins, e.percentiles.len()), (300.0, 10, 3));
    let n = 20.0 * 300.0;
    let [op] = e.operations.as_slice() else { panic!("{:?}", e.operations) };
    assert_eq!((op.kind, op.role.as_deref(), op.signature.as_deref()), ("systemOperation", Some("system_provides"), Some("handle")));
    assert!(near(op.count as f64, n, 0.05), "{}", op.count);
    // M/M/1 at 0.1 and 0.2: 5.6 + 12.5 ms mean
    assert!(op.mean_ms > 15.0 && op.mean_ms < 22.0, "{op:?}");
    let [p50, p95, p999] = op.percentiles_ms[..] else { panic!() };
    assert!(p50 < op.mean_ms && op.mean_ms < p95 && p95 < p999 && p999 <= op.max_ms);
    // the histogram holds every value; the bins add up to the window
    assert_eq!(op.histogram_counts.iter().sum::<u64>(), op.count);
    assert_eq!(op.histogram_edges_ms.len(), op.histogram_counts.len() + 1);
    assert!(op.histogram_edges_ms.windows(2).all(|w| w[0] < w[1]));
    assert_eq!((op.bin_counts.len(), op.bin_mean_ms.len(), op.bin_percentiles_ms.len()), (10, 10, 30));
    assert_eq!(op.bin_counts.iter().sum::<u64>(), op.count);
    let weighted: f64 = op.bin_counts.iter().zip(&op.bin_mean_ms).map(|(c, m)| *c as f64 * m).sum::<f64>() / op.count as f64;
    assert!(near(weighted, op.mean_ms, 1e-9));
    assert!(op.bin_percentiles_ms.chunks(3).all(|c| c[0] <= c[1] && c[1] <= c[2]));
    let s = e.sessions.iter().find(|s| s.key == "scenario").unwrap();
    assert_eq!((s.kind, s.count), ("scenario", op.count));
    assert!(e.calls.is_empty() && e.assembly_operations.is_empty(), "detailCalls is off");
    let counts: HashMap<&str, u64> = e.call_counts.iter().map(|(a, n)| (a.as_str(), *n)).collect();
    assert!(near(counts["Frontend_handle_callQuery"] as f64, n, 0.05));
    assert_eq!(counts["scenario_call"], op.count);
    for (c, u) in [("C1", 0.1), ("C2", 0.2)] {
        let x = e.resources.iter().find(|x| x.container == c).unwrap();
        assert!(!x.autoscaled && x.kind == "CPU" && x.cores == 1.0);
        assert_eq!((x.mean_ready_replicas, x.replica_core_seconds), (1.0, 300.0));
        assert!(x.bin_replicas.iter().chain(&x.bin_ready_replicas).all(|r| *r == 1.0));
        assert!(near(x.mean_utilisation, u, 0.08), "{c}: {}", x.mean_utilisation);
        assert_eq!(x.bin_utilisation.len(), 10);
        assert!(x.max_bin_utilisation >= x.mean_utilisation && x.max_bin_utilisation < 2.0 * u, "{c}: {:?}", x.bin_utilisation);
        assert!(near(x.bin_utilisation.iter().sum::<f64>() / 10.0, x.mean_utilisation, 1e-9));
    }
    // deterministic
    let again = eval(&fixture("two-tier.m", 20.0), &req);
    assert_eq!(Evaluation { wall_ms: 0.0, ..again }, Evaluation { wall_ms: 0.0, ..e.clone() });
    // with detailCalls: per call and assembly operation as well; the run is the same
    let d = eval(&fixture("two-tier.m", 20.0), &Request { detail_calls: true, ..req.clone() });
    assert_eq!(d.calls.len(), 1);
    assert_eq!(d.calls[0].count, op.count);
    assert_eq!(d.assembly_operations.len(), 2, "{:?}", d.assembly_operations.iter().map(|a| &a.key).collect::<Vec<_>>());
    assert_eq!(d.operations, e.operations);
}

/// The measured synchronous paths: a branch with probability 0.3 into C2, an asynchronous fork
/// into C3 (never on the path).
#[test]
fn sync_path_sets() {
    let e = eval(&fixture("paths.m", 20.0), &steady(30.0, 630.0));
    assert!(!e.failed, "{}", e.message);
    let sessions = e.sessions[0].count;
    assert!(sessions > 11_000);
    fn sorted(v: &[String]) -> Vec<&str> {
        let mut x: Vec<&str> = v.iter().map(String::as_str).collect();
        x.sort();
        x
    }
    let sets: HashMap<Vec<&str>, u64> = e.path_sets.iter().map(|s| (sorted(&s.containers), s.count)).collect();
    assert_eq!(sets.len(), 2, "{sets:?}");
    let total: u64 = sets.values().sum();
    assert!((total as i64 - sessions as i64).abs() <= 2, "{total} vs {sessions}");
    let with_c2 = sets[&vec!["C1", "C2"]] as f64 / total as f64;
    let sd = (0.3 * 0.7 / total as f64).sqrt();
    assert!((with_c2 - 0.3).abs() < 4.0 * sd, "{with_c2}");
    assert!(sets.contains_key(&vec!["C1"]));
    assert!(!e.path_sets_truncated);
    // the most frequent first
    assert!(e.path_sets[0].count >= e.path_sets[1].count);
    // a cap keeps the most frequent set and says so
    let capped = eval(&fixture("paths.m", 20.0), &Request { max_path_sets: 1, ..steady(30.0, 630.0) });
    assert!(capped.path_sets_truncated && capped.path_sets.len() == 1 && capped.path_sets[0].containers == ["C1"]);
    // call counts: the fork's call in every session, the query in 30 %
    let calls: HashMap<&str, u64> = e.call_counts.iter().map(|(a, n)| (a.as_str(), *n)).collect();
    assert!(near(calls["Frontend_handle_callRecord"] as f64, sessions as f64, 0.01));
    assert!((calls["Frontend_handle_callQuery"] as f64 / calls["Frontend_handle_callRecord"] as f64 - 0.3).abs() < 0.02);
    // C3 does work (the fork runs), but off the path
    let c3 = e.resources.iter().find(|x| x.container == "C3").unwrap();
    assert!(near(c3.mean_utilisation, 0.04, 0.1), "{}", c3.mean_utilisation);
}

fn backend_group(max: u32) -> ScalingGroup {
    ScalingGroup { max, ..ScalingGroup::new("C2") }
}

/// The HPA scales C2 (one core, 10 ms per request) at 100 requests/s (1 core of load) to two
/// pods at a target of 0.7; core-seconds of the pods; the pods count as C2 on the paths.
#[test]
fn autoscaled_replicas_cost_and_paths() {
    let mut req = Request { groups: vec![backend_group(6)], ..steady(360.0, 1200.0) };
    req.profile = vec![LoadStep { scenario: None, time: 0.0, factor: 0.1 }, LoadStep { scenario: None, time: 60.0, factor: 1.0 }];
    let e = eval(&fixture("two-tier.m", 100.0), &req);
    assert!(!e.failed && !e.capped, "{}", e.message);
    let c2 = e.resources.iter().find(|x| x.container == "C2").unwrap();
    assert!(c2.autoscaled);
    assert!((c2.mean_ready_replicas - 2.0).abs() <= 1.0, "{}", c2.mean_ready_replicas);
    assert!(near(c2.ready_replica_core_seconds, c2.mean_ready_replicas * 840.0, 1e-6));
    assert!(c2.replica_core_seconds >= c2.ready_replica_core_seconds);
    assert!(c2.peak_ready_replicas >= c2.mean_ready_replicas - 1e-3 && c2.created_pods >= 1 && c2.rescales >= 1, "{c2:?}");
    // per bin: the pods' time means average to the window's
    let mean_bins = c2.bin_ready_replicas.iter().sum::<f64>() / c2.bin_ready_replicas.len() as f64;
    assert!(near(mean_bins, c2.mean_ready_replicas, 1e-6), "{mean_bins}");
    assert!(c2.bin_replicas.iter().zip(&c2.bin_ready_replicas).all(|(a, r)| a >= r));
    // per ready pod about the load over the pods
    assert!(near(c2.mean_utilisation, 1.0 / c2.mean_ready_replicas, 0.15), "{} at {} pods", c2.mean_utilisation, c2.mean_ready_replicas);
    assert!(e.peak_pods >= c2.peak_ready_replicas);
    // every session touches C2 (on whichever pod); the plugin's load-balancer server is no
    // container of the model
    let total: u64 = e.path_sets.iter().map(|s| s.count).sum();
    assert!(total > 80_000);
    assert_eq!(e.path_sets.len(), 1, "{:?}", e.path_sets);
    let mut cs = e.path_sets[0].containers.clone();
    cs.sort();
    assert_eq!(cs, ["C1", "C2"]);
    assert!(!e.resources.iter().any(|x| x.container == "LoadBalancerServer"));
    assert!(e.call_counts.iter().all(|(a, _)| !a.contains("LoadBalancer")));
}

/// The HPA settings are arguments: a shorter sync period and a behaviour that limits scale-ups
/// to one pod per minute change the run.
#[test]
fn hpa_settings_are_arguments() {
    let mut req = Request { groups: vec![backend_group(8)], ..steady(0.0, 600.0) };
    req.profile = vec![LoadStep { scenario: None, time: 0.0, factor: 0.1 }, LoadStep { scenario: None, time: 60.0, factor: 3.0 }];
    let base = eval(&fixture("two-tier.m", 100.0), &req);
    let mut slow = req.clone();
    slow.groups[0].behavior = Some(serde_json::json!({"scaleUp": {"policies": [{"type": "Pods", "value": 1, "periodSeconds": 60}]}}));
    let s = eval(&fixture("two-tier.m", 100.0), &slow);
    assert!(!base.failed && !s.failed, "{} {}", base.message, s.message);
    let (b2, s2) = (base.resources.iter().find(|x| x.container == "C2").unwrap(), s.resources.iter().find(|x| x.container == "C2").unwrap());
    assert!(s2.ready_replica_core_seconds < b2.ready_replica_core_seconds, "{} vs {}", s2.ready_replica_core_seconds, b2.ready_replica_core_seconds);
    let mut fast = req.clone();
    fast.controller.sync_s = 5.0;
    let f = eval(&fixture("two-tier.m", 100.0), &fast);
    assert_ne!(f.events, base.events);
}

/// A run that stops at the pod cap, the wall-clock limit, or a design that cannot be simulated,
/// is an evaluation with `failed`, not an error; wrong arguments are errors.
#[test]
fn failures_are_evaluations() {
    let mut steps = Vec::new();
    for i in 0..40 {
        steps.push(LoadStep { scenario: None, time: 60.0 + 60.0 * i as f64, factor: if i % 2 == 0 { 2.0 } else { 0.1 } });
    }
    let mut g = backend_group(20);
    g.scale_down_s = Some(0.0);
    g.startup_s = 5.0;
    let req = Request { groups: vec![g], profile: steps, max_pods: 20, ..steady(60.0, 2460.0) };
    let e = eval(&fixture("two-tier.m", 100.0), &req);
    assert!(e.failed && e.capped && !e.timed_out, "{e:?}");
    assert!(e.message.contains("maxPods"), "{}", e.message);
    // the wall-clock limit
    let t = eval(&fixture("two-tier.m", 200.0), &Request { wall_s: 0.05, ..steady(0.0, 1e6) });
    assert!(t.failed && t.timed_out, "{}", t.message);
    // the event limit
    let ev = eval(&fixture("two-tier.m", 20.0), &Request { max_events: 1000, ..steady(0.0, 600.0) });
    assert!(ev.failed && ev.capped, "{}", ev.message);
    // no allocation of Backend: SimuLizar's error, as a failed evaluation
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    let text = text.replace("RATE", "20.0").replace("allocation.allocationContexts -- alloc_Backend\n", "").replace("alloc_Backend.resourceContainer -- BACKEND_CONTAINER\n", "");
    let text = text.replace("alloc_Backend : AllocationContext { }\nalloc_Backend.assemblyContext -- AssemblyBackend\n", "");
    let e = eval(&mfile::parse(&text).unwrap(), &steady(0.0, 60.0));
    assert!(e.failed && !e.capped && !e.message.is_empty(), "{e:?}");
    // a group on something that is not a container; a percentile outside 0..100
    let bad = Request { groups: vec![ScalingGroup::new("AssemblyBackend")], ..steady(0.0, 60.0) };
    assert!(evaluate::run_caller(&PcmCache::new(&fixture("two-tier.m", 20.0)), &bad, Monitors::default(), Default::default()).is_err());
    let bad = Request { percentiles: vec![101.0], ..steady(0.0, 60.0) };
    assert!(evaluate::run_caller(&PcmCache::new(&fixture("two-tier.m", 20.0)), &bad, Monitors::default(), Default::default()).is_err());
}

/// A per-scenario load profile scales the scenario's sessions; usage streams keep the users of
/// two designs the same.
#[test]
fn load_profile_and_usage_streams() {
    let base = steady(0.0, 300.0);
    let twice = Request { profile: vec![LoadStep { scenario: Some("scenario".into()), time: 100.0, factor: 2.0 }], ..base.clone() };
    let (a, b) = (eval(&fixture("two-tier.m", 20.0), &base), eval(&fixture("two-tier.m", 20.0), &twice));
    let (na, nb) = (a.sessions[0].count as f64, b.sessions[0].count as f64);
    assert!(near(nb / na, 5.0 / 3.0, 0.06), "{na} {nb}");
    let bins = &b.sessions[0].bin_counts;
    assert!(bins[9] as f64 > 1.6 * bins[1] as f64, "{bins:?}");
    // another design (a faster backend): the same arrivals, so nearly the same session count
    let fast = Request { overrides: vec![("C2_cpu".into(), 1, 2000.0)], ..base.clone() };
    let c = eval(&fixture("two-tier.m", 20.0), &fast);
    assert!((c.sessions[0].count as i64 - a.sessions[0].count as i64).abs() <= 3);
    assert!(c.operations[0].mean_ms < a.operations[0].mean_ms);
}

/// The design detail runs the evaluated simulation: its evaluation is pcmEvaluate's.
#[test]
fn the_detail_is_the_evaluated_run() {
    let m = fixture("paths.m", 20.0);
    let run = Request { groups: vec![ScalingGroup { max: 4, ..ScalingGroup::new("C2") }], ..steady(60.0, 360.0) };
    let e = eval(&m, &run);
    let d = detail::detail(&m, &DetailRequest { run: run.clone(), ..DetailRequest::default() }, Default::default()).unwrap();
    let mut de = d["evaluation"].clone();
    let mut want = e.to_json();
    de["wallMs"] = serde_json::json!(0);
    want["wallMs"] = serde_json::json!(0);
    assert_eq!(de, want);
    let so = &d["systemOperations"][0];
    assert_eq!(so["count"].as_u64().unwrap(), e.operations[0].count);
    assert_eq!(so["p95Ms"].as_f64().unwrap(), e.operations[0].percentiles_ms[2]);
    assert!(!d["assemblyOperations"].as_array().unwrap().is_empty());
    let c2 = d["containers"].as_array().unwrap().iter().find(|c| c["container"] == "C2").unwrap();
    assert_eq!(c2["autoscaled"], true);
    assert!(d["load"]["baseRatePerS"]["scenario"].as_f64().is_some_and(|r| near(r, 20.0, 0.05)), "{}", d["load"]);
    // the JSON request of the detail gives the same request
    let v = serde_json::json!({"durationS": 360, "warmUpS": 60, "binS": 30, "seed": 3, "timeUnitMs": 1000,
        "groups": [{"container": "C2", "max": 4}]});
    assert_eq!(DetailRequest::from_json(&v).unwrap().run, run);
}

/// `pcmEvaluate` as the platform calls it: named records, defaults filled in, nested lists; one
/// simulation per request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_script_function() {
    use palladio_sim_service::functions;
    let engine = Arc::new(Config::from_env().unwrap());
    let rec = |c: &str, f: Vec<(&str, WireValue)>| WireValue::Record { class_name: c.into(), fields: f.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
    let inst = |n: &str| WireValue::Instance(n.into());
    let cache = Arc::new(PcmCache::new(&fixture("paths.m", 20.0)));
    let mut lists: HashMap<i64, Vec<WireValue>> = HashMap::new();
    // a group with its defaults sent by the platform
    lists.insert(
        2,
        vec![rec(
            "PcmScalingGroup",
            vec![
                ("container", inst("C2")),
                ("minReplicas", WireValue::Int(1)),
                ("maxReplicas", WireValue::Int(4)),
                ("targetUtilisation", WireValue::Double(0.7)),
                ("initialReplicas", WireValue::Int(1)),
                ("startupSeconds", WireValue::Double(30.0)),
                ("scaleDownSeconds", WireValue::Double(-1.0)),
                ("cpuRequest", WireValue::Double(0.0)),
                ("behaviorJson", WireValue::String(String::new())),
            ],
        )],
    );
    // end, warmUp, binSeconds, msPerTimeUnit, profile, groups; the rest left to the defaults
    let args = vec![WireValue::Double(360.0), WireValue::Double(60.0), WireValue::Double(30.0), WireValue::Double(1000.0), WireValue::Null, WireValue::Ref(2)];
    let mut n = 1000;
    let mut next = move || {
        n += 1;
        n
    };
    let a = functions::call(&engine, "pcmEvaluate", &args, &lists, &cache, None, &mut next).await.unwrap();
    let field = |v: &WireValue, k: &str| -> WireValue {
        let WireValue::Record { fields, .. } = v else { panic!("not a record: {v:?}") };
        fields.iter().find(|(n, _)| n == k).unwrap_or_else(|| panic!("no {k}")).1.clone()
    };
    let list = |objs: &[HeapObject], v: WireValue| -> Vec<WireValue> {
        let WireValue::Ref(id) = v else { panic!("{v:?}") };
        objs.iter().find(|o| o.id == id).unwrap().elements.clone().unwrap()
    };
    assert_eq!(field(&a.value, "failed"), WireValue::Bool(false));
    assert_eq!(field(&a.value, "version"), WireValue::String(evaluate::VERSION.into()));
    let sets = list(&a.objects, field(&a.value, "pathSets"));
    assert_eq!(sets.len(), 2);
    assert!(!list(&a.objects, field(&sets[0], "containers")).is_empty());
    // the same evaluation as the library's with the defaults (seed 1)
    let want = eval(&fixture("paths.m", 20.0), &Request { groups: vec![ScalingGroup { max: 4, ..ScalingGroup::new("C2") }], seed: 1, ..steady(60.0, 360.0) });
    let ops = list(&a.objects, field(&a.value, "operations"));
    let pct = list(&a.objects, field(&ops[0], "percentilesMs"));
    assert_eq!(pct.len(), 4);
    assert_eq!(pct[2], WireValue::Double(want.operations[0].percentiles_ms[2]));
    let res = list(&a.objects, field(&a.value, "resources"));
    assert_eq!(list(&a.objects, field(&res[0], "binUtilisation")).len(), 10);
    // cached: the second call is the first's evaluation
    let b = functions::call(&engine, "pcmEvaluate", &args, &lists, &cache, None, &mut next).await.unwrap();
    assert_eq!(field(&b.value, "wallMs"), field(&a.value, "wallMs"));
}

// ---------------------------------------------------------------------------------------------
// Invariance to names and order

/// `m` with every instance renamed, every entity name changed, the instances and every
/// reference list shuffled (deterministically by `seed`); and the new name of each old name.
fn scramble(m: &WireModel, seed: u64) -> (WireModel, HashMap<String, String>) {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s >> 33
    };
    let mut ids: Vec<usize> = (0..m.instances.len()).collect();
    for i in (1..ids.len()).rev() {
        ids.swap(i, (next() % (i as u64 + 1)) as usize);
    }
    let rename: HashMap<String, String> = m.instances.iter().zip(&ids).map(|(x, i)| (x.name.clone(), format!("q{i}"))).collect();
    let mut instances: Vec<_> = m
        .instances
        .iter()
        .map(|x| {
            let mut y = x.clone();
            y.name = rename[&x.name].clone();
            for (k, v) in &mut y.attributes {
                if k == "entityName" {
                    *v = vec![WireValue::String(format!("renamed {}", next()))];
                }
            }
            for (_, ts) in &mut y.references {
                for t in ts.iter_mut() {
                    if let Some(n) = rename.get(t) {
                        *t = n.clone();
                    }
                }
            }
            y
        })
        .collect();
    fn shuffle<T>(v: &mut [T], next: &mut dyn FnMut() -> u64) {
        for i in (1..v.len()).rev() {
            v.swap(i, (next() % (i as u64 + 1)) as usize);
        }
    }
    let mut nx = {
        let mut t = seed ^ 0xABCDEF;
        move || {
            t = t.wrapping_mul(6364136223846793005).wrapping_add(1);
            t >> 33
        }
    };
    shuffle(&mut instances, &mut nx);
    for x in &mut instances {
        for (_, ts) in &mut x.references {
            shuffle(ts, &mut nx);
        }
    }
    (WireModel { metamodel_path: m.metamodel_path.clone(), instances }, rename)
}

/// The evaluation as JSON with names mapped by `back` (ids joined by `|` too).
fn renamed(e: &Evaluation, back: &HashMap<String, String>) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value, back: &HashMap<String, String>) {
        match v {
            serde_json::Value::String(s) => *s = s.split('|').map(|p| back.get(p).cloned().unwrap_or_else(|| p.to_string())).collect::<Vec<_>>().join("|"),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, back)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|x| walk(x, back)),
            _ => {}
        }
    }
    let mut v = Evaluation { wall_ms: 0.0, ..e.clone() }.to_json();
    walk(&mut v, back);
    v
}

/// Renaming every element (ids and entity names) and shuffling every list gives the same
/// evaluation, element for element and in the same list order.
fn invariant(m: &WireModel, req: &Request, rename_req: impl Fn(&Request, &HashMap<String, String>) -> Request) {
    let a = eval(m, req);
    assert!(!a.failed, "{}", a.message);
    // instances that colour refinement cannot tell apart (interchangeable): their labels may
    // swap, their values may not
    let ca = PcmCache::new(m);
    let tied: HashMap<String, String> = ca.canon.tied.iter().map(|c| (ca.canon.orig(c), "<tied>".to_string())).collect();
    let a = renamed(&a, &tied);
    let a = serde_json::from_value::<serde_json::Value>(a).unwrap();
    for seed in [1, 2] {
        let (sm, map) = scramble(m, seed);
        let back: HashMap<String, String> =
            map.iter().map(|(k, v)| (v.clone(), if tied.contains_key(k) { "<tied>".to_string() } else { k.clone() })).collect();
        let b = eval(&sm, &rename_req(req, &map));
        let (x, y) = (renamed(&b, &back), a.clone());
        if x != y && let Ok(d) = std::env::var("PCM_INVARIANCE_DUMP") {
            std::fs::write(format!("{d}/b{seed}.json"), serde_json::to_string_pretty(&x).unwrap()).unwrap();
            std::fs::write(format!("{d}/a.json"), serde_json::to_string_pretty(&y).unwrap()).unwrap();
        }
        assert!(x == y, "seed {seed}: the evaluations differ");
    }
}

fn rename_request(r: &Request, map: &HashMap<String, String>) -> Request {
    let m = |s: &str| map.get(s).cloned().unwrap_or_else(|| s.to_string());
    let mut r = r.clone();
    r.groups.iter_mut().for_each(|g| g.container = m(&g.container));
    r.overrides.iter_mut().for_each(|o| o.0 = m(&o.0));
    r.profile.iter_mut().for_each(|p| p.scenario = p.scenario.as_deref().map(m));
    r
}

#[test]
fn results_do_not_depend_on_names_or_order() {
    invariant(&fixture("paths.m", 20.0), &steady(30.0, 330.0), rename_request);
    // with autoscaling (pods created during the run), a per-scenario profile and an override
    let mut req = Request { groups: vec![backend_group(6)], overrides: vec![("C1_cpu".into(), 2, 1000.0)], ..steady(60.0, 600.0) };
    req.profile = vec![LoadStep { scenario: Some("scenario".into()), time: 0.0, factor: 0.2 }, LoadStep { scenario: Some("scenario".into()), time: 100.0, factor: 1.0 }];
    invariant(&fixture("two-tier.m", 100.0), &req, rename_request);
}

/// The same on a realised MiSArch design of the domain project (skipped without it).
#[test]
fn a_realised_misarch_design_does_not_depend_on_names_or_order() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../palladio-architecture-optimization-domain/build/domain/detail-work");
    let Some(path) = std::fs::read_dir(dir).ok().and_then(|d| d.flatten().map(|e| e.path().join("realised/design.model.json")).find(|p| p.exists())) else {
        eprintln!("no realised design in {dir}: skipped");
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let m = mfile::from_model_data(&v).unwrap();
    let t0 = std::time::Instant::now();
    let cache = PcmCache::new(&m);
    eprintln!("{}: {} PCM instances, canonical form {:.0} ms ({} ties), cache built in {:.0} ms", path.display(), cache.plain.instances.len(), cache.canon_ms, cache.canon.ties, t0.elapsed().as_secs_f64() * 1e3);
    invariant(&m, &Request { ..steady(0.0, 60.0) }, rename_request);
}

// ---------------------------------------------------------------------------------------------
// The script path and the command-line path give the same evaluation

fn gunzip_json(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("gzip").arg("-dc").arg(&path).output().expect("gzip");
    assert!(out.status.success(), "{path}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A realised MiSArch design (the domain project's today, day profile) as the platform sent it to
/// the plugin in a search (`misarch-today.wire.json.gz`, dumped with SIM_DUMP_DIR: integer
/// attributes as Int, enums as Enum, unset attributes as empty lists) and as model data on disk
/// (`misarch-today.model-data.json.gz`: the file the explorer's `detail` reads), with the search's
/// request (`misarch-today.request.json`). Both paths must give one canonical model (digest) and
/// the same evaluation: the script function on the wire model, the command line's evaluation on
/// the model data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_script_path_and_the_command_line_give_the_same_evaluation() {
    use palladio_sim_service::functions;
    let wire = mfile::from_wire_json(&gunzip_json("misarch-today.wire.json.gz")).unwrap();
    let data = mfile::from_model_data(&gunzip_json("misarch-today.model-data.json.gz")).unwrap();
    let (cw, cd) = (PcmCache::new(&wire), PcmCache::new(&data));
    assert_eq!(cw.digest(), cd.digest(), "one canonical model");
    assert_eq!(format!("{:?}", cw.plain.instances.iter().map(|x| (&x.name, &x.class_name, &x.references)).collect::<Vec<_>>()),
        format!("{:?}", cd.plain.instances.iter().map(|x| (&x.name, &x.class_name, &x.references)).collect::<Vec<_>>()));
    // the search's request, a shorter run
    let mut rv: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/misarch-today.request.json")).unwrap()).unwrap();
    rv["durationS"] = serde_json::json!(420.0);
    let req = Request::from_json(&rv).unwrap();
    assert_eq!(Request::from_json(&req.to_json()).unwrap(), req, "the request's JSON round trip");
    // the command line's path
    let want = evaluate::run_caller(&cd, &req, Monitors::default(), Default::default()).unwrap();
    assert!(!want.failed, "{}", want.message);
    // the script path: the request as the script's records and arguments
    let rec = |c: &str, f: Vec<(&str, WireValue)>| WireValue::Record { class_name: c.into(), fields: f.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
    let inst = |n: &serde_json::Value| WireValue::Instance(n.as_str().unwrap().to_string());
    let d = |x: &serde_json::Value| WireValue::Double(x.as_f64().unwrap());
    let i = |x: &serde_json::Value| WireValue::Int(x.as_f64().unwrap() as i32);
    let mut lists: HashMap<i64, Vec<WireValue>> = HashMap::new();
    lists.insert(1, rv["profile"].as_array().unwrap().iter().map(|p| rec("PcmLoadStep", vec![("scenario", p.get("scenario").map_or(WireValue::Null, inst)), ("time", d(&p["timeS"])), ("factor", d(&p["factor"]))])).collect());
    lists.insert(
        2,
        rv["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                rec(
                    "PcmScalingGroup",
                    vec![
                        ("container", inst(&g["container"])),
                        ("minReplicas", i(&g["min"])),
                        ("maxReplicas", i(&g["max"])),
                        ("targetUtilisation", d(&g["target"])),
                        ("initialReplicas", i(&g["initial"])),
                        ("startupSeconds", d(&g["startupS"])),
                        ("scaleDownSeconds", g.get("scaleDownS").map_or(WireValue::Double(-1.0), d)),
                        ("cpuRequest", g.get("cpuRequest").map_or(WireValue::Double(0.0), d)),
                        ("behaviorJson", WireValue::String(String::new())),
                    ],
                )
            })
            .collect(),
    );
    lists.insert(3, vec![]);
    lists.insert(4, rv["overrides"].as_array().unwrap().iter().map(|o| rec("PcmOverride", vec![("resource", inst(&o["resource"])), ("replicas", i(&o["replicas"])), ("rate", d(&o["rate"]))])).collect());
    lists.insert(5, rv["percentiles"].as_array().unwrap().iter().map(d).collect());
    let args = vec![
        d(&rv["durationS"]),
        d(&rv["warmUpS"]),
        d(&rv["binS"]),
        d(&rv["timeUnitMs"]),
        WireValue::Ref(1),
        WireValue::Ref(2),
        WireValue::Ref(3),
        WireValue::Ref(4),
        WireValue::Ref(5),
        WireValue::Bool(false),
        i(&rv["histogramPerDecade"]),
        i(&rv["seed"]),
        WireValue::Bool(true),
        WireValue::Bool(true),
        i(&rv["maxPods"]),
        d(&rv["hpaSyncS"]),
        d(&rv["hpaTolerance"]),
        d(&rv["hpaDownscaleStabilisationS"]),
        d(&rv["hpaCpuInitialisationS"]),
        i(&rv["maxPathSets"]),
        d(&rv["wallSeconds"]),
        WireValue::Long(rv["maxEvents"].as_i64().unwrap()),
    ];
    let config = Arc::new(Config::from_env().unwrap());
    let mut n = 0;
    let mut next = move || {
        n -= 1;
        n
    };
    let a = functions::call(&config, "pcmEvaluate", &args, &lists, &Arc::new(cw), None, &mut next).await.unwrap();
    let field = |v: &WireValue, k: &str| -> WireValue {
        let WireValue::Record { fields, .. } = v else { panic!("not a record: {v:?}") };
        fields.iter().find(|(n, _)| n == k).unwrap_or_else(|| panic!("no {k}")).1.clone()
    };
    let list = |v: WireValue| -> Vec<WireValue> {
        let WireValue::Ref(id) = v else { panic!("{v:?}") };
        a.objects.iter().find(|o| o.id == id).unwrap().elements.clone().unwrap()
    };
    assert_eq!(field(&a.value, "failed"), WireValue::Bool(false));
    assert_eq!(field(&a.value, "events"), WireValue::Long(want.events as i64));
    assert_eq!(field(&a.value, "peakPods"), WireValue::Double(want.peak_pods));
    let ops = list(field(&a.value, "operations"));
    assert_eq!(ops.len(), want.operations.len());
    for (o, w) in ops.iter().zip(&want.operations) {
        assert_eq!(field(o, "key"), WireValue::String(w.key.clone()));
        assert_eq!(field(o, "count"), WireValue::Long(w.count as i64));
        assert_eq!(list(field(o, "percentilesMs")), w.percentiles_ms.iter().map(|x| WireValue::Double(*x)).collect::<Vec<_>>());
    }
    let res = list(field(&a.value, "resources"));
    for (r, w) in res.iter().zip(&want.resources) {
        assert_eq!(field(r, "container"), WireValue::Instance(w.container.clone()));
        assert_eq!(field(r, "maxBinUtilisation"), WireValue::Double(w.max_bin_utilisation));
        assert_eq!(field(r, "replicaCoreSeconds"), WireValue::Double(w.replica_core_seconds));
    }
    assert_eq!(list(field(&a.value, "pathSets")).len(), want.path_sets.len());
}
