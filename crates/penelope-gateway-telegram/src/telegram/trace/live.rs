//! Boucle de la trace des outils (issue #222) : une bulle par tour, créée au premier
//! appel par la file d'envoi, puis modifiée en place par appel direct.
//!
//! - **Création** par `tg_outbox` : la file est ordonnée par chat, la bulle part avant la
//!   réponse finale, rédigée et durable comme tout envoi.
//! - **Modifications** hors de la file : un `message is not modified` y deviendrait une
//!   note d'échec visible. Une seule en vol par tour, qui porte toujours le dernier état
//!   (même schéma que les brouillons, #70), au plus une toutes les 1,5 s en privé et
//!   toutes les 3 s en groupe (Telegram : vingt messages par minute dans un groupe).
//! - **Fin du tour** : la dernière modification attend que la file du chat soit vide,
//!   elle ne prend jamais le créneau de la réponse.
//! - **Redémarrage** : chaque bulle ouverte est inscrite sous `tg.trace.open` ; au
//!   démarrage suivant, elle est close (« interrompu par un redémarrage »).
//! - **Narration** (`narre`, #273) : la bulle est créée avec la ligne `resume` ; c'est
//!   dans la tâche de chaque modification, et à la clôture une fois la réponse partie, que
//!   le modèle du rôle `trace` est appelé, borné, avec la phrase précédente. La boucle et
//!   la réponse ne l'attendent jamais.

use super::super::*;
use super::Trace;
use super::narrate::{self, Ask, Narrator};
use penelope_kernel::config::ToolTrace as Mode;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

/// Écart minimal entre deux modifications, en privé et en groupe.
const PRIVATE_EVERY: Duration = Duration::from_millis(1_500);
const GROUP_EVERY: Duration = Duration::from_millis(3_000);
/// Revérification de la focalisation, comme les brouillons (#10).
const FOCUS_EVERY: Duration = Duration::from_secs(2);
/// Pas de la boucle quand le bus se tait : une modification retenue part quand même.
const TICK: Duration = Duration::from_millis(250);
/// Attente maximale de la file du chat avant la dernière modification.
const FINAL_WAIT: Duration = Duration::from_secs(60);
/// Attente maximale du rattrapage de la boucle par `deliver` : au-delà, la boucle est
/// bloquée et la réponse ne l'attend plus.
const BARRIER_WAIT: Duration = Duration::from_secs(10);
/// Attente, à la clôture, d'une modification encore en vol (narration bornée comprise).
const SETTLE_WAIT: Duration = Duration::from_secs(5);
/// Bulles ouvertes, par tour : `kv` n'a pas de listage par préfixe, d'où une seule clé.
pub(crate) const OPEN_KEY: &str = "tg.trace.open";

/// Canal des demandes de rattrapage : chacune est acquittée une fois le bus lu.
pub(crate) type SyncTx = tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>;

/// Une bulle ouverte, telle qu'elle survit à un redémarrage.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Open {
    chat_id: i64,
    outbox_id: String,
    message_id: Option<i64>,
    trace: Trace,
    mode: Mode,
    group: bool,
    /// Dernière phrase narrée (`narre`), absente des bulles d'avant #273.
    #[serde(default)]
    phrase: Option<String>,
}

/// Ce qu'une modification rapporte à la boucle une fois finie.
struct Edited {
    /// L'état acquitté ; `None` : la modification a échoué, le prochain pas la rejoue.
    acked: Option<String>,
    /// Le texte posé sur la bulle, s'il a changé.
    shown: Option<String>,
    /// La phrase rendue par le modèle, s'il y en a eu une.
    phrase: Option<String>,
}

/// L'état d'un tour suivi par la boucle.
struct Live {
    turn_id: String,
    session_id: String,
    chat_id: i64,
    topic_id: Option<i64>,
    mode: Mode,
    group: bool,
    trace: Trace,
    /// Ligne `tg_outbox` de la création ; `None` tant qu'aucun outil n'a été appelé.
    outbox_id: Option<String>,
    message_id: Option<i64>,
    /// Dernier état acquitté : le rendu de la trace (création comprise). En `narre`,
    /// c'est la ligne `resume`, clé de l'état que la dernière narration a décrit.
    acked: String,
    /// Le texte réellement posé sur la bulle : en `narre`, la phrase.
    shown: String,
    /// Le modèle du rôle `trace` (`narre` seulement, résolu au début du tour).
    narrator: Option<Narrator>,
    /// La dernière phrase rendue, passée au modèle pour qu'il la garde si rien n'a changé.
    phrase: Option<String>,
    last: Instant,
    checked: Instant,
    /// Modification en vol : elle rend son texte si elle a abouti.
    in_flight: Option<tokio::task::JoinHandle<Edited>>,
    /// Session passée en arrière-plan : la bulle se fige (#10).
    frozen: bool,
}

impl Live {
    /// Le rendu de la trace : ce que la bulle montre hors narration, et la clé de l'état.
    fn text(&self) -> String {
        self.trace.render(self.mode, self.group)
    }
    fn every(&self) -> Duration {
        if self.group {
            GROUP_EVERY
        } else {
            PRIVATE_EVERY
        }
    }
    fn open(&self) -> Option<Open> {
        Some(Open {
            chat_id: self.chat_id,
            outbox_id: self.outbox_id.clone()?,
            message_id: self.message_id,
            trace: self.trace.clone(),
            mode: self.mode,
            group: self.group,
            phrase: self.phrase.clone(),
        })
    }
    fn apply(&mut self, e: Edited) {
        if let Some(k) = e.acked {
            self.acked = k;
        }
        if let Some(t) = e.shown {
            self.shown = t;
        }
        if e.phrase.is_some() {
            self.phrase = e.phrase;
        }
    }
    /// Relève ce qu'une modification terminée rapporte.
    async fn reap(&mut self) {
        if self.in_flight.as_ref().is_some_and(|h| h.is_finished())
            && let Some(h) = self.in_flight.take()
            && let Ok(e) = h.await
        {
            self.apply(e);
        }
    }
    /// Attend la modification en vol, bornée : à la clôture, la phrase qu'elle a obtenue
    /// est celle que la dernière narration doit connaître.
    async fn settle(&mut self) {
        if let Some(h) = self.in_flight.take() {
            match tokio::time::timeout(SETTLE_WAIT, h).await {
                Ok(Ok(e)) => self.apply(e),
                Ok(Err(_)) => {}
                Err(_) => tracing::warn!("modification de la trace des outils bloquée"),
            }
        }
    }
    /// Ce qu'une modification emporte pour composer le texte hors de la boucle.
    fn job(&self) -> Job {
        Job {
            turn_id: self.turn_id.clone(),
            session_id: self.session_id.clone(),
            mode: self.mode,
            narrator: self.narrator.clone(),
            trace: self.trace.clone(),
            previous: self.phrase.clone(),
        }
    }
}

/// L'état d'un tour au moment d'une modification, hors de la boucle.
struct Job {
    turn_id: String,
    session_id: String,
    mode: Mode,
    narrator: Option<Narrator>,
    trace: Trace,
    previous: Option<String>,
}

impl Job {
    /// Le texte à poser pour l'état `key` : en `narre`, la phrase du modèle (bornée, une
    /// tentative) coiffée des lignes de fin ; sans phrase, `key` (la ligne `resume`).
    async fn compose(&self, daemon: &Core, key: &str) -> (String, Option<String>) {
        let Some(n) = self.narrator.as_ref().filter(|_| self.mode.narrates()) else {
            return (key.to_string(), None);
        };
        let ask = Ask {
            session_id: &self.session_id,
            turn_id: &self.turn_id,
            previous: self.previous.as_deref(),
        };
        match narrate::narrate(
            &daemon.services,
            daemon.providers.as_ref(),
            n,
            &self.trace,
            ask,
        )
        .await
        {
            Some(p) => (self.trace.narrated(&p), Some(p)),
            None => (key.to_string(), None),
        }
    }
}

impl TelegramGateway {
    /// Lance la boucle de la trace, abonnée au bus **avant** de rendre la main : un tour
    /// mis en file juste après n'échappe pas à la trace (harnais de scénarios).
    pub fn spawn_trace(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let rx = self.daemon.bus.subscribe();
        tokio::spawn(self.clone().trace_events(rx))
    }

    /// Aucune bulle suivie ni en train d'être close.
    pub fn traces_idle(&self) -> bool {
        self.trace_busy.load(Ordering::SeqCst) == 0
    }

    pub(crate) async fn trace_loop(self: Arc<Self>) {
        let rx = self.daemon.bus.subscribe();
        self.trace_events(rx).await
    }

    async fn trace_events(
        self: Arc<Self>,
        mut rx: tokio::sync::broadcast::Receiver<Arc<penelope_app::bus::BusEvent>>,
    ) {
        let (tx, mut sync) = tokio::sync::mpsc::unbounded_channel();
        if let Ok(mut g) = self.trace_sync.lock() {
            *g = Some(tx);
        }
        let mut lives: HashMap<String, Live> = HashMap::new();
        while !self.shutting_down() {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => self.on_event(&mut lives, ev).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        for l in lives.values_mut() {
                            l.trace.mark_incomplete();
                        }
                    }
                    Err(_) => break,
                },
                Some(ack) = sync.recv() => {
                    self.catch_up(&mut rx, &mut lives).await;
                    let _ = ack.send(());
                }
                _ = tokio::time::sleep(TICK) => {}
            }
            for l in lives.values_mut() {
                self.maybe_edit(l).await;
            }
        }
        if let Ok(mut g) = self.trace_sync.lock() {
            *g = None;
        }
    }

    /// Lit tout ce que le bus a déjà reçu : un appel publié avant la demande a sa bulle
    /// enfilée quand elle est acquittée.
    async fn catch_up(
        self: &Arc<Self>,
        rx: &mut tokio::sync::broadcast::Receiver<Arc<penelope_app::bus::BusEvent>>,
        lives: &mut HashMap<String, Live>,
    ) {
        loop {
            match rx.try_recv() {
                Ok(ev) => self.on_event(lives, ev).await,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    for l in lives.values_mut() {
                        l.trace.mark_incomplete();
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// Attend que la boucle de trace ait lu tout ce que le bus porte déjà. Sans boucle
    /// (trace jamais lancée, arrêtée), rien à attendre.
    pub(crate) async fn trace_barrier(&self) {
        let (ack, done) = tokio::sync::oneshot::channel();
        let sent = match self.trace_sync.lock() {
            Ok(g) => g.as_ref().is_some_and(|tx| tx.send(ack).is_ok()),
            Err(_) => false,
        };
        if sent && tokio::time::timeout(BARRIER_WAIT, done).await.is_err() {
            tracing::warn!(
                "boucle de trace des outils en retard : réponse envoyée sans l'attendre"
            );
        }
    }

    async fn on_event(
        self: &Arc<Self>,
        lives: &mut HashMap<String, Live>,
        ev: Arc<penelope_app::bus::BusEvent>,
    ) {
        let Some((chat_id, topic_id)) = ev.origin.telegram_chat() else {
            return;
        };
        let turn_id = ev.turn_id.clone();
        if let BusKind::Started = ev.kind {
            // Relu à chaque tour : `config set telegram.tool_trace` s'applique à chaud.
            let cfg = self.daemon.services.config.config();
            let mut mode = cfg.telegram.tool_trace;
            if mode == Mode::Off || self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                return;
            }
            // `narre` sans modèle pour le rôle `trace` : `resume` pour ce tour, `doctor`
            // le dit (#273).
            let narrator = mode.narrates().then(|| narrate::role_model(&cfg)).flatten();
            if mode.narrates() && narrator.is_none() {
                tracing::debug!("trace des outils : aucun modèle pour le rôle trace, resume");
                mode = Mode::Resume;
            }
            self.trace_busy.fetch_add(1, Ordering::SeqCst);
            lives.insert(
                turn_id.clone(),
                Live {
                    turn_id,
                    session_id: ev.session_id.clone(),
                    chat_id,
                    topic_id,
                    mode,
                    group: chat_id < 0,
                    trace: Trace::default(),
                    outbox_id: None,
                    message_id: None,
                    acked: String::new(),
                    shown: String::new(),
                    narrator,
                    phrase: None,
                    last: Instant::now(),
                    checked: Instant::now(),
                    in_flight: None,
                    frozen: false,
                },
            );
            return;
        }
        if let BusKind::Finished(_) = ev.kind {
            // La modification en vol n'est pas abandonnée : `close_bubble` l'attend, pour
            // connaître le texte posé et la phrase obtenue avant la dernière narration.
            if let Some(mut l) = lives.remove(&turn_id) {
                l.trace.finish();
                let me = self.clone();
                tokio::spawn(async move { me.close_bubble(&turn_id, l).await });
            }
            return;
        }
        let Some(l) = lives.get_mut(&turn_id) else {
            return;
        };
        if !l.frozen && l.checked.elapsed() >= FOCUS_EVERY {
            l.checked = Instant::now();
            if self.out_of_focus(&l.session_id, chat_id, topic_id).await {
                l.frozen = true;
                if let Some(h) = l.in_flight.take() {
                    h.abort();
                }
            }
        }
        match &ev.kind {
            BusKind::Event(TurnEvent::ToolCall { name, args }) if !l.frozen => {
                l.trace.call(name, args);
                if l.outbox_id.is_none() {
                    self.create_bubble(&turn_id, l).await;
                }
            }
            BusKind::Event(TurnEvent::ToolResult { name, ok, preview }) if !l.frozen => {
                l.trace.result(name, *ok, preview);
            }
            _ => {}
        }
    }

    /// Le premier appel du tour : la bulle part par la file d'envoi. En `narre`, avec la
    /// ligne `resume` : le modèle n'est jamais attendu avant la réponse.
    async fn create_bubble(&self, turn_id: &str, l: &mut Live) {
        let text = l.text();
        let mut payload = json!({
            "chat_id": l.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_notification": true,
            "link_preview_options": {"is_disabled": true},
        });
        if let Some(t) = l.topic_id {
            payload["message_thread_id"] = json!(t);
        }
        match self
            .outbox_enqueue(l.chat_id, l.topic_id, "sendMessage", payload)
            .await
        {
            Ok(id) => {
                l.outbox_id = Some(id);
                l.acked = text.clone();
                l.shown = text;
                l.last = Instant::now();
                self.remember_open(turn_id, l.open()).await;
            }
            Err(e) => {
                // Sans bulle, la trace se tait pour ce tour : jamais au prix de la réponse.
                tracing::warn!(error = %e, "trace des outils non créée");
                l.frozen = true;
            }
        }
    }

    /// Une modification si l'état a changé, que rien n'est en vol et que la cadence le
    /// permet. Le texte est composé dans la tâche : en `narre`, c'est là que le modèle
    /// parle, et une phrase identique à celle posée ne modifie rien (pas de clignotement).
    async fn maybe_edit(&self, l: &mut Live) {
        l.reap().await;
        if l.frozen || l.outbox_id.is_none() || l.in_flight.is_some() {
            return;
        }
        let key = l.text();
        if key == l.acked || l.last.elapsed() < l.every() {
            return;
        }
        if l.message_id.is_none() {
            l.message_id = self.sent_message_id(l.outbox_id.as_deref()).await;
        }
        let Some(message_id) = l.message_id else {
            return;
        };
        l.last = Instant::now();
        let (bot, chat_id, shown) = (self.bot.clone(), l.chat_id, l.shown.clone());
        let (daemon, job) = (self.daemon.clone(), l.job());
        l.in_flight = Some(tokio::spawn(async move {
            let (text, phrase) = job.compose(&daemon, &key).await;
            if text == shown {
                return Edited {
                    acked: Some(key),
                    shown: None,
                    phrase,
                };
            }
            match bot.edit_trace(chat_id, message_id, &text).await {
                Ok(()) => Edited {
                    acked: Some(key),
                    shown: Some(text),
                    phrase,
                },
                Err(_) => Edited {
                    acked: None,
                    shown: None,
                    phrase,
                },
            }
        }));
    }

    /// Fin du tour : la file du chat se vide (la réponse est partie), puis l'état final.
    async fn close_bubble(&self, turn_id: &str, mut l: Live) {
        if l.outbox_id.is_some() && !l.frozen {
            let key = l.text();
            let started = Instant::now();
            while self.chat_pending(l.chat_id).await > 0
                && started.elapsed() < FINAL_WAIT
                && !self.shutting_down()
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            l.settle().await;
            if l.message_id.is_none() {
                l.message_id = self.sent_message_id(l.outbox_id.as_deref()).await;
            }
            if let Some(message_id) = l.message_id
                && key != l.acked
            {
                let (text, _) = l.job().compose(&self.daemon, &key).await;
                if text != l.shown {
                    self.final_edit(l.chat_id, message_id, &text).await;
                }
            }
        }
        self.forget_open(turn_id).await;
        self.trace_busy.fetch_sub(1, Ordering::SeqCst);
    }

    /// La dernière modification a droit à une seconde chance après un 429 : c'est elle
    /// qui reste sous les yeux du propriétaire.
    async fn final_edit(&self, chat_id: i64, message_id: i64, text: &str) {
        if let Err(TgError::RateLimited(secs)) =
            self.bot.edit_trace(chat_id, message_id, text).await
        {
            tokio::time::sleep(Duration::from_secs(secs.min(30))).await;
            let _ = self.bot.edit_trace(chat_id, message_id, text).await;
        }
    }

    /// Au démarrage : les bulles laissées ouvertes par une vie précédente sont closes,
    /// une fois, puis oubliées. Un tour rejoué pose une bulle neuve.
    pub(crate) async fn close_orphan_traces(&self) -> anyhow::Result<usize> {
        let _g = self.trace_lock.lock().await;
        let s = &self.daemon.services;
        let open = read_open(s.kv_get(OPEN_KEY).await?);
        for o in open.values() {
            let mut trace = o.trace.clone();
            trace.interrupt();
            let message_id = match o.message_id {
                Some(m) => Some(m),
                None => self.sent_message_id(Some(&o.outbox_id)).await,
            };
            if let Some(m) = message_id {
                self.final_edit(o.chat_id, m, &trace.render(o.mode, o.group))
                    .await;
            }
        }
        if !open.is_empty() {
            s.kv_delete(OPEN_KEY).await?;
        }
        Ok(open.len())
    }

    async fn remember_open(&self, turn_id: &str, open: Option<Open>) {
        let Some(open) = open else { return };
        let _g = self.trace_lock.lock().await;
        let s = &self.daemon.services;
        let mut all = read_open(s.kv_get(OPEN_KEY).await.ok().flatten());
        all.insert(turn_id.to_string(), open);
        if let Ok(raw) = serde_json::to_string(&all) {
            let _ = s.kv_set(OPEN_KEY, &raw).await;
        }
    }

    async fn forget_open(&self, turn_id: &str) {
        let _g = self.trace_lock.lock().await;
        let s = &self.daemon.services;
        let mut all = read_open(s.kv_get(OPEN_KEY).await.ok().flatten());
        if all.remove(turn_id).is_none() {
            return;
        }
        let _ = if all.is_empty() {
            s.kv_delete(OPEN_KEY).await
        } else {
            match serde_json::to_string(&all) {
                Ok(raw) => s.kv_set(OPEN_KEY, &raw).await,
                Err(_) => Ok(()),
            }
        };
    }

    /// L'identifiant Telegram d'une ligne de la file, une fois partie.
    async fn sent_message_id(&self, outbox_id: Option<&str>) -> Option<i64> {
        let id = outbox_id?.to_string();
        self.daemon
            .services
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT message_id FROM tg_outbox WHERE id = ?1 AND state = 'sent'",
                    [id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten())
            })
            .await
            .ok()
            .flatten()
    }

    /// Envois encore en attente dans ce chat.
    async fn chat_pending(&self, chat_id: i64) -> i64 {
        self.daemon
            .services
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM tg_outbox WHERE chat_id = ?1 AND state = 'pending'",
                    [chat_id],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap_or(0)
    }
}

fn read_open(raw: Option<String>) -> BTreeMap<String, Open> {
    raw.and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default()
}
