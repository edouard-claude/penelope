//! Ingestion de documents, descendue dans `penelope-dream` (T26) : réexportée sous son
//! ancien chemin jusqu'à T30. Les tests qui ont besoin d'un `Daemon` restent ici.

pub use penelope_dream::ingest::*;

#[cfg(test)]
mod tests;
