//! `/secrets <projet>` (#350) : un bloc de credentials collé dans le message suivant est
//! rangé au magasin de secrets **sans qu'aucun modèle ne le voie**.
//!
//! ```text
//!  /secrets mentaleco ──► kv tg.await_secrets.<chat>.<sujet> = "mentaleco <ms>" (5 min)
//!  bloc collé ──► capture() AVANT l'écriture de tg_updates :
//!                 ligne de dédoublonnage sans contenu, découpage, magasin, caviardage,
//!                 message supprimé, réponse qui ne donne que les noms
//! ```
//!
//! Le bloc peut aussi suivre la commande dans le même message. Aucun tour n'est mis en
//! file, rien du bloc n'entre en base, dans les journaux ni dans un prompt.

use super::*;

/// Durée pendant laquelle le message suivant est attendu.
const WAIT_MS: i64 = 5 * 60_000;

fn await_key(chat_id: i64, topic_id: Option<i64>) -> String {
    format!("tg.await_secrets.{chat_id}.{}", topic_id.unwrap_or(0))
}

impl TelegramGateway {
    /// `/secrets <projet>` : arme la capture du message suivant, ou range tout de suite
    /// un bloc collé après la commande (déjà intercepté par [`Self::capture_secrets`]).
    pub(super) async fn cmd_secrets(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        args: &str,
    ) -> anyhow::Result<()> {
        let project = args.split_whitespace().next().unwrap_or_default();
        if project.is_empty() {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    Some(message_id),
                    "Usage : `/secrets <projet>`, puis colle le bloc de credentials dans le \
                     message suivant. Il est rangé au trousseau sans passer par un modèle, \
                     puis effacé du chat.",
                )
                .await;
        }
        let s = &self.daemon.services;
        s.kv_set(
            &await_key(chat_id, topic_id),
            &format!("{project} {}", s.clock.now_ms()),
        )
        .await?;
        self.reply(
            chat_id,
            topic_id,
            Some(message_id),
            &format!(
                "🔐 Colle le bloc de `{project}` dans ton prochain message (5 min). Il ne passe \
                 par aucun modèle : rangé au trousseau, puis effacé du chat. Sections \
                 (`DEV :`, `[qa]`) et lignes `clé : valeur` ou `CLÉ=valeur`."
            ),
        )
        .await
    }

    /// Avant toute écriture : si `update` porte un bloc de secrets attendu (ou collé après
    /// `/secrets <projet>`), le range et rend `true` ; l'update ne va pas plus loin.
    pub(super) async fn capture_secrets(
        self: &Arc<Self>,
        update: &Value,
        access: &penelope_telegram::Access,
    ) -> anyhow::Result<bool> {
        let s = &self.daemon.services;
        let (update_id, chat_id, topic_id, message_id, project, block) =
            match classify(update, access) {
                Incoming::Command {
                    update_id,
                    chat_id,
                    from_id,
                    message_id,
                    topic_id,
                    command,
                    args,
                } if command == "secrets" && from_id == self.owner_id && args.contains('\n') => {
                    let (project, block) = args.split_once('\n').unwrap_or_default();
                    let project = project.trim().to_string();
                    (
                        update_id,
                        chat_id,
                        topic_id,
                        message_id,
                        project,
                        block.to_string(),
                    )
                }
                Incoming::Text {
                    update_id,
                    chat_id,
                    from_id,
                    message_id,
                    topic_id,
                    text,
                    ..
                } if from_id == self.owner_id => {
                    let key = await_key(chat_id, topic_id);
                    let Some(raw) = s.kv_get(&key).await?.filter(|r| !r.is_empty()) else {
                        return Ok(false);
                    };
                    let (project, at) = raw.split_once(' ').unwrap_or((raw.as_str(), "0"));
                    if s.clock.now_ms() - at.parse::<i64>().unwrap_or(0) > WAIT_MS {
                        s.kv_set(&key, "").await?;
                        return Ok(false);
                    }
                    // Un collage que Telegram coupe à 4 096 caractères : la suite est
                    // attendue encore une minute.
                    if text.chars().count() >= super::bursts::TELEGRAM_TEXT_LIMIT {
                        s.kv_set(
                            &key,
                            &format!("{project} {}", s.clock.now_ms() - WAIT_MS + 60_000),
                        )
                        .await?;
                    } else {
                        s.kv_set(&key, "").await?;
                    }
                    (
                        update_id,
                        chat_id,
                        topic_id,
                        message_id,
                        project.to_string(),
                        text,
                    )
                }
                _ => return Ok(false),
            };
        // La ligne de dédoublonnage, sans le contenu : rien du bloc n'entre en base.
        let (now, id) = (s.clock.now_rfc3339(), update_id);
        let fresh = s
            .store
            .write(move |tx| {
                Ok(tx.execute(
                    "INSERT OR IGNORE INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(?1, ?2, 1, '{}')",
                    params![id, now],
                )? == 1)
            })
            .await?;
        if !fresh {
            return Ok(true);
        }
        let note = self.store_block(&project, &block).await;
        if let Err(e) = self.bot.delete_message(chat_id, message_id).await {
            tracing::warn!(error = %e, "bloc de secrets non effacé du chat");
        }
        self.reply(chat_id, topic_id, None, &note).await?;
        Ok(true)
    }

    /// Découpe et range `block` ; rend la réponse, qui ne cite que des noms et des
    /// numéros de ligne.
    async fn store_block(&self, project: &str, block: &str) -> String {
        let parsed = match penelope_platform::secrets::import::parse_block(project, block) {
            Ok(p) => p,
            Err(e) => return format!("⚠️ Rien n'a été rangé : {e}."),
        };
        let platform = self.daemon.services.platform.clone();
        let entries = parsed.entries.clone();
        let stored = tokio::task::spawn_blocking(move || {
            entries
                .into_iter()
                .map(|(name, value)| {
                    penelope_observe::register_secret(&value);
                    let ok = platform.secrets.set(&name, &value).is_ok();
                    (name, ok)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut lines = vec![format!(
            "🔐 `{project}` : {} secret(s) rangé(s) au trousseau, message effacé.",
            stored.iter().filter(|(_, ok)| *ok).count()
        )];
        for (name, ok) in &stored {
            lines.push(format!(
                "{} `{name}`",
                if *ok { "✓" } else { "✗ non rangé :" }
            ));
        }
        if !parsed.unread.is_empty() {
            let n: Vec<String> = parsed.unread.iter().map(|n| n.to_string()).collect();
            lines.push(format!(
                "Lignes non reconnues (rien n'en est cité) : {}.",
                n.join(", ")
            ));
        }
        lines.push(
            "Pénélope les pose en CI par leur nom avec `ci_variable_set`, sans voir les valeurs."
                .into(),
        );
        lines.join("\n")
    }
}
