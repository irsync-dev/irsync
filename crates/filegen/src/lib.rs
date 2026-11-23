// The crate under its own name, so `tests/common/mod.rs`, which the unit
// tests of `run.rs` include, reaches the platform module as `filegen::`
// from both places.
extern crate self as filegen;

pub mod caps;
pub mod chain;
pub mod cli;
pub mod config;
pub mod content;
pub mod escape;
pub mod kind;
pub mod log;
pub mod model;
pub mod names;
pub mod ops;
pub mod pace;
pub mod pick;
pub mod platform;
pub mod run;
pub mod sizes;
pub mod state;
pub mod walk;
pub mod wtf8;
