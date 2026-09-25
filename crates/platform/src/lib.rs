//! Cross-cutting infrastructure shared by the `api` and `worker` binaries (spec §4):
//! configuration, telemetry, errors, database, object storage, health checks, shutdown.

pub mod auth_service;
pub mod config;
pub mod crypto;
pub mod db;
pub mod edge;
pub mod error;
pub mod health;
pub mod http;
pub mod mail;
pub mod metrics;
pub mod queue;
pub mod shutdown;
pub mod storage;
pub mod telemetry;

pub use error::{Error, Problem};
