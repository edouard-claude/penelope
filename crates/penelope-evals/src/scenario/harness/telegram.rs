//! Étape `telegram` : une mise à jour du propriétaire (commande `/…`, message, ou clic
//! sur un bouton) passe par la vraie passerelle Telegram, branchée sur un transport
//! simulé, comme dans les tests de `penelope-gateway-telegram`. Les écrans envoyés
//! pendant l'étape (texte, boutons) deviennent son issue ; l'état en base est relevé par
//! le monde, comme pour les autres étapes (critère 7, épopée #208).
//!
//! La passerelle est construite pour l'étape et relâchée à sa fin : ses jetons de
//! boutons vivent en base (`ActionStore`), un clic d'une étape suivante les retrouve,
//! et aucune copie du daemon ne survit à la vie (un redémarrage l'attend). Elle n'est
//! pas inscrite auprès du courtier d'élicitation, qui n'a pas de retrait.

use super::{HEARTBEAT, Harness, outcome_json};
use anyhow::Context as _;
use penelope_daemon::runner;
use penelope_gateway_telegram::TelegramGateway;
use penelope_telegram::api::method;
use penelope_telegram::mock::{MockTransport, updates};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Le propriétaire de la configuration de test (`Config::sample(42)`), en chat privé.
const OWNER: i64 = 42;
/// Premier identifiant de message rendu par le transport simulé, moins un.
const FIRST_MESSAGE_ID: i64 = 1000;
/// Pas d'attente du travail détaché, et nombre de pas sans nouvel envoi qui le dit fini.
const SETTLE_STEP: Duration = Duration::from_millis(20);
const SETTLE_QUIET: u32 = 5;
const SETTLE_MAX: u32 = 250;

/// Le chat du propriétaire, d'une étape à l'autre et d'une vie à l'autre : le transport
/// garde ses appels (les identifiants de messages en dépendent), les numéros de mise à
/// jour ne reviennent jamais (`tg_updates` déduplique).
pub(super) struct Chat {
    transport: Arc<MockTransport>,
    reported: usize,
    update_id: i64,
}

impl Default for Chat {
    fn default() -> Self {
        Chat {
            transport: MockTransport::new(),
            reported: 0,
            update_id: 0,
        }
    }
}

impl Harness<'_> {
    pub(super) async fn telegram(
        &mut self,
        text: Option<&str>,
        click: Option<&str>,
    ) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        if d.services.config.config().telegram.rate_per_chat_per_s < 1_000.0 {
            // Le limiteur réel dort une seconde par message : inutile ici.
            d.publish_config("scenario", |c| {
                c.telegram.rate_per_chat_per_s = 1_000.0;
                Ok(vec!["telegram.rate_per_chat_per_s".into()])
            })
            .context("limiteur Telegram")?;
        }
        self.telegram.update_id += 1;
        let id = self.telegram.update_id;
        let update = match (text, click) {
            (Some(text), None) => updates::text_message(id, OWNER, OWNER, text),
            (None, Some(label)) => {
                let (data, message) = self.button(label).await?;
                updates::callback(id, OWNER, &data, message)
            }
            _ => anyhow::bail!("`telegram` : `text` ou `click`, l'un des deux"),
        };
        let g = TelegramGateway::with_transport(d.clone(), self.telegram.transport.clone());
        let (messenger, delivery) = (d.hooks.messenger.get(), d.hooks.delivery.get());
        d.hooks.messenger.set(Some(g.clone() as Arc<_>));
        d.hooks.delivery.set(Some(g.clone() as Arc<_>));
        let result = self.play(&g, &update).await;
        d.hooks.messenger.set(messenger);
        d.hooks.delivery.set(delivery);
        drop(g);
        let turns = result?;
        let calls = self.telegram.transport.calls().await;
        let screens: Vec<Value> = calls[self.telegram.reported..]
            .iter()
            .filter_map(|(m, body)| screen(m, body))
            .collect();
        self.telegram.reported = calls.len();
        let mut v = json!({"screens": screens});
        if !turns.is_empty() {
            v["turns"] = json!(turns);
        }
        Ok(v)
    }

    /// La mise à jour, le travail détaché qu'elle lance, les tours qu'elle met en file
    /// (joués comme le pool de runners), puis la file d'envoi vidée.
    async fn play(&self, g: &Arc<TelegramGateway>, update: &Value) -> anyhow::Result<Vec<Value>> {
        g.process_update(update).await?;
        self.settle().await;
        let d = self.daemon()?;
        let mut turns = Vec::new();
        while let Some(turn) = self.claim().await? {
            turns.push(outcome_json(&runner::process(&d, turn, HEARTBEAT).await));
            self.settle().await;
        }
        g.flush_outbox().await?;
        Ok(turns)
    }

    /// Attend que plus rien ne parte pendant `SETTLE_QUIET` pas (clic, export, audit
    /// détachés, issue #69 et #73).
    async fn settle(&self) {
        let (mut last, mut quiet) = (usize::MAX, 0);
        for _ in 0..SETTLE_MAX {
            tokio::time::sleep(SETTLE_STEP).await;
            let n = self.telegram.transport.calls().await.len();
            quiet = if n == last { quiet + 1 } else { 0 };
            last = n;
            if quiet >= SETTLE_QUIET {
                return;
            }
        }
    }

    /// Le bouton dont le libellé contient `label`, sur le dernier message envoyé ou
    /// édité qui en porte un : son jeton et l'identifiant du message.
    async fn button(&self, label: &str) -> anyhow::Result<(String, i64)> {
        let mut next = FIRST_MESSAGE_ID;
        let mut found = None;
        for (m, body) in self.telegram.transport.calls().await {
            let message = match m.as_str() {
                method::SEND_MESSAGE
                | method::SEND_RICH_MESSAGE
                | method::SEND_DOCUMENT
                | method::SEND_VOICE
                | method::SEND_PHOTO
                | method::CREATE_FORUM_TOPIC => {
                    next += 1;
                    next
                }
                _ => body["message_id"].as_i64().unwrap_or(next),
            };
            let hit = buttons(&body)
                .into_iter()
                .flatten()
                .find(|(l, _)| l.contains(label));
            if let Some((_, data)) = hit {
                found = Some((data, message));
            }
        }
        found.with_context(|| format!("aucun bouton « {label} » dans les écrans envoyés"))
    }
}

/// Un appel du transport tel que le propriétaire le voit : méthode, texte ou légende,
/// fichier, libellés des boutons (les jetons sont des identifiants, pas l'écran).
/// Les indicateurs de frappe et les brouillons de flux n'en sont pas.
fn screen(m: &str, body: &Value) -> Option<Value> {
    if matches!(
        m,
        method::SEND_CHAT_ACTION | method::SEND_MESSAGE_DRAFT | method::SEND_RICH_MESSAGE_DRAFT
    ) {
        return None;
    }
    let mut v = json!({"method": m});
    for key in ["text", "caption", "document", "reaction", "name"] {
        if let Some(x) = body.get(key)
            && !x.is_null()
        {
            v[key] = x.clone();
        }
    }
    let labels: Vec<Vec<String>> = buttons(body)
        .into_iter()
        .map(|row| row.into_iter().map(|(l, _)| l).collect())
        .collect();
    if !labels.is_empty() {
        v["buttons"] = json!(labels);
    }
    Some(v)
}

/// Rangées de boutons d'un envoi : (libellé, `callback_data` ou URL).
fn buttons(body: &Value) -> Vec<Vec<(String, String)>> {
    let markup = match &body["reply_markup"] {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    markup["inline_keyboard"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            row.as_array()
                .into_iter()
                .flatten()
                .map(|b| {
                    let target = b["callback_data"].as_str().or(b["url"].as_str());
                    (
                        b["text"].as_str().unwrap_or_default().to_string(),
                        target.unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .collect()
}
