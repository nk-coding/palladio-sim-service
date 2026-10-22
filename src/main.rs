use palladio_arch::mfile;
use palladio_sim_service::config::Config;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: palladio-arch-service [serve]                 run the plugin service (port 3022; configured by the environment)
       palladio-arch-service evaluate MODEL [REQUEST.json] [--out FILE]
                                                      pcmEvaluate on a model's PCM (.m or model data
                                                      .json), the request as the detail's (without its
                                                      own fields): the PcmEvaluation as JSON
       palladio-arch-service detail MODEL [REQUEST.json] [--out FILE]
                                                      the design detail of a model's PCM (.m or model
                                                      data .json): pcmEvaluate's simulation of the
                                                      request with full monitoring, response-time
                                                      distributions and time series per scenario, call,
                                                      system and assembly operation, resource timelines,
                                                      SLO violations and the evaluation, as JSON (request
                                                      fields: API-pcmEvaluate.md, palladio_arch::detail);
                                                      also POST /pcm/detail on the service";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => palladio_arch::server::main(),
        Some("detail") if args.len() >= 2 => detail_cmd(&args[1..], false),
        Some("evaluate") if args.len() >= 2 => detail_cmd(&args[1..], true),
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
        let r = palladio_arch::evaluate::Request::from_json(&req)?;
        let cache = palladio_arch::pcm::PcmCache::new(&model);
        palladio_arch::evaluate::run(&cache, &r, Default::default(), limits)?.eval.to_json()
    } else {
        palladio_arch::detail::detail_json(&model, &req, limits)?
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
