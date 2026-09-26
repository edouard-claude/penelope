//! Fonctions partagées entre modules du daemon qui ne demandent que `&Services` ou rien :
//! clés kv, lecteurs de configuration, origine du propriétaire, binaire en cours.
//! Rassemblées ici pour que les modules cessent de se citer les uns les autres pour
//! une ligne (épopée #208, tâche T06) ; destinées au futur crate d'application.

use crate::bus::Origin;
use crate::services::Services;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------- Vault et heure du propriétaire (depuis conversation.rs)

/// Répertoire du vault, placeholders développés.
pub fn vault_dir(s: &Services) -> std::path::PathBuf {
    let cfg = s.config.config();
    s.platform.dirs.expand(&cfg.memory.vault_path)
}

/// Date et heure locales du propriétaire, lisibles.
pub fn local_now(s: &Services) -> String {
    let cfg = s.config.config();
    let utc = chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
    match cfg.owner.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => utc
            .with_timezone(&tz)
            .format("%A %d %B %Y, %H:%M")
            .to_string(),
        Err(_) => utc.format("%A %d %B %Y, %H:%M UTC").to_string(),
    }
}

// ---------------------------------------- Origine des messages du daemon (depuis scheduler.rs)

/// Conversation privée du propriétaire sur Telegram, s'il est configuré.
pub fn owner_origin_of(s: &Services) -> Origin {
    let owner = s.config.config().owner.telegram_user_id;
    if owner != 0 {
        Origin::Telegram {
            chat_id: owner,
            topic_id: None,
            message_id: None,
        }
    } else {
        Origin::Internal {
            source: "daemon".into(),
        }
    }
}

// ---------------------------------------- Montants (depuis rpc.rs)

/// Montant lisible : six décimales suffisent à distinguer un appel d'un autre.
pub fn round_usd(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

// ---------------------------------------- Configuration (depuis rpc.rs)

/// `config set a.b.c = valeur` : applique une modification par chemin.
pub fn set_config_path(s: &Services, path: &str, value: Value) -> anyhow::Result<u64> {
    let path_owned = path.to_string();
    let generation = s.publish_config("cli", move |c| {
        let mut v = serde_json::to_value(&*c).map_err(penelope_kernel::KernelError::Json)?;
        let parts: Vec<&str> = path_owned.split('.').collect();
        let mut cur = &mut v;
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                let obj = cur.as_object_mut().ok_or_else(|| {
                    penelope_kernel::KernelError::config(format!("chemin invalide : {path_owned}"))
                })?;
                // Une table à clés libres accepte une entrée nouvelle (#128) ; ailleurs,
                // une clé absente est une faute de frappe.
                let parent = parts[..i].join(".");
                if !obj.contains_key(*part)
                    && !penelope_kernel::config::MAP_PATHS.contains(&parent.as_str())
                {
                    return Err(penelope_kernel::KernelError::config(format!(
                        "clé inconnue : {path_owned}"
                    )));
                }
                // Une valeur seule vaut une liste d'un élément, comme pour `mcp edit`
                // (issue #138).
                let value = match obj.get(*part) {
                    Some(current) => {
                        penelope_kernel::config::list_value(&path_owned, current, value.clone())
                            .map_err(penelope_kernel::KernelError::config)?
                    }
                    None => value.clone(),
                };
                obj.insert((*part).to_string(), value);
            } else {
                cur = cur.get_mut(part).ok_or_else(|| {
                    penelope_kernel::KernelError::config(format!("clé inconnue : {path_owned}"))
                })?;
            }
        }
        let updated: penelope_kernel::Config =
            serde_json::from_value(v).map_err(penelope_kernel::KernelError::Json)?;
        // Un réglage qui annule sa propre intention est refusé, nommément (issue #16).
        let refused = penelope_kernel::coherence::new_refusals(c, &updated, &path_owned);
        if !refused.is_empty() {
            return Err(penelope_kernel::KernelError::config(format!(
                "réglage refusé : {}",
                refused
                    .iter()
                    .map(|r| r.message.clone())
                    .collect::<Vec<_>>()
                    .join(" ; ")
            )));
        }
        *c = updated;
        Ok(vec![path_owned.clone()])
    })?;
    Ok(generation)
}

// ---------------------------------------- Clés kv et lecteurs du canal (depuis telegram.rs)

/// Une valeur JSON telle qu'on la montre dans une bulle : une chaîne sans guillemets, un
/// nombre tel quel, une absence en « ? », jamais `null` (issue #115).
pub fn shown(v: &Value) -> String {
    match v {
        Value::Null => "?".into(),
        Value::String(s) if s.is_empty() => "?".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Lien vers un écran ou une commande du canal (issue #30), quand il sait en faire un :
/// c'est la passerelle qui le construit (T36).
pub async fn deep_link(s: &Services, payload: &str) -> Option<String> {
    s.channel.cards.get()?.deep_link(payload).await
}

/// Clé du nom d'un sujet Telegram, lu dans les messages du sujet (issue #119).
pub fn topic_name_key(chat_id: i64, topic_id: i64) -> String {
    format!("tg.topic_name.{chat_id}.{topic_id}")
}

/// Conversations refusées récemment, les plus récentes d'abord (issue #113).
pub const SEEN_CHATS_KEY: &str = "telegram.seen_chats";

/// Conversations refusées récemment.
pub async fn seen_chats(s: &crate::services::Services) -> Vec<Value> {
    s.store
        .read(|c| penelope_store::kv_get(c, SEEN_CHATS_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

// ---------------------------------------- Binaire en cours (depuis upgrade.rs)

/// Chemin réel du binaire en cours.
pub fn running_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Vrai pour un binaire de compilation (`target/debug`, `target/release`).
pub fn is_source_build(exe: &Path) -> bool {
    let parts: Vec<String> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    parts
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
}

// ---------------------------------------- Clés kv de session et de workflow (engine.rs, workflow.rs)

pub fn last_model_key(session_id: &str) -> String {
    format!("session.model_last.{session_id}")
}

/// Alias épinglé sur une session (`/model`, `penelope session model`).
pub fn pin_key(session_id: &str) -> String {
    format!("session.model_pin.{session_id}")
}

/// Alias épinglé sur une session, s'il existe encore dans la configuration. Lu par le
/// routage du moteur et par la compaction, qui y prend la fenêtre de la conversation
/// (épopée #208, T23 : la compaction ne passe plus par le daemon).
pub async fn pinned_model(s: &Services, session_id: &str) -> Option<penelope_llm::StickyModel> {
    let alias = s
        .kv_get(&pin_key(session_id))
        .await
        .ok()
        .flatten()
        .filter(|a| !a.is_empty())?;
    let cfg = s.config.config();
    match cfg.alias_model(&alias) {
        Some(id) => Some(penelope_llm::StickyModel {
            alias,
            model_id: id.to_string(),
        }),
        None => {
            tracing::warn!(session = session_id, alias = %alias, "alias épinglé disparu de la configuration");
            None
        }
    }
}

/// Au-delà, le cache des fournisseurs a expiré. Même valeur que
/// `penelope_agent::CACHE_TTL_MS`, que la conversation ne peut pas citer (elle ne dépend
/// pas de la boucle) ; un test du daemon tient les deux égales jusqu'à ce que la boucle
/// lise celle-ci (T30).
pub const CACHE_TTL_MS: i64 = 5 * 60_000;

/// Clé du `step_done()` / `return_value` d'un run.
pub fn step_done_key(run_id: &str) -> String {
    format!("wf.step_done.{run_id}")
}

// ---------------------------------------- Workspaces et bac à sable (depuis executor/mod.rs)

/// Chemins dont la lecture est refusée aux commandes sous bac à sable (issue #68).
pub fn denied_reads(s: &Services) -> Vec<PathBuf> {
    s.config
        .config()
        .sandbox
        .deny_read
        .iter()
        .map(|d| s.platform.dirs.expand(d))
        .collect()
}

/// Workspaces autorisés : configuration, sinon `{data}/workspace`.
pub fn canonical_workspace(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| penelope_platform::sandbox::normalise(path))
}

pub fn default_workspaces(s: &Services) -> Vec<PathBuf> {
    let cfg = s.config.config();
    let mut v: Vec<PathBuf> = cfg
        .sandbox
        .workspaces
        .iter()
        .map(|w| s.platform.dirs.expand(w))
        .collect();
    if v.is_empty() {
        let ws = s.platform.dirs.data().join("workspace");
        let _ = std::fs::create_dir_all(&ws);
        v.push(ws);
    }
    v.iter().map(|p| canonical_workspace(p)).collect()
}

// ---------------------------------------- Sessions (depuis titles.rs)

/// Titre avec sa date, pour les listes : « Refonte du site (16/09) ». La même forme que
/// `penelope_conversation::titles::label`, lue ici par la purge (`penelope-ops`), qui ne
/// dépend pas de la conversation (épopée #208, T28).
pub fn session_label(session: &penelope_kernel::session::Session) -> String {
    let date = session
        .last_activity
        .as_deref()
        .unwrap_or(&session.created_at);
    let day = match (date.get(8..10), date.get(5..7)) {
        (Some(d), Some(m)) => format!(" ({d}/{m})"),
        _ => String::new(),
    };
    let title = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("(sans titre)");
    format!("{title}{day}")
}

// ---------------------------------------- Adresses (depuis mcp_auth.rs et upgrade.rs)

/// Un code, un vérificateur PKCE, un jeton ou une release ne voyagent que chiffrés :
/// HTTPS, ou HTTP vers la boucle locale **exacte** (`127.0.0.0/8`, `[::1]`, `localhost`).
///
/// L'hôte est lu par un analyseur d'URL, pas par préfixe : `http://localhost.exemple.org`,
/// `http://127.0.0.1.exemple.org` ou `http://localhost@exemple.org` ne passent pas pour la
/// boucle locale. Une URL portant des identifiants est toujours refusée. Une seule règle
/// pour l'OAuth des serveurs MCP et le téléchargement des releases (épopée #208, T28).
pub fn check_endpoint(raw: &str) -> Result<(), String> {
    let refuse = || {
        Err(format!(
            "point d'accès OAuth refusé (HTTPS obligatoire hors boucle locale) : {raw}"
        ))
    };
    let Ok(url) = url::Url::parse(raw) else {
        return refuse();
    };
    if !url.username().is_empty() || url.password().is_some() {
        return refuse();
    }
    match (url.scheme(), url.host()) {
        ("https", Some(_)) => Ok(()),
        ("http", Some(url::Host::Ipv4(ip))) if ip.is_loopback() => Ok(()),
        ("http", Some(url::Host::Ipv6(ip))) if ip.is_loopback() => Ok(()),
        ("http", Some(url::Host::Domain(host))) if host.eq_ignore_ascii_case("localhost") => Ok(()),
        _ => refuse(),
    }
}

// ---------------------------------------- Journal (depuis runtime_events.rs)

/// Les résultats volumineux (listings, pages, images) n'envahissent pas le journal.
/// La rédaction précède le calcul de taille pour ne jamais exposer le brut.
pub fn bounded_redacted(value: &Value) -> Value {
    let cleaned = penelope_observe::redact_json(value);
    let bytes = cleaned.to_string().len();
    if bytes > 64 * 1024 {
        serde_json::json!({"truncated": true, "bytes": bytes})
    } else {
        cleaned
    }
}
