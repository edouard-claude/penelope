//! Moteur de workflows : il part dans `penelope-orchestrator` (épopée #208, T27) ;
//! `engine/` en est le code, déplacé tel quel, et ce module la façade qui garde les
//! chemins `crate::workflow::*`.

mod engine;
pub use engine::*;
