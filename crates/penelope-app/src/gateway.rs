//! Port `Gateway` : un canal que le daemon démarre sans connaître son type (épopée #208,
//! T29, `design/v1/decoupage-daemon.md` §2.2).
//!
//! ```text
//!  penelope-cli : construit la passerelle (sans réseau), puis Daemon::run(Some(gateway))
//!  Daemon::run  : reprise … boucles de fond
//!                 ├── passerelle présente : Broker::expect_owner()   (issue #12)
//!                 ├── serveurs MCP démarrés
//!                 └── Gateway::start : jeton vérifié, ports branchés, boucles lancées
//! ```
//!
//! Le port est ici plutôt que dans le daemon : il ne nomme que des types de la plateforme
//! d'exécution, comme les autres ports (`ChannelDelivery`, `Messenger`, `OwnerChannel`), et
//! une passerelle future qui ne dépendrait que de `penelope-app` pourra l'implémenter.

use std::sync::Arc;

/// Un canal de conversation composé au-dessus du daemon.
///
/// L'ordre est tenu par `Daemon::run` : la seule présence d'une passerelle annonce un
/// propriétaire joignable avant que les serveurs MCP se connectent, pour qu'une
/// élicitation sache qu'elle aura une réponse ; `start` n'est appelé qu'après.
#[async_trait::async_trait]
pub trait Gateway: Send + Sync {
    /// Nom du canal, pour les journaux (« Telegram non démarré »).
    fn name(&self) -> &'static str;

    /// Vérifie l'accès au canal, se branche dans le daemon (`Messenger`,
    /// `ChannelDelivery`, `OwnerChannel`) et lance ses boucles supervisées. Une erreur
    /// laisse le daemon tourner sans ce canal.
    async fn start(self: Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String>;
}
