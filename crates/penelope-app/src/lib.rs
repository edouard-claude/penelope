//! `penelope-app` : le socle de l'application (épopée #208, T21).
//!
//! `Services` et les ports que les crates au-dessus se partagent : le daemon compose,
//! les modules métier ne connaissent que ce qui est ici. La crate ne dépend que des
//! crates métier, jamais du daemon ni de ce qui en sortira.

#![forbid(unsafe_code)]

pub mod bus;
pub mod elicitation;
pub mod jobs;
pub mod outcome;
pub mod services;
