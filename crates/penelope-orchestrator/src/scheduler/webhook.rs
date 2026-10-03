//! Webhooks entrants (#294) : la première porte par laquelle l'extérieur pousse un
//! événement dans Pénélope, ouverte sur `127.0.0.1` seulement par défaut.
//!
//! ```text
//!  POST /hook/<jeton> ──► chemin connu et actif ? (404) ──► HMAC-SHA256 de `<ts>.<corps>`
//!  X-Penelope-Timestamp                                     horodatage à ±5 min (401)
//!  X-Penelope-Signature                                     signature déjà vue ? (409)
//!        │
//!        ├─► débit par hook (429) ─► corps JSON (400) ─► `filter` (202, écarté)
//!        └─► cible prompt : plafond de tours par heure, tous hooks (429) ─► fire ─► 202
//!
//!  chaque réception ──► événement `webhook.received` (statut, motif, taille, empreinte,
//!                       adresse ; jamais le corps ni les en-têtes)
//! ```
//!
//! Le secret vit dans le magasin de secrets sous `spec.secret_ref` ; le store ne porte que
//! ce nom, et la valeur n'est montrée qu'une fois, à la création en ligne de commande. Le
//! corps destiné à un prompt n'est jamais substitué dans le texte : il y est ajouté
//! encadré comme non fiable (`wrap_untrusted`, #92), comme un message transféré. Un
//! listener dédié plutôt que le retour OAuth de `mcp.callback_port` : décision 0019.

use super::*;
use penelope_kernel::canonical::sha256_hex;
use penelope_kernel::event::EventDraft;
use penelope_kernel::hmac::{constant_time_eq, hmac_sha256};
use penelope_workflow::schedules::{WEBHOOK_PATH_PREFIX, passes_filter};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};

mod http;
use http::{Request, Response, read_request, write_response};

/// En-tête de la signature : `sha256=<hex>` du HMAC-SHA256, par le secret, de
/// l'horodatage, d'un point et du corps brut (`<ts>.<corps>`).
pub const SIGNATURE_HEADER: &str = "x-penelope-signature";
/// En-tête de l'horodatage signé, en secondes Unix : sans lui, une requête capturée
/// resterait rejouable à jamais.
pub const TIMESTAMP_HEADER: &str = "x-penelope-timestamp";
/// Écart admis entre l'horodatage signé et l'horloge, dans les deux sens.
const TOLERANCE_S: i64 = 300;
/// Variable de tir qui porte le corps reçu, à encadrer comme non fiable dans un prompt.
pub(super) const UNTRUSTED_BODY: &str = "corps_non_fiable";
/// Variable de tir : identifiant de la livraison, qui rend chaque réception unique.
pub(super) const DELIVERY: &str = "livraison";
/// Préfixe des noms de secret des webhooks dans le magasin.
const SECRET_PREFIX: &str = "webhook_";
/// Une connexion qui n'a pas fini dans ce délai est coupée.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);

// ------------------------------------------------------------------ création, retrait

/// Création : le chemin et le secret sont attribués ici, jamais choisis par l'appelant ;
/// le secret est rangé dans le magasin et la spécification n'en garde que le nom. Rend la
/// spécification complétée et le secret, à montrer une seule fois. `draw` tire les
/// jetons (`secret_token` hors tests) : sans aléa du système, pas de webhook, jamais un
/// secret devinable.
pub(super) fn prepare(
    s: &Services,
    spec: Value,
    draw: impl Fn(usize) -> Result<String, String>,
) -> Result<(Value, String), String> {
    let mut spec = match spec {
        Value::Null => json!({}),
        v @ Value::Object(_) => v,
        _ => return Err("`spec` d'un webhook : un objet, vide ou avec `filter`".into()),
    };
    for k in ["path", "secret_ref", "secret"] {
        if spec.get(k).is_some() {
            return Err(format!(
                "`{k}` est attribué par Pénélope à la création : ne pas le donner"
            ));
        }
    }
    let refused = |e: String| format!("webhook non créé, chemin et secret non tirés : {e}");
    let secret = draw(48).map_err(refused)?;
    let token = draw(24).map_err(refused)?;
    let secret_ref = format!("{SECRET_PREFIX}{token}");
    s.platform.secrets.set(&secret_ref, &secret).map_err(|e| {
        format!(
            "secret du webhook non rangé dans le magasin ({}) : {e}",
            s.platform.secrets.backend()
        )
    })?;
    spec["path"] = json!(format!("{WEBHOOK_PATH_PREFIX}{token}"));
    spec["secret_ref"] = json!(secret_ref);
    Ok((spec, secret))
}

/// Oublie le secret d'un webhook dont la planification part : le magasin ne garde pas un
/// secret que plus rien ne vérifie. Sans effet pour un autre déclencheur.
pub(super) fn forget_secret(s: &Services, sched: &Schedule) {
    if sched.kind != TriggerKind::Webhook {
        return;
    }
    if let Some(name) = sched.spec.get("secret_ref").and_then(|v| v.as_str())
        && let Err(e) = s.platform.secrets.delete(name)
    {
        tracing::warn!(schedule = %sched.id, error = %e, "secret du webhook non effacé");
    }
}

/// Supprime une planification (#221) et, pour un webhook, son secret (#294). Faux si elle
/// n'existe pas ou est déjà supprimée.
pub async fn remove(s: &Services, id: &str) -> anyhow::Result<bool> {
    let before = s.schedules.get(id).await?;
    if !s.schedules.set_state(id, "deleted").await? {
        return Ok(false);
    }
    if let Some(sched) = before {
        forget_secret(s, &sched);
    }
    Ok(true)
}

/// Pause, reprise ou suppression d'une planification (#221) ; la suppression passe par
/// `remove`, qui efface le secret d'un webhook (#294). Faux si elle est inconnue.
pub async fn set_state(s: &Services, id: &str, state: &str) -> anyhow::Result<bool> {
    match state {
        "deleted" => remove(s, id).await,
        _ => Ok(s.schedules.set_state(id, state).await?),
    }
}

/// Adresse locale d'un hook, pour la réponse de création : ce que le tunnel ou le réseau
/// privé devra joindre.
pub(super) fn local_url(listen: &str, path: &str) -> String {
    match listen.trim() {
        "" => path.to_string(),
        host => format!("http://{host}{path}"),
    }
}

/// La réponse d'un outil entre dans le journal de la conversation : le secret n'y passe
/// pas. Le propriétaire le pose lui-même, ou le lit à une création en ligne de commande.
pub fn withhold_secret(v: &mut Value) {
    if let Some(o) = v.as_object_mut()
        && o.remove("secret").is_some()
    {
        o.insert(
            "secret".into(),
            json!(
                "non montré ici : rangé dans le magasin de secrets sous `secret_ref` ; le \
                 propriétaire le remplace par le sien avec `penelope secret set <secret_ref>`"
            ),
        );
    }
}

// ------------------------------------------------------------------ serveur

/// Fenêtres glissantes : réceptions par hook (`rate_per_minute`) et tours `prompt`
/// déclenchés par tous les hooks (`prompt_turns_per_hour`). En mémoire : un redémarrage
/// remet les compteurs à zéro, ce qui ne desserre rien de dangereux.
#[derive(Default)]
struct Limits {
    per_hook: Mutex<HashMap<String, VecDeque<i64>>>,
    prompts: Mutex<VecDeque<i64>>,
    seen: Mutex<Seen>,
}

/// Les signatures admises, par hook, avec l'instant (ms) où leur horodatage sort de la
/// fenêtre : au-delà, la signature est refusée par l'horodatage, plus besoin de s'en
/// souvenir. Seules des signatures valides y entrent, et le débit par hook en borne le
/// nombre. En mémoire : après un redémarrage, une requête capturée dans les cinq
/// dernières minutes passerait une fois de plus (dit dans la doc).
type Seen = HashMap<(String, [u8; 32]), i64>;

impl Limits {
    /// Retient une signature ; faux si elle l'est déjà (un rejeu). Vérifier et retenir
    /// sous le même verrou : deux copies simultanées ne passent pas toutes les deux.
    fn first_sight(&self, key: &(String, [u8; 32]), expires_ms: i64, now_ms: i64) -> bool {
        let mut seen = lock(&self.seen);
        seen.retain(|_, until| *until > now_ms);
        if seen.contains_key(key) {
            return false;
        }
        seen.insert(key.clone(), expires_ms);
        true
    }

    /// Oublie une signature dont la livraison a été refusée par le débit : l'appelant qui
    /// réessaie la même livraison plus tard ne doit pas être pris pour un rejeu.
    fn forget(&self, key: &(String, [u8; 32])) {
        lock(&self.seen).remove(key);
    }
}

/// Admet un passage dans la fenêtre, ou rend l'attente avant le prochain, en millisecondes.
fn admit(window: &mut VecDeque<i64>, now_ms: i64, span_ms: i64, max: u32) -> Result<(), i64> {
    while window.front().is_some_and(|t| now_ms - *t >= span_ms) {
        window.pop_front();
    }
    if window.len() >= max as usize {
        let wait = window
            .front()
            .map(|t| t + span_ms - now_ms)
            .unwrap_or(span_ms);
        return Err(wait.max(1));
    }
    window.push_back(now_ms);
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Le serveur : le contexte de l'ordonnanceur, ses branchements et les fenêtres de débit.
pub(crate) struct Server {
    d: Context,
    ports: Ports,
    limits: Limits,
}

impl Server {
    pub(crate) fn new(d: Context, ports: Ports) -> Server {
        Server {
            d,
            ports,
            limits: Limits::default(),
        }
    }
}

/// Boucle du serveur, jusqu'à l'arrêt du daemon : lie `webhooks.listen` (lu au lancement,
/// la clé demande un redémarrage) et sert. Une adresse indisponible est réessayée toutes
/// les 30 s plutôt que de finir la boucle : `doctor` la verrait finie, et le port peut se
/// libérer.
pub async fn webhook_server(d: Context, ports: Ports) {
    let listen = d
        .services
        .config
        .config()
        .webhooks
        .listen
        .trim()
        .to_string();
    if listen.is_empty() {
        tracing::info!("webhooks entrants : `webhooks.listen` vide, aucun serveur");
        return;
    }
    let server = Arc::new(Server::new(d, ports));
    while !server.d.handle.is_shutting_down() {
        match TcpListener::bind(&listen).await {
            Ok(listener) => {
                tracing::info!(adresse = %listen, "webhooks entrants à l'écoute");
                serve_on(server, listener).await;
                return;
            }
            Err(e) => {
                tracing::warn!(adresse = %listen, error = %e, "webhooks entrants : adresse indisponible, nouvel essai dans 30 s");
                wait(&server.d, Duration::from_secs(30)).await;
            }
        }
    }
}

async fn wait(d: &Context, total: Duration) {
    let step = Duration::from_millis(500);
    let mut waited = Duration::ZERO;
    while waited < total && !d.handle.is_shutting_down() {
        tokio::time::sleep(step).await;
        waited += step;
    }
}

/// Sert les connexions d'un listener déjà lié, jusqu'à l'arrêt. Chaque connexion est une
/// tâche bornée dans le temps, récoltée à l'itération suivante ; l'arrêt coupe celles
/// qui restent.
pub(crate) async fn serve_on(server: Arc<Server>, listener: TcpListener) {
    let mut connections = tokio::task::JoinSet::new();
    while !server.d.handle.is_shutting_down() {
        while connections.try_join_next().is_some() {}
        let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        let Ok(Ok((stream, peer))) = accepted else {
            continue;
        };
        let server = server.clone();
        connections.spawn(async move {
            let done =
                tokio::time::timeout(CONNECTION_TIMEOUT, handle_connection(&server, stream, peer))
                    .await;
            match done {
                Ok(Err(e)) => tracing::debug!(%peer, error = %e, "connexion webhook"),
                Err(_) => tracing::debug!(%peer, "connexion webhook : délai dépassé"),
                Ok(Ok(())) => {}
            }
        });
    }
    connections.shutdown().await;
}

async fn handle_connection(
    server: &Server,
    mut stream: TcpStream,
    peer: SocketAddr,
) -> std::io::Result<()> {
    let max_body = server.d.services.config.config().webhooks.max_body_bytes;
    let response = match read_request(&mut stream, max_body).await {
        Ok(req) => server.dispatch(&req, peer).await,
        Err(e) => {
            let (status, reason) = e.status();
            server
                .journal(&Reception {
                    status,
                    reason: Some(reason.clone()),
                    remote: peer.to_string(),
                    ..Reception::default()
                })
                .await;
            Response::json(status, &json!({"accepted": false, "reason": reason}))
        }
    };
    write_response(&mut stream, &response).await
}

/// Ce que le journal garde d'une réception : jamais le corps ni les en-têtes.
#[derive(Default)]
struct Reception {
    schedule: Option<String>,
    path: Option<String>,
    method: Option<String>,
    status: u16,
    reason: Option<String>,
    bytes: usize,
    body_sha256: Option<String>,
    delivery: Option<String>,
    remote: String,
}

/// Ce qu'une requête acceptée a donné.
enum Outcome {
    /// La cible a tiré (ou son tour est enfilé).
    Fired { schedule: String, delivery: String },
    /// Le filtre a écarté le corps : accepté, rien de déclenché, pour que l'appelant ne
    /// réessaie pas.
    Filtered { schedule: String, delivery: String },
}

struct Refusal {
    status: u16,
    reason: String,
    retry_after_s: Option<u64>,
}

impl Refusal {
    fn new(status: u16, reason: impl Into<String>) -> Refusal {
        Refusal {
            status,
            reason: reason.into(),
            retry_after_s: None,
        }
    }
    fn retry_after_ms(mut self, ms: i64) -> Refusal {
        self.retry_after_s = Some((ms.max(1000) / 1000) as u64);
        self
    }
}

impl Server {
    async fn dispatch(&self, req: &Request, peer: SocketAddr) -> Response {
        let mut rec = Reception {
            path: Some(req.path().to_string()),
            method: Some(req.method.clone()),
            bytes: req.body.len(),
            remote: peer.to_string(),
            ..Reception::default()
        };
        let response = match self.handle(req, &mut rec).await {
            Ok(Outcome::Fired { schedule, delivery }) => {
                rec.status = 202;
                Response::json(
                    202,
                    &json!({"accepted": true, "schedule": schedule, "delivery": delivery}),
                )
            }
            Ok(Outcome::Filtered { schedule, delivery }) => {
                rec.status = 202;
                rec.reason = Some("filtre".into());
                Response::json(
                    202,
                    &json!({"accepted": false, "reason": "filtre", "schedule": schedule,
                            "delivery": delivery}),
                )
            }
            Err(r) => {
                rec.status = r.status;
                rec.reason = Some(r.reason.clone());
                let mut resp =
                    Response::json(r.status, &json!({"accepted": false, "reason": r.reason}));
                if r.status == 405 {
                    resp = resp.with_header("Allow", "POST");
                }
                if let Some(s) = r.retry_after_s {
                    resp = resp.with_header("Retry-After", s);
                }
                resp
            }
        };
        self.journal(&rec).await;
        response
    }

    /// La chaîne des contrôles, dans l'ordre du schéma du module. La signature passe
    /// avant le débit : un flot non signé n'épuise pas le budget du vrai appelant, et
    /// un HMAC ne coûte rien.
    async fn handle(&self, req: &Request, rec: &mut Reception) -> Result<Outcome, Refusal> {
        let s = &self.d.services;
        if req.method != "POST" {
            return Err(Refusal::new(405, "seule la méthode POST déclenche"));
        }
        let path = req.path();
        if !path.starts_with(WEBHOOK_PATH_PREFIX) {
            return Err(Refusal::new(404, "chemin inconnu"));
        }
        let sched = self
            .find(path)
            .await
            .map_err(|e| Refusal::new(500, format!("planifications illisibles : {e}")))?
            .ok_or_else(|| Refusal::new(404, "hook inconnu ou en pause"))?;
        rec.schedule = Some(sched.id.clone());
        let secret_ref = sched.spec["secret_ref"].as_str().unwrap_or_default();
        let secret = match s.platform.secrets.get(secret_ref) {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Err(Refusal::new(
                    500,
                    format!(
                        "secret `{secret_ref}` absent du magasin : `penelope secret set {secret_ref}`"
                    ),
                ));
            }
            Err(e) => {
                return Err(Refusal::new(
                    500,
                    format!("secret `{secret_ref}` illisible : {e}"),
                ));
            }
        };
        let now = s.clock.now_ms();
        let (mac, ts) = verify(
            req.header(TIMESTAMP_HEADER),
            req.header(SIGNATURE_HEADER),
            secret.as_bytes(),
            &req.body,
            now.div_euclid(1000),
        )
        .map_err(|why| Refusal::new(401, why))?;
        let seen = (sched.id.clone(), mac);
        if !self
            .limits
            .first_sight(&seen, (ts + TOLERANCE_S + 1) * 1000, now)
        {
            return Err(Refusal::new(
                409,
                "livraison déjà reçue : même horodatage, même signature (rejeu refusé)",
            ));
        }
        let (per_minute, per_hour) = {
            let cfg = s.config.config();
            (
                cfg.webhooks.rate_per_minute,
                cfg.webhooks.prompt_turns_per_hour,
            )
        };
        let rate = admit(
            lock(&self.limits.per_hook)
                .entry(sched.id.clone())
                .or_default(),
            now,
            60_000,
            per_minute,
        );
        if let Err(wait_ms) = rate {
            self.limits.forget(&seen);
            return Err(Refusal::new(
                429,
                format!("plus de {per_minute} réceptions dans la minute pour ce hook"),
            )
            .retry_after_ms(wait_ms));
        }
        let body: Value = serde_json::from_slice(&req.body)
            .map_err(|e| Refusal::new(400, format!("corps JSON attendu : {e}")))?;
        let fingerprint = sha256_hex(&req.body);
        rec.body_sha256 = Some(fingerprint.clone());
        let delivery = penelope_kernel::ids::Ulid::new().to_string();
        rec.delivery = Some(delivery.clone());
        if !passes_filter(&body, sched.spec.get("filter")) {
            return Ok(Outcome::Filtered {
                schedule: sched.id.clone(),
                delivery,
            });
        }
        let prompt = sched.target_kind() == Some(TargetKind::Prompt);
        if prompt
            && let Err(wait_ms) = admit(&mut lock(&self.limits.prompts), now, 3_600_000, per_hour)
        {
            return Err(Refusal::new(
                429,
                format!("plafond de {per_hour} tours prompt par heure atteint pour les webhooks"),
            )
            .retry_after_ms(wait_ms));
        }

        let mut vars = BTreeMap::new();
        vars.insert(DELIVERY.to_string(), delivery.clone());
        vars.insert("hook".to_string(), path.to_string());
        // Un prompt ne reçoit le corps qu'encadré, après le texte (`fire`) ; une
        // notification ou un workflow le reçoivent comme élément (`{{payload}}`,
        // `{{item}}`, champs de premier niveau) : pas de modèle entre les deux.
        let items = if prompt {
            vars.insert(
                UNTRUSTED_BODY.to_string(),
                serde_json::to_string_pretty(&body).unwrap_or_default(),
            );
            Vec::new()
        } else {
            vars.insert("payload".to_string(), body.to_string());
            vec![PolledItem {
                id: delivery.clone(),
                fingerprint,
                value: body,
            }]
        };
        // Hors fournée des heures calmes (#296) : la livraison est servie dans la requête,
        // son corps n'est gardé nulle part pour partir plus tard.
        let result = fire(&self.d, &self.ports, &sched, &items, &vars, None)
            .await
            .map(|_| true);
        let failed = result.as_ref().err().map(|e| e.to_string());
        let mut report = TickReport::default();
        finish(&self.d, &self.ports, &sched, result, &mut report)
            .await
            .map_err(|e| Refusal::new(500, format!("tir non enregistré : {e}")))?;
        match failed {
            Some(e) => Err(Refusal::new(500, format!("cible en échec : {e}"))),
            None => Ok(Outcome::Fired {
                schedule: sched.id.clone(),
                delivery,
            }),
        }
    }

    /// La planification active qui porte ce chemin.
    async fn find(&self, path: &str) -> anyhow::Result<Option<Schedule>> {
        Ok(self
            .d
            .services
            .schedules
            .list()
            .await?
            .into_iter()
            .find(|s| s.state == "active" && s.webhook_path() == Some(path)))
    }

    async fn journal(&self, rec: &Reception) {
        let draft = EventDraft::new(
            "webhook.received",
            json!({
                "schedule": rec.schedule,
                "path": rec.path,
                "method": rec.method,
                "status": rec.status,
                "reason": rec.reason,
                "bytes": rec.bytes,
                "body_sha256": rec.body_sha256,
                "delivery": rec.delivery,
                "remote": rec.remote,
            }),
        );
        if let Err(e) = self.d.services.events.append(draft).await {
            tracing::warn!(error = %e, "réception de webhook non journalisée");
        }
    }
}

/// Vérifie l'horodatage (secondes Unix, chiffres seuls, à ±`TOLERANCE_S` de `now_s`) puis
/// la signature `sha256=<hex>` (préfixe facultatif), comparée en temps constant au HMAC de
/// `<ts>.<corps>`. Rend le HMAC, clé de l'anti-rejeu, et l'horodatage ; sinon le motif.
fn verify(
    timestamp: Option<&str>,
    signature: Option<&str>,
    secret: &[u8],
    body: &[u8],
    now_s: i64,
) -> Result<([u8; 32], i64), String> {
    let ts = timestamp
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 12 && t.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|t| t.parse::<i64>().ok())
        .ok_or("horodatage absent ou illisible (X-Penelope-Timestamp: <secondes Unix>)")?;
    if (now_s - ts).abs() > TOLERANCE_S {
        return Err(format!(
            "horodatage hors de la fenêtre de ±{TOLERANCE_S} s (X-Penelope-Timestamp: {ts}, \
             horloge : {now_s}) : signer au moment de l'envoi"
        ));
    }
    let malformed = "signature absente ou invalide (X-Penelope-Signature: sha256=<HMAC-SHA256 \
                     hexadécimal de `<horodatage>.<corps>`>)";
    let given = signature
        .map(str::trim)
        .and_then(|h| decode_hex(h.strip_prefix("sha256=").unwrap_or(h)))
        .ok_or(malformed)?;
    let mut message = Vec::with_capacity(body.len() + 13);
    message.extend_from_slice(ts.to_string().as_bytes());
    message.push(b'.');
    message.extend_from_slice(body);
    let mac = hmac_sha256(secret, &message);
    if !constant_time_eq(&given, &mac) {
        return Err(malformed.into());
    }
    Ok((mac, ts))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect()
}

#[cfg(test)]
mod tests;
