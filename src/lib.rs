//! Offline storage core extracted from a Windows notebook.
//! API is experimental. Callers serialize access and choose an isolated data directory.
pub mod features;
pub mod model;
pub mod portability;
pub mod repository;
pub use model::*;
pub use repository::Repository;
#[cfg(test)]
use repository::{rows, set_setting};
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_v2;
