//! Ce que Pénélope sait d'elle-même : `self_status` et `config_set`.
//!
//! Le propriétaire la sollicite pour la régler : elle doit connaître sa configuration
//! effective, le modèle qui répond, ses coûts et l'état de sa machine, sans rien deviner.
//! Rien de secret n'en sort : les noms de secrets, jamais leurs valeurs.

use crate::runtime::Services;
use serde_json::{Map, Value, json};

/// Accès du daemon dont les outils ont besoin pour parler de lui.
#[async_trait::async_trait]
pub trait Admin: Send + Sync {
    fn uptime_s(&self) -> u64;
    /// Écrit un réglage (chemin pointé) et le publie à chaud. Renvoie la génération.
    async fn set_config(&self, path: &str, value: Value) -> Result<u64, String>;
    /// Serveurs MCP : état, outils, dernière erreur.
    async fn mcp_servers(&self) -> Value {
        Value::Null
    }
}

/// Modèle qui répond au tour en cours.
#[derive(Debug, Clone, Default)]
pub struct TurnModel {
    pub alias: String,
    pub model_id: String,
}

/// Rapport d'état. `section` : `all`, `model`, `config`, `costs` ou `machine`.
pub async fn status(
    s: &Services,
    session_id: &str,
    turn: Option<&TurnModel>,
    admin: Option<&dyn Admin>,
    section: &str,
) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let all = section == "all" || section.is_empty();
    let mut out = Map::new();

    if all {
        let dirs = &s.platform.dirs;
        out.insert(
            "penelope".into(),
            json!({
                "version": crate::VERSION,
                "uptime_s": admin.map(|a| a.uptime_s()),
                "pid": std::process::id(),
                "binary": std::env::current_exe().ok(),
                "os_backend": s.platform.os_name(),
                "config_file": s.config.path(),
                "config_generation": s.config.generation(),
                "data_dir": dirs.data(),
                "logs_dir": dirs.logs(),
                "vault": dirs.vault(),
                "secrets_backend": s.platform.secrets.backend(),
                "rss_mb": (crate::runtime::rss_mb() * 10.0).round() / 10.0,
            }),
        );
    }

    if all || section == "model" {
        let routing = &cfg.models.routing;
        let model_view = |id: &str| {
            let info = s.catalog.get(penelope_llm::catalog::strip_provider(id));
            json!({
                "id": id,
                "known_in_catalog": if s.catalog.is_empty() { Value::Null } else { json!(info.is_some()) },
                "context_window": info.as_ref().map(|i| i.context_window),
                "usd_per_m_in": info.as_ref().map(|i| per_m(i.price_prompt)),
                "usd_per_m_out": info.as_ref().map(|i| per_m(i.price_completion)),
                "tools": info.as_ref().map(|i| i.supports_tools()),
                "reasoning": info.as_ref().map(|i| i.supports_reasoning()),
                "images_in": info.as_ref().map(|i| i.accepts_images()),
            })
        };
        let session = s.sessions.get(session_id).await.ok().flatten();
        out.insert(
            "this_turn".into(),
            json!({
                "session_id": session_id,
                "session_title": session.as_ref().and_then(|x| x.title.clone()),
                "alias": turn.map(|t| t.alias.clone()),
                "model": turn.map(|t| model_view(&t.model_id)),
                "sticky_alias": session.as_ref().and_then(|x| x.model_alias.clone()),
            }),
        );
        let alias_of = |a: &str| json!({"alias": a, "model": cfg.alias_model(a)});
        out.insert(
            "models".into(),
            json!({
                "aliases": cfg.models.aliases,
                "roles": cfg.models.roles,
                "routing": {
                    "mode": if routing.classifier { "adaptatif (classifieur)" } else { "fixe (tout sur chat_default)" },
                    "classifier": routing.classifier,
                    "simple": alias_of(&routing.low),
                    "ordinaire": alias_of(&routing.medium),
                    "difficile": alias_of(&routing.high),
                    "sticky": routing.sticky,
                    "fallback_on_failure": routing.fallback,
                    "note": "l'alias `low` ne colle jamais à une session ; `/model auto on|off` bascule le mode",
                },
                "catalog_size": s.catalog.len(),
            }),
        );
    }

    if all || section == "config" {
        let secrets = s.platform.secrets.list().unwrap_or_default();
        let has = |name: &str| secrets.iter().any(|n| n == name);
        let summary = json!({
            "owner": {
                "telegram_user_id_set": cfg.owner.telegram_user_id != 0,
                "timezone": cfg.owner.timezone,
                "language": cfg.owner.language,
            },
            "providers": {
                "openrouter": {
                    "enabled": cfg.providers.openrouter.enabled,
                    "api_key_stored": has("openrouter_api_key"),
                    "routing_preferences": cfg.providers.openrouter.routing,
                },
                "local_openai_compat": {
                    "enabled": cfg.providers.local.enabled,
                    "base_url": cfg.providers.local.base_url,
                },
                "speech_to_text": {
                    "alias": cfg.role_alias("stt"),
                    "model": cfg.alias_model(&cfg.role_alias("stt")),
                },
            },
            "telegram": {
                "bot_token_stored": has("telegram_bot_token"),
                "mode": cfg.telegram.mode,
                "rich_messages": cfg.telegram.rich_messages,
            },
            "sandbox": {
                "shell_profile": cfg.sandbox.default_profile,
                "shell_network": cfg.sandbox.shell_network,
                "workspaces": crate::executor::default_workspaces(s),
            },
            "budget": cfg.budget,
            "secrets_stored": secrets,
        });
        if section == "config" {
            out.insert("config_summary".into(), summary);
            out.insert("config_full".into(), redacted_config(&cfg));
        } else {
            out.insert("config".into(), summary);
        }
    }

    if (all || section == "config")
        && let Some(a) = admin
    {
        out.insert("mcp_servers".into(), a.mcp_servers().await);
    }

    if all || section == "costs" {
        let today = s.clock.now_rfc3339().chars().take(10).collect::<String>();
        let rows = |r: Vec<penelope_kernel::budget::UsageRow>| {
            r.into_iter()
                .map(|x| {
                    json!({"key": x.key, "label": x.label, "usd": crate::rpc::round_usd(x.cost_usd),
                           "calls": x.calls, "tokens": x.tokens})
                })
                .collect::<Vec<_>>()
        };
        out.insert(
            "costs".into(),
            json!({
                "today_usd": crate::rpc::round_usd(s.budget.spent_today().await?),
                "daily_limit_usd": cfg.budget.daily_usd,
                "this_session_usd": crate::rpc::round_usd(s.budget.spent_session(session_id).await?),
                "session_limit_usd": cfg.budget.session_usd,
                "today_by_model": rows(s.budget.report("model", None, Some(&today), 5).await?),
                "this_session_by_request": rows(s.budget.report("turn", Some(session_id), None, 5).await?),
                "context": crate::compaction::context_view(
                    s,
                    session_id,
                    turn.map(|t| t.model_id.as_str()),
                )
                .await?,
            }),
        );
        if all {
            out.insert(
                "activity".into(),
                json!({
                    "turns_queued": s.turns.pending_count().await?,
                    "approvals_pending": s.approvals.count_pending().await?,
                    "skills": s.skills.all().len(),
                }),
            );
        }
    }

    if all || section == "machine" {
        let platform = s.platform.clone();
        let now = s.clock.now_ms() / 1000;
        let host = tokio::task::spawn_blocking(move || platform.host_status(now))
            .await
            .unwrap_or_default();
        out.insert("machine".into(), serde_json::to_value(host)?);
    }

    Ok(Value::Object(out))
}

fn per_m(x: f64) -> f64 {
    (x * 1_000_000.0 * 100.0).round() / 100.0
}

/// Configuration complète, sans valeur sensible : un champ dont le nom évoque un secret
/// n'est montré que s'il contient une référence `${SECRET:…}` (le nom d'un secret n'en
/// est pas un) ; toute autre chaîne passe par la détection de motifs de secrets.
fn redacted_config(cfg: &penelope_kernel::config::Config) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(o) => {
                for (k, val) in o.iter_mut() {
                    let key = k.to_lowercase();
                    let sensitive = ["key", "token", "secret", "password", "passphrase"]
                        .iter()
                        .any(|w| key.contains(w));
                    match val {
                        Value::String(s) if sensitive => {
                            if !s.is_empty() && !s.starts_with("${SECRET:") {
                                *s = penelope_observe::redact::MASK.into();
                            }
                        }
                        other => walk(other),
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            Value::String(s) => *s = penelope_observe::redact(s),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(cfg).unwrap_or(Value::Null);
    walk(&mut v);
    v
}

/// Chemins que `config_set` refuse : secrets et identité du propriétaire.
pub fn forbidden_path(path: &str) -> Option<String> {
    let p = path.to_lowercase();
    if ["api_key", "token", "secret", "password", "passphrase"]
        .iter()
        .any(|w| p.contains(w))
    {
        return Some(
            "un secret ne se règle jamais depuis une conversation : en SSH, \
             `penelope secret set <nom>`"
                .into(),
        );
    }
    if p.starts_with("owner.telegram_user_id") {
        return Some(
            "l'identité du propriétaire ne se change qu'en ligne de commande : \
             `penelope config set owner.telegram_user_id <id>`"
                .into(),
        );
    }
    None
}

/// Chemins dont la modification demande une double confirmation.
pub fn sensitive_path(path: &str) -> bool {
    [
        "sandbox.",
        "providers.",
        "telegram.",
        "tools.",
        "mcp.",
        "policies",
        "runners.",
    ]
    .iter()
    .any(|pre| path.starts_with(pre))
}

/// `"true"` devient un booléen, `"12"` un nombre, `"[…]"`/`"{…}"` du JSON, le reste une
/// chaîne, comme `penelope config set`.
pub fn parse_scalar(raw: &str) -> Value {
    let t = raw.trim();
    if let Ok(b) = t.parse::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = t.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = t.parse::<f64>() {
        return json!(f);
    }
    if ((t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')))
        && let Ok(v) = serde_json::from_str::<Value>(t)
    {
        return v;
    }
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    struct FakeAdmin;

    #[async_trait::async_trait]
    impl Admin for FakeAdmin {
        fn uptime_s(&self) -> u64 {
            42
        }
        async fn set_config(&self, _: &str, _: Value) -> Result<u64, String> {
            Ok(7)
        }
    }

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
    }

    #[tokio::test]
    async fn the_report_names_the_model_the_routing_and_the_machine() {
        let (_d, s) = services().await;
        s.platform
            .secrets
            .set("openrouter_api_key", "sk-or-v1-abcdef0123456789abcdef")
            .unwrap();
        let turn = TurnModel {
            alias: "main".into(),
            model_id: "openrouter:z-ai/glm-5.3".into(),
        };
        let v = status(&s, "s1", Some(&turn), Some(&FakeAdmin), "all")
            .await
            .unwrap();
        assert_eq!(v["this_turn"]["alias"], "main");
        assert_eq!(v["this_turn"]["model"]["id"], "openrouter:z-ai/glm-5.3");
        assert_eq!(v["penelope"]["uptime_s"], 42);
        assert_eq!(v["penelope"]["version"], crate::VERSION);
        assert!(v["models"]["routing"]["simple"]["model"].is_string());
        assert_eq!(
            v["config"]["providers"]["openrouter"]["api_key_stored"],
            true
        );
        assert!(v["machine"]["arch"].is_string());
        assert!(v["costs"]["today_usd"].is_number());
        let text = v.to_string();
        assert!(!text.contains("sk-or-v1-abcdef"), "aucune valeur de secret");
    }

    #[tokio::test]
    async fn full_config_masks_inline_secrets() {
        let (_d, s) = services().await;
        s.config
            .mutate("test", |c| {
                c.providers.openrouter.api_key = "sk-or-v1-inline0123456789abcdef".into();
                Ok(vec!["providers.openrouter.api_key".into()])
            })
            .unwrap();
        let v = status(&s, "s1", None, None, "config").await.unwrap();
        assert_eq!(
            v["config_full"]["providers"]["openrouter"]["api_key"],
            penelope_observe::redact::MASK
        );
        assert_eq!(
            v["config_full"]["telegram"]["token"], "${SECRET:telegram_bot_token}",
            "une référence n'est pas un secret"
        );
        assert!(
            v.get("machine").is_none(),
            "une section ne rapporte qu'elle-même"
        );
    }

    #[test]
    fn config_set_guards() {
        assert!(forbidden_path("providers.openrouter.api_key").is_some());
        assert!(forbidden_path("telegram.token").is_some());
        assert!(forbidden_path("owner.telegram_user_id").is_some());
        assert!(forbidden_path("models.aliases.main").is_none());
        assert!(sensitive_path("sandbox.default_profile"));
        assert!(!sensitive_path("models.routing.classifier"));
        assert_eq!(parse_scalar("false"), json!(false));
        assert_eq!(parse_scalar("30"), json!(30));
        assert_eq!(parse_scalar("[\"fast\"]"), json!(["fast"]));
        assert_eq!(
            parse_scalar("openrouter:z-ai/glm-5.3"),
            json!("openrouter:z-ai/glm-5.3")
        );
    }
}
