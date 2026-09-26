//! Configuration de référence (§19) et **générations à chaud** (§4.4).
//!
//! La configuration effective est une génération immuable `Arc<Config>` publiée via
//! `ArcSwap`. Un tour lit un instantané à son démarrage et le garde jusqu'à sa fin.

use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use arc_swap::ArcSwap;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------- structures

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub owner: Owner,
    pub telegram: Telegram,
    pub providers: Providers,
    pub models: Models,
    pub budget: Budget,
    pub context: Context,
    pub memory: Memory,
    pub mcp: Mcp,
    pub runners: Runners,
    pub sandbox: Sandbox,
    pub observability: Observability,
    pub tools: Tools,
    pub workflows: Workflows,
    pub upgrade: Upgrade,
    pub voice: Voice,
    pub retention: Retention,
    pub backup: Backup,
    pub history: History,
    pub approval: Approval,
}

mod approval;
mod channel;
mod duration;
mod edit;
mod providers;
mod sections;
mod store;
mod validate;
mod write;
pub use approval::*;
pub use channel::*;
pub use duration::*;
pub use edit::edit_toml;
use edit::{SAMPLE_HEADER, unknown_keys};
pub use providers::*;
pub use sections::*;
pub use store::*;
pub use write::atomic_write;
use write::diff_paths;

#[cfg(test)]
mod tests;
