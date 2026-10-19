//! The generic `pcm*` analyses against the earlier translation: on the plain PCM that `translate`
//! produces, they must give the translation's own analytic values (offered loads, failures).
//! Uses the unified project's MiSArch model when it is around (skipped otherwise).

use palladio_arch::{mfile, pcm::PcmCache, translate};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const TODAY: &str = "../palladio-architecture-optimization-unified/models/misarch-today.m";

#[test]
fn pcm_analyses_match_the_translation() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(TODAY);
    if !path.exists() {
        eprintln!("skipped: {} is missing", path.display());
        return;
    }
    let model = mfile::read(&path).unwrap();
    let t = translate::translate(&model).unwrap();
    let cache = PcmCache::new(&t.pcm);
    // offered loads: the same demand per resource as the translation's final analysis
    for r in &t.loads.resources {
        let p = cache.loads.resources.iter().find(|x| x.spec == r.spec).expect("resource");
        let d = cache.loads.demand.get(&r.spec).copied().unwrap_or(0.0);
        assert!((p.busy - r.busy).abs() <= 1e-12 * r.busy.abs().max(1.0), "{}: busy {} vs {}", r.spec, p.busy, r.busy);
        assert!((d - r.busy * r.rate).abs() <= 1e-9 * d.abs().max(1.0), "{}: demand {} vs {}", r.spec, d, r.busy * r.rate);
    }
    // O2: the stubs nest their skeleton and callee, so without fate groups and with no transparent
    // assembly every container is marked as by the translation (sidecars carry no unavailability)
    let failure = translate::weighted(t.quality.failure.as_ref().unwrap(), &["login".into()], |n| n.to_string());
    let unavailable: HashMap<String, f64> = t.quality.unavailability.clone().into_iter().collect();
    let (f, _) = cache.failure(&unavailable, &HashSet::new(), &["login".into()]);
    assert!((f - failure).abs() <= 1e-15, "failure {f} vs {failure}");
    // operations: one per entry-level call, rates summing to the scenarios' (one call each)
    let ops: f64 = cache.loads.entry_calls.values().sum();
    let scs: f64 = cache.loads.arrivals.values().sum();
    assert!((ops - scs).abs() <= 1e-9 * scs, "operations {ops} vs scenarios {scs}");
}
