//! An mdeo plugin service for the architecture optimisation of plain Palladio (PCM) models:
//! one script function that simulates the PCM part of a model with SimOxide, through
//! `palladio-sim-service`, over per-scenario load profiles and with Kubernetes-style autoscaling
//! (SPD and SimOxide's HPA). The architecture layers above PCM are realised in the model by model
//! transformations (the domain project); the plugin knows PCM only.
//!
//! - [`mfile`]: reads `.m` files and model data (for the command line and the tests).
//! - [`graph`]: the model as an editable instance graph.
//! - [`canon`]: the canonical form of a model (names and order from its structure), so that
//!   results do not depend on names and order.
//! - [`pcm`]: the plain PCM of a model, held per model; [`evaluate`]: `pcmEvaluate`, one
//!   simulation and its reduction, on [`dynamic`] (the run, SPD and the HPA) and [`lb`] (the
//!   load balancers of SPD's service groups); [`detail`]: the design detail of the same run.
//! - [`functions`], [`session`], [`server`]: the plugin service.

pub mod canon;
pub mod detail;
pub mod dynamic;
pub mod evaluate;
pub mod functions;
pub mod graph;
pub mod lb;
pub mod mfile;
pub mod pcm;
pub mod server;
pub mod session;

use palladio_sim_service::protocol::WireModel;

/// Writes a received model to `dir/<n>.txt`, for inspecting what the platform sent.
pub fn dump(dir: &std::path::Path, model: &WireModel) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _ = std::fs::create_dir_all(dir);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("received-{n:06}.txt")), format!("{model:#?}"));
}
