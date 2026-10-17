//! An mdeo plugin service that simulates Palladio models with SimOxide, offered to scripts as
//! external functions. The plugin build runs SimOxide's fast mode (SimuLizar 5.2.2's semantics
//! and statistics, not its random numbers); a local build with the feature `exact` runs the
//! SimuLizar-exact mode.
//!
//! - [`schema`]: the Palladio metamodel as mdeo sees it, derived from the PCM Ecore metamodel;
//!   [`mm`] writes it as a `.mm` file.
//! - [`import`] and [`mtext`]: PCM XMI → mdeo model (`.m`), for bringing existing models in.
//! - [`xmi`]: mdeo model → PCM XMI, for the simulator.
//! - [`engine`]: compiled models, parallel runs, caches.
//! - [`functions`], [`protocol`], [`session`], [`server`], [`auth`]: the plugin service.

pub mod auth;
pub mod config;
pub mod engine;
pub mod functions;
pub mod import;
pub mod mm;
pub mod mtext;
pub mod protocol;
pub mod schema;
pub mod server;
pub mod session;
pub mod xmi;
