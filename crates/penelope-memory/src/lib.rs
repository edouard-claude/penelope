//! `penelope-memory` : vault, index, provenance, rappel, consolidation, intentions (§6).
//!
//! Principes appliqués (§6.1) :
//! 1. **Écrire est la partie difficile** : la curation est hors du chemin de réponse.
//! 2. **Pas d'état caché** : tout savoir durable est un fichier Markdown lisible en SSH.
//! 3. **Portes déterministes, jugement du modèle à l'intérieur.**
//! 4. **L'humain est ambigu** : le savoir est modélisé en règles défaisables.
//! 5. **La mémoire ne bloque jamais une réponse** (150 ms et repli).
//! 6. **Le chemin d'écriture est la frontière de sécurité.**

#![forbid(unsafe_code)]

pub mod candidates;
pub mod consolidation;
pub mod index;
pub mod intents;
pub mod provenance;
pub mod recall;
pub mod vault;

pub use candidates::{Candidate, CandidateStore, CandidateType};
pub use consolidation::{DreamReport, Gate, Operation, PromotionGates};
pub use index::{IndexedEntry, MemoryIndex, SearchFilter};
pub use intents::{Intent, IntentStore};
pub use provenance::{Origin, Provenance, TurnContamination};
pub use recall::{CurrentContext, Recall, RecallParams, RecallResult, Snapshots};
pub use vault::{Level, Practice, When};
