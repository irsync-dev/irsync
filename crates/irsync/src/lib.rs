// Unsafe code calls into the operating system, which is the platform
// module's work. Anywhere else it needs its own allow and a reason.
#![deny(unsafe_code)]

// TEMPORARY, removed by Task 8 of the Windows port: every
// `#[cfg(not(windows))]` below. On Windows only `platform` and `wtf8`
// build yet, so only they are compiled there and `cargo test -p irsync
// --lib` runs their tests; on every other system each module is compiled
// as before.
#[cfg(not(windows))]
pub mod authz;
#[cfg(not(windows))]
pub mod autoexport;
#[cfg(not(windows))]
pub mod build;
#[cfg(not(windows))]
pub mod cli;
#[cfg(not(windows))]
pub mod config;
#[cfg(not(windows))]
pub mod daemon;
#[cfg(not(windows))]
pub mod daemon_addr;
#[cfg(not(windows))]
pub mod daemon_lock;
#[cfg(not(windows))]
pub mod delete;
#[cfg(not(windows))]
pub mod delta;
#[cfg(not(windows))]
pub mod file_report;
#[cfg(not(windows))]
pub mod filter;
#[cfg(not(windows))]
pub mod fs;
#[cfg(not(windows))]
pub mod hasher;
#[cfg(not(windows))]
pub mod identity;
#[cfg(not(windows))]
pub mod index;
#[cfg(not(windows))]
pub mod manage;
#[cfg(not(windows))]
pub mod net;
#[cfg(not(windows))]
pub mod plan;
#[allow(unsafe_code)]
pub mod platform;
#[cfg(not(windows))]
pub mod recv;
#[cfg(not(windows))]
pub mod resolve;
#[cfg(not(windows))]
pub mod scan;
#[cfg(not(windows))]
pub mod send;
#[cfg(not(windows))]
pub mod session;
#[cfg(not(windows))]
pub mod signature;
#[cfg(not(windows))]
pub mod ssh;
#[cfg(not(windows))]
pub mod telemetry;
#[cfg(not(windows))]
pub mod tree_hash;
#[cfg(not(windows))]
pub mod warm;
pub mod wtf8;
