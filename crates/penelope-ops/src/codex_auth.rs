//! Connexion au compte ChatGPT et jetons du fournisseur `codex` (issue #142).
//!
//! La machine n'a pas de navigateur : la connexion passe par le **code d'appareil** du
//! compte ChatGPT (flux propriétaire d'OpenAI, pas RFC 8628). Le propriétaire ouvre une
//! page et saisit un code à six caractères ; Pénélope sonde jusqu'à l'accord, échange le
//! code contre des jetons, et range le tout dans le magasin de secrets sous `codex.oauth`.
//! `~/.codex/auth.json` n'est **jamais** touché : le magasin de Codex CLI ne nous
//! appartient pas.
//!
//! Le `refresh_token` est **rotatif à usage unique** : deux rafraîchissements concurrents
//! valent `refresh_token_reused`, c'est-à-dire une déconnexion définitive. D'où un verrou
//! de processus, une relecture du magasin sous ce verrou, et l'écriture de la rotation
//! **avant** tout usage du jeton neuf.

use crate::ports::Slot;
use base64::Engine;
use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use penelope_llm::{CodexToken, LlmError, LlmErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Nom du secret qui porte la connexion.
pub const SECRET: &str = "codex.oauth";
/// Clé de l'identifiant d'installation, stable, envoyé sur toutes les requêtes.
const INSTALL_KEY: &str = "codex.installation_id";
/// Marge avant expiration du jeton d'accès : en deçà, on rafraîchit.
const REFRESH_MARGIN_MS: i64 = 5 * 60_000;
/// Un jeton dormant depuis plus de huit jours est rafraîchi, même valide.
const REFRESH_MAX_AGE_MS: i64 = 8 * 24 * 3_600_000;
/// Validité d'un code d'appareil.
pub const DEVICE_CODE_TTL_MS: i64 = 15 * 60_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Un seul rafraîchissement à la fois dans ce processus : la rotation est à usage unique.
static REFRESH_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Connexion au compte ChatGPT, rangée dans le magasin de secrets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Grant {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    /// `chatgpt_account_id` du jeton d'identité.
    pub account_id: String,
    pub plan_type: String,
    #[serde(default)]
    pub fedramp: bool,
    /// Compte, pour l'affichage.
    #[serde(default)]
    pub email: String,
    /// Expiration du jeton d'accès, en millisecondes depuis l'époque.
    pub expires_at: i64,
    /// Dernier rafraîchissement réussi, en millisecondes.
    pub last_refresh: i64,
    /// Raison d'une déconnexion définitive ; tant qu'elle est là, plus rien ne part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnected: Option<String>,
}

impl Grant {
    /// Vrai si le jeton d'accès doit être renouvelé avant usage.
    pub fn needs_refresh(&self, now_ms: i64) -> bool {
        self.expires_at - now_ms < REFRESH_MARGIN_MS
            || now_ms - self.last_refresh > REFRESH_MAX_AGE_MS
    }
    fn token(&self) -> CodexToken {
        CodexToken {
            access_token: self.access_token.clone(),
            account_id: self.account_id.clone(),
            plan_type: self.plan_type.clone(),
            fedramp: self.fedramp,
        }
    }
}

/// Échecs qui ne se réessaient pas : le compte est déconnecté, il faut se reconnecter.
const PERMANENT: &[&str] = &[
    "refresh_token_expired",
    "refresh_token_reused",
    "refresh_token_invalidated",
    "invalid_grant",
];

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ magasin

/// Lit la connexion, si le compte est connecté.
pub fn load(s: &Services) -> Result<Option<Grant>, String> {
    let Some(raw) = s.platform.secrets.get(SECRET).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Écrit la connexion, et masque ses jetons dans les journaux **à chaque rotation**
/// (issue #26) : un jeton neuf non enregistré finirait en clair dans une trace.
pub fn store(s: &Services, g: &Grant) -> Result<(), String> {
    let json = serde_json::to_string(g).map_err(|e| e.to_string())?;
    for token in [&g.access_token, &g.refresh_token, &g.id_token] {
        if !token.is_empty() {
            penelope_observe::register_secret(token);
        }
    }
    // Le magasin écrit la valeur **en hexadécimal** ; c'est sous cette forme qu'elle est
    // ressortie en clair quand l'écriture a échoué (issue #148). Les deux formes sont donc
    // connues du rédacteur **avant** la première tentative.
    penelope_observe::register_secret(&json);
    penelope_observe::register_secret(&hex(&json));
    s.platform
        .secrets
        .set(SECRET, &json)
        .map_err(|e| e.to_string())
}

fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Révoque les jetons d'une connexion côté OpenAI. Appelée quand le rangement échoue :
/// une session d'appareil ouverte que Pénélope ne sait plus joindre reste utilisable par
/// quiconque a vu les jetons passer (issue #148).
pub async fn revoke(s: &Services, g: &Grant) -> bool {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    let Ok(client) = http() else {
        return false;
    };
    let mut done = false;
    for token in [&g.refresh_token, &g.access_token] {
        if token.is_empty() {
            continue;
        }
        let sent = client
            .post(format!("{}/oauth/revoke", c.issuer))
            .form(&[
                ("client_id", c.client_id.as_str()),
                ("token", token.as_str()),
            ])
            .send()
            .await;
        done |= sent.is_ok_and(|r| r.status().is_success());
    }
    let _ = s
        .events
        .append(EventDraft::new(
            "llm.provider_tokens_revoked",
            json!({"provider": "codex", "ok": done}),
        ))
        .await;
    done
}

/// Identifiant d'installation, créé au premier besoin puis stable.
pub async fn installation_id(s: &Services) -> String {
    if let Ok(Some(id)) = s.kv_get(INSTALL_KEY).await
        && !id.trim().is_empty()
    {
        return id;
    }
    let id = penelope_kernel::ids::Ulid::new()
        .to_string_upper()
        .to_lowercase();
    let _ = s.kv_set(INSTALL_KEY, &id).await;
    id
}

// -------------------------------------------------------- code d'appareil

/// Code d'appareil en cours, à montrer au propriétaire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLogin {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_url: String,
    pub interval_s: u64,
    pub expires_at_ms: i64,
}

/// Demande un code d'appareil. Le propriétaire a quinze minutes pour le saisir.
pub async fn start(s: &Services) -> Result<DeviceLogin, String> {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    let resp = http()?
        .post(format!("{}/api/accounts/deviceauth/usercode", c.issuer))
        .json(&json!({"client_id": c.client_id}))
        .send()
        .await
        .map_err(|e| format!("demande de code d'appareil : {e}"))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status == 404 {
        return Err(
            "le code d'appareil n'est pas activé sur ce compte ChatGPT : se connecter une \
             première fois sur chatgpt.com, puis réessayer"
                .into(),
        );
    }
    if status >= 400 {
        return Err(format!(
            "demande de code d'appareil refusée ({status}) : {body}"
        ));
    }
    let device_auth_id = body
        .get("device_auth_id")
        .and_then(|v| v.as_str())
        .ok_or("réponse sans `device_auth_id`")?
        .to_string();
    let user_code = body
        .get("user_code")
        .or_else(|| body.get("usercode"))
        .and_then(|v| v.as_str())
        .ok_or("réponse sans `user_code`")?
        .to_string();
    // `interval` est une **chaîne** chez OpenAI, pas un nombre.
    let interval_s = body
        .get("interval")
        .and_then(|v| match v {
            Value::String(s) => s.parse::<u64>().ok(),
            Value::Number(n) => n.as_u64(),
            _ => None,
        })
        .unwrap_or(5)
        .clamp(1, 60);
    Ok(DeviceLogin {
        device_auth_id,
        user_code,
        verification_url: format!("{}/codex/device", c.issuer),
        interval_s,
        expires_at_ms: s.clock.now_ms() + DEVICE_CODE_TTL_MS,
    })
}

/// Clé de la demande en cours, entre l'affichage du code et l'attente.
const PENDING_KEY: &str = "codex.oauth.pending";

/// Demande un code et le retient : l'appelant l'affiche, puis appelle [`wait_pending`].
pub async fn start_pending(s: &Services) -> Result<DeviceLogin, String> {
    let login = start(s).await?;
    s.kv_set(
        PENDING_KEY,
        &serde_json::to_string(&login).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(login)
}

/// Attend la validation du code affiché plus tôt. La demande est oubliée dans tous les
/// cas : un code mort ne doit pas être resservi.
pub async fn wait_pending(s: &Services) -> Result<Grant, String> {
    let stored = s.kv_get(PENDING_KEY).await;
    let raw = stored
        .map_err(|e| e.to_string())?
        .ok_or("aucune connexion Codex en attente : relancer `penelope model auth codex`")?;
    let login: DeviceLogin = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let out = wait_for(s, &login).await;
    let _ = s.kv_set(PENDING_KEY, "").await;
    out
}

/// Sonde jusqu'à ce que le propriétaire ait validé le code, puis échange et range les
/// jetons. Un 403 ou un 404 veut dire « pas encore » ; tout autre statut est fatal.
pub async fn wait_for(s: &Services, login: &DeviceLogin) -> Result<Grant, String> {
    let cfg = s.config.config();
    let c = cfg.providers.codex.clone();
    let client = http()?;
    loop {
        if s.clock.now_ms() > login.expires_at_ms {
            return Err("code d'appareil expiré : relancer `penelope model auth codex`".into());
        }
        tokio::time::sleep(Duration::from_secs(login.interval_s)).await;
        let resp = client
            .post(format!("{}/api/accounts/deviceauth/token", c.issuer))
            .json(&json!({
                "device_auth_id": login.device_auth_id,
                "user_code": login.user_code,
            }))
            .send()
            .await
            .map_err(|e| format!("sondage du code d'appareil : {e}"))?;
        let status = resp.status().as_u16();
        // En attente : le propriétaire n'a pas encore validé.
        if status == 403 || status == 404 {
            continue;
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status >= 400 {
            return Err(format!("code d'appareil refusé ({status}) : {body}"));
        }
        let field = |k: &str| {
            body.get(k)
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or_else(|| format!("réponse sans `{k}`"))
        };
        // Le PKCE vient du serveur : c'est lui qui a posé le défi.
        let form = [
            ("grant_type", "authorization_code".to_string()),
            ("client_id", c.client_id.clone()),
            ("code", field("authorization_code")?),
            ("redirect_uri", format!("{}/deviceauth/callback", c.issuer)),
            ("code_verifier", field("code_verifier")?),
        ];
        let resp = client
            .post(format!("{}/oauth/token", c.issuer))
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("échange du code : {e}"))?;
        let status = resp.status().as_u16();
        let tokens: Value = resp.json().await.unwrap_or(Value::Null);
        if status >= 400 {
            return Err(format!("échange du code refusé ({status}) : {tokens}"));
        }
        let grant = grant_from_tokens(&tokens, None, s.clock.now_ms())?;
        // Connexion réussie mais rangement impossible : ne rien laisser d'ouvert côté
        // OpenAI, et ne jamais recopier la raison brute — elle a déjà contenu les jetons
        // (issue #148).
        if let Err(e) = store(s, &grant) {
            tracing::error!(error = %penelope_observe::redact(&e), "rangement de la connexion Codex");
            let revoked = revoke(s, &grant).await;
            return Err(format!(
                "connexion annulée : les jetons n'ont pas pu être rangés dans le magasin \
                 de secrets, {}. Détail dans le journal ; relancer `penelope model auth \
                 codex` une fois le magasin réparé (`penelope doctor`).",
                if revoked {
                    "ils ont été révoqués, rien à faire de ton côté"
                } else {
                    "et leur révocation a échoué : révoquer la session d'appareil dans \
                     les réglages de sécurité ChatGPT"
                }
            ));
        }
        s.events
            .append(EventDraft::new(
                "llm.provider_connected",
                json!({"provider": "codex", "plan": grant.plan_type}),
            ))
            .await
            .ok();
        return Ok(grant);
    }
}

/// Construit la connexion à partir d'une réponse de jetons. `previous` porte le
/// `refresh_token` courant, gardé quand le serveur n'en renvoie pas de neuf.
pub fn grant_from_tokens(
    v: &Value,
    previous: Option<&Grant>,
    now_ms: i64,
) -> Result<Grant, String> {
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let access_token = str_of("access_token");
    if access_token.is_empty() {
        return Err("réponse sans `access_token`".into());
    }
    let id_token = match str_of("id_token") {
        t if t.is_empty() => previous.map(|p| p.id_token.clone()).unwrap_or_default(),
        t => t,
    };
    let refresh_token = match str_of("refresh_token") {
        t if t.is_empty() => previous
            .map(|p| p.refresh_token.clone())
            .unwrap_or_default(),
        t => t,
    };
    let claims = jwt_claims(&id_token).unwrap_or_default();
    let auth = claims
        .get("https://api.openai.com/auth")
        .cloned()
        .unwrap_or(Value::Null);
    let auth_str = |k: &str| {
        auth.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    // `exp` du jeton d'accès, sinon `expires_in`, sinon une heure : mieux vaut
    // rafraîchir trop tôt que servir un jeton mort.
    let expires_at = jwt_claims(&access_token)
        .and_then(|c| c.get("exp").and_then(|e| e.as_i64()))
        .map(|exp| exp * 1000)
        .or_else(|| {
            v.get("expires_in")
                .and_then(|e| e.as_i64())
                .map(|secs| now_ms + secs * 1000)
        })
        .unwrap_or(now_ms + 3_600_000);
    let previous_plan = previous.map(|p| p.plan_type.clone()).unwrap_or_default();
    let previous_account = previous.map(|p| p.account_id.clone()).unwrap_or_default();
    let plan_type = match auth_str("chatgpt_plan_type") {
        p if p.is_empty() => previous_plan,
        p => p,
    };
    let account_id = match auth_str("chatgpt_account_id") {
        a if a.is_empty() => previous_account,
        a => a,
    };
    Ok(Grant {
        access_token,
        refresh_token,
        id_token,
        account_id,
        plan_type,
        fedramp: auth
            .get("chatgpt_account_is_fedramp")
            .and_then(|f| f.as_bool())
            .unwrap_or(false),
        email: claims
            .get("email")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string(),
        expires_at,
        last_refresh: now_ms,
        disconnected: None,
    })
}

/// Charge utile d'un JWT, **sans vérifier la signature** : ces claims ne servent qu'à
/// l'affichage et au routage (compte, plan, expiration). Le serveur, lui, vérifie.
pub fn jwt_claims(token: &str) -> Option<serde_json::Map<String, Value>> {
    let payload = token.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<Value>(&raw)
        .ok()?
        .as_object()
        .cloned()
}

// ------------------------------------------------------------ jetons servis

/// Jeton valide pour un appel, rafraîchi si besoin.
pub async fn valid_token(s: &Services) -> Result<CodexToken, String> {
    let grant = load(s)?.ok_or(NOT_CONNECTED)?;
    if let Some(why) = &grant.disconnected {
        return Err(format!("{NOT_CONNECTED} ({why})"));
    }
    if !grant.needs_refresh(s.clock.now_ms()) {
        return Ok(grant.token());
    }
    refresh(s).await
}

/// Message unique de l'absence de connexion : le routeur s'en sert pour se replier.
pub const NOT_CONNECTED: &str = "aucun compte ChatGPT connecté : `penelope model auth codex`";

/// Rafraîchit le jeton d'accès. Sérialisé : la rotation est à usage unique, deux appels
/// concurrents déconnecteraient le compte pour de bon.
pub async fn refresh(s: &Services) -> Result<CodexToken, String> {
    let _guard = REFRESH_LOCK.lock().await;
    // Sous le verrou : un autre appel a pu rafraîchir pendant l'attente.
    let grant = load(s)?.ok_or(NOT_CONNECTED)?;
    if let Some(why) = &grant.disconnected {
        return Err(format!("{NOT_CONNECTED} ({why})"));
    }
    if !grant.needs_refresh(s.clock.now_ms()) {
        return Ok(grant.token());
    }
    if grant.refresh_token.is_empty() {
        return Err(format!(
            "{NOT_CONNECTED} (pas de jeton de rafraîchissement)"
        ));
    }
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    // Le rafraîchissement est en **JSON**, là où l'échange du code est en formulaire.
    let resp = http()?
        .post(format!("{}/oauth/token", c.issuer))
        .json(&json!({
            "grant_type": "refresh_token",
            "client_id": c.client_id,
            "refresh_token": grant.refresh_token,
        }))
        .send()
        .await
        .map_err(|e| format!("rafraîchissement du jeton Codex : {e}"))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status >= 400 {
        let code = body
            .get("error")
            .and_then(|e| {
                e.as_str().map(String::from).or_else(|| {
                    e.get("code")
                        .or_else(|| e.get("type"))
                        .and_then(|c| c.as_str())
                        .map(String::from)
                })
            })
            .unwrap_or_default();
        let permanent = status == 401 || PERMANENT.contains(&code.as_str());
        if permanent {
            disconnect(s, &grant, &code).await;
            return Err(format!(
                "compte ChatGPT déconnecté ({code}) : se reconnecter avec \
                 `penelope model auth codex`"
            ));
        }
        return Err(format!("rafraîchissement refusé ({status}) : {body}"));
    }
    let next = grant_from_tokens(&body, Some(&grant), s.clock.now_ms())?;
    // La rotation est écrite **avant** tout usage : un jeton servi mais non persisté
    // serait rejoué au redémarrage, donc réutilisé, donc fatal.
    store(s, &next)?;
    Ok(next.token())
}

/// Marque la connexion morte : plus rien ne part, le routeur se replie, et le
/// propriétaire est averti.
async fn disconnect(s: &Services, grant: &Grant, reason: &str) {
    let dead = Grant {
        disconnected: Some(if reason.is_empty() {
            "jeton de rafraîchissement refusé".into()
        } else {
            reason.to_string()
        }),
        ..grant.clone()
    };
    let _ = store(s, &dead);
    let _ = s
        .events
        .append(EventDraft::new(
            "llm.provider_disconnected",
            json!({"provider": "codex", "reason": reason}),
        ))
        .await;
}

/// État de la connexion, pour `model auth status`, `doctor` et `/model`.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub connected: bool,
    pub plan: String,
    pub account: String,
    pub expires_at_ms: i64,
    pub last_refresh_ms: i64,
    pub disconnected: Option<String>,
}

pub fn status(s: &Services) -> Result<Option<Status>, String> {
    Ok(load(s)?.map(|g| Status {
        connected: g.disconnected.is_none(),
        plan: g.plan_type,
        account: if g.email.is_empty() {
            g.account_id
        } else {
            g.email
        },
        expires_at_ms: g.expires_at,
        last_refresh_ms: g.last_refresh,
        disconnected: g.disconnected,
    }))
}

/// Déconnecte : révoque côté serveur, puis oublie les jetons.
pub async fn logout(s: &Services) -> Result<(), String> {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    if let Ok(Some(g)) = load(s)
        && !g.refresh_token.is_empty()
        && let Ok(client) = http()
    {
        let _ = client
            .post(format!("{}/oauth/revoke", c.issuer))
            .form(&[
                ("client_id", c.client_id.as_str()),
                ("token", g.refresh_token.as_str()),
            ])
            .send()
            .await;
    }
    s.platform
        .secrets
        .delete(SECRET)
        .map_err(|e| e.to_string())?;
    let _ = s
        .events
        .append(EventDraft::new(
            "llm.provider_disconnected",
            json!({"provider": "codex", "reason": "logout"}),
        ))
        .await;
    Ok(())
}

// ------------------------------------------------------- source pour le LLM

/// Source de jetons donnée au fournisseur : le daemon garde le magasin, le verrou et la
/// rotation ; `penelope-llm` ne voit qu'un jeton.
pub struct DaemonTokens {
    services: Arc<Services>,
}

impl DaemonTokens {
    pub fn new(services: Arc<Services>) -> Self {
        DaemonTokens { services }
    }
}

fn auth_error(message: String) -> LlmError {
    LlmError::new(LlmErrorKind::Auth, message)
}

#[async_trait::async_trait]
impl penelope_llm::TokenSource for DaemonTokens {
    async fn token(&self) -> penelope_llm::Result<CodexToken> {
        valid_token(&self.services).await.map_err(auth_error)
    }
    async fn refreshed(&self) -> penelope_llm::Result<CodexToken> {
        refresh(&self.services).await.map_err(auth_error)
    }
}

/// Rafraîchissement **hors tour** : une vérification par minute, pour qu'un tour ne
/// commence jamais par attendre un jeton. Prévient le propriétaire une fois quand la
/// connexion est morte.
pub async fn refresh_loop(s: Arc<Services>, messenger: Slot<dyn Messenger>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if !s.config.config().providers.codex.enabled {
            continue;
        }
        // Les jauges du plan sont lues à chaque réponse ; l'alerte part d'ici, une fois
        // par fenêtre (#142).
        if let Err(e) = crate::codex_quota::check_alert(&s, messenger.get()).await {
            tracing::warn!(error = %e, "alerte de quota Codex non vérifiée");
        }
        let Ok(Some(grant)) = load(&s) else { continue };
        if grant.disconnected.is_some() {
            notify_disconnected(&s, messenger.get(), &grant).await;
            continue;
        }
        if !grant.needs_refresh(s.clock.now_ms()) {
            continue;
        }
        if let Err(e) = refresh(&s).await {
            tracing::warn!(error = %e, "jeton Codex non rafraîchi");
            if let Ok(Some(g)) = load(&s)
                && g.disconnected.is_some()
            {
                notify_disconnected(&s, messenger.get(), &g).await;
            }
        }
    }
}

/// Une seule annonce par déconnexion : la suivante attend une reconnexion.
async fn notify_disconnected(s: &Services, messenger: Option<Arc<dyn Messenger>>, grant: &Grant) {
    let reason = grant.disconnected.clone().unwrap_or_default();
    let key = format!("codex.disconnected.notified.{reason}");
    if s.kv_get(&key).await.ok().flatten().is_some() {
        return;
    }
    let _ = s.kv_set(&key, &s.clock.now_rfc3339()).await;
    let text = format!(
        "🔌 **Compte ChatGPT déconnecté** ({reason}).\n\nLes modèles `codex:` repassent \
         par OpenRouter en attendant. Pour reconnecter : `penelope model auth codex`, ou \
         `/model auth codex` ici."
    );
    match messenger {
        Some(m) => {
            let origin = crate::bus::Origin::Internal {
                source: "codex".into(),
            };
            if let Err(e) = m.send_text(&origin, &text).await {
                tracing::warn!(error = %e, "avis de déconnexion Codex non envoyé");
            }
        }
        None => tracing::warn!(avis = %text, "déconnexion Codex sans canal"),
    }
}

#[cfg(test)]
mod tests;
