//! Translation tests (DESIGN.md §10.1): every rule against its expected effect, simulated.
//!
//! The fixture `two-tier.m`: Frontend (5 ms CPU, exponential) calls Backend (10 ms CPU); one
//! open usage scenario. At a light load the mean response time is the sum of the demands.

use palladio_arch::graph::Graph;
use palladio_arch::translate::{self, Translation, node_slowdown};
use palladio_arch::{functions, mfile};
use palladio_sim_service::config::Config;
use palladio_sim_service::engine::{Engine, Outcome, Prepared, RunParams};
use palladio_sim_service::protocol::{WireModel, WireValue};
use palladio_sim_service::schema::schema;
use std::sync::{Arc, OnceLock};

fn engine() -> Arc<Engine> {
    static E: OnceLock<Arc<Engine>> = OnceLock::new();
    E.get_or_init(|| Arc::new(Engine::new(Config { sim_threads: 4, ..Config::default() }))).clone()
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static R: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    R.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap())
}

/// The fixture with `rate` users per second, Backend on `backend`, and `decisions` appended.
fn fixture(rate: f64, backend: &str, decisions: &str) -> WireModel {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/two-tier.m")).unwrap();
    let text = text.replace("RATE", &format!("{rate:?}")).replace("BACKEND_CONTAINER", backend);
    mfile::parse(&format!("{text}\n{decisions}")).unwrap()
}

fn simulate(model: &WireModel, measurements: i64) -> (Arc<Prepared>, Arc<Outcome>) {
    let params = RunParams { seed: 7, max_measurements: measurements, max_sim_time: -1, replications: 1, simulate_linking_resources: false };
    runtime().block_on(async {
        let p = engine().prepare(schema(), Arc::new(model.clone())).await.unwrap();
        let o = engine().simulate(p.clone(), params).await.unwrap();
        (p, o)
    })
}

/// Mean response time of the scenario, in ms.
fn response_ms(model: &WireModel) -> f64 {
    let (p, o) = simulate(model, 20_000);
    1000.0 * functions::stat(&o, &p, "scenario", "Response Time", "mean").unwrap()
}

fn translated(model: &WireModel) -> Translation {
    translate::translate(model).unwrap()
}

fn close(got: f64, want: f64, rel: f64) {
    assert!((got - want).abs() <= rel * want.abs(), "got {got}, want {want} (±{:.1} %)", rel * 100.0);
}

const ROOT: &str = "decisions : ArchitectureDecisions { msPerTimeUnit = 1000.0 }\n";

fn remote(caller: f64, callee: f64, latency: f64) -> String {
    format!(
        "{ROOT}rc : RemoteCalls {{ callerCpuMs = {caller:?} calleeCpuMs = {callee:?} latencyMs = {latency:?} \
         publishCpuMs = 0.0 deliverCpuMs = 0.0 sidecar = false }}\ndecisions.remoteCalls -- rc\n"
    )
}

fn site(mechanism: &str) -> String {
    format!(
        "{ROOT}site : CallSite {{ }}\nsite.call -- Frontend_handle_callQuery\nsite.mechanism -- m\nsite.allowed -- m\n\
         decisions.callSites -- site\ndecisions.mechanisms -- m\n{mechanism}\n"
    )
}

/// The mean demand (units) of every ParametricResourceDemand of the actions with entity name
/// `entity`, from its `Exp(rate)` or constant specification.
fn demands_of(g: &Graph, entity: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for a in g.all("InternalAction") {
        if g.str_attr(&a, "entityName").as_deref() != Some(entity) {
            continue;
        }
        for d in g.refs(&a, "resourceDemand") {
            let spec = g.str_attr(&d, "specification").unwrap();
            let mean = match spec.strip_prefix("Exp(").and_then(|x| x.strip_suffix(')')) {
                Some(r) => 1.0 / r.parse::<f64>().unwrap(),
                None => spec.parse().unwrap(),
            };
            out.push((g.str_attr(&d, "requiredResource").unwrap(), mean));
        }
    }
    out
}

#[test]
fn remote_call_costs_exactly_what_remote_calls_says() {
    let t = translated(&fixture(0.5, "C2", &remote(2.0, 3.0, 4.0)));
    let g = Graph::new(&t.pcm);
    // one stub and one skeleton, with exactly the configured demands (1 unit = 1 ms)
    assert_eq!(demands_of(&g, "marshal"), vec![("CPU".into(), 2.0)]);
    assert_eq!(demands_of(&g, "unmarshal"), vec![("CPU".into(), 3.0)]);
    // the latency is a constant delay in the model's time unit (seconds)
    assert_eq!(demands_of(&g, "latency"), vec![("DELAY".into(), 0.004)]);
    // the offered load moves accordingly: 0.5/s × (5 + 2) ms on C1, 0.5/s × (3 + 10) ms on C2
    let busy = |c: &str| t.loads.resources.iter().find(|r| r.container == c && r.kind == "CPU").unwrap().busy;
    close(busy("C1"), 0.5 * 0.007, 1e-9);
    close(busy("C2"), 0.5 * 0.013, 1e-9);
    // simulated: 5 + 2 + 4 + 3 + 10 = 24 ms at a light load
    close(response_ms(&t.pcm), 24.0, 0.02);
    // co-located, nothing is added
    let local = translated(&fixture(0.5, "C1", &remote(2.0, 3.0, 4.0)));
    assert!(demands_of(&Graph::new(&local.pcm), "marshal").is_empty());
    close(response_ms(&local.pcm), 15.0, 0.02);
}

#[test]
fn cached_call_gives_the_expected_mean() {
    let t = translated(&fixture(0.5, "C2", &site("m : CachedCall { hitRatio = 0.75 hitCpuMs = 1.0 }")));
    // 5 + 0.75 × 1 + 0.25 × 10 = 8.25 ms
    close(response_ms(&t.pcm), 8.25, 0.02);
    let busy = |c: &str| t.loads.resources.iter().find(|r| r.container == c && r.kind == "CPU").unwrap().busy;
    close(busy("C1"), 0.5 * (0.005 + 0.75 * 0.001), 1e-9);
    close(busy("C2"), 0.5 * 0.25 * 0.010, 1e-9);
}

#[test]
fn async_call_runs_in_the_background() {
    let t = translated(&fixture(0.5, "C2", &site("m : AsyncCall { }")));
    // the caller does not wait for Backend: 5 ms
    close(response_ms(&t.pcm), 5.0, 0.02);
    // Backend's work still loads C2
    let busy = t.loads.resources.iter().find(|r| r.container == "C2" && r.kind == "CPU").unwrap().busy;
    close(busy, 0.5 * 0.010, 1e-9);
    let g = Graph::new(&t.pcm);
    assert_eq!(g.all("ForkAction").len(), 1);
}

/// A broker component (1 ms of CPU per message) on its own container C3.
const BROKER: &str = r#"
repo.interfaces -- IBroker
repo.components -- Broker
IBroker : OperationInterface { entityName = "IBroker" }
IBroker.signatures -- deliver
deliver : OperationSignature { entityName = "deliver" }
Broker : BasicComponent { entityName = "Broker" }
Broker.providedRoles -- Broker_provides
Broker.serviceEffectSpecifications -- Broker_deliver
Broker_provides : OperationProvidedRole { }
Broker_provides.providedInterface -- IBroker
Broker_deliver : ResourceDemandingSEFF { }
Broker_deliver.describedService -- deliver
Broker_deliver.steps -- Broker_deliver_start
Broker_deliver.steps -- Broker_deliver_cpu
Broker_deliver.steps -- Broker_deliver_stop
Broker_deliver_start : StartAction { }
Broker_deliver_cpu : InternalAction { entityName = "broker" }
Broker_deliver_cpu.resourceDemand -- Broker_deliver_cpu_demand
Broker_deliver_cpu_demand : ParametricResourceDemand { specification = "Exp(1.0)" requiredResource = ProcessingResourceType.CPU }
Broker_deliver_stop : StopAction { }
Broker_deliver_start.successor -- Broker_deliver_cpu
Broker_deliver_cpu.successor -- Broker_deliver_stop
system.assemblyContexts -- AssemblyBroker
AssemblyBroker : AssemblyContext { entityName = "AssemblyBroker" }
AssemblyBroker.encapsulatedComponent -- Broker
env.resourceContainer -- C3
C3 : ResourceContainer { entityName = "C3" }
C3.activeResourceSpecifications -- C3_cpu
C3_cpu : ProcessingResourceSpecification {
    schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU
    processingRate = "1000" numberOfReplicas = 1
}
lan.connectedResourceContainers -- C3
allocation.allocationContexts -- alloc_Broker
alloc_Broker : AllocationContext { }
alloc_Broker.assemblyContext -- AssemblyBroker
alloc_Broker.resourceContainer -- C3
"#;

#[test]
fn async_remote_call_goes_through_the_broker() {
    let decisions = format!(
        "{}{}{BROKER}rc.broker -- AssemblyBroker\n",
        remote(2.0, 3.0, 4.0).replace("publishCpuMs = 0.0 deliverCpuMs = 0.0", "publishCpuMs = 0.5 deliverCpuMs = 0.7"),
        site("m : AsyncCall { }").replace(ROOT, "")
    );
    let t = translated(&fixture(0.5, "C2", &decisions));
    let g = Graph::new(&t.pcm);
    assert_eq!(demands_of(&g, "publish"), vec![("CPU".into(), 0.5)]);
    assert_eq!(demands_of(&g, "deliver"), vec![("CPU".into(), 0.7)]);
    assert!(demands_of(&g, "marshal").is_empty());
    // the caller waits for the publish only (on its path, as in msa-sim-service); the broker and
    // Backend are loaded by every message
    close(response_ms(&t.pcm), 5.5, 0.02);
    let busy = |c: &str| t.loads.resources.iter().find(|r| r.container == c && r.kind == "CPU").unwrap().busy;
    close(busy("C3"), 0.5 * 0.001, 1e-9);
    close(busy("C2"), 0.5 * (0.0007 + 0.010), 1e-9);
    close(busy("C1"), 0.5 * (0.005 + 0.0005), 1e-9);
}

fn deploy(n: u32, q: f64) -> String {
    format!(
        "{ROOT}d : Deployable {{ cpuPerReplica = {q:?} }}\nd.container -- C2\nd.replicas -- r\nd.allowedReplicas -- r\n\
         r : ReplicaOption {{ count = {n} }}\ndecisions.deployables -- d\ndecisions.replicaOptions -- r\n"
    )
}

#[test]
fn replicas_use_the_limit_speed_rule() {
    let spec = |t: &Translation| {
        let g = Graph::new(&t.pcm);
        (g.num_attr("C2_cpu", "numberOfReplicas").unwrap(), g.str_attr("C2_cpu", "processingRate").unwrap())
    };
    // n replicas of q cores: n·⌈q⌉ cores at q/⌈q⌉ of full speed
    assert_eq!(spec(&translated(&fixture(0.5, "C2", &deploy(2, 0.5)))), (2.0, "500.0".into()));
    assert_eq!(spec(&translated(&fixture(0.5, "C2", &deploy(1, 1.5)))), (2.0, "750.0".into()));
    assert_eq!(spec(&translated(&fixture(0.5, "C2", &deploy(3, 1.0)))), (3.0, "1000.0".into()));
    // a 0.5-core replica serves Backend's 10 ms in 20 ms: 5 + 20 = 25 ms at a light load
    let t = translated(&fixture(0.5, "C2", &deploy(2, 0.5)));
    close(response_ms(&t.pcm), 25.0, 0.02);
    // utilisation: busy cores over cores, at the reduced speed: 0.5/s × 20 ms / 2
    close(t.utilisation("C2", |n| n.to_string()).unwrap(), 0.5 * 0.020 / 2.0, 1e-9);
}

/// msa-sim-service's formula (flow.rs `node_slowdown`), copied as the reference.
fn msa_node_slowdown(c: u32, u: f64) -> f64 {
    fn erlang_c(c: u32, a: f64) -> f64 {
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
    if c == 0 || !(u > 0.0) {
        return 1.0;
    }
    let cf = c as f64;
    if u >= cf {
        return f64::INFINITY;
    }
    1.0 + erlang_c(c, u) / (cf - u)
}

#[test]
fn node_slowdown_matches_msa_sim_service() {
    for c in [1u32, 2, 4, 8, 21] {
        for k in 0..40 {
            let u = c as f64 * k as f64 / 40.0;
            assert_eq!(node_slowdown(c as f64, u), msa_node_slowdown(c, u), "c {c}, u {u}");
        }
    }
    // M/M/1: 1/(1 − u); M/M/2 at u = 1: 1 + (1/3)/1
    close(node_slowdown(1.0, 0.5), 2.0, 1e-12);
    close(node_slowdown(2.0, 1.0), 4.0 / 3.0, 1e-12);

    // In a model: both containers on a 2-core node with 1.2 cores of background load.
    let rate = 20.0;
    let node = format!(
        "{ROOT}n : Node {{ cores = 2 backgroundCores = 1.2 }}\nn.containers -- C1\nn.containers -- C2\ndecisions.nodes -- n\n"
    );
    let t = translated(&fixture(rate, "C2", &node));
    let u = 1.2 + rate * 0.015; // 5 ms + 10 ms of CPU per request
    let s = msa_node_slowdown(2, u);
    close(t.nodes[0].load, u, 1e-9);
    close(t.nodes[0].slowdown, s, 1e-12);
    let g = Graph::new(&t.pcm);
    for c in ["C1_cpu", "C2_cpu"] {
        close(g.str_attr(c, "processingRate").unwrap().parse().unwrap(), 1000.0 / s, 1e-12);
    }
}

/// A model as a canonical, order-free text: instances sorted by name, references sorted.
fn canonical(m: &WireModel) -> String {
    let mut lines: Vec<String> = m
        .instances
        .iter()
        .map(|x| {
            let mut attrs: Vec<String> =
                x.attributes.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v:?}")).collect();
            attrs.sort();
            let mut refs: Vec<String> = x
                .references
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| {
                    let mut v = v.clone();
                    v.sort();
                    format!("{k}->{}", v.join(","))
                })
                .collect();
            refs.sort();
            format!("{} : {} {} {}", x.name, x.class_name, attrs.join(" "), refs.join(" "))
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// Every decision kind at once, on the two-tier fixture with a broker.
fn everything() -> String {
    format!(
        "{}{}{}",
        remote(2.0, 3.0, 4.0).replace("sidecar = false", "sidecar = true"),
        site("m : CachedCall { hitRatio = 0.5 hitCpuMs = 1.0 }").replace(ROOT, ""),
        deploy(2, 0.5).replace(ROOT, "")
            + "n : Node { cores = 4 backgroundCores = 0.5 }\nn.containers -- C1\nn.containers -- C2\ndecisions.nodes -- n\n"
            + "cost : CostModel { perContainer = 10.0 perCore = 5.0 perReplicaOverhead = 1.0 perCache = 2.0 }\ndecisions.costModel -- cost\n"
    )
}

#[test]
fn translation_is_deterministic_and_order_independent() {
    let model = fixture(2.0, "C2", &everything());
    let a = translated(&model);
    let b = translated(&model);
    assert_eq!(a.pcm, b.pcm, "two translations of one model differ");
    assert!(a.pcm.instances.len() > model.instances.len());
    // permute the instances and every reference list
    let mut seed = 12345u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for _ in 0..3 {
        let mut shuffled = model.clone();
        let n = shuffled.instances.len();
        for i in (1..n).rev() {
            let j = next() % (i + 1);
            shuffled.instances.swap(i, j);
        }
        for x in &mut shuffled.instances {
            x.references.reverse();
            for (_, v) in &mut x.references {
                v.reverse();
            }
        }
        let c = translated(&shuffled);
        assert_eq!(canonical(&a.pcm), canonical(&c.pcm), "a permuted model translates differently");
        assert_eq!(a.cost, c.cost);
        assert_eq!(a.modularity, c.modularity);
        assert_eq!(format!("{:?}", a.nodes), format!("{:?}", c.nodes));
    }
}

/// The model without its decisions, as palladio-sim-service simulates it.
fn plain(model: &WireModel) -> WireModel {
    WireModel {
        metamodel_path: model.metamodel_path.clone(),
        instances: model.instances.iter().filter(|x| palladio_arch::graph::is_pcm(&x.class_name)).cloned().collect(),
    }
}

fn identical_to_palladio_sim_service(model: &WireModel) {
    let t = translated(model);
    assert!(!t.translated);
    let (p1, o1) = simulate(&plain(model), 5000);
    let (p2, o2) = simulate(&t.pcm, 5000);
    assert_eq!(p1.digest, p2.digest, "the PCM files differ");
    assert_eq!(o1, o2);
}

#[test]
fn models_without_decisions_simulate_as_in_palladio_sim_service() {
    identical_to_palladio_sim_service(&fixture(5.0, "C2", ""));
    // decisions that change nothing (a placement, a synchronous call site) translate to nothing
    let placement = format!(
        "{ROOT}p : Placement {{ }}\np.allocation -- alloc_Backend\np.candidates -- C1\np.candidates -- C2\ndecisions.placements -- p\n"
    );
    identical_to_palladio_sim_service(&fixture(5.0, "C2", &placement));
    let model = fixture(5.0, "C2", &site("m : SyncCall { }"));
    let t = translated(&model);
    assert_eq!(simulate(&plain(&model), 5000).1, simulate(&t.pcm, 5000).1);
    // the Palladio Simulation project's models
    for m in ["business-trip", "mediastore", "fork-join", "passive-resources", "stoex-distributions", "single-server"] {
        let path = format!("{}/../palladio-simulation/models/{m}.m", env!("CARGO_MANIFEST_DIR"));
        if let Ok(model) = mfile::read(std::path::Path::new(&path)) {
            identical_to_palladio_sim_service(&model);
        }
    }
}

#[test]
fn local_copy_reads_the_copy_and_writers_feed_it() {
    // Backend's query reads a copy kept by a store assembly on C1 (with an HDD); Backend itself
    // is the writer (its CPU action feeds the copy).
    let hdd = "C1.activeResourceSpecifications -- C1_hdd\nC1_hdd : ProcessingResourceSpecification { \
               schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.HDD \
               processingRate = \"1000\" numberOfReplicas = 1 }\n";
    let decisions = format!(
        "{}{hdd}ms : MicroserviceDecisions {{ }}\nms.copyFeeds -- feed\nfeed : CopyFeed {{ }}\nfeed.site -- site\n\
         feed.writers -- Backend_query_cpu\nfeed.store -- AssemblyFrontend\n",
        site("m : LocalCopy { readDbMs = 1.0 writeCpuMs = 0.5 writeDbMs = 2.0 }")
    );
    let t = translated(&fixture(0.5, "C2", &decisions));
    let g = Graph::new(&t.pcm);
    assert_eq!(demands_of(&g, "readCopy"), vec![("HDD".into(), 1.0)]);
    let mut w = demands_of(&g, "writeCopy");
    w.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(w, vec![("CPU".into(), 0.5), ("HDD".into(), 2.0)]);
    // The handle no longer calls Backend: 5 ms + 1 ms read. Nothing calls Backend any more, so
    // its feeding write never runs either.
    close(response_ms(&t.pcm), 6.0, 0.02);
    let hdd_busy = t.loads.resources.iter().find(|r| r.container == "C1" && r.kind == "HDD").unwrap().busy;
    close(hdd_busy, 0.5 * 0.001, 1e-9);
}

#[test]
fn cost_and_modularity() {
    let t = translated(&fixture(2.0, "C2", &everything()));
    // C1: 10 + 5 × 1 core + 1; C2 (2 replicas of 0.5): 10 + 5 × 1.0 + 2 × 1; one cache: 2
    close(t.cost, 16.0 + 17.0 + 2.0, 1e-12);
    // no Placement: nothing is movable, no cluster
    assert_eq!(t.modularity, 0.0);
    let _ = WireValue::Null;
}

/// Placements for both components, Backend's candidates given by `backend_candidates`.
fn placements(backend_candidates: &[&str]) -> String {
    let mut out = String::from(
        "pf : Placement { }\npf.allocation -- alloc_Frontend\npf.candidates -- C1\npf.candidates -- C2\n\
         decisions.placements -- pf\npb : Placement { }\npb.allocation -- alloc_Backend\ndecisions.placements -- pb\n",
    );
    for c in backend_candidates {
        out += &format!("pb.candidates -- {c}\n");
    }
    out
}

#[test]
fn modularity_is_cohesive_mq_over_movable_components() {
    // 2 requests/s: Frontend and Backend each execute 2/s, and Frontend calls Backend 2/s.
    let mq = |backend: &str, mechanism: &str, extra: &str| {
        translated(&fixture(2.0, backend, &format!("{}{}{extra}", site(mechanism), placements(&["C1", "C2"]))))
            .modularity
    };
    let sync = "m : SyncCall { }";
    // apart: each container 2 / (2 + 2/2)
    close(mq("C2", sync, ""), 2.0 * 2.0 / 3.0, 1e-9);
    // together: one cluster, (2 + 2 + 2) / 6
    close(mq("C1", sync, ""), 1.0, 1e-9);
    // an asynchronous call couples by 0.3: 2 / (2 + 0.6/2) each
    close(mq("C2", "m : AsyncCall { }", ""), 2.0 * 2.0 / 2.3, 1e-9);
    // inside one container the mechanism does not matter
    close(mq("C1", "m : AsyncCall { }", ""), 1.0, 1e-9);
    // a cache couples as the call
    close(mq("C2", "m : CachedCall { hitRatio = 0.5 hitCpuMs = 1.0 }", ""), 2.0 * 2.0 / 3.0, 1e-9);
    // a component that can only sit in direct containers (a database server) is infrastructure:
    // Frontend alone, 2 / 2
    let rc = remote(1.0, 1.0, 1.0).replace(ROOT, "") + "rc.direct -- C2\n";
    let t = translated(&fixture(2.0, "C2", &format!("{}{}{rc}", site(sync), placements(&["C2"]))));
    close(t.modularity, 1.0, 1e-9);
    // a local copy (0.5 requests/s): Backend no longer executes for Frontend (own work 0), the
    // read couples by 0.5: Frontend 0.5 / (0.5 + 0.25/2), Backend 0
    let m = copy_model_with("C2", "m : LocalCopy { readDbMs = 1.0 writeCpuMs = 0.5 writeDbMs = 2.0 }", &placements(&["C1", "C2"]));
    close(translated(&m).modularity, 0.5 / 0.625, 1e-9);
}

#[test]
fn direct_containers_get_no_middleware() {
    // Backend's container is reached directly (a database server): no stub, no skeleton
    let t = translated(&fixture(0.5, "C2", &format!("{}rc.direct -- C2\n", remote(2.0, 3.0, 4.0))));
    let g = Graph::new(&t.pcm);
    assert!(demands_of(&g, "marshal").is_empty());
    assert!(demands_of(&g, "unmarshal").is_empty());
    close(response_ms(&t.pcm), 15.0, 0.02);
    // a direct container elsewhere changes nothing for this connector
    let t = translated(&fixture(0.5, "C2", &format!("{}rc.direct -- C1\n", remote(2.0, 3.0, 4.0))));
    assert_eq!(demands_of(&Graph::new(&t.pcm), "marshal"), vec![("CPU".into(), 2.0)]);
}

#[test]
fn sidecar_cores_per_replica() {
    let decisions = format!(
        "{}{}",
        remote(2.0, 3.0, 4.0).replace("sidecar = false", "sidecar = true"),
        deploy(2, 0.5).replace(ROOT, "").replace("cpuPerReplica = 0.5", "cpuPerReplica = 0.5 sidecarCores = 3")
    );
    let t = translated(&fixture(0.5, "C2", &decisions));
    let g = Graph::new(&t.pcm);
    // C2: 2 replicas with 3 sidecar cores each; C1 (no Deployable): one replica, one core
    assert_eq!(g.num_attr("C2_sidecar_cpu", "numberOfReplicas"), Some(6.0));
    assert_eq!(g.num_attr("C1_sidecar_cpu", "numberOfReplicas"), Some(1.0));
    assert_eq!(g.str_attr("C2_sidecar_cpu", "processingRate").as_deref(), Some("1000.0"));
}

#[test]
fn container_prices_replace_the_cost_rule() {
    let priced = everything() + "cost.prices -- price\nprice : ContainerPrice { price = 7.0 }\nprice.containers -- C2\n";
    let t = translated(&fixture(2.0, "C2", &priced));
    // C1 by the rule: 10 + 5 × 1 core + 1; C2 priced 7; one cache: 2
    close(t.cost, 16.0 + 7.0 + 2.0, 1e-12);
    // a priced container that hosts nothing costs nothing
    let t = translated(&fixture(2.0, "C1", &priced));
    close(t.cost, 16.0 + 2.0, 1e-12);
}

fn copy_model(backend: &str, mechanism: &str) -> WireModel {
    copy_model_with(backend, mechanism, "")
}

fn copy_model_with(backend: &str, mechanism: &str, extra: &str) -> WireModel {
    let hdd = "C1.activeResourceSpecifications -- C1_hdd\nC1_hdd : ProcessingResourceSpecification { \
               schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.HDD \
               processingRate = \"1000\" numberOfReplicas = 1 }\n";
    let decisions = format!(
        "{}{hdd}ms : MicroserviceDecisions {{ }}\nms.copyFeeds -- feed\nfeed : CopyFeed {{ }}\nfeed.site -- site\n\
         feed.writers -- Backend_query_cpu\nfeed.store -- AssemblyFrontend\n",
        site(mechanism)
    );
    fixture(0.5, backend, &format!("{decisions}{extra}"))
}

#[test]
fn local_copy_read_cpu_runs_in_the_store() {
    let t = translated(&copy_model("C2", "m : LocalCopy { readDbMs = 1.0 writeCpuMs = 0.5 writeDbMs = 2.0 readCpuMs = 2.0 }"));
    let g = Graph::new(&t.pcm);
    let mut r = demands_of(&g, "readCopy");
    r.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(r, vec![("CPU".into(), 2.0), ("HDD".into(), 1.0)]);
    // 5 ms + 2 ms CPU + 1 ms read
    close(response_ms(&t.pcm), 8.0, 0.02);
    let busy = |kind: &str| t.loads.resources.iter().find(|r| r.container == "C1" && r.kind == kind).unwrap().busy;
    close(busy("CPU"), 0.5 * 0.007, 1e-9);
    close(busy("HDD"), 0.5 * 0.001, 1e-9);
}

#[test]
fn a_writer_in_the_stores_container_feeds_nothing() {
    let m = "m : LocalCopy { readDbMs = 1.0 writeCpuMs = 0.5 writeDbMs = 2.0 }";
    let feeds = |t: &Translation| Graph::new(&t.pcm).all("ExternalCallAction").into_iter().filter(|a| a.contains("_feed_")).count();
    // Backend (the writer) on C2, the store on C1: one write message
    assert_eq!(feeds(&translated(&copy_model("C2", m))), 1);
    // both on C1: the copy is the writer's own data
    let t = translated(&copy_model("C1", m));
    assert_eq!(feeds(&t), 0);
    assert!(Graph::new(&t.pcm).all("ForkAction").is_empty());
}

// ---------------------------------------------------------------------------------------------
// The chain fixture (P6): composed calls, bypasses, workflows, read stores, quota, publishing

/// `chain.m` with `rate` requests/s, Backend (and Other) on `backend`, Notifier on `notifier`,
/// DbRead on `read`, and `decisions` appended.
fn chain(rate: f64, backend: &str, notifier: &str, read: &str, decisions: &str) -> WireModel {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/chain.m")).unwrap();
    let text = text
        .replace("RATE", &format!("{rate:?}"))
        .replace("BACKEND_C", backend)
        .replace("NOTIFIER_C", notifier)
        .replace("READ_C", read);
    mfile::parse(&format!("{text}\n{decisions}")).unwrap()
}

/// RemoteCalls: caller 2, callee 3, latency 4 ms, publish 0.5, deliver 0.7; C3 and C6 (database
/// servers) are reached directly.
fn chain_remote() -> String {
    format!(
        "{}rc.direct -- C3\nrc.direct -- C6\n",
        remote(2.0, 3.0, 4.0).replace("publishCpuMs = 0.0 deliverCpuMs = 0.0", "publishCpuMs = 0.5 deliverCpuMs = 0.7")
    )
}

/// A broker on C5 (1 ms per message) for the chain.
const CHAIN_BROKER: &str = r#"
repo.interfaces -- IBroker
repo.components -- Broker
IBroker : OperationInterface { entityName = "IBroker" }
IBroker.signatures -- deliver
deliver : OperationSignature { entityName = "deliver" }
Broker : BasicComponent { entityName = "Broker" }
Broker.providedRoles -- Broker_provides
Broker.serviceEffectSpecifications -- Broker_deliver
Broker_provides : OperationProvidedRole { }
Broker_provides.providedInterface -- IBroker
Broker_deliver : ResourceDemandingSEFF { }
Broker_deliver.describedService -- deliver
Broker_deliver.steps -- Broker_deliver_start
Broker_deliver.steps -- Broker_deliver_cpu
Broker_deliver.steps -- Broker_deliver_stop
Broker_deliver_start : StartAction { }
Broker_deliver_cpu : InternalAction { entityName = "broker" }
Broker_deliver_cpu.resourceDemand -- Broker_deliver_cpu_demand
Broker_deliver_cpu_demand : ParametricResourceDemand { specification = "Exp(1.0)" requiredResource = ProcessingResourceType.CPU }
Broker_deliver_stop : StopAction { }
Broker_deliver_start.successor -- Broker_deliver_cpu
Broker_deliver_cpu.successor -- Broker_deliver_stop
system.assemblyContexts -- AssemblyBroker
AssemblyBroker : AssemblyContext { entityName = "AssemblyBroker" }
AssemblyBroker.encapsulatedComponent -- Broker
allocation.allocationContexts -- alloc_Broker
alloc_Broker : AllocationContext { }
alloc_Broker.assemblyContext -- AssemblyBroker
alloc_Broker.resourceContainer -- C5
rc.broker -- AssemblyBroker
"#;

/// A call site on `call` with `mechanism` (named m).
fn chain_site(call: &str, mechanism: &str) -> String {
    format!(
        "site : CallSite {{ }}\nsite.call -- {call}\nsite.mechanism -- m\nsite.allowed -- m\n\
         decisions.callSites -- site\ndecisions.mechanisms -- m\n{mechanism}\n"
    )
}

/// The analytic no-load response time in ms.
fn no_load_ms(t: &Translation) -> f64 {
    1000.0 * t.no_load_time(&[], |x| x.to_string())
}

/// Busy CPU of a container, in ms per request at `rate` requests/s.
fn cpu_ms(t: &Translation, container: &str, rate: f64) -> f64 {
    t.loads.resources.iter().filter(|r| r.container == container && r.kind == "CPU").map(|r| r.busy).sum::<f64>() / rate * 1000.0
}

#[test]
fn chain_baseline() {
    // all synchronous and apart: 1 + 9 + 5 + (9 + 10 + 2 + 7) + (9 + 3 + 2 + 9 + 4) = 70 ms
    let t = translated(&chain(0.5, "C2", "C4", "C3", &chain_remote()));
    close(no_load_ms(&t), 70.0, 1e-9);
    close(response_ms(&t.pcm), 70.0, 0.02);
}

#[test]
fn composed_call_moves_to_the_composer() {
    let composed = |backend: &str, m: &str| {
        let d = format!(
            "{}{}site.composer -- Gateway_request_callHandle\n",
            chain_remote(),
            chain_site("Frontend_handle_callQuery", m)
        );
        translated(&chain(0.5, backend, "C4", "C3", &d))
    };
    let t = composed("C2", "m : ComposedCall { composeCpuMs = 1.5 }");
    // the gateway makes the query after Frontend answered: 1 + 9 + 5 + 27 (update), then 1.5 +
    // 9 + 19 (query) = 71.5 ms
    close(no_load_ms(&t), 71.5, 1e-9);
    close(response_ms(&t.pcm), 71.5, 0.02);
    // C0: 1 + 2 + 1.5 + 2 (the gateway's two stubs); C1 lost the query's caller CPU: 3 + 5 + 2
    close(cpu_ms(&t, "C0", 0.5), 6.5, 1e-9);
    close(cpu_ms(&t, "C1", 0.5), 10.0, 1e-9);
    close(cpu_ms(&t, "C2", 0.5), 28.0, 1e-9);
    // with the gateway's cache: hit 0.75 × 1 ms, miss 0.25 × (9 + 19)
    let t = composed("C2", "m : ComposedCall { composeCpuMs = 1.5 hitRatio = 0.75 hitCpuMs = 1.0 }");
    close(no_load_ms(&t), 42.0 + 1.5 + 0.75 + 0.25 * 28.0, 1e-9);
    close(cpu_ms(&t, "C0", 0.5), 1.0 + 2.0 + 1.5 + 0.75 + 0.25 * 2.0, 1e-9);
    // in one container the caller calls itself: 1 + 9 + 5 + 19 + (3 + 2 + 9 + 4) = 52, no compose
    let t = composed("C1", "m : ComposedCall { composeCpuMs = 1.5 }");
    close(no_load_ms(&t), 52.0, 1e-9);
    assert!(demands_of(&Graph::new(&t.pcm), "compose").is_empty());
    // a composed call couples by 0.5: Frontend 0.5 / (0.5 + 0.75/2), Backend 1 / (1 + 0.75/2)
    let d = format!(
        "{}{}site.composer -- Gateway_request_callHandle\n\
         pf : Placement {{ }}\npf.allocation -- alloc_Frontend\npf.candidates -- C1\npf.candidates -- C2\ndecisions.placements -- pf\n\
         pb : Placement {{ }}\npb.allocation -- alloc_Backend\npb.candidates -- C1\npb.candidates -- C2\ndecisions.placements -- pb\n",
        chain_remote(),
        chain_site("Frontend_handle_callQuery", "m : ComposedCall { composeCpuMs = 1.5 }")
    );
    let t = translated(&chain(0.5, "C2", "C4", "C3", &d));
    close(t.modularity, 0.5 / 0.875 + 1.0 / 1.375, 1e-9);
}

#[test]
fn bypass_does_the_callees_work_and_reaches_its_database() {
    let bypass = |backend: &str| {
        let d = format!("{}{}", chain_remote(), chain_site("Frontend_handle_callQuery", "m : Bypass { latencyMs = 4.0 }"));
        translated(&chain(0.5, backend, "C4", "C3", &d))
    };
    let t = bypass("C2");
    // Frontend does Backend's 10 ms, waits 4 ms and reads DbRead (2 ms) itself; Other (7 ms) is
    // skipped: 1 + 9 + 5 + 16 + 27 = 58 ms
    close(no_load_ms(&t), 58.0, 1e-9);
    close(response_ms(&t.pcm), 58.0, 0.02);
    close(cpu_ms(&t, "C1", 0.5), 3.0 + 5.0 + 10.0 + 2.0, 1e-9);
    close(cpu_ms(&t, "C2", 0.5), 3.0 + 3.0 + 2.0, 1e-9);
    close(cpu_ms(&t, "C3", 0.5), 4.0, 1e-9);
    // in one container it is a plain call: 1 + 9 + 5 + 19 + 18 = 52
    close(no_load_ms(&bypass("C1")), 52.0, 1e-9);
    // modularity: Backend no longer executes the query; the bypass couples by 2:
    // both 0.5 / (0.5 + 1.5/2)
    let d = format!(
        "{}{}pf : Placement {{ }}\npf.allocation -- alloc_Frontend\npf.candidates -- C1\npf.candidates -- C2\ndecisions.placements -- pf\n\
         pb : Placement {{ }}\npb.allocation -- alloc_Backend\npb.candidates -- C1\npb.candidates -- C2\ndecisions.placements -- pb\n",
        chain_remote(),
        chain_site("Frontend_handle_callQuery", "m : Bypass { latencyMs = 4.0 }")
    );
    close(translated(&chain(0.5, "C2", "C4", "C3", &d)).modularity, 0.8, 1e-9);
}

fn workflow(style: &str, asynchronous: bool) -> String {
    let mut d = format!(
        "{}wf : Workflow {{ }}\nwf.trigger -- Frontend_handle\nwf.steps -- Frontend_handle_callUpdate\n\
         wf.steps -- Backend_update_callNotify\nwf.style -- {style}\nwf.allowed -- choreo\nwf.allowed -- orch\n\
         decisions.workflows -- wf\nchoreo : Choreographed {{ }}\norch : Orchestrated {{ stepCpuMs = 0.5 }}\n\
         decisions.coordinations -- choreo\ndecisions.coordinations -- orch\n",
        chain_remote()
    );
    if asynchronous {
        d += CHAIN_BROKER;
        d += "a : AsyncCall { }\ndecisions.mechanisms -- a\n";
        for (s, call) in [("s1", "Frontend_handle_callUpdate"), ("s2", "Backend_update_callNotify")] {
            d += &format!("{s} : CallSite {{ }}\n{s}.call -- {call}\n{s}.mechanism -- a\n{s}.allowed -- a\ndecisions.callSites -- {s}\n");
        }
    }
    d
}

#[test]
fn orchestration_moves_the_steps_to_the_trigger() {
    // choreography as written: 70 ms
    close(no_load_ms(&translated(&chain(0.5, "C2", "C4", "C3", &workflow("choreo", false)))), 70.0, 1e-9);
    // orchestrated, synchronous steps: Frontend coordinates both: 1 + 9 + 5 + 28 + (0.5 + 9 + 3 +
    // 2) + (0.5 + 9 + 4) = 71 ms
    let t = translated(&chain(0.5, "C2", "C4", "C3", &workflow("orch", false)));
    close(no_load_ms(&t), 71.0, 1e-9);
    close(response_ms(&t.pcm), 71.0, 0.02);
    close(cpu_ms(&t, "C1", 0.5), 3.0 + 5.0 + 2.0 + 0.5 + 2.0 + 0.5 + 2.0, 1e-9);
    close(cpu_ms(&t, "C2", 0.5), 3.0 + 10.0 + 7.0 + 3.0 + 3.0, 1e-9);
    // asynchronous steps, choreographed: the request waits for the first publish only
    let t = translated(&chain(0.5, "C2", "C4", "C3", &workflow("choreo", true)));
    close(no_load_ms(&t), 43.5, 1e-9);
    close(cpu_ms(&t, "C5", 0.5), 2.0, 1e-9);
    // orchestrated: all in the background (the first step is asynchronous and remote); each step
    // is a request and a reply: 4 messages, publish and deliver on both sides
    let t = translated(&chain(0.5, "C2", "C4", "C3", &workflow("orch", true)));
    close(no_load_ms(&t), 43.0, 1e-9);
    close(response_ms(&t.pcm), 43.0, 0.02);
    close(cpu_ms(&t, "C5", 0.5), 4.0, 1e-9);
    close(cpu_ms(&t, "C1", 0.5), 3.0 + 5.0 + 2.0 + 2.0 * (0.5 + 0.5 + 0.7), 1e-9);
    close(cpu_ms(&t, "C2", 0.5), 3.0 + 10.0 + 7.0 + 0.7 + 3.0 + 0.5, 1e-9);
    close(cpu_ms(&t, "C4", 0.5), 0.7 + 4.0 + 0.5, 1e-9);
    // Notifier with the orchestrator: that step runs inline
    let t = translated(&chain(0.5, "C2", "C1", "C3", &workflow("orch", false)));
    close(no_load_ms(&t), 71.0 - 9.0, 1e-9);
}

fn projection() -> String {
    format!(
        "{ROOT}ms : MicroserviceDecisions {{ }}\nms.projections -- pr\npr : Projection {{ cpuMs = 0.5 }}\n\
         pr.reads -- Assembly_DbRead\npr.source -- Assembly_Db\npr.writers -- Backend_update_callWrite\npr.apply -- IDbRead_apply\n"
    )
}

#[test]
fn read_store_is_fed_by_projection() {
    // read side with the write side: nothing changes (34 ms, all local)
    let t = translated(&chain(0.5, "C2", "C2", "C3", &projection()));
    close(no_load_ms(&t), 34.0, 1e-9);
    assert!(demands_of(&Graph::new(&t.pcm), "project").is_empty());
    // a read store of its own (C6): the write projects 0.5 ms on Backend, then applies 1 ms there
    let t = translated(&chain(0.5, "C2", "C2", "C6", &projection()));
    close(no_load_ms(&t), 34.5, 1e-9);
    close(response_ms(&t.pcm), 34.5, 0.02);
    close(cpu_ms(&t, "C2", 0.5), 10.0 + 7.0 + 3.0 + 0.5 + 4.0, 1e-9);
    close(cpu_ms(&t, "C6", 0.5), 2.0 + 1.0, 1e-9);
    close(cpu_ms(&t, "C3", 0.5), 2.0, 1e-9);
}

#[test]
fn quota_runs_at_full_speed_when_idle() {
    let quota = |n: u32, q: f64| {
        format!(
            "{ROOT}d : Deployable {{ cpuPerReplica = {q:?} quota = true }}\nd.container -- C2\nd.replicas -- r\nd.allowedReplicas -- r\n\
             r : ReplicaOption {{ count = {n} }}\ndecisions.deployables -- d\ndecisions.replicaOptions -- r\n"
        )
    };
    // 10 requests/s, Backend and Other on C2: 20 ms each, u = 0.2 cores
    let t = translated(&chain(10.0, "C2", "C4", "C3", &quota(1, 0.5)));
    let g = Graph::new(&t.pcm);
    // speed max(0.5, 1 − 0.2 · 0.5/0.5) = 0.8; utilisation on the capacity 0.5: 0.4
    assert_eq!(g.str_attr("C2_cpu", "processingRate").as_deref(), Some("800.0"));
    close(t.utilisation("C2", |n| n.to_string()).unwrap(), 0.4, 1e-9);
    // the no-load time is at full speed (34 ms), as msa's
    close(no_load_ms(&t), 34.0, 1e-9);
    // a limit of 2.4 cores: 3 cores at max(0.8, 1 − 0.2/3 · 0.25)
    let t = translated(&chain(10.0, "C2", "C4", "C3", &quota(1, 2.4)));
    let g = Graph::new(&t.pcm);
    assert_eq!(g.num_attr("C2_cpu", "numberOfReplicas"), Some(3.0));
    close(g.str_attr("C2_cpu", "processingRate").unwrap().parse().unwrap(), 1000.0 * (1.0 - 0.2 / 3.0 * 0.25), 1e-12);
    close(t.utilisation("C2", |n| n.to_string()).unwrap(), 0.2 / 2.4, 1e-9);
    // limit-speed for comparison: Backend's 20 ms at half speed: 34 + 20
    let t = translated(&chain(10.0, "C2", "C4", "C3", &quota(1, 0.5).replace(" quota = true", "")));
    close(no_load_ms(&t), 54.0, 1e-9);
}

#[test]
fn a_write_publishes_once_for_all_its_copies() {
    // Frontend's CPU action writes; Backend (C2) and Gateway (C0) keep copies of it
    let hdd = |c: &str| {
        format!(
            "{c}.activeResourceSpecifications -- {c}_hdd\n{c}_hdd : ProcessingResourceSpecification {{ \
             schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.HDD \
             processingRate = \"1000\" numberOfReplicas = 1 }}\n"
        )
    };
    let mut d = chain_remote() + CHAIN_BROKER + &hdd("C0") + &hdd("C2");
    d += "copy : LocalCopy { readDbMs = 0.3 writeCpuMs = 1.0 writeDbMs = 0.9 }\ndecisions.mechanisms -- copy\n\
          ms : MicroserviceDecisions { }\n";
    for (s, call, store) in [("sa", "Backend_query_callOther", "Assembly_Backend"), ("sb", "Backend_update_callNotify", "Assembly_Gateway")] {
        d += &format!(
            "{s} : CallSite {{ }}\n{s}.call -- {call}\n{s}.mechanism -- copy\n{s}.allowed -- copy\ndecisions.callSites -- {s}\n\
             ms.copyFeeds -- feed_{s}\nfeed_{s} : CopyFeed {{ }}\nfeed_{s}.site -- {s}\nfeed_{s}.writers -- Frontend_handle_cpu\n\
             feed_{s}.store -- {store}\n"
        );
    }
    let t = translated(&chain(0.5, "C2", "C4", "C3", &d));
    let g = Graph::new(&t.pcm);
    // one publish (the publisher's), two messages through the broker
    assert_eq!(demands_of(&g, "publish"), vec![("CPU".into(), 0.5)]);
    close(cpu_ms(&t, "C5", 0.5), 2.0, 1e-9);
    // Frontend's container: 3 + 5 + 2 + 2 (its stubs) + 0.5 (one publish)
    close(cpu_ms(&t, "C1", 0.5), 3.0 + 5.0 + 2.0 + 2.0 + 0.5, 1e-9);
    // each copy: deliver 0.7 + write 1.0 on the holder's CPU (Gateway's container also serves
    // Backend's read of the copy it keeps: 3 ms callee CPU)
    close(cpu_ms(&t, "C0", 0.5), 1.0 + 2.0 + 3.0 + 0.7 + 1.0, 1e-9);
}
