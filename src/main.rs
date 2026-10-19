use palladio_arch::{functions, mfile, translate};
use palladio_sim_service::config::Config;
use palladio_sim_service::engine::{Engine, RunParams};
use palladio_sim_service::{mtext, schema, xmi};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "\
usage: palladio-arch-service [serve]                 run the plugin service (port 3022; configured by the environment)
       palladio-arch-service translate MODEL OUT_DIR  write the translated model: OUT_DIR/<name>.m (plain PCM,
                                                      formatted with tools/format_model.py when it is found) and
                                                      OUT_DIR/pcm/*.xml-style PCM files
       palladio-arch-service eval MODEL [--seed N] [--measurements N] [--max-sim-time S] [--replications N]
                                  [--plain] [--json]
                                                      translate, simulate and print the series, utilisation, cost
                                                      and modularity; --plain ignores the decisions
       palladio-arch-service hpa MODEL [--seed N] [--exclude A,B] [--json] [--dump DIR]
                                                      a dynamic run with Kubernetes-style autoscaling over the
                                                      model's load profile (archHpaSimulate); --dump writes its
                                                      PCM, SPD and HPA files
       palladio-arch-service oracle MODEL... [--exclude A,B]
                                                      the analytic arch* values of each model (.m or
                                                      model data .json) as one JSON object per line:
                                                      the reference of the layer-2 realisation by
                                                      model transformations (domain project, B3)
       palladio-arch-service detail MODEL [REQUEST.json] [--out FILE]
                                                      the design detail of a model's PCM (.m or model
                                                      data .json): one simulation with full monitoring,
                                                      response-time distributions and time series per
                                                      scenario, call, system and assembly operation,
                                                      resource timelines, SLO violations, as JSON
                                                      (request fields: palladio_arch::detail); also
                                                      POST /pcm/detail on the service";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => palladio_arch::server::main(),
        Some("translate") if args.len() == 3 => translate_cmd(Path::new(&args[1]), Path::new(&args[2])),
        Some("eval") if args.len() >= 2 => eval_cmd(&args[1..]),
        Some("hpa") if args.len() >= 2 => hpa_cmd(&args[1..]),
        Some("oracle") if args.len() >= 2 => oracle_cmd(&args[1..]),
        Some("detail") if args.len() >= 2 => detail_cmd(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// `to` relative to the directory `from` (both made absolute).
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    let to = std::fs::canonicalize(to).unwrap_or_else(|_| to.to_path_buf());
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    out
}

fn translate_cmd(model_path: &Path, out: &Path) -> Result<(), String> {
    let model = mfile::read(model_path)?;
    let t = translate::translate(&model)?;
    for w in &t.warnings {
        eprintln!("warning: {w}");
    }
    std::fs::create_dir_all(out.join("pcm")).map_err(|e| e.to_string())?;
    let model_dir = model_path.parent().unwrap_or(Path::new("."));
    let mm_dir = model_dir.join(&model.metamodel_path).parent().map(Path::to_path_buf).unwrap_or_default();
    let using = relative(out, &mm_dir).join("palladio.mm");
    let stem = model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    let m_path = out.join(format!("{stem}.translated.m"));
    let text = mtext::write(schema::schema(), &t.pcm, &using.to_string_lossy())?;
    std::fs::write(&m_path, text).map_err(|e| e.to_string())?;
    // Format it, when the project's formatter is around.
    let formatter = model_dir.ancestors().map(|d| d.join("tools/format_model.py")).find(|p| p.exists());
    match formatter {
        Some(f) => {
            let st = std::process::Command::new("python3").arg(&f).arg("-i").arg(&m_path).status().map_err(|e| e.to_string())?;
            if !st.success() {
                eprintln!("warning: {} failed; {} is left unformatted", f.display(), m_path.display());
            }
        }
        None => eprintln!("note: no tools/format_model.py found; {} is unformatted", m_path.display()),
    }
    let files = xmi::to_xmi(schema::schema(), &t.pcm)?;
    for (name, text) in &files.files {
        std::fs::write(out.join("pcm").join(name), text).map_err(|e| e.to_string())?;
    }
    println!(
        "{}: {} instances ({} before), {} PCM files in {}",
        m_path.display(),
        t.pcm.instances.len(),
        model.instances.len(),
        files.files.len(),
        out.join("pcm").display()
    );
    Ok(())
}

fn eval_cmd(args: &[String]) -> Result<(), String> {
    let mut params = RunParams { seed: 1, max_measurements: 10_000, max_sim_time: -1, replications: 1, simulate_linking_resources: false };
    let (mut plain, mut json) = (false, false);
    let mut i = 1;
    let num = |i: usize| -> Result<i64, String> {
        args.get(i).ok_or("missing value")?.parse::<i64>().map_err(|e| format!("{}: {e}", args[i]))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => {
                params.seed = num(i + 1)?;
                i += 1;
            }
            "--measurements" => {
                params.max_measurements = num(i + 1)?;
                i += 1;
            }
            "--max-sim-time" => {
                params.max_sim_time = num(i + 1)?;
                i += 1;
            }
            "--replications" => {
                params.replications = num(i + 1)? as u32;
                i += 1;
            }
            "--plain" => plain = true,
            "--json" => json = true,
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        i += 1;
    }
    let clock = std::time::Instant::now();
    let lap = |what: &str| {
        if std::env::var_os("ARCH_TIMING").is_some() {
            eprintln!("timing {what}: {:.1} ms", clock.elapsed().as_secs_f64() * 1000.0);
        }
    };
    let mut model = mfile::read(Path::new(&args[0]))?;
    if plain {
        model.instances.retain(|x| palladio_arch::graph::is_pcm(&x.class_name));
    }
    lap("read");
    let t = translate::translate(&model)?;
    lap("translate");
    let engine = Arc::new(Engine::new(Config::from_env()?));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|e| e.to_string())?;
    let p = rt.block_on(engine.prepare(schema::schema(), Arc::new(t.pcm.clone())))?;
    lap("prepare");
    let (prepared, outcome) = rt.block_on(async {
        let o = engine.simulate(p.clone(), params.clone()).await?;
        Ok::<_, String>((p, o))
    })?;
    lap("simulate");
    let entity = |n: &str| -> String {
        t.pcm
            .instances
            .iter()
            .find(|x| x.name == n)
            .and_then(|x| x.attributes.iter().find(|(k, _)| k == "entityName"))
            .and_then(|(_, v)| match v.first() {
                Some(palladio_sim_service::protocol::WireValue::String(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| n.to_string())
    };
    if json {
        let series: Vec<serde_json::Value> = outcome
            .series
            .iter()
            .map(|s| {
                serde_json::json!({
                    "measuringPoint": s.measuring_point, "kind": functions::kind_of(&s.measuring_point),
                    "element": s.elements.first(), "elementName": s.elements.first().and_then(|e| prepared.entity_names.get(e)),
                    "metric": s.metric, "count": s.count, "mean": s.mean, "p90": s.p90, "p95": s.p95,
                    "timeWeightedMean": s.time_weighted_mean,
                })
            })
            .collect();
        let resources: Vec<serde_json::Value> = t
            .loads
            .resources
            .iter()
            .map(|r| serde_json::json!({"spec": r.spec, "container": entity(&r.container), "kind": r.kind, "replicas": r.replicas, "rate": r.rate, "busy": r.busy, "utilisation": r.utilisation()}))
            .collect();
        let out = serde_json::json!({
            "translated": t.translated, "instances": t.pcm.instances.len(), "events": outcome.events,
            "simulatedTime": outcome.simulated_time, "series": series, "resources": resources,
            "nodes": t.nodes.iter().map(|n| serde_json::json!({"name": n.name, "cores": n.cores, "load": n.load, "slowdown": n.slowdown})).collect::<Vec<_>>(),
            "cost": t.cost, "modularity": t.modularity, "warnings": t.warnings,
            "replicas": t.replicas.iter().map(|(c, n)| (entity(c), *n)).collect::<std::collections::BTreeMap<_, _>>(),
            "quality": serde_json::json!({
                "failure": t.quality.failure.as_ref().map(|m| m.iter().map(|(sc, (r, f))| serde_json::json!({"scenario": entity(sc), "rate": r, "value": f})).collect::<Vec<_>>()),
                "inconsistency": t.quality.inconsistency.as_ref().map(|m| m.iter().map(|(sc, (r, f))| serde_json::json!({"scenario": entity(sc), "rate": r, "value": f})).collect::<Vec<_>>()),
                "changeCoupling": t.quality.change_coupling,
                "unavailability": t.quality.unavailability.iter().map(|(c, u)| (entity(c), *u)).collect::<std::collections::BTreeMap<_, _>>(),
                "latencyViolation": t.latency_violation(),
                "peak": t.peak_utilisation().map(|(u, l)| serde_json::json!({"utilisation": u, "limit": l})),
            }),
            "noLoad": t.no_load.iter().map(|(sc, (r, time))| serde_json::json!({"scenario": entity(sc), "rate": r, "time": time})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
        return Ok(());
    }
    println!(
        "translated: {} ({} instances), events: {}, simulated time: {}",
        t.translated,
        t.pcm.instances.len(),
        outcome.events,
        outcome.simulated_time
    );
    println!("\nseries:");
    for s in &outcome.series {
        let name = s.elements.first().and_then(|e| prepared.entity_names.get(e)).cloned().unwrap_or_default();
        println!(
            "  {} {} | {} | n={}, mean={:?}, p90={:?}{}",
            functions::kind_of(&s.measuring_point),
            name,
            s.metric,
            s.count,
            s.mean,
            s.p90,
            s.time_weighted_mean.map(|v| format!(", time-weighted={v:?}")).unwrap_or_default()
        );
    }
    println!("\noffered load:");
    for r in &t.loads.resources {
        if r.kind == "DELAY" {
            continue;
        }
        println!(
            "  {:<40} {:<4} replicas {:>4} rate {:<12} busy {:.4} utilisation {:.4}",
            entity(&r.container),
            r.kind,
            r.replicas,
            r.rate,
            r.busy,
            r.utilisation()
        );
    }
    for n in &t.nodes {
        println!("  node {:<35} cores {} load {:.4} slowdown {:.4}", n.name, n.cores, n.load, n.slowdown);
    }
    let used: std::collections::BTreeSet<&String> = t.placement.values().collect();
    let scaled: Vec<String> = t.replicas.iter().filter(|(c, _)| used.contains(c)).map(|(c, n)| format!("{} {n}", entity(c))).collect();
    if !scaled.is_empty() {
        println!("\nreplicas: {}", scaled.join(", "));
    }
    println!("\ncost: {}\nmodularity: {}", t.cost, t.modularity);
    let login = vec!["login".to_string()];
    if let Some(f) = &t.quality.failure {
        println!("failed requests (all but login): {:.1} ppm", 1e6 * palladio_arch::translate::weighted(f, &login, |n| entity(n)));
    }
    if let Some(c) = t.quality.change_coupling {
        println!("change coupling: {c}");
    }
    if let Some(i) = &t.quality.inconsistency {
        println!("inconsistency exposure (all but login): {}", palladio_arch::translate::weighted(i, &login, |n| entity(n)));
    }
    if !t.quality.budgets.is_empty() {
        println!("latency budget violation: {}", t.latency_violation());
    }
    if let Some((u, l)) = t.peak_utilisation() {
        println!("peak utilisation of unscaled containers: {u:.3} (limit {l})");
    }
    println!("no-load response time (rate-weighted, all scenarios): {}", t.no_load_time(&[], |x| x.to_string()));
    for w in &t.warnings {
        println!("warning: {w}");
    }
    Ok(())
}

fn hpa_cmd(args: &[String]) -> Result<(), String> {
    use palladio_arch::dynamic;
    let mut params = dynamic::HpaParams { seed: 1, exclude: vec!["login".into()] };
    let (mut json, mut dump) = (false, None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => {
                params.seed = args.get(i + 1).ok_or("missing value")?.parse().map_err(|e| format!("--seed: {e}"))?;
                i += 1;
            }
            "--exclude" => {
                params.exclude = args.get(i + 1).ok_or("missing value")?.split(',').map(String::from).filter(|x| !x.is_empty()).collect();
                i += 1;
            }
            "--dump" => {
                dump = Some(PathBuf::from(args.get(i + 1).ok_or("missing value")?));
                i += 1;
            }
            "--json" => json = true,
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        i += 1;
    }
    let t0 = std::time::Instant::now();
    let model = mfile::read(Path::new(&args[0]))?;
    let (_, dm) = translate::translate_dynamic(&model)?;
    let t_translate = t0.elapsed().as_secs_f64();
    let config = Config::from_env()?;
    let engine = dynamic::engine(&config);
    let p = dynamic::prepare(Arc::new(dm), engine.max_pods)?;
    if let Some(d) = &dump {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        for (n, text) in &p.files {
            std::fs::write(d.join(n), text).map_err(|e| e.to_string())?;
        }
    }
    let limits = simoxide_sim::Limits::default();
    let o = dynamic::run(&p, &params, limits)?;
    if json {
        let groups: Vec<serde_json::Value> = o
            .groups
            .iter()
            .map(|g| serde_json::json!({"name": g.name, "meanReplicas": g.mean_replicas, "idealReplicas": g.ideal_replicas,
                "maxReplicas": g.max_replicas, "rescales": g.rescales, "createdPods": g.created, "lag": g.lag, "reversals": g.reversals}))
            .collect();
        let mut out = serde_json::Map::new();
        for st in functions::HPA_STATISTICS {
            out.insert(st.to_string(), serde_json::json!(functions::hpa_stat(&o, st)));
        }
        out.insert("events".into(), serde_json::json!(o.events));
        out.insert("wallMs".into(), serde_json::json!(o.wall_ms));
        out.insert("adjustMs".into(), serde_json::json!(o.adjust_ms));
        out.insert("translateMs".into(), serde_json::json!(t_translate * 1e3));
        out.insert("adjustments".into(), serde_json::json!(o.adjustments));
        out.insert("groups".into(), serde_json::Value::Array(groups));
        println!("{}", serde_json::to_string_pretty(&serde_json::Value::Object(out)).map_err(|e| e.to_string())?);
        return Ok(());
    }
    println!(
        "dynamic run: {} groups, {} events, {:.0} ms (adjustments {:.0} ms; translation {:.0} ms)",
        o.groups.len(),
        o.events,
        o.wall_ms,
        o.adjust_ms,
        t_translate * 1e3
    );
    println!(
        "cost {:.3} (ideal autoscaler {:.3}), ready replica-seconds {:.0}, SLO violations {:.4} (after increases {:.4}) of {} requests",
        o.cost, o.ideal_cost, o.ready_replica_seconds, o.slo, o.slo_steps, o.requests
    );
    println!(
        "response time mean {:.2} ms, p95 {:.2} ms; lag mean {:.1} s, max {:.1} s; flapping {:.2}/h; peak pods {}; rescales {}, pods created {}, adjustments {}{}",
        o.mean_ms,
        o.p95_ms,
        o.lag,
        o.lag_max,
        o.flapping,
        o.peak_replicas,
        o.rescales,
        o.created,
        o.adjustments,
        if o.capped { " (CAPPED)" } else { "" }
    );
    for g in &o.groups {
        println!(
            "  {:<24} mean {:6.2} (ideal {:6.2}) max {:3} rescales {:3} created {:3} lag {:6.1} s reversals {}",
            g.name, g.mean_replicas, g.ideal_replicas, g.max_replicas, g.rescales, g.created, g.lag, g.reversals
        );
    }
    Ok(())
}

/// A model file: `.m` text, or model data as the platform stores it (`.json`: instances with
/// properties, links with both ends).
fn detail_cmd(args: &[String]) -> Result<(), String> {
    let mut out = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--out" {
            out = Some(args.get(i + 1).ok_or("--out needs a file")?.clone());
            i += 2;
            continue;
        }
        rest.push(args[i].clone());
        i += 1;
    }
    let model = read_any(Path::new(rest.first().ok_or(USAGE)?))?;
    let req = match rest.get(1) {
        Some(f) => {
            let text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            palladio_arch::detail::DetailRequest::from_json(&serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?)?
        }
        None => palladio_arch::detail::DetailRequest::default(),
    };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into())).with_writer(std::io::stderr).try_init();
    let config = Config::from_env()?;
    let limits = simoxide_sim::Limits { max_events: config.sim_max_events, ..simoxide_sim::Limits::default() };
    let v = palladio_arch::detail::detail(&model, &req, limits)?;
    let text = v.to_string();
    match out {
        Some(f) => std::fs::write(&f, text).map_err(|e| format!("{f}: {e}")),
        None => {
            println!("{text}");
            Ok(())
        }
    }
}

fn read_any(path: &Path) -> Result<palladio_sim_service::protocol::WireModel, String> {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return mfile::read(path);
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    mfile::from_model_data(&v).map_err(|e| format!("{}: {e}", path.display()))
}

/// The analytic values of the `arch*` functions (as the objectives call them, scenarios in
/// `--exclude`, default `login`, left out), per model one JSON line.
fn oracle_cmd(args: &[String]) -> Result<(), String> {
    let mut files = Vec::new();
    let mut exclude = vec!["login".to_string()];
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--exclude" {
            exclude = args.get(i + 1).ok_or("--exclude needs a value")?.split(',').map(str::to_string).filter(|x| !x.is_empty()).collect();
            i += 2;
            continue;
        }
        files.push(args[i].clone());
        i += 1;
    }
    let num = |x: f64| if x.is_finite() { format!("{x:e}") } else { "null".to_string() };
    for f in files {
        let model = read_any(Path::new(&f))?;
        let t = translate::translate(&model)?;
        let entity = |n: &str| -> String {
            t.pcm
                .instances
                .iter()
                .chain(model.instances.iter())
                .find(|x| x.name == n)
                .and_then(|x| x.attributes.iter().find(|(k, _)| k == "entityName"))
                .and_then(|(_, v)| match v.first() {
                    Some(palladio_sim_service::protocol::WireValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| n.to_string())
        };
        let mut o: Vec<(String, String)> = vec![("model".into(), format!("{f:?}"))];
        o.push(("cost".into(), num(t.cost)));
        o.push(("modularity".into(), num(t.modularity)));
        o.push(("utilisation".into(), num(t.utilisation("", &entity)?)));
        o.push(("replicas".into(), num(t.replicas_of("", &entity)?)));
        o.push(("noLoadTime".into(), num(t.no_load_time(&exclude, &entity))));
        o.push(("noLoadTimeAll".into(), num(t.no_load_time(&[], &entity))));
        if let Some(per) = &t.quality.failure {
            o.push(("failure".into(), num(translate::weighted(per, &exclude, &entity))));
        }
        if let Some(c) = t.quality.change_coupling {
            o.push(("changeCoupling".into(), num(c)));
        }
        if let Some(per) = &t.quality.inconsistency {
            o.push(("inconsistency".into(), num(translate::weighted(per, &exclude, &entity))));
        }
        o.push(("latencyViolation".into(), num(t.latency_violation())));
        if let Some((u, limit)) = t.peak_utilisation() {
            o.push(("peakUtilisation".into(), num(u)));
            o.push(("peakViolation".into(), num((u - limit).max(0.0))));
        }
        let scen: Vec<String> = t.no_load.iter().map(|(sc, (r, time))| format!("{:?}:[{},{}]", entity(sc), num(*r), num(*time))).collect();
        o.push(("scenarios".into(), format!("{{{}}}", scen.join(","))));
        let reps: Vec<String> = t.replicas.iter().map(|(c, n)| format!("{:?}:{}", entity(c), num(*n))).collect();
        o.push(("replicasOf".into(), format!("{{{}}}", reps.join(","))));
        let mut util = std::collections::BTreeMap::new();
        for r in &t.loads.resources {
            let e = util.entry(format!("{}/{}", entity(&r.container), r.kind)).or_insert(0.0f64);
            *e = e.max(r.utilisation());
        }
        let us: Vec<String> = util.iter().map(|(k, v)| format!("{k:?}:{}", num(*v))).collect();
        o.push(("utilisationOf".into(), format!("{{{}}}", us.join(","))));
        let nodes: Vec<String> = t.nodes.iter().map(|n| format!("{:?}:[{},{}]", n.name, num(n.load), num(n.slowdown))).collect();
        o.push(("nodes".into(), format!("{{{}}}", nodes.join(","))));
        let ws: Vec<String> = t.warnings.iter().map(|w| format!("{w:?}")).collect();
        o.push(("warnings".into(), format!("[{}]", ws.join(","))));
        println!("{{{}}}", o.iter().map(|(k, v)| format!("{k:?}:{v}")).collect::<Vec<_>>().join(","));
    }
    Ok(())
}
