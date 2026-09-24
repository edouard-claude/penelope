//! `penelope-context` : moteur de contexte, compaction 0..4, LCM, assemblage du prompt (§5).

#![forbid(unsafe_code)]

pub mod anchors;
pub mod compaction;
pub mod engine;
pub mod journal;
pub mod lcm;
pub mod store;
pub mod tiers;
pub mod transcript;

pub use anchors::{Anchor, AnchorKind};
pub use compaction::{AppliedStep, CompactionParams, Cooldown, Projection};
pub use engine::{ContextEngine, SummaryJob, TurnContext};
pub use lcm::{Lcm, Manifest, Node, NodeKind};
pub use store::{Artifact, GrepHit, HistoryStore};
pub use tiers::{Tiers, TiersBuilder};
pub use transcript::{Entry, Group, GroupKind};
