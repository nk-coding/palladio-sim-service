//! An mdeo plugin service for the architecture optimisation of plain Palladio (PCM) models: script
//! functions that analyse the PCM part of a model and simulate it with SimOxide, through
//! `palladio-sim-service`, also over load profiles and with Kubernetes-style autoscaling. The
//! architecture layers above PCM are realised in the model by model transformations (the domain
//! project); the plugin knows PCM only.
//!
//! - [`mfile`]: reads `.m` files (for the command line and the tests).
//! - [`graph`]: the model as an editable instance graph.
//! - [`load`]: offered load and call frequencies of a PCM model, without simulating.
//! - [`pcm`]: the analyses of the `pcm*` functions; [`pcmrun`]: simulations with load profiles
//!   and usage streams; [`pcmhpa`]: dynamic runs with autoscaling, on [`dynamic`] (SimOxide's
//!   HPA); [`detail`]: the design detail of one simulation.
//! - [`translate`]: the completion rules of layer 2 (DESIGN.md §4, §5), cost, modularity and
//!   the quality values: the reference the domain project's realisation is checked against
//!   (`palladio-arch-service oracle`, `translate`).
//! - [`functions`], [`session`], [`server`]: the plugin service.

pub mod dynamic;
pub mod functions;
pub mod graph;
pub mod load;
pub mod mfile;
pub mod pcm;
pub mod pcmhpa;
pub mod pcmrun;
pub mod detail;
pub mod server;
pub mod session;
pub mod translate;

use palladio_sim_service::protocol::WireModel;

/// Writes a received model to `dir/<n>.txt`, for inspecting what the platform sent.
pub fn dump(dir: &std::path::Path, model: &WireModel) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _ = std::fs::create_dir_all(dir);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("received-{n:06}.txt")), format!("{model:#?}"));
}
