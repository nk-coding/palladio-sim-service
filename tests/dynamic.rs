//! Dynamic runs with Kubernetes-style autoscaling (rule 5, `palladio_arch::dynamic`).
//!
//! The fixture `two-tier.m`: Frontend (5 ms CPU, on C1) calls Backend (10 ms CPU, on C2); C2 is
//! an autoscaled Deployable of 0.5-core replicas at a target utilisation of 0.7. At λ = 100/s
//! Backend's load is 1.0 core, so the ideal autoscaler runs 3 replicas.

use palladio_arch::dynamic::{self, HpaParams};
use palladio_arch::graph::Graph;
use palladio_arch::translate::{self, DynamicModel};
use palladio_arch::{functions, mfile};
use palladio_sim_service::config::Config;
use palladio_sim_service::protocol::{WireModel, WireValue};
use std::sync::Arc;

const ROOT: &str = "decisions : ArchitectureDecisions { msPerTimeUnit = 1000.0 }\n";

/// The fixture with `rate` users per second, Backend on C2 autoscaled (`attrs` on the
/// Autoscaling), a latency budget of 30 ms and the load profile `steps` (`(s, factor)`) with
/// `warm_up` and `end` (s).
fn model(rate: f64, attrs: &str, steps: &[(f64, f64)], warm_up: f64, end: f64) -> WireModel {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    let text = text.replace("RATE", &format!("{rate:?}")).replace("BACKEND_CONTAINER", "C2");
    let mut d = format!(
        "{ROOT}d : Deployable {{ cpuPerReplica = 0.5 }}\nd.container -- C2\nd.autoscaling -- auto\ndecisions.deployables -- d\n\
         auto : Autoscaling {{ targetUtilisation = 0.7 {attrs} }}\ndecisions.autoscalings -- auto\n\
         cost : CostModel {{ perContainer = 0.0 perCore = 1.0 perReplicaOverhead = 0.2 perCache = 0.0 }}\ndecisions.costModel -- cost\n\
         budget : LatencyBudget {{ ms = 30.0 }}\nbudget.scenario -- scenario\ndecisions.latencyBudgets -- budget\n\
         profile : LoadProfile {{ warmUpSeconds = {warm_up:?} endSeconds = {end:?} sloFactor = 2.0 }}\ndecisions.loadProfile -- profile\n"
    );
    for (i, (t, f)) in steps.iter().enumerate() {
        d += &format!("profile.steps -- step{i}\nstep{i} : LoadStep {{ seconds = {t:?} factor = {f:?} }}\n");
    }
    mfile::parse(&format!("{text}\n{d}")).unwrap()
}

fn dynamic_model(m: &WireModel) -> DynamicModel {
    translate::translate_dynamic(m).unwrap().1
}

fn run(m: &WireModel, max_pods: u32) -> dynamic::HpaOutcome {
    let p = dynamic::prepare(Arc::new(dynamic_model(m)), max_pods).unwrap();
    dynamic::run(&p, &HpaParams { seed: 3, exclude: vec![] }, Default::default()).unwrap()
}

fn canonical(m: &WireModel) -> String {
    let mut v: Vec<String> = m.instances.iter().map(|i| format!("{i:?}")).collect();
    v.sort();
    v.join("\n")
}

#[test]
fn the_static_translation_is_unchanged() {
    let m = model(100.0, "", &[(0.0, 1.0)], 60.0, 600.0);
    let a = translate::translate(&m).unwrap();
    let (b, _) = translate::translate_dynamic(&m).unwrap();
    assert_eq!(canonical(&a.pcm), canonical(&b.pcm));
    assert_eq!(a.cost, b.cost);
    assert_eq!(a.replicas, b.replicas);
}

#[test]
fn one_pod_behind_a_load_balancer() {
    let m = model(100.0, "", &[(0.0, 0.5), (300.0, 1.0)], 60.0, 600.0);
    let dm = dynamic_model(&m);
    let g = Graph::new(&dm.pcm);
    let [grp] = dm.groups.as_slice() else { panic!("{:?}", dm.groups) };
    assert_eq!(grp.name, "C2");
    // 0.5 core of load at the start: 0.5 / 0.35 → 2 pods; at the peak 1.0 core: 3 (max 6)
    assert_eq!(grp.initial, 2);
    assert_eq!(grp.ideal(1.0), 3.0);
    assert_eq!((grp.min, grp.max), (1, 6));
    // the pod: one replica's CPU, ⌈0.5⌉ core at half speed; its request is that core
    assert_eq!(g.num_attr("C2_cpu", "numberOfReplicas"), Some(1.0));
    assert_eq!(g.str_attr("C2_cpu", "processingRate").as_deref(), Some("500.0"));
    assert_eq!(grp.cpu_request, 1.0);
    // Frontend's connector now ends at the load balancer, which calls Backend
    let [(asm, lb)] = grp.services.as_slice() else { panic!() };
    let into_lb: Vec<String> = g.referrers(lb, "providingAssemblyContext");
    assert_eq!(into_lb.len(), 1);
    assert_eq!(g.entity(&g.ref1(&into_lb[0], "requiringAssemblyContext").unwrap()), "AssemblyFrontend");
    let into_backend: Vec<String> = g.referrers(asm, "providingAssemblyContext");
    assert_eq!(into_backend.len(), 1);
    assert_eq!(g.ref1(&into_backend[0], "requiringAssemblyContext").as_deref(), Some(lb.as_str()));
    // its SEFF: one branch with one transition that calls `query`
    let comp = g.ref1(lb, "encapsulatedComponent").unwrap();
    let seffs = g.refs(&comp, "serviceEffectSpecifications");
    let [seff] = seffs.as_slice() else { panic!() };
    let branches: Vec<String> = g.refs(seff, "steps").into_iter().filter(|a| g.class(a) == "BranchAction").collect();
    assert_eq!(branches.len(), 1);
    assert_eq!(g.refs(&branches[0], "branches").len(), 1);
    // the load balancers' container joins the link
    assert!(g.refs("lan", "connectedResourceContainers").contains(&"LoadBalancerServer".to_string()));
    // the SPD and the semantic configuration name the group, the HPA its target
    let p = dynamic::prepare(Arc::new(dm), 400).unwrap();
    let file = |n: &str| p.files.iter().find(|f| f.0 == n).unwrap().1.clone();
    assert!(file("dynamic.spd").contains("entityName=\"C2\""));
    assert!(file("dynamic.semanticspd").contains("ServiceGroupCfg"));
    let hpa: serde_json::Value = serde_json::from_str(&file("hpa.json")).unwrap();
    assert_eq!(hpa["items"][0]["spec"]["metrics"][0]["resource"]["target"]["averageUtilization"], 70);
    assert_eq!(hpa["items"][0]["spec"]["maxReplicas"], 6);
    assert_eq!(hpa["items"][0]["simoxide"]["startupDelay"], 30.0);
}

/// At a constant load the HPA's replicas settle where the ideal autoscaler (`archReplicas`)
/// puts them (within Kubernetes' 10 % tolerance band: at most one pod off), from one pod.
#[test]
fn at_constant_load_the_hpa_settles_at_the_ideal_count() {
    for (rate, want) in [(50.0, 2.0), (100.0, 3.0), (150.0, 5.0)] {
        // (Frontend's one core on C1 is not scaled: 150/s is 0.75 of it)
        // the ideal autoscaler at that load
        let ideal = translate::translate(&model(rate, "", &[(0.0, 1.0)], 60.0, 600.0)).unwrap();
        assert_eq!(ideal.replicas_of("C2", |n| n.to_string()).unwrap(), want);
        // start at a tenth of the load (one pod), then the load from 60 s on; measure from 360 s
        let m = model(rate, "maxReplicas = 20", &[(0.0, 0.1), (60.0, 1.0)], 360.0, 1200.0);
        let o = run(&m, 400);
        let g = &o.groups[0];
        assert!(!o.capped);
        assert!((g.mean_replicas - want).abs() <= 1.0, "λ = {rate}: HPA {} pods, ideal {want}", g.mean_replicas);
        assert_eq!(g.ideal_replicas, want);
        // and the cost is the fixed part plus the pods' price
        let t = translate::translate_dynamic(&m).unwrap().1;
        let fixed = t.fixed_cost;
        assert!((o.cost - (fixed + 0.7 * g.mean_replicas)).abs() < 1e-9, "{} vs {fixed} + 0.7 · {}", o.cost, g.mean_replicas);
        // (at 0.67 utilisation of a pod at half speed, a third of the requests take more than
        // twice the 30 ms budget: M/M/1-like queueing, no lag)
        assert!(o.slo < 0.5, "{}", o.slo);
    }
}

/// A load step makes requests miss their budget until new pods are ready; the outcome counts
/// them, the scaling lag and the pods.
#[test]
fn a_step_costs_service_level_and_shows_lag() {
    let steady = run(&model(100.0, "", &[(0.0, 1.0)], 60.0, 900.0), 400);
    let step = run(&model(100.0, "", &[(0.0, 0.3), (300.0, 1.0), (600.0, 0.3)], 60.0, 900.0), 400);
    assert!(step.slo_steps > steady.slo, "{} vs {}", step.slo_steps, steady.slo);
    assert!(step.lag > 15.0 && step.lag < 300.0, "{}", step.lag);
    assert!(step.rescales >= 2 && step.created >= 1, "{step:?}");
    assert!(step.peak_replicas >= 3.0, "{}", step.peak_replicas);
    // a scale-in after the stabilisation window: up, then down
    assert!(step.groups[0].reversals >= 1, "{:?}", step.groups);
    assert!(steady.requests > 50_000);
}

/// An evaluation never grows without bound: past `max_pods` created pods the run stops and is
/// penalised (every pod at the maximum, every request over budget).
#[test]
fn pod_creation_is_capped_and_penalised() {
    let mut steps = Vec::new();
    for i in 0..40 {
        steps.push((60.0 + 60.0 * i as f64, if i % 2 == 0 { 2.0 } else { 0.1 }));
    }
    let m = model(100.0, "scaleDownSeconds = 0.0 startupSeconds = 5.0", &steps, 60.0, 2460.0);
    let o = run(&m, 20);
    assert!(o.capped, "{o:?}");
    assert_eq!(o.slo, 1.0);
    assert!(o.created <= 21, "{}", o.created);
    let dm = dynamic_model(&m);
    assert!((o.cost - (dm.fixed_cost + 0.7 * dm.groups[0].max as f64)).abs() < 1e-9);
}

#[test]
fn outcomes_are_cached_by_content() {
    let m = model(100.0, "", &[(0.0, 1.0)], 60.0, 300.0);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let e = dynamic::engine(&Config { sim_threads: 2, ..Config::default() });
    let p = Arc::new(dynamic::prepare(Arc::new(dynamic_model(&m)), e.max_pods).unwrap());
    let params = HpaParams { seed: 1, exclude: vec![] };
    let before = e.runs.load(std::sync::atomic::Ordering::Relaxed);
    let a = rt.block_on(e.simulate(p.clone(), params.clone())).unwrap();
    // the same content again, also as another preparation of the same model
    let q = Arc::new(dynamic::prepare(Arc::new(dynamic_model(&m)), e.max_pods).unwrap());
    let b = rt.block_on(e.simulate(q, params.clone())).unwrap();
    assert_eq!(e.runs.load(std::sync::atomic::Ordering::Relaxed), before + 1);
    assert_eq!(a, b);
    // another seed runs again
    let c = rt.block_on(e.simulate(p, HpaParams { seed: 2, exclude: vec![] })).unwrap();
    assert_eq!(e.runs.load(std::sync::atomic::Ordering::Relaxed), before + 2);
    assert_ne!(a.requests, c.requests);
    // the functions answer from the outcome
    let mut id = -1;
    let mut next = || {
        id -= 1;
        id
    };
    let args = [WireValue::String("lag".into()), WireValue::Int(1), WireValue::String(String::new())];
    let v = functions::call_dynamic("archHpaStat", &args, &a, &mut next).unwrap();
    assert!(matches!(v.value, WireValue::Double(x) if x == a.lag));
    let v = functions::call_dynamic("archHpaCost", &[WireValue::Int(1), WireValue::String(String::new())], &a, &mut next).unwrap();
    assert!(matches!(v.value, WireValue::Double(x) if x == a.cost));
    assert!(functions::is_dynamic("archHpaSimulate") && !functions::is_dynamic("archCost"));
}
