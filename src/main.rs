use palladio_sim_service::config::Config;
use palladio_sim_service::import::{entry_files, run_spec};
use palladio_sim_service::{import, mfile, mtext, schema, xmi};
use simoxide_sim::{CompiledModel, Outputs, RunSpec};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: palladio-sim-service [serve]                  run the plugin service (port 3022; configured by the environment)
       palladio-sim-service evaluate MODEL [REQUEST.json] [--out FILE]
                                                      pcmEvaluate on a model's PCM (.m or model data
                                                      .json), the request as the detail's (without its
                                                      own fields): the PcmEvaluation as JSON
       palladio-sim-service detail MODEL [REQUEST.json] [--out FILE]
                                                      the design detail of a model's PCM (.m or model
                                                      data .json): pcmEvaluate's simulation of the
                                                      request with full monitoring, response-time
                                                      distributions and time series per scenario, call,
                                                      system and assembly operation, resource timelines,
                                                      SLO violations and the evaluation, as JSON (request
                                                      fields: API-pcmEvaluate.md, palladio_sim_service::detail);
                                                      also POST /pcm/detail on the service
       palladio-sim-service gen-metamodel [DIR]       write the Palladio metamodel into DIR (default: the current
                                                      directory): palladio.mm, which models and scripts use, and
                                                      the parts it imports, pcm/*.mm
       palladio-sim-service import [--readable] DIR [MM]
                                                      print the PCM model of DIR (XMI) as an mdeo model (.m); MM is
                                                      the metamodel import path (default ../palladio.mm); --readable
                                                      names instances after their entity names
       palladio-sim-service roundtrip DIR...          PCM -> mdeo -> PCM for corpus models: simulate both and
                                                      compare (and with DIR/expected/measurements.csv in an exact
                                                      build) byte for byte";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => palladio_sim_service::server::main(),
        Some("detail") if args.len() >= 2 => detail_cmd(&args[1..], false),
        Some("evaluate") if args.len() >= 2 => detail_cmd(&args[1..], true),
        Some("gen-metamodel") => gen_metamodel(Path::new(args.get(1).map_or(".", |s| s))),
        Some("import") if args.len() >= 2 => {
            let readable = args.iter().any(|a| a == "--readable");
            let rest: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with("--")).collect();
            let naming = if readable { import::Naming::Readable } else { import::Naming::Ids };
            import_cmd(Path::new(rest[0]), rest.get(1).map_or("../palladio.mm", |s| s), naming)
        }
        Some("roundtrip") if args.len() >= 2 => {
            let readable = args.iter().any(|a| a == "--readable");
            let dirs: Vec<String> = args[1..].iter().filter(|a| !a.starts_with("--")).cloned().collect();
            roundtrip(&dirs, if readable { import::Naming::Readable } else { import::Naming::Ids })
        }
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

/// A model file: `.m` text, or model data as the platform stores it (`.json`: instances with
/// properties, links with both ends).
fn detail_cmd(args: &[String], evaluate: bool) -> Result<(), String> {
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
    let v = if evaluate {
        let r = palladio_sim_service::evaluate::Request::from_json(&req)?;
        let cache = palladio_sim_service::pcm::PcmCache::new(&model);
        let mut v = palladio_sim_service::evaluate::run_caller(&cache, &r, Default::default(), limits)?.to_json();
        v["canonicalMs"] = serde_json::json!(cache.canon_ms);
        v["ties"] = serde_json::json!(cache.canon.ties);
        v["modelDigest"] = serde_json::json!(cache.digest().iter().map(|b| format!("{b:02x}")).collect::<String>());
        v
    } else {
        palladio_sim_service::detail::detail_json(&model, &req, limits)?
    };
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

fn gen_metamodel(dir: &Path) -> Result<(), String> {
    for (path, text) in schema::schema().metamodel_files() {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("{}", path.display());
    }
    Ok(())
}

fn import_cmd(dir: &Path, mm: &str, naming: import::Naming) -> Result<(), String> {
    let spec = run_spec(dir)?;
    let imported = import::import_files(&entry_files(dir, &spec)?, "/palladio.mm", naming)?;
    for w in &imported.warnings {
        eprintln!("warning: {w}");
    }
    println!("{}", mtext::write(schema::schema(), &imported.model, mm)?);
    Ok(())
}

fn roundtrip(dirs: &[String], naming: import::Naming) -> Result<(), String> {
    let mut failed = 0;
    for d in dirs {
        let dir = Path::new(d);
        match roundtrip_one(dir, naming) {
            Ok(n) => println!("ok       {} ({n} tuples)", dir.display()),
            Err(e) => {
                failed += 1;
                println!("MISMATCH {}: {e}", dir.display());
            }
        }
    }
    if failed > 0 { Err(format!("{failed} of {} models differ", dirs.len())) } else { Ok(()) }
}

fn roundtrip_one(dir: &Path, naming: import::Naming) -> Result<u64, String> {
    let spec = run_spec(dir)?;
    let mut imported = import::import_files(&entry_files(dir, &spec)?, "/palladio.mm", naming)?;
    // As the platform sends it: every reference list sorted by name.
    for inst in &mut imported.model.instances {
        for (_, targets) in &mut inst.references {
            targets.sort();
        }
    }
    let files = xmi::to_xmi(schema::schema(), &imported.model)?;
    // The entry files the run names, by the names the round trip gave them.
    let renamed = |f: &String| -> Result<String, String> {
        let stem = Path::new(f).file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let root = imported
            .roots
            .iter()
            .find(|(file, _)| file == stem)
            .ok_or_else(|| format!("no root for {f}"))?;
        let ext = Path::new(f).extension().and_then(|s| s.to_str()).unwrap_or_default();
        Ok(format!("{}.{ext}", root.1))
    };
    let generic = RunSpec {
        usagemodel: spec.usagemodel.as_ref().map(renamed).transpose()?,
        allocation: spec.allocation.iter().map(renamed).collect::<Result<_, _>>()?,
        monitorrepository: spec.monitorrepository.as_ref().map(renamed).transpose()?,
        ..spec.clone()
    };
    let mut got = simulate_csv(&generic, || generic.load_model_memory(&files.files));
    // The oracle: SimOxide on the original files, which reproduces the reference.
    let mut want = simulate_csv(&spec, || spec.load_model(dir));
    if naming == import::Naming::Readable {
        // Renamed instances rename the measuring points; every value must stay.
        for r in [&mut got, &mut want] {
            if let Ok((csv, _)) = r {
                let mut rows: Vec<String> = csv.lines().map(|l| l.splitn(2, "],").last().unwrap_or(l).to_string()).collect();
                rows.sort();
                *csv = rows.join("\n");
            }
        }
        if got != want {
            return Err(describe_difference(&got, &want));
        }
        return got.map(|(_, n)| n);
    }
    if got != want {
        return Err(describe_difference(&got, &want));
    }
    // And the reference's own output, where the corpus keeps it (only an exact build reproduces
    // it; the fast mode has other random numbers)
    if !cfg!(feature = "exact") {
        return got.map(|(_, n)| n);
    }
    let reference = std::fs::read_to_string(dir.join("expected/measurements.csv")).ok().or_else(|| {
        let out = std::process::Command::new("gzip").arg("-dc").arg(dir.join("expected/measurements.csv.gz")).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    });
    if let (Some(reference), Ok((csv, _))) = (reference, &got)
        && &reference != csv
    {
        return Err(format!("differs from the reference: {}", describe_difference(&got, &Ok((reference, 0)))));
    }
    match got {
        Ok((_, n)) => Ok(n),
        Err(e) => {
            println!("         both abort: {e}");
            Ok(0)
        }
    }
}

fn simulate_csv(
    spec: &RunSpec,
    load: impl FnOnce() -> Result<simoxide_model::Model, String>,
) -> Result<(String, u64), String> {
    let cm = CompiledModel::compile(load()?).map_err(|e| e.to_string())?;
    let r = simoxide_sim::run(&cm, spec.sim_config(""), Outputs::default()).map_err(|e| e.to_string())?;
    Ok((r.measurements.to_csv(), r.measurements.count))
}

fn describe_difference(got: &Result<(String, u64), String>, want: &Result<(String, u64), String>) -> String {
    match (got, want) {
        (Ok((g, _)), Ok((w, _))) => {
            let (gl, wl): (Vec<&str>, Vec<&str>) = (g.lines().collect(), w.lines().collect());
            let i = gl.iter().zip(&wl).position(|(a, b)| a != b).unwrap_or(gl.len().min(wl.len()));
            format!(
                "line {}: got {:?}, want {:?} ({} vs {} lines)",
                i + 1,
                gl.get(i).unwrap_or(&"<end>"),
                wl.get(i).unwrap_or(&"<end>"),
                gl.len(),
                wl.len()
            )
        }
        (g, w) => format!("got {:?}, want {:?}", g.as_ref().map(|x| x.1), w.as_ref().map(|x| x.1)),
    }
}
