//! Étape `telegram` : une mise à jour du propriétaire (commande `/…`, message, ou clic
//! sur un bouton) passe par la vraie passerelle Telegram, branchée sur un transport
//! simulé, comme dans les tests de `penelope-gateway-telegram`. Les écrans envoyés
//! pendant l'étape (texte, boutons) deviennent son issue ; l'état en base est relevé par
//! le monde, comme pour les autres étapes (critère 7, épopée #208).
//!
//! La boucle de la trace des outils tourne pendant l'étape (#222) : sa bulle est créée par
//! la file d'envoi, et sa seule modification part une fois la file vidée, après la
//! réponse ; le rejeu ne dépend pas du minutage.
//!
//! La passerelle est construite pour l'étape et relâchée à sa fin : ses jetons de
//! boutons vivent en base (`ActionStore`), un clic d'une étape suivante les retrouve,
//! et aucune copie du daemon ne survit à la vie (un redémarrage l'attend). Elle n'est
//! pas inscrite auprès du courtier d'élicitation, qui n'a pas de retrait.
//!
//! Un clic sur une opération travaille dans une tâche détachée (issue #73) : l'étape ne
//! se conclut pas tant qu'elle court (`clicks_idle`), quel que soit son silence ; sur un
//! runner chargé, « Vas-y » mettait parfois plus de 100 ms avant sa première carte, qui
//! glissait dans l'étape suivante (#269).

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
/// Pas d'attente du travail détaché, nombre de pas sans nouvel envoi qui le dit fini, et
/// plafond de pas au-delà duquel un clic encore en vol est une erreur de l'étape.
const SETTLE_STEP: Duration = Duration::from_millis(20);
const SETTLE_QUIET: u32 = 5;
const SETTLE_MAX: u32 = 250;
/// Passages du pilote au plus, dans une étape `drive`.
const DRIVE_PASSES: usize = 20;

/// Le chat du propriétaire, d'une étape à l'autre et d'une vie à l'autre : le transport
/// garde ses appels (les identifiants de messages en dépendent), les numéros de mise à
/// jour ne reviennent jamais (`tg_updates` déduplique).
pub(super) struct Chat {
    pub(super) transport: Arc<MockTransport>,
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
        let g = self.gateway(&d);
        let (messenger, delivery) = (d.hooks.messenger.get(), d.hooks.delivery.get());
        d.hooks.messenger.set(Some(g.clone() as Arc<_>));
        d.hooks.delivery.set(Some(g.clone() as Arc<_>));
        let result = self.play(&g, &update).await;
        d.hooks.messenger.set(messenger);
        d.hooks.delivery.set(delivery);
        drop(g);
        let turns = result?;
        let mut v = self.screens().await;
        if !turns.is_empty() {
            v["turns"] = json!(turns);
        }
        // Un arrêt demandé depuis le chat dit son origine, comme par la socket (#225).
        if d.handle.is_shutting_down() {
            v["stop"] = json!(d.handle.stop_reason());
        }
        Ok(v)
    }

    fn gateway(&self, d: &penelope_daemon::runtime::Daemon) -> Arc<TelegramGateway> {
        TelegramGateway::with_transport(d.core.clone(), self.telegram.transport.clone())
    }

    /// Étape `drive` (#191) : le pilote passe sur les runs, la passerelle branchée pour
    /// leurs cartes, jusqu'à ce qu'un passage ne change plus rien.
    pub(super) async fn drive(&mut self) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        let cx = penelope_daemon::workflow::orchestrator_of(&d.core).context;
        let g = self.gateway(&d);
        let (messenger, delivery) = (d.hooks.messenger.get(), d.hooks.delivery.get());
        d.hooks.messenger.set(Some(g.clone() as Arc<_>));
        d.hooks.delivery.set(Some(g.clone() as Arc<_>));
        let result = drive_until_still(&cx).await;
        let flushed = g.flush_outbox().await;
        d.hooks.messenger.set(messenger);
        d.hooks.delivery.set(delivery);
        drop(g);
        let runs = result?;
        flushed?;
        let mut v = self.screens().await;
        v["runs"] = json!(runs);
        Ok(v)
    }

    /// Les écrans envoyés depuis la dernière étape qui les a relevés.
    async fn screens(&mut self) -> Value {
        let calls = self.telegram.transport.calls().await;
        let fresh = &calls[self.telegram.reported..];
        let screens: Vec<Value> = fresh.iter().filter_map(|(m, b)| screen(m, b)).collect();
        // Les réactions partent de tâches détachées : leur place parmi les envois n'est
        // pas reproductible, leur ordre entre elles l'est.
        let reactions: Vec<Value> = fresh
            .iter()
            .filter(|(m, _)| m == method::SET_MESSAGE_REACTION)
            .flat_map(|(_, b)| b["reaction"].as_array().cloned().unwrap_or_default())
            .map(|r| r["emoji"].clone())
            .collect();
        self.telegram.reported = calls.len();
        let mut v = json!({"screens": screens});
        if !reactions.is_empty() {
            v["reactions"] = json!(reactions);
        }
        v
    }

    /// La mise à jour, le travail détaché qu'elle lance, les tours qu'elle met en file
    /// (joués comme le pool de runners), puis la file d'envoi vidée.
    async fn play(&self, g: &Arc<TelegramGateway>, update: &Value) -> anyhow::Result<Vec<Value>> {
        // La trace des outils (#222) suit le bus : abonnée avant la mise à jour, elle
        // pose sa bulle au premier appel et la close une fois la file vidée.
        let trace = g.spawn_trace();
        let turns = self.play_update(g, update).await;
        for _ in 0..SETTLE_MAX {
            if g.traces_idle() && g.clicks_idle() {
                break;
            }
            tokio::time::sleep(SETTLE_STEP).await;
            g.flush_outbox().await?;
        }
        trace.abort();
        turns
    }

    async fn play_update(
        &self,
        g: &Arc<TelegramGateway>,
        update: &Value,
    ) -> anyhow::Result<Vec<Value>> {
        g.process_update(update).await?;
        self.settle(g).await?;
        let d = self.daemon()?;
        let mut turns = Vec::new();
        while let Some(turn) = self.claim().await? {
            turns.push(outcome_json(&runner::process(&d, turn, HEARTBEAT).await));
            self.settle(g).await?;
        }
        g.flush_outbox().await?;
        Ok(turns)
    }

    /// Attend que plus rien ne parte pendant `SETTLE_QUIET` pas (export, audit détachés,
    /// issue #69) et qu'aucun clic ne travaille plus (issue #73, #269) : le silence d'une
    /// tâche qui n'a pas encore envoyé sa carte ne conclut pas l'étape. Un clic qui court
    /// encore au plafond est une erreur, jamais une attente sans fin.
    async fn settle(&self, g: &TelegramGateway) -> anyhow::Result<()> {
        let (mut last, mut quiet) = (usize::MAX, 0);
        for _ in 0..SETTLE_MAX {
            tokio::time::sleep(SETTLE_STEP).await;
            let n = self.telegram.transport.calls().await.len();
            quiet = if n == last { quiet + 1 } else { 0 };
            last = n;
            if quiet >= SETTLE_QUIET && g.clicks_idle() {
                return Ok(());
            }
        }
        anyhow::ensure!(
            g.clicks_idle(),
            "le travail détaché d'un clic court encore après {} s : l'étape ne peut pas conclure",
            (SETTLE_STEP * SETTLE_MAX).as_secs()
        );
        Ok(())
    }

    /// Le bouton dont le libellé contient `label`, sur le dernier message envoyé ou
    /// édité qui en porte un : son jeton et l'identifiant du message.
    async fn button(&self, label: &str) -> anyhow::Result<(String, i64)> {
        let mut next = FIRST_MESSAGE_ID;
        let mut found = None;
        let mut seen: Vec<String> = Vec::new();
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
            let row: Vec<(String, String)> = buttons(&body).into_iter().flatten().collect();
            seen.extend(row.iter().map(|(l, _)| l.clone()));
            let hit = row.into_iter().find(|(l, _)| l.contains(label));
            if let Some((_, data)) = hit {
                found = Some((data, message));
            }
        }
        found.with_context(|| {
            format!(
                "aucun bouton « {label} » dans les écrans envoyés ; boutons vus : {}",
                seen.join(" | ")
            )
        })
    }
}

/// Passages du pilote jusqu'au repos : chaque run `running` avance, dans l'ordre de
/// démarrage, tant qu'un passage fait bouger un run (borné). Rend l'état de chaque run.
async fn drive_until_still(cx: &penelope_orchestrator::Context) -> anyhow::Result<Vec<Value>> {
    use penelope_workflow::RunState;
    let s = &cx.services;
    let snapshot = || async {
        let mut runs = s.runs.list(None, 50).await?;
        runs.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        anyhow::Ok(runs)
    };
    let mut before = snapshot().await?;
    for _ in 0..DRIVE_PASSES {
        for run in before.iter().filter(|r| r.state == RunState::Running) {
            penelope_orchestrator::workflow::drive(cx, &run.id).await?;
        }
        let after = snapshot().await?;
        if after == before {
            break;
        }
        before = after;
    }
    Ok(before
        .iter()
        .map(|r| {
            json!({"run": r.id, "workflow": r.workflow_id, "state": r.state.as_str(),
                   "step": r.current_step, "iterations": r.iterations, "error": r.error})
        })
        .collect())
}

/// Un appel du transport tel que le propriétaire le voit : méthode, texte ou légende,
/// fichier, libellés des boutons (les jetons sont des identifiants, pas l'écran).
/// Les indicateurs de frappe, les brouillons de flux et les réactions n'en sont pas.
fn screen(m: &str, body: &Value) -> Option<Value> {
    if matches!(
        m,
        method::SEND_CHAT_ACTION
            | method::SEND_MESSAGE_DRAFT
            | method::SEND_RICH_MESSAGE_DRAFT
            | method::SET_MESSAGE_REACTION
    ) {
        return None;
    }
    let mut v = json!({"method": m});
    for key in ["text", "caption", "document", "name"] {
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
