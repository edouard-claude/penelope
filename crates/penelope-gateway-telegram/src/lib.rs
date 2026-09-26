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

pub mod telegram;
#[cfg(test)]
mod ticket_to_deploy_e2e;

pub use telegram::channel::cards;
pub use telegram::{TelegramGateway, parse_params};

use penelope_app::gateway::Gateway;
use penelope_daemon::runtime::Daemon;
use std::sync::Arc;

/// La passerelle que la composition (`penelope-cli`) passe à `Daemon::run` : construite
/// ici, sans réseau, avant `run`, qui l'annonce avant les serveurs MCP (issue #12) puis la
/// démarre. `None` sans propriétaire ni jeton, ou si le transport ne se construit pas.
pub async fn compose(d: &Arc<Daemon>) -> Option<Arc<dyn Gateway>> {
    match TelegramGateway::from_config(d.core.clone()).await {
        Ok(Some(gw)) => Some(gw),
        Ok(None) => {
            tracing::info!(
                "Telegram non configuré (owner.telegram_user_id ou telegram_bot_token absent)"
            );
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "Telegram non démarré");
            None
        }
    }
}
