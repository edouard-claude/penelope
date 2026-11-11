//! Les cartes d'une demande retirée (#464) : son demandeur a disparu (sous-agent arrêté
//! ou perdu au redémarrage, run terminé, session close), il n'y a plus rien à approuver.
//!
//! Le cœur retire la demande (`penelope_orchestrator::withdrawn`) et prévient le canal,
//! qui désarme ses cartes : le texte dit qu'elle n'attend plus, les boutons disparaissent.
//! Une carte qui n'a pas pu être désarmée (pas encore partie, ligne purgée) reste
//! cliquable : le clic répond alors qu'il n'y a plus rien à approuver, et rien d'autre.

use super::*;
use penelope_telegram::render::escape_html;

/// Limite de Telegram pour un texte, en unités UTF-16.
const TEXT_MAX: usize = 4_096;

impl TelegramGateway {
    /// Désarme les cartes déjà parties de la demande `approval_id`, relances comprises :
    /// on les retrouve par les jetons de leurs boutons. Rend leur nombre.
    pub(super) async fn retire_cards(&self, approval_id: &str, why: &str) -> anyhow::Result<usize> {
        let id = approval_id.to_string();
        let cards: Vec<(i64, i64, String)> = self
            .daemon
            .services
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT o.chat_id, o.message_id, o.payload FROM tg_outbox o
                     WHERE o.state = 'sent' AND o.message_id IS NOT NULL
                       AND EXISTS (SELECT 1 FROM tg_actions t
                                   WHERE t.target = ?1 AND instr(o.payload, t.token) > 0)",
                )?;
                let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let line = format!("⌛ <i>Demande retirée : {}.</i>", escape_html(why));
        for (chat_id, message_id, payload) in &cards {
            let payload: Value = serde_json::from_str(payload).unwrap_or_default();
            // Le texte de la carte, complété, quand il est en HTML et que la ligne y tient ;
            // sinon les boutons seuls sont retirés.
            let told = match payload["text"].as_str() {
                Some(text)
                    if payload["parse_mode"] == "HTML"
                        && text.encode_utf16().count() + line.encode_utf16().count() + 2
                            <= TEXT_MAX =>
                {
                    let html = format!("{text}\n\n{line}");
                    self.bot
                        .edit_text(*chat_id, *message_id, &html, None)
                        .await
                        .is_ok()
                }
                _ => false,
            };
            if !told {
                let _ = self.bot.edit_markup(*chat_id, *message_id, None).await;
            }
        }
        Ok(cards.len())
    }

    /// Un clic sur une demande qui n'attend plus de décision : retirée, expirée ou
    /// annulée. On le dit, et rien n'est tranché. Vrai : le clic est traité.
    pub(super) async fn nothing_to_approve(
        &self,
        approval: &ApprovalRequest,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<bool> {
        let s = &self.daemon.services;
        let gone = penelope_orchestrator::withdrawn::nothing_to_decide;
        let Some(why) = gone(s, Some(self as &dyn Messenger), approval).await? else {
            return Ok(false);
        };
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        let note = format!(
            "⌛ Plus rien à approuver pour `{}` : {why}.",
            approval.subject
        );
        self.reply(chat_id, topic_id, None, &note).await?;
        Ok(true)
    }
}
