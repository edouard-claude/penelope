//! Élicitation MCP (§8.4, issue #12).
//!
//! Un serveur demande au propriétaire de remplir un formulaire (`requestedSchema`), de
//! confirmer une action (schéma sans champ) ou d'ouvrir un lien (mode URL). La demande part
//! sur le canal du propriétaire (Telegram) ; sans réponse avant `elicitation_timeout`, elle
//! est annulée. Chaque issue reste notée une heure pour annoter le résultat de l'outil
//! appelant : le modèle sait qui a répondu, et n'invente pas de cause.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Durée de conservation des issues, pour les annotations.
const NOTE_TTL: Duration = Duration::from_secs(3600);

/// Où présenter une demande : la conversation qui a déclenché l'appel d'outil (issue
/// #143). Vide : la demande n'a pas de session — serveur relancé par le superviseur,
/// tâche de fond —, le canal choisit alors son repli.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Destination {
    pub session_id: Option<String>,
    pub chat_id: Option<i64>,
    pub topic_id: Option<i64>,
}

impl Destination {
    /// Vrai si la demande sait où revenir.
    pub fn is_known(&self) -> bool {
        self.chat_id.is_some()
    }
}

/// Une demande présentée au propriétaire.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: String,
    pub server: String,
    pub message: String,
    pub kind: Kind,
    /// Attente maximale avant annulation.
    pub timeout: Duration,
    /// Conversation qui a déclenché l'appel : la carte y retourne (issue #143).
    pub to: Destination,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Formulaire ; sans champ, simple confirmation.
    Form { schema: Value },
    /// Lien à ouvrir hors de Pénélope. Avec `elicitation_id`, le serveur signale la fin
    /// (`notifications/elicitation/complete`) ; sans (MRTR, 2026-07-28), le propriétaire
    /// dit « J'ai terminé ».
    Url {
        url: String,
        elicitation_id: Option<String>,
    },
}

impl Request {
    /// Champs d'un formulaire (0 : confirmation ou lien).
    pub fn field_count(&self) -> usize {
        match &self.kind {
            Kind::Form { schema } => schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map_or(0, |p| p.len()),
            Kind::Url { .. } => 0,
        }
    }

    /// Domaine d'un lien.
    pub fn host(&self) -> Option<String> {
        match &self.kind {
            Kind::Url { url, .. } => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(String::from)),
            Kind::Form { .. } => None,
        }
    }
}

/// Réponse transmise au serveur.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Accept(Option<Value>),
    Decline,
    Cancel,
}

/// Qui a tranché.
#[derive(Debug, Clone, PartialEq)]
pub enum By {
    Owner,
    Timeout(Duration),
    /// Personne n'a pu être sollicité.
    Auto(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub action: Action,
    pub by: By,
}

impl Outcome {
    fn auto(reason: impl Into<String>) -> Outcome {
        Outcome {
            action: Action::Cancel,
            by: By::Auto(reason.into()),
        }
    }

    /// `ElicitResult` renvoyé au serveur.
    pub fn result(&self) -> Value {
        match &self.action {
            Action::Accept(Some(content)) => json!({"action": "accept", "content": content}),
            Action::Accept(None) => json!({"action": "accept"}),
            Action::Decline => json!({"action": "decline"}),
            Action::Cancel => json!({"action": "cancel"}),
        }
    }

    pub fn accepted(&self) -> bool {
        matches!(self.action, Action::Accept(_))
    }
}

/// Canal qui sait présenter une demande au propriétaire.
#[async_trait::async_trait]
pub trait OwnerChannel: Send + Sync {
    /// Présente la demande ; rend l'identifiant de la carte, pour la mettre à jour.
    async fn show(&self, request: &Request) -> Result<Option<i64>, String>;
    /// Remplace la carte par une issue (délai dépassé, lien terminé). `retry` : la
    /// demande peut être relancée — le canal propose alors un bouton (issue #143).
    async fn close(&self, request: &Request, card: Option<i64>, markdown: &str, retry: bool);
    /// Rappelle la demande à mi-délai, là où elle a été présentée (issue #143, patron
    /// des rappels d'approbation de #97). Une seule fois ; sans effet par défaut.
    async fn remind(&self, request: &Request, card: Option<i64>) {
        let _ = (request, card);
    }
}

/// Retire la conversation d'un appel d'outil quand il se termine, quoi qu'il arrive.
pub struct ScopeGuard {
    broker: Arc<Broker>,
    server: String,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = self.broker.contexts.lock()
            && let Some(stack) = c.get_mut(&self.server)
        {
            stack.pop();
            if stack.is_empty() {
                c.remove(&self.server);
            }
        }
    }
}

struct Waiting {
    request: Request,
    card: Option<i64>,
    tx: oneshot::Sender<Action>,
}

/// Lien accepté dont le serveur doit signaler la fin.
struct Link {
    request: Request,
    card: Option<i64>,
    waiters: Vec<oneshot::Sender<()>>,
}

type LinkKey = (String, String);

#[derive(Default)]
pub struct Broker {
    /// Conversation en cours par serveur : posée pendant l'appel d'outil, pour que
    /// l'élicitation qui en naît revienne au bon endroit (issue #143). Le protocole ne
    /// rattache pas une élicitation à l'appel qui l'a provoquée : la pile suffit, et
    /// deux sessions qui appellent le même serveur en même temps se partagent la
    /// dernière posée — les deux sont sous les yeux du propriétaire.
    contexts: Mutex<HashMap<String, Vec<Destination>>>,
    channel: RwLock<Option<Arc<dyn OwnerChannel>>>,
    /// Canal configuré mais pas encore branché (démarrage) : les serveurs qui se connectent
    /// entre-temps peuvent déjà compter sur lui.
    expected: AtomicBool,
    waiting: Mutex<HashMap<String, Waiting>>,
    links: Mutex<HashMap<LinkKey, Link>>,
    completed: Mutex<HashSet<LinkKey>>,
    notes: Mutex<Vec<(String, Instant, String)>>,
}

impl Broker {
    pub fn attach(&self, channel: Arc<dyn OwnerChannel>) {
        if let Ok(mut g) = self.channel.write() {
            *g = Some(channel);
        }
    }

    pub fn expect_owner(&self) {
        self.expected.store(true, Ordering::SeqCst);
    }

    /// Un propriétaire peut répondre : l'élicitation est annoncée aux serveurs.
    pub fn reachable(&self) -> bool {
        self.channel().is_some() || self.expected.load(Ordering::SeqCst)
    }

    fn channel(&self) -> Option<Arc<dyn OwnerChannel>> {
        self.channel.read().ok().and_then(|g| g.clone())
    }

    /// Pose la conversation d'un appel d'outil, le temps de cet appel : l'élicitation
    /// qui en naît y revient. Le garde retire la destination à sa chute, même si l'appel
    /// échoue.
    pub fn scope(self: &Arc<Self>, server: &str, to: Destination) -> ScopeGuard {
        if let Ok(mut c) = self.contexts.lock() {
            c.entry(server.to_string()).or_default().push(to);
        }
        ScopeGuard {
            broker: self.clone(),
            server: server.to_string(),
        }
    }

    fn destination(&self, server: &str) -> Destination {
        self.contexts
            .lock()
            .ok()
            .and_then(|c| c.get(server).and_then(|v| v.last().cloned()))
            .unwrap_or_default()
    }

    /// Présente une demande `elicitation/create` et attend l'issue. `Err` : paramètres
    /// invalides (−32602 pour le serveur).
    pub async fn ask(
        &self,
        server: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Outcome, String> {
        let mut request = parse(server, params, timeout)?;
        request.to = self.destination(server);
        let outcome = self.present(&request).await;
        self.note(&request, &outcome);
        Ok(outcome)
    }

    async fn present(&self, request: &Request) -> Outcome {
        let Some(channel) = self.channel() else {
            return Outcome::auto(
                "aucun canal pour joindre le propriétaire (Telegram non configuré ou arrêté)",
            );
        };
        let (tx, mut rx) = oneshot::channel();
        // Inscrite avant l'envoi : un clic très rapide trouve la demande.
        self.lock_waiting().insert(
            request.id.clone(),
            Waiting {
                request: request.clone(),
                card: None,
                tx,
            },
        );
        match channel.show(request).await {
            Ok(card) => {
                if let Some(w) = self.lock_waiting().get_mut(&request.id) {
                    w.card = card;
                }
            }
            Err(e) => {
                self.lock_waiting().remove(&request.id);
                return Outcome::auto(format!("la carte n'a pas pu être envoyée ({e})"));
            }
        }
        let answered = |action| Outcome {
            action,
            by: By::Owner,
        };
        // Un rappel à mi-délai, une seule fois : sans lui, le propriétaire ne découvre
        // l'attente qu'à l'annulation (issue #143, patron de #97).
        let half = request.timeout / 2;
        let rest = request.timeout - half;
        if !half.is_zero() {
            match tokio::time::timeout(half, &mut rx).await {
                Ok(Ok(action)) => return answered(action),
                Ok(Err(_)) => return Outcome::auto("demande abandonnée"),
                Err(_) => {
                    let card = self.lock_waiting().get(&request.id).and_then(|w| w.card);
                    channel.remind(request, card).await;
                }
            }
        }
        match tokio::time::timeout(rest, &mut rx).await {
            Ok(Ok(action)) => answered(action),
            Ok(Err(_)) => Outcome::auto("demande abandonnée"),
            Err(_) => match self.take(&request.id) {
                Some(w) => {
                    channel
                        .close(
                            request,
                            w.card,
                            &format!(
                                "⏱ Sans réponse après {}, la demande est annulée : `{}` en est \
                                 informé. Rien n'a été écrit.",
                                human(request.timeout),
                                request.server
                            ),
                            true,
                        )
                        .await;
                    Outcome {
                        action: Action::Cancel,
                        by: By::Timeout(request.timeout),
                    }
                }
                // Réponse arrivée au même instant que le délai.
                None => match rx.await {
                    Ok(action) => answered(action),
                    Err(_) => Outcome::auto("demande abandonnée"),
                },
            },
        }
    }

    /// Demandes en attente du propriétaire, avec leur carte.
    pub fn open(&self) -> Vec<(Request, Option<i64>)> {
        self.lock_waiting()
            .values()
            .map(|w| (w.request.clone(), w.card))
            .collect()
    }

    /// Demande encore ouverte, avec sa carte.
    pub fn request(&self, id: &str) -> Option<(Request, Option<i64>)> {
        self.lock_waiting()
            .get(id)
            .map(|w| (w.request.clone(), w.card))
    }

    /// Réponse du propriétaire.
    pub fn resolve(&self, id: &str, action: Action) -> Result<Request, String> {
        let w = self
            .take(id)
            .ok_or_else(|| "cette demande a expiré ou a déjà reçu une réponse".to_string())?;
        if let (
            Action::Accept(_),
            Kind::Url {
                elicitation_id: Some(eid),
                ..
            },
        ) = (&action, &w.request.kind)
        {
            let key = (w.request.server.clone(), eid.clone());
            if let Ok(mut links) = self.links.lock() {
                links.insert(
                    key,
                    Link {
                        request: w.request.clone(),
                        card: w.card,
                        waiters: Vec::new(),
                    },
                );
            }
        }
        w.tx.send(action)
            .map_err(|_| "cette demande a expiré ou a déjà reçu une réponse".to_string())?;
        Ok(w.request)
    }

    /// `notifications/elicitation/complete` : un identifiant inconnu est ignoré.
    pub async fn complete(&self, server: &str, elicitation_id: &str) {
        let key = (server.to_string(), elicitation_id.to_string());
        let Some(link) = self.links.lock().ok().and_then(|mut l| l.remove(&key)) else {
            return;
        };
        if let Ok(mut done) = self.completed.lock() {
            if done.len() > 1_000 {
                done.clear();
            }
            done.insert(key);
        }
        for w in link.waiters {
            let _ = w.send(());
        }
        if let Some(channel) = self.channel() {
            channel
                .close(
                    &link.request,
                    link.card,
                    &format!(
                        "✅ `{}` signale que l'interaction via le lien est terminée.",
                        link.request.server
                    ),
                    false,
                )
                .await;
        }
    }

    /// Attend la fin d'un lien accepté, avant de retenter l'appel qui l'exigeait.
    pub async fn wait_completion(
        &self,
        server: &str,
        elicitation_id: &str,
        timeout: Duration,
    ) -> bool {
        let key = (server.to_string(), elicitation_id.to_string());
        let (tx, rx) = oneshot::channel();
        {
            if self.completed.lock().is_ok_and(|d| d.contains(&key)) {
                return true;
            }
            let Ok(mut links) = self.links.lock() else {
                return false;
            };
            match links.get_mut(&key) {
                Some(link) => link.waiters.push(tx),
                None => return false,
            }
        }
        matches!(tokio::time::timeout(timeout, rx).await, Ok(Ok(())))
    }

    fn lock_waiting(&self) -> std::sync::MutexGuard<'_, HashMap<String, Waiting>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Retire une demande en attente (verrou relâché aussitôt).
    fn take(&self, id: &str) -> Option<Waiting> {
        self.lock_waiting().remove(id)
    }

    fn note(&self, request: &Request, outcome: &Outcome) {
        let text = explain(request, outcome);
        tracing::info!(server = %request.server, note = %text, "élicitation MCP");
        if let Ok(mut notes) = self.notes.lock() {
            notes.retain(|(_, at, _)| at.elapsed() < NOTE_TTL);
            notes.push((request.server.clone(), Instant::now(), text));
        }
    }

    /// Issues des élicitations d'un serveur depuis `since` : jointes au résultat de l'outil.
    pub fn notes_since(&self, server: &str, since: Instant) -> Vec<String> {
        self.notes
            .lock()
            .map(|notes| {
                notes
                    .iter()
                    .filter(|(s, at, _)| s == server && *at >= since)
                    .map(|(_, _, t)| t.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Lit les paramètres d'`elicitation/create`.
pub fn parse(server: &str, params: &Value, timeout: Duration) -> Result<Request, String> {
    let message = params
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let kind = match params
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("form")
    {
        "form" => {
            let schema = params
                .get("requestedSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            if schema.get("properties").is_some_and(|p| !p.is_object()) {
                return Err("`requestedSchema.properties` doit être un objet".into());
            }
            if schema
                .get("properties")
                .and_then(|p| p.as_object())
                .is_some_and(|p| !p.is_empty())
            {
                penelope_telegram::forms::fields_from_schema(&schema)
                    .map_err(|e| format!("`requestedSchema` : {e}"))?;
            }
            Kind::Form { schema }
        }
        "url" => {
            let raw = params
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or("mode `url` sans `url`")?;
            let parsed = url::Url::parse(raw).map_err(|e| format!("`url` invalide : {e}"))?;
            if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
                return Err(format!("`url` doit être en http(s) : `{raw}`"));
            }
            Kind::Url {
                url: raw.to_string(),
                elicitation_id: params
                    .get("elicitationId")
                    .and_then(|i| i.as_str())
                    .map(String::from),
            }
        }
        other => return Err(format!("mode d'élicitation inconnu : `{other}`")),
    };
    Ok(Request {
        id: penelope_kernel::ids::short_token(12),
        server: server.to_string(),
        message,
        kind,
        timeout,
        // Posée par `ask` d'après l'appel d'outil en cours (issue #143).
        to: Destination::default(),
    })
}

/// Première ligne d'un message de serveur, tronquée : de quoi reconnaître la demande
/// dans un rappel, sans recopier tout le formulaire.
pub fn first_line(message: &str) -> String {
    let line = message.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Phrase pour le modèle : qui a répondu, et comment.
pub fn explain(request: &Request, outcome: &Outcome) -> String {
    let what = match (&outcome.by, &outcome.action) {
        (By::Owner, Action::Accept(_)) => match &request.kind {
            Kind::Url { .. } => "le propriétaire a accepté d'ouvrir le lien sur Telegram \
                                 (accept) ; la suite se passe hors de Pénélope"
                .to_string(),
            Kind::Form { .. } if request.field_count() > 0 => {
                "le propriétaire a rempli et envoyé le formulaire sur Telegram (accept)".into()
            }
            Kind::Form { .. } => "le propriétaire a accepté sur Telegram (accept)".into(),
        },
        (By::Owner, Action::Decline) => "le propriétaire a refusé sur Telegram (decline)".into(),
        (By::Owner, Action::Cancel) => {
            "le propriétaire a annulé sans choisir, sur Telegram (cancel)".into()
        }
        (By::Timeout(d), _) => format!(
            "le propriétaire n'a pas répondu en {} : Pénélope a annulé la demande (cancel), \
             il n'a rien refusé",
            human(*d)
        ),
        (By::Auto(reason), _) => format!(
            "Pénélope a annulé la demande sans solliciter le propriétaire (cancel) : {reason}"
        ),
    };
    let message: String = request.message.chars().take(160).collect();
    format!(
        "[Pénélope, demande de confirmation du serveur `{}` « {} » : {what}.]",
        request.server, message
    )
}

/// « 10 min », « 45 s », « 2 h ».
pub fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{} ms", d.as_millis()),
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        shown: Mutex<Vec<Request>>,
        /// Texte de l'issue, et si elle proposait de relancer (issue #143).
        closed: Mutex<Vec<(String, bool)>>,
        reminders: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl OwnerChannel for Recorder {
        async fn show(&self, request: &Request) -> Result<Option<i64>, String> {
            self.shown.lock().unwrap().push(request.clone());
            Ok(Some(42))
        }
        async fn close(&self, _request: &Request, card: Option<i64>, markdown: &str, retry: bool) {
            assert_eq!(card, Some(42));
            self.closed
                .lock()
                .unwrap()
                .push((markdown.to_string(), retry));
        }
        async fn remind(&self, request: &Request, _card: Option<i64>) {
            self.reminders.lock().unwrap().push(request.id.clone());
        }
    }

    fn recorder() -> Arc<Recorder> {
        Arc::new(Recorder::default())
    }

    #[tokio::test]
    async fn without_a_channel_the_request_is_cancelled_and_says_why() {
        let b = Broker::default();
        assert!(!b.reachable());
        let since = Instant::now();
        let o = b
            .ask(
                "redmine",
                &json!({"message": "Modifier ?"}),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(o.result(), json!({"action": "cancel"}));
        let notes = b.notes_since("redmine", since);
        assert!(
            notes[0].contains("sans solliciter le propriétaire"),
            "{notes:?}"
        );
        assert!(b.notes_since("autre", since).is_empty());
    }

    #[tokio::test]
    async fn the_owner_answers_or_the_request_expires() {
        let b = Arc::new(Broker::default());
        let r = recorder();
        b.attach(r.clone());
        assert!(b.reachable());

        let asking = {
            let b = b.clone();
            tokio::spawn(async move {
                b.ask(
                    "redmine",
                    &json!({"message": "Modifier ?"}),
                    Duration::from_secs(5),
                )
                .await
            })
        };
        let id = loop {
            if let Some(req) = r.shown.lock().unwrap().first() {
                break req.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(b.request(&id).unwrap().1, Some(42));
        b.resolve(&id, Action::Decline).unwrap();
        let o = asking.await.unwrap().unwrap();
        assert_eq!((o.action, o.by), (Action::Decline, By::Owner));
        assert!(b.resolve(&id, Action::Cancel).is_err(), "une seule réponse");

        let since = Instant::now();
        let o = b
            .ask(
                "redmine",
                &json!({"message": "Encore ?"}),
                Duration::from_millis(50),
            )
            .await
            .unwrap();
        assert_eq!(o.by, By::Timeout(Duration::from_millis(50)));
        assert_eq!(o.result(), json!({"action": "cancel"}));
        assert!(r.closed.lock().unwrap()[0].0.contains("Sans réponse"));
        assert!(b.notes_since("redmine", since)[0].contains("il n'a rien refusé"));
    }

    #[tokio::test]
    async fn an_accepted_link_waits_for_its_completion() {
        let b = Arc::new(Broker::default());
        let r = recorder();
        b.attach(r.clone());
        let params = json!({
            "mode": "url",
            "elicitationId": "e-1",
            "url": "https://auth.example.com/connect?x=1",
            "message": "Connecte ton compte."
        });
        let asking = {
            let b = b.clone();
            tokio::spawn(async move { b.ask("drive", &params, Duration::from_secs(5)).await })
        };
        let req = loop {
            if let Some(req) = r.shown.lock().unwrap().first() {
                break req.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(req.host().as_deref(), Some("auth.example.com"));
        b.resolve(&req.id, Action::Accept(None)).unwrap();
        assert!(asking.await.unwrap().unwrap().accepted());

        let waiting = {
            let b = b.clone();
            tokio::spawn(async move {
                b.wait_completion("drive", "e-1", Duration::from_secs(5))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        b.complete("drive", "inconnu").await;
        b.complete("drive", "e-1").await;
        assert!(waiting.await.unwrap());
        assert!(r.closed.lock().unwrap()[0].0.contains("terminée"));
        assert!(
            b.wait_completion("drive", "e-1", Duration::from_millis(10))
                .await
        );
    }

    #[test]
    fn invalid_requests_are_rejected() {
        let t = Duration::from_secs(1);
        assert!(parse("s", &json!({"mode": "url", "url": "file:///etc/passwd"}), t).is_err());
        assert!(parse("s", &json!({"mode": "url"}), t).is_err());
        assert!(parse("s", &json!({"mode": "audio"}), t).is_err());
        let confirm = parse("s", &json!({"message": "ok ?"}), t).unwrap();
        assert_eq!(confirm.field_count(), 0);
        assert_eq!(human(Duration::from_secs(600)), "10 min");
    }

    /// #143 : la carte revient dans la conversation qui a déclenché l'appel ; sans
    /// conversation, la destination reste vide et le canal prend son repli.
    #[tokio::test]
    async fn a_request_goes_back_to_the_calling_conversation() {
        let broker = Arc::new(Broker::default());
        let to = Destination {
            session_id: Some("s1".into()),
            chat_id: Some(-100_394),
            topic_id: Some(17),
        };
        {
            let _scope = broker.scope("redmine", to.clone());
            assert_eq!(broker.destination("redmine"), to);
            // Un autre serveur ne voit rien de cette conversation.
            assert_eq!(broker.destination("autre"), Destination::default());
        }
        // Le garde tombe avec l'appel : plus de destination.
        assert_eq!(broker.destination("redmine"), Destination::default());
        assert!(!Destination::default().is_known());
        assert!(to.is_known());
    }

    /// #143 : un rappel à mi-délai, une seule fois, puis l'annulation.
    #[tokio::test(start_paused = true)]
    async fn a_pending_request_is_reminded_once_then_cancelled() {
        let broker = Arc::new(Broker::default());
        let rec = Arc::new(Recorder::default());
        broker.attach(rec.clone());
        let out = broker
            .ask(
                "redmine",
                &json!({"message": "Modifier le ticket 42 ?"}),
                Duration::from_secs(600),
            )
            .await
            .expect("demande valide");
        assert_eq!(out.action, Action::Cancel);
        assert_eq!(rec.reminders.lock().unwrap().len(), 1, "un seul rappel");
        let closed = rec.closed.lock().unwrap().clone();
        assert_eq!(closed.len(), 1);
        assert!(closed[0].0.contains("Sans réponse"), "{closed:?}");
        assert!(closed[0].1, "l'annulation propose de relancer");
    }
}
