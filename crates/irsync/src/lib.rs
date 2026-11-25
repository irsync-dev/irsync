// Unsafe code calls into the operating system, which is the platform
// module's work. Anywhere else it needs its own allow and a reason.
#![deny(unsafe_code)]

pub mod authz;
pub mod autoexport;
pub mod build;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod daemon_addr;
pub mod daemon_lock;
pub mod data_stream;
pub mod delete;
pub mod delta;
pub mod file_report;
pub mod filter;
pub mod fs;
pub mod gc;
pub mod hasher;
pub mod identity;
pub mod index;
pub mod location;
pub mod manage;
pub mod net;
pub mod options;
pub mod part_file;
pub mod plan;
#[allow(unsafe_code)]
pub mod platform;
pub mod resolve;
pub mod scan;
pub mod session;
pub mod signature;
pub mod ssh;
pub mod telemetry;
pub mod tree_hash;
pub mod warm;
pub mod wtf8;
