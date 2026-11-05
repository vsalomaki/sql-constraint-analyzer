//! Checks SQL exports (Oracle first: DDL and `INSERT`s as SQL Developer writes them) for rows that violate the declared
//! constraints, and writes fixed copies.

pub mod dialect;
pub mod finding;
#[cfg(feature = "gen")]
pub mod synth;
