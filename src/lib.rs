//! Checks SQL exports (Oracle first: DDL and `INSERT`s as SQL Developer writes them) for rows that violate the declared
//! constraints, and writes fixed copies.

pub mod catalog;
pub mod check;
pub mod dialect;
pub mod finding;
pub mod input;
pub mod report;
pub mod source;
pub mod statement;
#[cfg(feature = "gen")]
pub mod synth;
pub mod value;
