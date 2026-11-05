//! Checks Oracle SQL exports (DDL and `INSERT`s, as SQL Developer writes them) for rows that violate the declared
//! constraints, and writes fixed copies.

pub mod finding;
#[cfg(feature = "gen")]
pub mod synth;
