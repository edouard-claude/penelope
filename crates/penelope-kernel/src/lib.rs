//! `penelope-kernel` : event log, ledger d'effets, générations de configuration.
//!
//! Le noyau ne dépend d'aucun crate métier : il fournit les contrats (`api`), les
//! primitives durables (`event`, `effects`) et la configuration (`config`) dont tous les
//! autres crates se servent. `penelope-store` est une infrastructure, pas un crate métier
//! (voir `docs/decisions/0001-kernel-depend-de-store.md`).
//!
//! Principes appliqués ici (§0) :
//! - **tout est durable** : aucun état métier en mémoire seule ;
//! - **aucun effet sans trace** : le ledger précède l'exécution ;
//! - **tout est à chaud** : la configuration est une génération immuable publiée par
//!   `ArcSwap`, jamais mutée en place.

#![forbid(unsafe_code)]

pub mod api;
pub mod budget;
pub mod canonical;
pub mod clock;
pub mod coherence;
pub mod config;
pub mod cron;
pub mod effects;
pub mod error;
pub mod event;
pub mod frontmatter;
pub mod ids;
pub mod risk;
pub mod schema;
pub mod session;
pub mod turn;

pub use clock::{Clock, SharedClock, SystemClock, TestClock};
pub use config::{ApplyResult, Config, ConfigStore, Generation};
pub use effects::{Effect, EffectKind, EffectLedger, EffectSpec, EffectState, Planned};
pub use error::{KernelError, Result};
pub use event::{Event, EventDraft, EventLog, VerifyReport};
pub use ids::{ApprovalId, ArtifactId, EffectId, NodeId, RunId, SessionId, TurnId, Ulid};
pub use risk::{PolicyDecision, PolicyWindow, RiskClass};

/// Version du binaire, injectée par Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
