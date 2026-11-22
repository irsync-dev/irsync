//! A per-field byte account of protobuf messages.

#![forbid(unsafe_code)]

pub mod walk;

pub use walk::{walk, Field, Sizes, Value, MAX_DEPTH};
