//! ColdStore virtual tape library harness and mhVTL command wrappers.
//!
//! The crate is split into safe, unit-testable pieces:
//! - [`model`] contains device/tape/changer value types.
//! - [`discover`] parses `lsscsi -g` output without touching the host.
//! - [`command`] describes and executes external commands behind a small trait.
//! - [`mhvtl`] exposes stable `lsscsi`/`mtx`/`mt`/`sg3_utils` command builders.
//! - [`simulator`] provides an in-memory VTL for unit tests and phase-1 logic.
//!
//! Live mhVTL usage is intentionally explicit: callers must provide a
//! [`command::SystemCommandRunner`] or another runner that actually executes
//! host commands.

pub mod command;
pub mod discover;
pub mod error;
pub mod interface;
pub mod mhvtl;
pub mod model;
pub mod simulator;

pub use error::{Result, VtlError};
