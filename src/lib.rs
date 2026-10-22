//! Palladio Simulation: an mdeo plugin service that simulates the plain Palladio (PCM) part of a
//! model with SimOxide and returns what the simulation measured, as one script function
//! (`pcmEvaluate`), over per-scenario load profiles and with Kubernetes-style autoscaling (SPD and
//! SimOxide's HPA). The architecture layers above PCM are realised in the model by model
//! transformations (the domain project); the plugin knows PCM only.
//!
//! - The Palladio metamodel and models: [`schema`] (the metamodel as mdeo sees it, derived from
//!   the PCM Ecore metamodel), [`mm`] (written as `.mm` files, `gen-metamodel`), [`import`] and
//!   [`mtext`] (PCM XMI → mdeo model text), [`xmi`] (mdeo model → PCM XMI, for SimOxide),
//!   [`mfile`] (`.m` files and model data), [`graph`] (the model as an editable instance graph).
//! - The simulation: [`canon`] (the canonical form of a model, so results do not depend on names
//!   and order), [`pcm`] (the plain PCM of a model, held per model), [`evaluate`] (`pcmEvaluate`:
//!   one simulation and its reduction), [`dynamic`] (the run, SPD and the HPA), [`lb`] (the load
//!   balancers of SPD's service groups), [`detail`] (the design detail of the same run).
//! - The plugin service: [`functions`] (manifest and calls), [`protocol`] (the `script-functions`
//!   wire protocol), [`session`], [`server`], [`auth`] (session tokens), [`config`].

pub mod auth;
pub mod canon;
pub mod config;
pub mod detail;
pub mod dynamic;
pub mod evaluate;
pub mod functions;
pub mod graph;
pub mod import;
pub mod lb;
pub mod mfile;
pub mod mm;
pub mod mtext;
pub mod pcm;
pub mod protocol;
pub mod schema;
pub mod server;
pub mod session;
pub mod xmi;

use protocol::WireModel;

/// Writes a received model to `dir/<n>.txt`, for inspecting what the platform sent.
pub fn dump(dir: &std::path::Path, model: &WireModel) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _ = std::fs::create_dir_all(dir);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("received-{n:06}.txt")), format!("{model:#?}"));
}
