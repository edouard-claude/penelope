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

/// Une demande présentée au propriétaire.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: String,
    pub server: String,
    pub message: String,
    pub kind: Kind,
    /// Attente maximale avant annulation.
    pub timeout: Duration,
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
    /// Remplace la carte par une issue (délai dépassé, lien terminé).
    async fn close(&self, request: &Request, card: Option<i64>, markdown: &str);
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

    /// Présente une demande `elicitation/create` et attend l'issue. `Err` : paramètres
    /// invalides (−32602 pour le serveur).
    pub async fn ask(
        &self,
        server: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Outcome, String> {
        let request = parse(server, params, timeout)?;
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
        match tokio::time::timeout(request.timeout, &mut rx).await {
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
                                 informé.",
                                human(request.timeout),
                                request.server
                            ),
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
    })
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

    struct Recorder {
        shown: Mutex<Vec<Request>>,
        closed: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl OwnerChannel for Recorder {
        async fn show(&self, request: &Request) -> Result<Option<i64>, String> {
            self.shown.lock().unwrap().push(request.clone());
            Ok(Some(42))
        }
        async fn close(&self, _request: &Request, card: Option<i64>, markdown: &str) {
            assert_eq!(card, Some(42));
            self.closed.lock().unwrap().push(markdown.to_string());
        }
    }

    fn recorder() -> Arc<Recorder> {
        Arc::new(Recorder {
            shown: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
        })
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
        assert!(r.closed.lock().unwrap()[0].contains("Sans réponse"));
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
        assert!(r.closed.lock().unwrap()[0].contains("terminée"));
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
}
