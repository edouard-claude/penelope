//! Activité affichée et brouillons : `sendChatAction`, `sendMessageDraft`, rapport d'arrêt.

use super::*;

/// Indicateur d'activité d'un tour : renvoyé tant qu'il vit, l'action suit ce qu'il fait.
pub(super) struct Activity {
    action: Arc<std::sync::Mutex<&'static str>>,
    task: tokio::task::JoinHandle<()>,
}

/// Action d'activité d'un outil : un fichier qui part, une voix, une image, sinon « écrit ».
fn activity_for(tool: &str) -> &'static str {
    match tool {
        "send_file" | "artifact_read" => "upload_document",
        "send_voice" => "record_voice",
        "image_generate" => "upload_photo",
        _ => "typing",
    }
}

/// Résumé d'un appel d'outil pour la ligne d'état du brouillon : la commande, le
/// chemin, la requête ou l'adresse, raccourcis (issue #121).
fn tool_status(name: &str, args: &Value) -> String {
    let detail = ["command", "path", "query", "url", "name", "id"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .map(|d| {
            let d = d.split_whitespace().collect::<Vec<_>>().join(" ");
            if d.chars().count() > 60 {
                format!("{}…", d.chars().take(59).collect::<String>())
            } else {
                d
            }
        });
    match detail {
        Some(d) => format!("⚙️ {name} · {d}"),
        None => format!("⚙️ {name}…"),
    }
}

/// Ce que `/stop` a trouvé, et ce qu'il en dit (issue #155).
///
/// Le 21/09, `/stop` puis `/stop tout` ont répondu « Rien à arrêter » alors qu'un run était
/// **bloqué** dans le sujet depuis vingt minutes et que la dernière réponse le disait « en
/// cours ». Seuls les runs `running` étaient regardés ; un run bloqué — précisément celui
/// qui *paraît* en cours — n'était ni touché ni même nommé.
#[derive(Debug, Default)]
pub(crate) struct StopReport {
    pub running: bool,
    pub queued: usize,
    pub burst: usize,
    pub sessions: usize,
    /// Runs mis en pause.
    pub paused: usize,
    /// Runs laissés ouverts, nommés : ils ne sont pas annulés à la place du propriétaire.
    pub left: Vec<String>,
    /// Tous les runs ouverts du chat : identifiant et état.
    pub open: Vec<(String, String)>,
    /// Ingestions de documents en cours (issue #155).
    pub ingests: usize,
    /// Celles que `/stop tout` vient d'interrompre.
    pub cancelled_ingests: usize,
    /// Jobs d'outils coupés (issue #204) : `/stop` ceux de la session, `/stop tout` ceux
    /// de toutes les sessions du chat.
    pub cancelled_jobs: usize,
    pub tout: bool,
}

impl StopReport {
    pub(crate) fn render(&self) -> String {
        let mut note = match (self.running, self.queued) {
            // « Rien à arrêter » seulement quand il n'y a vraiment rien : un run ouvert
            // compte, quel que soit son état.
            (false, 0) if self.open.is_empty() && self.ingests == 0 && self.cancelled_jobs == 0 => {
                "Rien à arrêter.".to_string()
            }
            (false, 0) => "⏹ Aucun tour en cours.".to_string(),
            (true, 0) => "⏹ Tour arrêté.".to_string(),
            (false, n) => format!("⏹ {n} message(s) en attente annulé(s)."),
            (true, n) => format!("⏹ Tour arrêté, {n} message(s) en attente annulé(s)."),
        };
        if self.burst > 0 {
            note.push_str(&format!(
                " {} morceau(x) reçus à l'instant écartés.",
                self.burst
            ));
        }
        if self.sessions > 0 {
            note.push_str(&format!(
                " {} autre(s) session(s) de ce chat vidée(s).",
                self.sessions
            ));
        }
        if self.paused > 0 {
            note.push_str(&format!(
                " {} run(s) de workflow mis en pause.",
                self.paused
            ));
        }
        if self.cancelled_ingests > 0 {
            note.push_str(&format!(
                " {} ingestion(s) de document interrompue(s).",
                self.cancelled_ingests
            ));
        }
        if self.cancelled_jobs > 0 {
            note.push_str(&format!(
                " {} job(s) d'outil interrompu(s).",
                self.cancelled_jobs
            ));
        }
        if !self.left.is_empty() {
            note.push_str(&format!(
                "\n\nRun(s) laissé(s) ouvert(s) : {}. Ils ne sont pas annulés à ta place : \
                 `/run cancel <id>` pour en finir, la carte du run pour réessayer ou passer \
                 l'étape.",
                self.left.join(", ")
            ));
        }
        // Ne promettre que ce qui est fait : l'ingestion n'était pas interrompue malgré la
        // phrase qui l'annonçait, et les runs ouverts n'étaient pas nommés.
        if !self.tout && (!self.open.is_empty() || self.ingests > 0) {
            let mut rest: Vec<String> = Vec::new();
            if !self.open.is_empty() {
                rest.push(format!(
                    "{} run(s) ouvert(s) ({})",
                    self.open.len(),
                    self.open
                        .iter()
                        .map(|(id, st)| format!("`{id}` {st}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if self.ingests > 0 {
                rest.push(format!("{} ingestion(s) de document", self.ingests));
            }
            note.push_str(&format!(
                "\n\nContinue : {}. `/stop tout` met en pause ce qui tourne, interrompt les \
                 ingestions et nomme le reste.",
                rest.join(", ")
            ));
        }
        note
    }
}

impl TelegramGateway {
    /// Intervalle de l'indicateur d'activité (tests).
    pub fn set_activity_every(&self, every: Duration) {
        self.activity_every_ms.store(
            every.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    /// Un indicateur d'activité, jetable : hors de la file d'envoi durable, et son échec ne
    /// touche jamais le tour (issue #121).
    pub(super) fn chat_action(&self, chat_id: i64, topic_id: Option<i64>, action: &str) {
        let bot = self.bot.clone();
        let mut payload = json!({"chat_id": chat_id, "action": action});
        if let Some(t) = topic_id {
            payload["message_thread_id"] = json!(t);
        }
        tokio::spawn(async move {
            let _ = bot
                .call(
                    penelope_telegram::api::method::SEND_CHAT_ACTION,
                    None,
                    payload,
                )
                .await;
        });
    }
    /// Démarre l'indicateur d'un tour : renvoyé toutes les quatre secondes tant que le tour
    /// vit, dans son sujet, arrêté avec lui (issue #121).
    fn start_activity(&self, turn_id: &str, session_id: &str, chat_id: i64, topic_id: Option<i64>) {
        let action = Arc::new(std::sync::Mutex::new("typing"));
        let every = Duration::from_millis(
            self.activity_every_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(10),
        );
        let (bot, daemon, a, sid) = (
            self.bot.clone(),
            self.daemon.clone(),
            action.clone(),
            session_id.to_string(),
        );
        let task = tokio::spawn(async move {
            // Un tour dont la fin aurait échappé au bus ne garde pas l'indicateur allumé.
            while daemon.bus.is_active(&sid) && !daemon.handle.is_shutting_down() {
                let current = *a.lock().unwrap_or_else(|p| p.into_inner());
                let mut payload = json!({"chat_id": chat_id, "action": current});
                if let Some(t) = topic_id {
                    payload["message_thread_id"] = json!(t);
                }
                let _ = bot
                    .call(
                        penelope_telegram::api::method::SEND_CHAT_ACTION,
                        None,
                        payload,
                    )
                    .await;
                tokio::time::sleep(every).await;
            }
        });
        if let Ok(mut g) = self.activities.lock()
            && let Some(old) = g.insert(turn_id.to_string(), Activity { action, task })
        {
            old.task.abort();
        }
    }
    fn set_activity(&self, turn_id: &str, action: &'static str) {
        if let Ok(g) = self.activities.lock()
            && let Some(a) = g.get(turn_id)
            && let Ok(mut current) = a.action.lock()
        {
            *current = action;
        }
    }
    fn stop_activity(&self, turn_id: &str) {
        if let Ok(mut g) = self.activities.lock()
            && let Some(a) = g.remove(turn_id)
        {
            a.task.abort();
        }
    }
    // ================================================================ brouillons

    pub(super) async fn draft_loop(self: Arc<Self>) {
        struct Draft {
            chat_id: i64,
            topic_id: Option<i64>,
            draft_id: i64,
            text: String,
            last: Instant,
            /// Dernière vérification du focus : une session quittée cesse d'écrire.
            checked: Instant,
            /// Brouillon en vol : un seul à la fois, le suivant porte le dernier texte
            /// (issue #70).
            in_flight: Option<tokio::task::JoinHandle<()>>,
            /// Texte du dernier brouillon effectivement envoyé.
            sent: String,
        }
        const FOCUS_EVERY: Duration = Duration::from_secs(2);
        let mut rx = self.daemon.bus.subscribe();
        let mut drafts: HashMap<String, Draft> = HashMap::new();
        while !self.shutting_down() {
            let ev = match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Err(_) => continue,
                Ok(Ok(ev)) => ev,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
            };
            let Some((chat_id, topic_id)) = ev.origin.telegram_chat() else {
                continue;
            };
            // Indicateur d'activité, dans toute conversation, sujet compris (issue #121).
            match &ev.kind {
                BusKind::Started => {
                    if !self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                        self.start_activity(&ev.turn_id, &ev.session_id, chat_id, topic_id);
                    }
                }
                BusKind::Event(TurnEvent::ToolCall { name, .. }) => {
                    self.set_activity(&ev.turn_id, activity_for(name));
                }
                BusKind::Event(TurnEvent::ToolResult { .. }) => {
                    self.set_activity(&ev.turn_id, "typing");
                }
                BusKind::Finished(_) => self.stop_activity(&ev.turn_id),
                _ => {}
            }
            // `sendMessageDraft` ne vaut que pour une conversation privée.
            if chat_id <= 0 {
                continue;
            }
            // Session en arrière-plan : ni brouillon ni « écrit… » (issue #10).
            if let Some(d) = drafts.get_mut(&ev.turn_id)
                && d.checked.elapsed() >= FOCUS_EVERY
            {
                d.checked = Instant::now();
                if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                    drafts.remove(&ev.turn_id);
                    continue;
                }
            }
            match &ev.kind {
                BusKind::Started => {
                    if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                        continue;
                    }
                    drafts.insert(
                        ev.turn_id.clone(),
                        Draft {
                            chat_id,
                            topic_id,
                            draft_id: penelope_app::bus::draft_id_for(&ev.turn_id),
                            text: String::new(),
                            last: Instant::now() - self.draft_interval,
                            checked: Instant::now(),
                            in_flight: None,
                            sent: String::new(),
                        },
                    );
                }
                BusKind::Event(TurnEvent::Delta(t)) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        d.text.push_str(t);
                        // Un seul brouillon en vol : tant qu'il n'est pas parti, le texte
                        // continue de s'accumuler et le suivant portera tout (issue #70).
                        let busy = d.in_flight.as_ref().is_some_and(|h| !h.is_finished());
                        if !busy && d.last.elapsed() >= self.draft_interval && d.text != d.sent {
                            d.last = Instant::now();
                            d.sent = d.text.clone();
                            d.in_flight =
                                self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, &d.text);
                        }
                    }
                }
                BusKind::Event(TurnEvent::ToolCall { name, args }) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        let busy = d.in_flight.as_ref().is_some_and(|h| !h.is_finished());
                        if !busy {
                            d.last = Instant::now();
                            // Ligne d'état : ce qui tourne, pas seulement son nom (#121).
                            let preview =
                                format!("{}\n\n{}", d.text.trim_end(), tool_status(name, args));
                            d.sent = preview.clone();
                            d.in_flight =
                                self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, preview.trim());
                        }
                    }
                }
                BusKind::Finished(_) => {
                    // La réponse finale part tout de suite : le brouillon en vol ne doit
                    // pas prendre le créneau devant elle (issue #70).
                    if let Some(d) = drafts.remove(&ev.turn_id)
                        && let Some(h) = d.in_flight
                    {
                        h.abort();
                    }
                }
                _ => {}
            }
        }
    }
    /// Envoie un brouillon en tâche de fond ; la poignée sert à savoir s'il est encore en
    /// vol (issue #70).
    fn spawn_draft(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        draft_id: i64,
        text: &str,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let text: String = text
            .chars()
            .rev()
            .take(4_000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if text.trim().is_empty() {
            return None;
        }
        let bot = self.bot.clone();
        Some(tokio::spawn(async move {
            let _ = bot.send_draft(chat_id, topic_id, draft_id, &text).await;
        }))
    }
}
