//! `penelope-vault` : la mémoire en fichiers et ce qui la sert (épopée #208, T22).
//!
//! Le vault Markdown est la vérité, l'index est reconstructible : écriture et
//! réindexation (`vault_ops`), wiki de concepts, inventaire, historique git, notes de
//! travail, secrets mis de côté, embeddings, retour d'usage, sujet des sessions,
//! découpage et audit de la mémoire, revue des tours, épisodes, et les tuiles du prompt
//! qui injectent la mémoire (`snapshot`, `tiers`). La crate ne connaît que `penelope-app`
//! et les crates métier, jamais le daemon.

#![forbid(unsafe_code)]

pub mod concepts;
pub mod embeddings;
pub mod episodes;
pub mod mem_audit;
pub mod mem_split;
pub mod review;
pub mod secret_shelf;
pub mod session_notes;
pub mod session_project;
pub mod snapshot;
pub mod tiers;
pub mod usage_feedback;
pub mod vault_git;
pub mod vault_inventory;
pub mod vault_ops;
