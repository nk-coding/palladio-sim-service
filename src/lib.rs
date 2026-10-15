//! An mdeo plugin service that optimises the architecture of plain Palladio (PCM) models: it
//! translates the architecture decisions of a model (`architecture.mm`, `microservices.mm`) into
//! plain PCM and simulates the result with SimOxide, through `palladio-sim-service`.
//!
//! - [`mfile`]: reads `.m` files (for the command line and the tests).
//! - [`graph`]: the model as an editable instance graph.
//! - [`translate`]: the completion rules (DESIGN.md §4, §5), cost and modularity.
//! - [`load`]: offered load and call frequencies of a PCM model, without simulating.
//! - [`functions`], [`session`], [`server`]: the plugin service.

pub mod functions;
pub mod graph;
pub mod load;
pub mod mfile;
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
