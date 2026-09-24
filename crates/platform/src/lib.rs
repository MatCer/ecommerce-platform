//! Cross-cutting infrastructure shared by the `api` and `worker` binaries (spec §4):
//! configuration, telemetry, errors, database, object storage, health checks, shutdown.

pub mod config;
pub mod db;
pub mod error;
pub mod health;
pub mod shutdown;
pub mod storage;
pub mod telemetry;

pub use error::{Error, Problem};
