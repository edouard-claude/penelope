//! `penelope-dream` : la mémoire qui mûrit (épopée #208, T26).
//!
//! Entretien d'accueil (`onboarding`) ; la consolidation nocturne (`dream/`) et
//! l'ingestion de documents (`ingest`) y descendent quand leurs signatures ne citent plus
//! `Daemon`. Au-dessus de `penelope-app` et de `penelope-vault`, sous le daemon, qui
//! réexporte ces modules sous leurs anciens chemins jusqu'à T30.

#![forbid(unsafe_code)]

pub mod onboarding;

// Modules du socle et du vault, sous les chemins que les fichiers déplacés du daemon
// nomment encore (`crate::helpers`…).
pub(crate) use penelope_app::{helpers, machine};
pub(crate) use penelope_vault::vault_ops;
