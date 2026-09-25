//! Bus d'événements du daemon, registre des tours actifs, attente d'une réponse.
//!
//! Deux garanties différentes, deux mécanismes :
//! - les **fragments** (brouillons, `penelope tail`) passent par un `broadcast` : un
//!   abonné lent peut en perdre, ce n'est qu'un aperçu ;
//! - l'**issue** d'un tour passe aussi par des attentes nominatives et un petit cache :
//!   elle n'est jamais perdue, même si l'attente arrive après la fin du tour.

use crate::outcome::{TurnEvent, TurnOutcome};
use penelope_llm::CancelToken;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, broadcast, oneshot};

/// D'où vient un tour, donc où va sa réponse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Origin {
    Telegram {
        chat_id: i64,
        #[serde(default)]
        topic_id: Option<i64>,
        #[serde(default)]
        message_id: Option<i64>,
    },
    Cli,
    Internal {
        source: String,
    },
}

impl Origin {
    /// Lit `payload.origin` ; un tour sans origine connue est interne.
    pub fn from_payload(payload: &Value) -> Origin {
        payload
            .get("origin")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(Origin::Internal {
                source: "inconnu".into(),
            })
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn telegram_chat(&self) -> Option<(i64, Option<i64>)> {
        match self {
            Origin::Telegram {
                chat_id, topic_id, ..
            } => Some((*chat_id, *topic_id)),
            _ => None,
        }
    }
}

/// Livraison durable de l'issue d'un tour vers son canal (Telegram).
///
/// Ne passe pas par le `broadcast` : une réponse finale ou une carte d'approbation ne
/// doivent jamais être perdues parce qu'un abonné a pris du retard.
#[async_trait::async_trait]
pub trait ChannelDelivery: Send + Sync {
    async fn deliver(
        &self,
        turn_id: &str,
        session_id: &str,
        origin: &Origin,
        outcome: &TurnOutcome,
    );

    /// Seuils de la carte de rafale pour un tour né de `origin` (issues #49, #161) :
    /// `None`, le canal n'en pose pas pour cette origine.
    fn burst_limits(&self, _origin: &Origin) -> Option<BurstLimits> {
        None
    }

    /// Propose les actions de la carte de rafale pour des messages déjà en file.
    async fn offer_burst(
        &self,
        _session_id: &str,
        _origin: &Origin,
        _parts: Vec<String>,
    ) -> Result<(), String> {
        Err("canal sans carte de rafale".into())
    }

    /// Une session vient de recevoir son titre automatique.
    async fn session_titled(&self, _session_id: &str, _title: &str) {}

    /// Alerte d'une planification qui n'a pas pu s'exécuter, avec ses boutons (issue #39).
    async fn schedule_alert(
        &self,
        _origin: &Origin,
        _schedule_id: &str,
        _text: &str,
    ) -> Result<(), String> {
        Err("canal sans alerte de planification".into())
    }
}

/// Seuils d'une rafale : au-delà de `messages` messages ou de `chars` caractères
/// depuis le début du tour, le canal demande quoi en faire au lieu d'appeler le modèle.
/// Zéro : pas de seuil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstLimits {
    pub messages: usize,
    pub chars: usize,
}

impl BurstLimits {
    pub const fn new(messages: usize, chars: usize) -> Self {
        BurstLimits { messages, chars }
    }

    /// Vrai si ces morceaux de texte dépassent l'un des seuils.
    pub fn exceeded(&self, parts: &[String]) -> bool {
        let chars: usize = parts.iter().map(|part| part.chars().count()).sum();
        (self.messages > 0 && parts.len() >= self.messages)
            || (self.chars > 0 && chars >= self.chars)
    }
}

/// Un événement publié sur le bus.
#[derive(Debug, Clone)]
pub struct BusEvent {
    pub turn_id: String,
    pub session_id: String,
    pub origin: Origin,
    pub kind: BusKind,
}

#[derive(Debug, Clone)]
pub enum BusKind {
    Started,
    Event(TurnEvent),
    Finished(TurnOutcome),
}

/// Un tour en cours d'exécution.
#[derive(Clone)]
pub struct ActiveTurn {
    pub turn_id: String,
    pub session_id: String,
    pub origin: Origin,
    pub cancel: CancelToken,
    /// Identifiant du brouillon Telegram de ce tour : entier non nul, stable.
    pub draft_id: i64,
}

const RECENT_OUTCOMES: usize = 256;

pub struct Bus {
    tx: broadcast::Sender<Arc<BusEvent>>,
    waiters: Mutex<HashMap<String, Vec<oneshot::Sender<TurnOutcome>>>>,
    recent: Mutex<VecDeque<(String, TurnOutcome)>>,
    active: Mutex<HashMap<String, ActiveTurn>>,
    /// Ingestions de documents en cours, par session (issue #155). Elles vivent hors d'un
    /// tour — le document arrive, le résumé part au modèle en tâche détachée — donc
    /// `cancel_session` ne les voyait pas : `/stop tout` promettait de les mettre en pause
    /// et ne touchait rien.
    ingests: Mutex<HashMap<String, Vec<(u64, CancelToken)>>>,
    next_ingest: std::sync::atomic::AtomicU64,
    enqueued: Notify,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(2048);
        Bus {
            tx,
            waiters: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            active: Mutex::new(HashMap::new()),
            ingests: Mutex::new(HashMap::new()),
            next_ingest: std::sync::atomic::AtomicU64::new(1),
            enqueued: Notify::new(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BusEvent>> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: BusEvent) {
        // Aucun abonné n'est pas une erreur.
        let _ = self.tx.send(Arc::new(event));
    }

    /// Attend l'issue d'un tour. Si le tour est déjà fini, la réponse est immédiate.
    pub fn wait_for(&self, turn_id: &str) -> oneshot::Receiver<TurnOutcome> {
        let (tx, rx) = oneshot::channel();
        let recent = lock(&self.recent);
        if let Some((_, o)) = recent.iter().find(|(id, _)| id == turn_id) {
            let _ = tx.send(o.clone());
            return rx;
        }
        // Le verrou `recent` est tenu pendant l'enregistrement : `finish` ne peut pas
        // passer entre la vérification et l'inscription.
        lock(&self.waiters)
            .entry(turn_id.to_string())
            .or_default()
            .push(tx);
        drop(recent);
        rx
    }

    /// Publie l'issue d'un tour et réveille ceux qui l'attendent.
    pub fn finish(&self, turn_id: &str, session_id: &str, origin: &Origin, outcome: TurnOutcome) {
        {
            let mut recent = lock(&self.recent);
            recent.push_back((turn_id.to_string(), outcome.clone()));
            while recent.len() > RECENT_OUTCOMES {
                recent.pop_front();
            }
            if let Some(ws) = lock(&self.waiters).remove(turn_id) {
                for w in ws {
                    let _ = w.send(outcome.clone());
                }
            }
        }
        self.publish(BusEvent {
            turn_id: turn_id.to_string(),
            session_id: session_id.to_string(),
            origin: origin.clone(),
            kind: BusKind::Finished(outcome),
        });
    }

    /// Enregistre un tour qui démarre et renvoie son jeton d'annulation.
    pub fn begin(&self, turn_id: &str, session_id: &str, origin: &Origin) -> ActiveTurn {
        let active = ActiveTurn {
            turn_id: turn_id.to_string(),
            session_id: session_id.to_string(),
            origin: origin.clone(),
            cancel: CancelToken::new(),
            draft_id: draft_id_for(turn_id),
        };
        lock(&self.active).insert(session_id.to_string(), active.clone());
        self.publish(BusEvent {
            turn_id: turn_id.to_string(),
            session_id: session_id.to_string(),
            origin: origin.clone(),
            kind: BusKind::Started,
        });
        active
    }

    pub fn end(&self, session_id: &str, turn_id: &str) {
        let mut g = lock(&self.active);
        if g.get(session_id)
            .map(|a| a.turn_id == turn_id)
            .unwrap_or(false)
        {
            g.remove(session_id);
        }
    }

    /// Un tour de cette session est-il en cours ?
    pub fn is_active(&self, session_id: &str) -> bool {
        lock(&self.active).contains_key(session_id)
    }

    /// Annule le tour en cours d'une session. Vrai s'il y en avait un.
    pub fn cancel_session(&self, session_id: &str) -> bool {
        match lock(&self.active).get(session_id) {
            Some(a) => {
                a.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Déclare une ingestion en cours pour cette session : rend son identifiant et son
    /// jeton. L'identifiant sert à la retirer, `CancelToken` n'étant pas comparable.
    pub fn start_ingest(&self, session_id: &str) -> (u64, CancelToken) {
        let id = self
            .next_ingest
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let token = CancelToken::new();
        lock(&self.ingests)
            .entry(session_id.to_string())
            .or_default()
            .push((id, token.clone()));
        (id, token)
    }

    /// Retire une ingestion terminée.
    pub fn end_ingest(&self, session_id: &str, id: u64) {
        let mut g = lock(&self.ingests);
        if let Some(v) = g.get_mut(session_id) {
            v.retain(|(k, _)| *k != id);
            if v.is_empty() {
                g.remove(session_id);
            }
        }
    }

    /// Annule les ingestions en cours d'une session, et rend leur nombre (issue #155).
    pub fn cancel_ingests(&self, session_id: &str) -> usize {
        match lock(&self.ingests).get(session_id) {
            Some(v) => {
                for (_, t) in v {
                    t.cancel();
                }
                v.len()
            }
            None => 0,
        }
    }

    /// Combien d'ingestions tournent pour cette session.
    pub fn ingests_of(&self, session_id: &str) -> usize {
        lock(&self.ingests).get(session_id).map_or(0, |v| v.len())
    }

    /// Annule un tour précis, s'il est bien celui qui tourne pour cette session. Vrai
    /// s'il y en avait un (#43 : un lease perdu n'annule pas le tour de son successeur).
    pub fn cancel_turn(&self, session_id: &str, turn_id: &str) -> bool {
        match lock(&self.active).get(session_id) {
            Some(a) if a.turn_id == turn_id => {
                a.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Retrouve la session d'un brouillon arrêté par le bouton « stop » de Telegram.
    pub fn session_for_draft(&self, draft_id: i64) -> Option<String> {
        lock(&self.active)
            .values()
            .find(|a| a.draft_id == draft_id)
            .map(|a| a.session_id.clone())
    }

    pub fn active_turns(&self) -> Vec<ActiveTurn> {
        lock(&self.active).values().cloned().collect()
    }

    /// Signale qu'un tour vient d'être mis en file.
    pub fn notify_enqueued(&self) {
        self.enqueued.notify_waiters();
    }

    /// Attend un tour mis en file, ou l'échéance.
    pub async fn wait_enqueued(&self, timeout: std::time::Duration) {
        let _ = tokio::time::timeout(timeout, self.enqueued.notified()).await;
    }
}

/// Identifiant de brouillon : dérivé du tour, non nul, dans les entiers positifs.
pub fn draft_id_for(turn_id: &str) -> i64 {
    let h = penelope_kernel::canonical::sha256_hex(turn_id.as_bytes());
    let v = i64::from_str_radix(&h[..15], 16).unwrap_or(1);
    v.max(1)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Sink qui publie les événements d'un tour sur le bus.
pub struct BusSink {
    pub bus: Arc<Bus>,
    pub turn_id: String,
    pub session_id: String,
    pub origin: Origin,
}

impl crate::outcome::TurnSink for BusSink {
    fn emit(&self, event: TurnEvent) {
        self.bus.publish(BusEvent {
            turn_id: self.turn_id.clone(),
            session_id: self.session_id.clone(),
            origin: self.origin.clone(),
            kind: BusKind::Event(event),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_roundtrip_through_the_payload() {
        let o = Origin::Telegram {
            chat_id: 42,
            topic_id: Some(7),
            message_id: Some(99),
        };
        let payload = serde_json::json!({"text": "x", "origin": o.to_value()});
        assert_eq!(Origin::from_payload(&payload), o);
        assert_eq!(
            Origin::from_payload(&serde_json::json!({})),
            Origin::Internal {
                source: "inconnu".into()
            }
        );
    }

    #[tokio::test]
    async fn a_late_waiter_still_gets_the_outcome() {
        let bus = Bus::new();
        bus.finish("t1", "s1", &Origin::Cli, TurnOutcome::Cancelled);
        let got = bus.wait_for("t1").await.unwrap();
        assert_eq!(got, TurnOutcome::Cancelled);
    }

    #[tokio::test]
    async fn an_early_waiter_is_woken() {
        let bus = Arc::new(Bus::new());
        let rx = bus.wait_for("t2");
        let b = bus.clone();
        tokio::spawn(async move {
            b.finish(
                "t2",
                "s1",
                &Origin::Cli,
                TurnOutcome::Failed { error: "x".into() },
            );
        });
        assert!(matches!(rx.await.unwrap(), TurnOutcome::Failed { .. }));
    }

    #[test]
    fn cancelling_a_session_reaches_its_turn() {
        let bus = Bus::new();
        let a = bus.begin("t3", "s3", &Origin::Cli);
        assert!(!a.cancel.is_cancelled());
        assert!(bus.cancel_session("s3"));
        assert!(a.cancel.is_cancelled());
        assert_eq!(bus.session_for_draft(a.draft_id).as_deref(), Some("s3"));
        bus.end("s3", "t3");
        assert!(!bus.cancel_session("s3"));
    }

    #[test]
    fn draft_ids_are_stable_and_non_zero() {
        assert_eq!(draft_id_for("t_abc"), draft_id_for("t_abc"));
        assert_ne!(draft_id_for("t_abc"), draft_id_for("t_abd"));
        assert!(draft_id_for("") > 0);
    }
}
