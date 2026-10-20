//! `pcmSourceFailure`: failure sources (common causes) that each take down a set of containers.
//! Hand-built systems where the values are known analytically: a dedicated database server, a
//! shared one, a request that reaches a server only through another tenant's schema, branches;
//! one source per container reproduces `pcmFailure` exactly.

use palladio_arch::pcm::PcmCache;
use palladio_arch::{mfile, translate};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

/// A component: name, container, the components its one operation calls (in order).
struct Comp<'a>(&'a str, &'a str, &'a [&'a str]);

/// A scenario: name, arrival rate, and its behaviour: a list of (probability, entry component);
/// one entry with probability 1 is a plain call, several form a probabilistic branch.
struct Scen<'a>(&'a str, f64, &'a [(f64, &'a str)]);

fn model(comps: &[Comp], scens: &[Scen]) -> String {
    let mut m = String::from("repo : Repository { entityName = \"repo\" }\n");
    let mut containers: Vec<&str> = Vec::new();
    for Comp(n, c, callees) in comps {
        if !containers.contains(c) {
            containers.push(c);
        }
        writeln!(m, "repo.interfaces -- I{n}\nI{n} : OperationInterface {{ entityName = \"I{n}\" }}\nI{n}.signatures -- op_{n}\nop_{n} : OperationSignature {{ entityName = \"op\" }}").unwrap();
        writeln!(m, "repo.components -- {n}\n{n} : BasicComponent {{ entityName = \"{n}\" }}\n{n}.providedRoles -- {n}_p\n{n}_p : OperationProvidedRole {{ }}\n{n}_p.providedInterface -- I{n}").unwrap();
        writeln!(m, "{n}.serviceEffectSpecifications -- {n}_seff\n{n}_seff : ResourceDemandingSEFF {{ }}\n{n}_seff.describedService -- op_{n}").unwrap();
        writeln!(m, "{n}_seff.steps -- {n}_start\n{n}_start : StartAction {{ }}\n{n}_seff.steps -- {n}_stop\n{n}_stop : StopAction {{ }}").unwrap();
        let mut prev = format!("{n}_start");
        for (i, x) in callees.iter().enumerate() {
            writeln!(m, "{n}.requiredRoles -- {n}_r{i}\n{n}_r{i} : OperationRequiredRole {{ }}\n{n}_r{i}.requiredInterface -- I{x}").unwrap();
            writeln!(m, "{n}_seff.steps -- {n}_call{i}\n{n}_call{i} : ExternalCallAction {{ }}\n{n}_call{i}.role -- {n}_r{i}\n{n}_call{i}.calledService -- op_{x}").unwrap();
            writeln!(m, "{prev}.successor -- {n}_call{i}").unwrap();
            prev = format!("{n}_call{i}");
        }
        writeln!(m, "{prev}.successor -- {n}_stop").unwrap();
    }
    m.push_str("system : System { entityName = \"system\" }\n");
    for Comp(n, _, callees) in comps {
        writeln!(m, "system.assemblyContexts -- A{n}\nA{n} : AssemblyContext {{ entityName = \"A{n}\" }}\nA{n}.encapsulatedComponent -- {n}").unwrap();
        writeln!(m, "system.providedRoles -- sys_{n}\nsys_{n} : OperationProvidedRole {{ }}\nsys_{n}.providedInterface -- I{n}").unwrap();
        writeln!(m, "system.connectors -- d_{n}\nd_{n} : ProvidedDelegationConnector {{ }}\nd_{n}.assemblyContext -- A{n}\nd_{n}.innerProvidedRole -- {n}_p\nd_{n}.outerProvidedRole -- sys_{n}").unwrap();
        for (i, x) in callees.iter().enumerate() {
            writeln!(m, "system.connectors -- k_{n}_{i}\nk_{n}_{i} : AssemblyConnector {{ }}\nk_{n}_{i}.requiringAssemblyContext -- A{n}\nk_{n}_{i}.requiredRole -- {n}_r{i}\nk_{n}_{i}.providingAssemblyContext -- A{x}\nk_{n}_{i}.providedRole -- {x}_p").unwrap();
        }
    }
    m.push_str("env : ResourceEnvironment { }\n");
    for c in &containers {
        writeln!(m, "env.resourceContainer -- {c}\n{c} : ResourceContainer {{ entityName = \"{c}\" }}\n{c}.activeResourceSpecifications -- {c}_cpu").unwrap();
        writeln!(m, "{c}_cpu : ProcessingResourceSpecification {{\n schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING\n activeResourceType = ProcessingResourceType.CPU\n processingRate = \"1000\"\n numberOfReplicas = 1\n}}").unwrap();
    }
    m.push_str("allocation : Allocation { entityName = \"allocation\" }\nallocation.system -- system\nallocation.targetResourceEnvironment -- env\n");
    for Comp(n, c, _) in comps {
        writeln!(m, "allocation.allocationContexts -- al_{n}\nal_{n} : AllocationContext {{ }}\nal_{n}.assemblyContext -- A{n}\nal_{n}.resourceContainer -- {c}").unwrap();
    }
    m.push_str("usage : UsageModel { }\n");
    for Scen(s, rate, calls) in scens {
        writeln!(m, "usage.usageScenario -- {s}\n{s} : UsageScenario {{ entityName = \"{s}\" }}\n{s}.scenarioBehaviour -- {s}_b\n{s}.workload -- {s}_wl").unwrap();
        writeln!(m, "{s}_wl : OpenWorkload {{ interArrivalTime = \"Exp({rate})\" }}\n{s}_b : ScenarioBehaviour {{ }}").unwrap();
        writeln!(m, "{s}_b.actions -- {s}_start\n{s}_start : Start {{ }}\n{s}_b.actions -- {s}_stop\n{s}_stop : Stop {{ }}").unwrap();
        let call = |m: &mut String, beh: &str, id: &str, n: &str| {
            writeln!(m, "{beh}.actions -- {id}\n{id} : EntryLevelSystemCall {{ }}\n{id}.providedRole -- sys_{n}\n{id}.operationSignature -- op_{n}").unwrap();
        };
        if let [(_, n)] = calls {
            call(&mut m, &format!("{s}_b"), &format!("{s}_call"), n);
            writeln!(m, "{s}_start.successor -- {s}_call\n{s}_call.successor -- {s}_stop").unwrap();
        } else {
            writeln!(m, "{s}_b.actions -- {s}_br\n{s}_br : Branch {{ }}\n{s}_start.successor -- {s}_br\n{s}_br.successor -- {s}_stop").unwrap();
            for (i, (p, n)) in calls.iter().enumerate() {
                let b = format!("{s}_t{i}b");
                writeln!(m, "{s}_br.branchTransitions -- {s}_t{i}\n{s}_t{i} : BranchTransition {{ branchProbability = {p} }}\n{s}_t{i}.branchedBehaviour -- {b}\n{b} : ScenarioBehaviour {{ }}").unwrap();
                writeln!(m, "{b}.actions -- {b}_start\n{b}_start : Start {{ }}\n{b}.actions -- {b}_stop\n{b}_stop : Stop {{ }}").unwrap();
                call(&mut m, &b, &format!("{b}_call"), n);
                writeln!(m, "{b}_start.successor -- {b}_call\n{b}_call.successor -- {b}_stop").unwrap();
            }
        }
    }
    m
}

fn cache(comps: &[Comp], scens: &[Scen]) -> PcmCache {
    PcmCache::new(&mfile::parse(&model(comps, scens)).unwrap())
}

fn sets(x: &[&[&str]]) -> Vec<Vec<String>> {
    x.iter().map(|s| s.iter().map(|c| c.to_string()).collect()).collect()
}

fn none() -> HashSet<String> {
    HashSet::new()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-14 * a.abs().max(b.abs()).max(1e-300)
}

const UA: f64 = 2e-4; // an app container's own unavailability
const UD: f64 = 1e-4; // a database server's own unavailability
const UR: f64 = 1.1415525114155251e-5; // a release: 1 change × 0.1 × 1 h / 8760 h

#[test]
fn dedicated_database_counts_the_release_once() {
    // A on CA writes its schema SA on its own server DA
    let c = cache(&[Comp("A", "CA", &["SA"]), Comp("SA", "DA", &[])], &[Scen("o", 1.0, &[(1.0, "A")])]);
    // old (pcmFailure, tenant share 1): the release counted on both containers
    let old: HashMap<String, f64> = [("CA".to_string(), UA + UR), ("DA".to_string(), UD + UR)].into();
    let (f_old, _) = c.failure(&old, &none(), &[]);
    assert!(close(f_old, 1.0 - (1.0 - UA - UR) * (1.0 - UD - UR)));
    // new: CA alone, DA alone, the release over {CA, DA}
    let (f, _) = c.source_failure(&sets(&[&["CA"], &["DA"], &["CA", "DA"]]), &[UA, UD, UR], &none(), &[]);
    assert!(close(f, 1.0 - (1.0 - UA) * (1.0 - UD) * (1.0 - UR)), "{f}");
    // the difference is the second count of the release (to first order)
    assert!((f_old - f - UR).abs() < 1e-3 * UR, "{}", f_old - f);
}

#[test]
fn shared_server_fails_with_every_tenants_release() {
    // A and B with their schemas on one server D; o calls A, p calls B, q reaches D only through
    // A's schema (C on CC reads SA)
    let comps = [Comp("A", "CA", &["SA"]), Comp("B", "CB", &["SB"]), Comp("C", "CC", &["SA"]), Comp("SA", "D", &[]), Comp("SB", "D", &[])];
    let c = cache(&comps, &[Scen("o", 1.0, &[(1.0, "A")]), Scen("p", 3.0, &[(1.0, "B")]), Scen("q", 6.0, &[(1.0, "C")])]);
    let (ra, rb) = (UR, 2.0 * UR);
    let s = sets(&[&["CA"], &["CB"], &["CC"], &["D"], &["CA", "D"], &["CB", "D"]]);
    let u = [UA, UA, UA, UD, ra, rb];
    // every request touches D, so both releases take each one down
    let fo = 1.0 - (1.0 - UA) * (1.0 - UD) * (1.0 - ra) * (1.0 - rb);
    let fp = fo;
    let fq = 1.0 - (1.0 - UA) * (1.0 - UD) * (1.0 - ra) * (1.0 - rb);
    for (ex, want) in [("p,q", fo), ("o,q", fp), ("o,p", fq)] {
        let exclude: Vec<String> = ex.split(',').map(String::from).collect();
        let (f, _) = c.source_failure(&s, &u, &none(), &exclude);
        assert!(close(f, want), "{ex}: {f} vs {want}");
    }
    let (f, _) = c.source_failure(&s, &u, &none(), &[]);
    assert!(close(f, (fo + 3.0 * fp + 6.0 * fq) / 10.0), "{f}");
    // the same with dedicated servers DA, DB: o no longer sees B's release, q only A's
    let comps = [Comp("A", "CA", &["SA"]), Comp("B", "CB", &["SB"]), Comp("C", "CC", &["SA"]), Comp("SA", "DA", &[]), Comp("SB", "DB", &[])];
    let c = cache(&comps, &[Scen("o", 1.0, &[(1.0, "A")]), Scen("p", 3.0, &[(1.0, "B")]), Scen("q", 6.0, &[(1.0, "C")])]);
    let s = sets(&[&["CA"], &["CB"], &["CC"], &["DA"], &["DB"], &["CA", "DA"], &["CB", "DB"]]);
    let u = [UA, UA, UA, UD, UD, ra, rb];
    let (f, _) = c.source_failure(&s, &u, &none(), &["p".into(), "q".into()]);
    assert!(close(f, 1.0 - (1.0 - UA) * (1.0 - UD) * (1.0 - ra)), "{f}");
    let (f, _) = c.source_failure(&s, &u, &none(), &["o".into(), "p".into()]);
    assert!(close(f, 1.0 - (1.0 - UA) * (1.0 - UD) * (1.0 - ra)), "{f}");
}

#[test]
fn branches_and_chains_mark_a_set_once() {
    // o: 0.3 → A (→ SA on D), 0.7 → B (→ SB on D)
    let comps = [Comp("A", "CA", &["SA"]), Comp("B", "CB", &["SB"]), Comp("SA", "D", &[]), Comp("SB", "D", &[])];
    let c = cache(&comps, &[Scen("o", 1.0, &[(0.3, "A"), (0.7, "B")])]);
    // the walk's per-set probabilities: separate calls combine as independent marks
    // (pcmFailure's rule), a call from inside the set does not mark it again
    let s = sets(&[&["CA"], &["D"], &["CA", "D"], &["CA", "CB"], &["CB", "D"]]);
    let p = [0.3, 1.0 - 0.7 * 0.3, 1.0 - 0.7 * 0.3, 1.0 - 0.7 * 0.3, 1.0 - 0.7 * 0.3];
    // with u = 1 for one source at a time the failure is that source's probability
    for i in 0..s.len() {
        let mut u = vec![0.0; s.len()];
        u[i] = 1.0;
        let (f, _) = c.source_failure(&s, &u, &none(), &[]);
        assert!(close(f, p[i]), "set {:?}: {f} vs {}", s[i], p[i]);
    }
    // a set's probability is neither the max nor the sum of its members' ({CA, CB}: 0.3, 0.7 → 0.79)
    let (f, _) = c.source_failure(&s[3..4], &[UR], &none(), &[]);
    assert!((f - p[3] * UR).abs() < 1e-9 * f, "{f}");
    // a chain inside one set: A → SA, both in {CA, D}: once
    let c = cache(&comps, &[Scen("o", 1.0, &[(1.0, "A")])]);
    let (f, _) = c.source_failure(&sets(&[&["CA", "D"]]), &[UR], &none(), &[]);
    assert!((f - UR).abs() < 1e-9 * UR, "{f}");
    // transparent assemblies are neither marked nor followed, as in pcmFailure
    let t: HashSet<String> = ["ASA".to_string()].into();
    let (f, _) = c.source_failure(&sets(&[&["D"]]), &[UR], &t, &[]);
    assert_eq!(f, 0.0);
    // an empty set never fails; a container without calls neither
    let (f, _) = c.source_failure(&sets(&[&[], &["CB"]]), &[1.0, 1.0], &none(), &[]);
    assert_eq!(f, 0.0);
}

#[test]
fn one_source_per_container_is_pcm_failure() {
    let comps = [Comp("A", "CA", &["SA", "B"]), Comp("B", "CB", &["SB"]), Comp("SA", "D", &[]), Comp("SB", "D", &[])];
    let c = cache(&comps, &[Scen("o", 2.0, &[(0.4, "A"), (0.6, "B")]), Scen("p", 1.0, &[(1.0, "A")])]);
    let us: HashMap<String, f64> = [("CA".to_string(), 3e-4), ("CB".to_string(), 2e-4), ("D".to_string(), 5e-4)].into();
    let (old, _) = c.failure(&us, &none(), &[]);
    let names: Vec<&String> = us.keys().collect();
    let s: Vec<Vec<String>> = names.iter().map(|n| vec![n.to_string()]).collect();
    let u: Vec<f64> = names.iter().map(|n| us[*n]).collect();
    let (new, _) = c.source_failure(&s, &u, &none(), &[]);
    assert!((new - old).abs() <= 1e-15 * old, "{new} vs {old}");
}

const TODAY: &str = "../palladio-architecture-optimization-domain/models/misarch-today.m";

#[test]
fn one_source_per_container_is_pcm_failure_on_misarch() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(TODAY);
    if !path.exists() {
        eprintln!("skipped: {} is missing", path.display());
        return;
    }
    let t = translate::translate(&mfile::read(&path).unwrap()).unwrap();
    let c = PcmCache::new(&t.pcm);
    let unavailable: HashMap<String, f64> = t.quality.unavailability.clone().into_iter().collect();
    let (old, _) = c.failure(&unavailable, &none(), &["login".into()]);
    let s: Vec<Vec<String>> = unavailable.keys().map(|k| vec![k.clone()]).collect();
    let u: Vec<f64> = unavailable.keys().map(|k| unavailable[k]).collect();
    let t0 = std::time::Instant::now();
    let (new, _) = c.source_failure(&s, &u, &none(), &["login".into()]);
    eprintln!("misarch-today: {} sources in {:?}", s.len(), t0.elapsed());
    assert!((new - old).abs() <= 1e-12 * old, "{new} vs {old}");
    // merging every pair of containers into one more source costs one walk
    let mut s2 = s.clone();
    let mut u2 = u.clone();
    let keys: Vec<&String> = unavailable.keys().collect();
    for i in 0..keys.len() {
        for j in i + 1..keys.len() {
            s2.push(vec![keys[i].clone(), keys[j].clone()]);
            u2.push(1e-6);
        }
    }
    let t0 = std::time::Instant::now();
    let (more, _) = c.source_failure(&s2, &u2, &none(), &["login".into()]);
    eprintln!("misarch-today: {} sources in {:?}", s2.len(), t0.elapsed());
    assert!(more > new);
}

/// Cost per call on a realised model (mdeo model data): `PCM_SOURCES_MODEL=… cargo test --release
/// --test pcm_sources -- --ignored --nocapture`. Sources: every container alone plus one
/// two-container set per container (as a change unit over its app and database containers).
#[test]
#[ignore]
fn cost_per_call() {
    let Ok(path) = std::env::var("PCM_SOURCES_MODEL") else { return };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let c = PcmCache::new(&mfile::from_model_data(&v).unwrap());
    let cs = c.graph.all("ResourceContainer");
    let unavailable: HashMap<String, f64> = cs.iter().map(|k| (k.clone(), 1e-4)).collect();
    let mut s: Vec<Vec<String>> = cs.iter().map(|k| vec![k.clone()]).collect();
    for i in 0..cs.len() {
        s.push(vec![cs[i].clone(), cs[(i + 1) % cs.len()].clone()]);
    }
    let u = vec![1e-4; s.len()];
    let time = |f: &dyn Fn() -> f64| {
        let mut ts: Vec<f64> = (0..21)
            .map(|_| {
                let t0 = std::time::Instant::now();
                std::hint::black_box(f());
                t0.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ts.sort_by(f64::total_cmp);
        ts[10]
    };
    let old = time(&|| c.failure(&unavailable, &none(), &["login".into()]).0);
    let new = time(&|| c.source_failure(&s, &u, &none(), &["login".into()]).0);
    eprintln!("{path}: {} containers, {} sources: pcmFailure {old:.2} ms, pcmSourceFailure {new:.2} ms (median of 21)", cs.len(), s.len());
}
