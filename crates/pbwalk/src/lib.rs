//! A per-field byte account of protobuf messages.

#![forbid(unsafe_code)]

pub mod account;
pub mod schema;
pub mod walk;

pub use account::{raw, unwrap, Entry, Kind, Prefix};
pub use schema::Schema;
pub use walk::{walk, Field, Sizes, Value, MAX_DEPTH};
