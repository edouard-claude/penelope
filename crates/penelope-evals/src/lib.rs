//! `penelope-evals` : suites déterministes, mocks, rejeu (§20).
//!
//! Les suites **sans réseau** sont la définition de « terminé » : elles doivent être
//! vertes avant toute livraison.

#![forbid(unsafe_code)]

pub mod ca_matrix;
pub mod live;
pub mod mcp_servers;
pub mod mem_bench;
pub mod scenario;
pub mod suites;

pub use suites::{Suite, SuiteResult, all_suites, requires_network};
