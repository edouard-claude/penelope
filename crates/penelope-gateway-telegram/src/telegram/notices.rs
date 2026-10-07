//! Annonces d'écart de modèle (#333) : le cœur les verse au journal
//! (`penelope_app::model_watch`), le canal les écrit dans la conversation concernée.
//!
//! ```text
//!  model.notice ─► origine connue ? ──► son chat et son sujet
//!                └► session d'un chat ? ──► le chat et le sujet de la session
//!                └► sinon ──────────────► le foyer (`telegram.home`)
//! ```

use super::*;
use penelope_app::model_watch::NOTICE_EVENT;

impl TelegramGateway {
    /// Écrit chaque annonce d'écart au moment où le cœur la publie.
    pub(super) async fn notice_loop(self: Arc<Self>) {
        let mut rx = self.daemon.services.events.subscribe();
        while !self.shutting_down() {
            let ev = match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Err(_) => continue,
                Ok(Ok(ev)) => ev,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
            };
            if ev.kind != NOTICE_EVENT {
                continue;
            }
            let Some(text) = ev.payload["text"].as_str().filter(|t| !t.is_empty()) else {
                continue;
            };
            let (chat_id, topic_id) = self.notice_place(&ev).await;
            if let Err(e) = self.reply(chat_id, topic_id, None, text).await {
                tracing::warn!(error = %e, "annonce d'écart de modèle non livrée");
            }
        }
    }

    /// Où écrire une annonce : l'origine qu'elle porte, la conversation de sa session,
    /// sinon le foyer.
    pub(super) async fn notice_place(
        &self,
        ev: &penelope_kernel::event::Event,
    ) -> (i64, Option<i64>) {
        let origin = Origin::from_payload(&ev.payload);
        if let Some(place) = origin.telegram_chat() {
            return place;
        }
        if let Some(sid) = &ev.session_id
            && let Ok(Some(sess)) = self.daemon.services.sessions.get(sid).await
            && let Some(chat) = sess.tg_chat_id
        {
            return (chat, sess.tg_topic_id);
        }
        self.home_chat()
    }
}
