//! Reproducible swarm workloads for Tessel's A/B evidence: a seeded generator of scripted edits
//! against a small demo repository, run uncoordinated (`off`, plain git, local) and coordinated
//! (`on`, the real protocol against a coordinator), with the results written as JSON and a
//! Markdown table.

pub mod code;
pub mod conn;
pub mod demo;
pub mod endpoint;
pub mod events;
pub mod git;
pub mod guard;
pub mod live;
pub mod local;
pub mod off;
pub mod on;
pub mod report;
pub mod rng;
pub mod run;
pub mod tasks;
