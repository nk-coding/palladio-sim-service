use palladio_sim_service::import::{entry_files, run_spec};
use palladio_sim_service::{import, mtext, schema, xmi};
use simoxide_sim::{CompiledModel, Outputs, RunSpec};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: palladio-sim-service [serve]              run the plugin service (configured by the environment)
       palladio-sim-service gen-metamodel [DIR]   write the Palladio metamodel into DIR (default: the current
                                                  directory): palladio.mm, which models and scripts use, and
                                                  the parts it imports, pcm/*.mm
       palladio-sim-service import [--readable] DIR [MM]
                                                  print the PCM model of DIR as an mdeo model (.m); MM is the
                                                  metamodel import path (default ../palladio.mm); --readable names
                                                  instances after their entity names instead of their PCM ids
       palladio-sim-service summary DIR [SEED [MAX_MEASUREMENTS [MAX_SIM_TIME]]]
                                                  simulate the PCM files of DIR directly and print the
                                                  statistics pcmSimulate() would report
       palladio-sim-service roundtrip DIR...      PCM -> mdeo -> PCM for corpus models: simulate and compare
                                                  with DIR/expected/measurements.csv byte for byte";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => palladio_sim_service::server::main(),
        Some("gen-metamodel") => gen_metamodel(Path::new(args.get(1).map_or(".", |s| s))),
        Some("import") if args.len() >= 2 => {
            let readable = args.iter().any(|a| a == "--readable");
            let rest: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with("--")).collect();
            let naming = if readable { import::Naming::Readable } else { import::Naming::Ids };
            import_cmd(Path::new(rest[0]), rest.get(1).map_or("../palladio.mm", |s| s), naming)
        }
        Some("summary") if args.len() >= 2 => summary(&args[1..]),
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

fn summary(args: &[String]) -> Result<(), String> {
    let dir = Path::new(&args[0]);
    let num = |i: usize, d: i64| args.get(i).map_or(Ok(d), |s| s.parse::<i64>().map_err(|e| format!("{s}: {e}")));
    let mut spec = run_spec(dir)?;
    spec.seed = num(1, 1)?;
    spec.max_measurements = num(2, 10_000)?;
    spec.max_sim_time = num(3, -1)?;
    let cm = CompiledModel::compile(spec.load_model(dir)?).map_err(|e| e.to_string())?;
    let r = simoxide_sim::run(&cm, spec.sim_config(""), Outputs::default()).map_err(|e| e.to_string())?;
    println!("events: {}, measurements: {}, simulated time: {} s", r.events, r.measurements.count, r.end_ns as f64 / 1e9);
    for s in r.measurements.summaries() {
        println!("{} | {} | n={}, mean={:?}, p90={:?}", s.measuring_point, s.metric, s.count, s.mean, s.p90);
    }
    Ok(())
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
    // And the reference's own output, where the corpus keeps it.
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
