use palladio_arch::{mfile, translate};
use palladio_sim_service::config::Config;
use palladio_sim_service::{mtext, schema, xmi};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: palladio-arch-service [serve]                 run the plugin service (port 3022; configured by the environment)
       palladio-arch-service translate MODEL OUT_DIR  write the translated model: OUT_DIR/<name>.m (plain PCM,
                                                      formatted with tools/format_model.py when it is found) and
                                                      OUT_DIR/pcm/*.xml-style PCM files
       palladio-arch-service oracle MODEL... [--exclude A,B]
                                                      the analytic values of each model's translation
                                                      (.m or model data .json; cost, failure, ..., as
                                                      the former arch* functions) as one JSON object
                                                      per line: the reference of the layer-2
                                                      realisation by model transformations (domain
                                                      project, B3)
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
    let req: serde_json::Value = match rest.get(1) {
        Some(f) => {
            let text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?
        }
        None => serde_json::json!({}),
    };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into())).with_writer(std::io::stderr).try_init();
    let config = Config::from_env()?;
    let limits = simoxide_sim::Limits { max_events: config.sim_max_events, ..simoxide_sim::Limits::default() };
    let v = palladio_arch::detail::detail_json(&model, &req, limits)?;
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

/// The analytic values of the translation (as the former `arch*` functions gave them to the
/// objectives, scenarios in `--exclude`, default `login`, left out), per model one JSON line.
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
