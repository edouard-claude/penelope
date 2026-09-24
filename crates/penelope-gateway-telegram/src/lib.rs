//! `penelope-gateway-telegram` : la passerelle Telegram, au-dessus du daemon (épopée #208,
//! T29, `design/v1/decoupage-daemon.md` §3.1).
//!
//! ```text
//!  penelope-cli ──compose──► penelope-gateway-telegram ──appelle──► penelope-daemon
//!                                     ▲                                   │
//!                                     └── Gateway, ChannelDelivery, ──────┘
//!                                         Messenger, OwnerChannel
//! ```

#![forbid(unsafe_code)]

// Modules du daemon sous le chemin qu'ils avaient avant l'extraction : le code déplacé les
// cite par `crate::…` sans changement de corps. T30 réécrira ces chemins.
pub(crate) use penelope_daemon::{
    VERSION, agent, approval_mode, budget_alert, bus, codex_quota, compaction, dream, elicitation,
    episodes, executor, helpers, ingest, mcp_auth, media, mem_audit, onboarding, rpc, runtime,
    scheduler, session_notes, session_ops, session_project, tasks, titles, upgrade, vault_ops,
    workflow,
};
// Cités par les tests seulement.
#[cfg(test)]
pub(crate) use penelope_daemon::{doctor, mcp, runner, supervisor};

pub mod telegram;
#[cfg(test)]
mod ticket_to_deploy_e2e;

pub use telegram::{TelegramGateway, parse_params};
