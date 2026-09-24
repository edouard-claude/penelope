//! `penelope-app` : le socle de l'application (épopée #208, T21).
//!
//! `Services` et les ports que les crates au-dessus se partagent : le daemon compose,
//! les modules métier ne connaissent que ce qui est ici. La crate ne dépend que des
//! crates métier, jamais du daemon ni de ce qui en sortira.

#![forbid(unsafe_code)]

pub mod bus;
pub mod codex_scope;
pub mod conversation;
pub mod elicitation;
pub mod gateway;
pub mod helpers;
pub mod jobs;
pub mod journal;
pub mod machine;
pub mod media;
pub mod outcome;
pub mod ports;
pub mod services;
pub mod tasks;
pub mod testing;
pub mod tool_executor;
pub mod vision;
