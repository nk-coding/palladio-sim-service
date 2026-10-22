//! Configuration from the environment. The platform's plugin-service variables (`PORT`, `HOST`,
//! `BACKEND_API_URL`, `JWT_ISSUER`, `MAX_SESSIONS`) mean what they mean for every plugin service;
//! the `SIM_*` ones size the simulation engine.

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub host: String,
    /// The backend API; session tokens are verified against `<url>/.well-known/jwks.json`.
    pub backend_api_url: String,
    pub jwt_issuer: String,
    /// Sessions open at once; another is refused with 4503. Every optimizer worker holds one.
    pub max_sessions: usize,
    /// Simulation threads, shared by all sessions and all replications (0: one per CPU).
    pub sim_threads: usize,
    /// Wall-clock limit of one simulation run, in seconds (0: none).
    pub sim_timeout_seconds: u64,
    /// Event limit of one simulation run (0: none).
    pub sim_max_events: u64,
    /// Evaluations kept, by canonical model content and arguments. Runs are deterministic, so a
    /// cached evaluation is exactly what running again would give.
    pub result_cache_size: usize,
    /// When set, every model a session receives is written there, for inspecting what the
    /// platform sent.
    pub dump_dir: Option<std::path::PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            port: num("PORT", 3020)?,
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            backend_api_url: std::env::var("BACKEND_API_URL").unwrap_or_else(|_| "http://localhost:8080/api".into()),
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "mdeo-platform".into()),
            max_sessions: num("MAX_SESSIONS", 256)?,
            sim_threads: num("SIM_THREADS", 0)?,
            sim_timeout_seconds: num("SIM_TIMEOUT_SECONDS", 600)?,
            sim_max_events: num("SIM_MAX_EVENTS", 0)?,
            result_cache_size: num("SIM_RESULT_CACHE_SIZE", 4096)?.max(1),
            dump_dir: std::env::var_os("SIM_DUMP_DIR").map(Into::into),
        })
    }

    /// The number of simulation threads actually used.
    pub fn threads(&self) -> usize {
        if self.sim_threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            self.sim_threads
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 3020,
            host: "127.0.0.1".into(),
            backend_api_url: "http://localhost:8080/api".into(),
            jwt_issuer: "mdeo-platform".into(),
            max_sessions: 256,
            sim_threads: 0,
            sim_timeout_seconds: 600,
            sim_max_events: 0,
            result_cache_size: 4096,
            dump_dir: None,
        }
    }
}

fn num<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(v) => v.trim().parse().map_err(|_| format!("{name}={v} is not a valid whole number")),
    }
}
