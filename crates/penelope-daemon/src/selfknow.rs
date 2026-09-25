//! Ce que Pénélope sait d'elle-même : `self_status` et `config_set`.
//!
//! Le propriétaire la sollicite pour la régler : elle doit connaître sa configuration
//! effective, le modèle qui répond, ses coûts et l'état de sa machine, sans rien deviner.
//! Rien de secret n'en sort : les noms de secrets, jamais leurs valeurs.

use crate::runtime::Services;
use serde_json::{Map, Value, json};

pub use penelope_app::ports::Admin;

/// Modèle qui répond au tour en cours.
#[derive(Debug, Clone, Default)]
pub struct TurnModel {
    pub alias: String,
    pub model_id: String,
}

/// Workflows disponibles : identifiant, rôle, paramètres requis et facultatifs, portée.
pub fn workflows_inventory(s: &Services) -> Value {
    let mut all = s.workflows.all();
    all.sort_by(|a, b| a.workflow.metadata.id.cmp(&b.workflow.metadata.id));
    json!(
        all.iter()
            .map(|e| {
                let m = &e.workflow.metadata;
                let params = |required: bool| {
                    m.parameters
                        .iter()
                        .filter(|p| p.required == required)
                        .map(|p| json!({"id": p.id, "label": p.label, "type": p.kind}))
                        .collect::<Vec<_>>()
                };
                json!({
                    "id": m.id,
                    "name": m.name,
                    "role": m.description,
                    "required": params(true),
                    "optional": params(false),
                    "scope": e.scope.as_str(),
                    "steps": e.workflow.steps.len(),
                    "runs_here": e.workflow.runs_on(s.platform.os_name()),
                })
            })
            .collect::<Vec<_>>()
    )
}

/// Parties de l'inventaire de soi (issue #34).
pub const INVENTORY: [&str; 8] = [
    "workflows",
    "skills",
    "tools",
    "mcp",
    "commands",
    "schedules",
    "install",
    "limits",
];

async fn inventory_section(
    s: &Services,
    admin: Option<&dyn Admin>,
    section: &str,
) -> anyhow::Result<Option<Value>> {
    Ok(Some(match section {
        "workflows" => workflows_inventory(s),
        "skills" => json!(
            s.skills
                .all()
                .iter()
                .map(|k| json!({"name": k.name, "description": k.description, "scope": k.scope.as_str()}))
                .collect::<Vec<_>>()
        ),
        "tools" => json!(
            penelope_tools::all_tools()
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "role": t.description,
                    "risk": t.risk.as_str(),
                    "workflow_only": t.workflow_only,
                }))
                .collect::<Vec<_>>()
        ),
        "mcp" => match admin {
            Some(a) => a.mcp_servers().await,
            None => Value::Null,
        },
        "commands" => json!(
            penelope_telegram::commands::all()
                .iter()
                .map(|c| json!({
                    "command": format!("/{}", c.name),
                    "category": c.category,
                    "description": c.description,
                    "example": c.example,
                }))
                .collect::<Vec<_>>()
        ),
        "schedules" => json!({
            "schedules": s.schedules.list().await?.iter().filter(|x| x.state != "deleted").map(|x| json!({
                "id": x.id, "kind": x.kind.as_str(), "spec": x.spec, "state": x.state,
                "target": x.target, "next_run": x.next_run,
            })).collect::<Vec<_>>(),
            "intents": s.intents.all().await?.iter().filter(|i| i.etat == penelope_memory::intents::IntentState::Armee).map(|i| json!({
                "id": i.id, "text": i.texte, "triggers": i.declencheurs, "expires": i.expire_at,
            })).collect::<Vec<_>>(),
        }),
        "install" => {
            let cfg = s.config.config();
            let exe = crate::helpers::running_binary().ok();
            json!({
                "version": crate::VERSION,
                "mode": match &exe {
                    Some(b) if crate::helpers::is_source_build(b) => "sources",
                    Some(_) => "releases",
                    None => "inconnu",
                },
                "binary": exe,
                "service_launches": penelope_platform::service::launchd_plist_path()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .and_then(|raw| penelope_platform::service::launchd_program(&raw)),
                "docs": crate::selfdocs::url("README.md", None),
                // Répondre en vocal (issue #41) : outil `send_voice`, synthèse locale.
                "voice": {
                    "send_voice": true,
                    "tts_model": crate::voice::tts_model(&cfg),
                    "voice": cfg.voice.tts_voice,
                    "max_chars": cfg.voice.max_chars,
                    "reply_in_kind": cfg.voice.reply_in_kind,
                    "ffmpeg": penelope_platform::audio::ffmpeg().is_some(),
                    "local_provider_enabled": cfg.providers.local.enabled,
                },
            })
        }
        "limits" => crate::selfdocs::known_limits(),
        _ => return Ok(None),
    }))
}

/// Rapport d'état. `section` : `all`, `model`, `config`, `costs`, `machine`, une partie de
/// l'inventaire ([`INVENTORY`]), ou `inventory` pour tout l'inventaire.
/// État du fournisseur `codex` : compte, plan, jauges du plan (issue #142). Le coût en
/// dollars n'y veut rien dire — un abonnement ne facture pas l'appel —, c'est le quota
/// qui borne.
async fn codex_view(s: &Services, cfg: &penelope_kernel::config::Config) -> Value {
    let status = crate::codex_auth::status(s).ok().flatten();
    let quota = crate::codex_quota::snapshot(s).await;
    json!({
        "enabled": cfg.providers.codex.enabled,
        "connected": status.as_ref().map(|st| st.connected).unwrap_or(false),
        "plan": status.as_ref().map(|st| st.plan.clone()),
        "account": status.as_ref().map(|st| st.account.clone()),
        "disconnected": status.as_ref().and_then(|st| st.disconnected.clone()),
        "quota": quota
            .as_ref()
            .map(|q| crate::codex_quota::gauge_line(q, s.clock.now_ms())),
        "note": "l'abonnement ne facture pas l'appel : la limite est le quota du plan, \
                 pas `budget.*`",
    })
}

#[allow(clippy::too_many_lines)] // gel 0.17 : assemblage du statut
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

    if section == "inventory" || INVENTORY.contains(&section) {
        for part in INVENTORY {
            if (section == "inventory" || section == part)
                && let Some(v) = inventory_section(s, admin, part).await?
            {
                out.insert(part.into(), v);
            }
        }
        return Ok(Value::Object(out));
    }

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
                "project": crate::session_project::of_session(s, session_id).await.0,
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
                "codex": codex_view(s, &cfg).await,
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
        out.insert("memory_search".into(), a.memory_search().await);
    }

    if all || section == "costs" {
        let today = s.budget.today();
        let rows = |r: Vec<penelope_kernel::budget::UsageRow>| {
            r.into_iter()
                .map(|x| {
                    json!({"key": x.key, "label": x.label, "usd": crate::helpers::round_usd(x.cost_usd),
                           "calls": x.calls, "tokens": x.tokens})
                })
                .collect::<Vec<_>>()
        };
        out.insert(
            "costs".into(),
            json!({
                "today_usd": crate::helpers::round_usd(s.budget.spent_today().await?),
                "daily_limit_usd": cfg.budget.daily_usd,
                "this_session_usd": crate::helpers::round_usd(s.budget.spent_session(session_id).await?),
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
            // Inventaire en résumé : le détail par `section` (issue #34).
            out.insert(
                "inventory".into(),
                json!({
                    "workflows": s.workflows.all().iter().map(|e| e.workflow.metadata.id.clone()).collect::<Vec<_>>(),
                    "skills": s.skills.all().len(),
                    "native_tools": penelope_tools::all_tools().len(),
                    "telegram_commands": penelope_telegram::commands::all().len(),
                    "detail": "self_status section=workflows|skills|tools|mcp|commands|schedules|install|limits, ou inventory ; documentation : self_docs",
                }),
            );
        }
    }

    // #204 : ce qui tourne hors des tours. En section propre, et dans le tableau complet.
    if all || section == "jobs" {
        let now = s.clock.now_ms();
        let live = crate::tool_jobs::store(s).live().await.unwrap_or_default();
        out.insert(
            "jobs".into(),
            json!({
                "running": live.iter().map(|j| j.view(now)).collect::<Vec<_>>(),
                "in_this_process": s.jobs.len(),
                "max_per_session": cfg.tools.jobs_per_session,
                "max_total": cfg.tools.jobs_total,
            }),
        );
    }

    if all || section == "machine" {
        let platform = s.platform.clone();
        let now = s.clock.now_ms() / 1000;
        let host = tokio::task::spawn_blocking(move || platform.host_status(now))
            .await
            .unwrap_or_default();
        let mut machine = serde_json::to_value(host)?;
        // Ce que la machine sait faire (issue #156) : binaires, versions, connexions.
        // Le dernier inventaire connu, sans sonder — `penelope doctor` le rafraîchit.
        if let Some(inv) = crate::machine::cached(s).await
            && let Value::Object(m) = &mut machine
        {
            m.insert("inventory".into(), serde_json::to_value(&inv)?);
            m.insert("prompt_line".into(), json!(inv.prompt_line()));
        }
        out.insert("machine".into(), machine);
    }

    // Sauvegardes : de quoi répondre « ta dernière sauvegarde date de cette nuit » (#42).
    if (all || section == "backup")
        && let Some(a) = admin
        && let Ok(v) = a.backup_status().await
    {
        out.insert("backup".into(), v);
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

    /// #204 : `self_status` montre les jobs vivants, leur âge et leur session — en
    /// section `jobs` comme dans le tableau complet.
    #[tokio::test]
    async fn the_status_shows_the_living_tool_jobs() {
        let (_d, s) = services().await;
        crate::tool_jobs::store(&s)
            .create(crate::tool_jobs::NewJob {
                session_id: "s1".into(),
                run_id: None,
                turn_id: None,
                call_id: None,
                tool: "shell_exec".into(),
                request: json!({"command": "cargo test"}),
                effect_id: Some("e_1".into()),
            })
            .await
            .unwrap();
        for section in ["jobs", "all"] {
            let v = status(&s, "s1", None, None, section).await.unwrap();
            let jobs = v["jobs"]["running"].as_array().unwrap_or_else(|| {
                panic!("section {section} sans jobs : {v}");
            });
            assert_eq!(jobs.len(), 1, "{v}");
            assert_eq!(jobs[0]["tool"], "shell_exec");
            assert_eq!(jobs[0]["session"], "s1");
            assert!(jobs[0]["age_s"].is_number());
        }
    }

    /// Issue #34 : l'inventaire liste `ticket-to-deploy` avec ses paramètres requis et
    /// facultatifs, et le prompt d'un tour indexe les workflows sans bouger d'un octet.
    #[tokio::test]
    async fn the_inventory_and_the_prompt_know_the_workflows() {
        let (_d, s) = services().await;
        let v = status(&s, "s1", None, None, "workflows").await.unwrap();
        let ttd = v["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == "ticket-to-deploy")
            .expect("ticket-to-deploy inventorié")
            .clone();
        let ids = |key: &str| -> Vec<String> {
            ttd[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids("required"), vec!["ticket_url", "ticket_id"]);
        assert_eq!(ids("optional"), vec!["repo", "tracker"]);
        assert!(v.get("model").is_none(), "seule la section demandée");

        let all = status(&s, "s1", None, None, "inventory").await.unwrap();
        for part in INVENTORY {
            assert!(all.get(part).is_some(), "{part}");
        }
        assert!(
            all["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "self_docs")
        );
        assert!(
            all["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["command"] == "/run")
        );
        assert!(!all["limits"].as_array().unwrap().is_empty());

        let first = crate::conversation::build_tiers(&s, "bonjour", &[], None).await;
        assert!(
            first.index.contains("## Workflows disponibles"),
            "{}",
            first.index
        );
        assert!(
            first
                .index
                .contains("`ticket-to-deploy` : Du ticket au déploiement, avec approbations aux points sensibles. (paramètres requis : ticket_url, ticket_id)"),
            "{}",
            first.index
        );
        assert!(first.identity.contains("self_docs"), "règle du harnais");
        let second = crate::conversation::build_tiers(&s, "autre message", &[], None).await;
        assert_eq!(
            first.prefix_hash(),
            second.prefix_hash(),
            "préfixe identique"
        );
    }

    /// #156 : l'inventaire de la machine sort par `self_status`, et sa ligne entre dans
    /// le message système sans casser le préfixe mis en cache (#104, décision 0008).
    #[tokio::test]
    async fn the_machine_inventory_reaches_the_model_and_keeps_the_prefix() {
        let (_d, s) = services().await;
        // Avant la première passe, rien : le prompt ne parle pas de la machine plutôt que
        // d'en inventer une.
        let blank = crate::conversation::build_tiers(&s, "bonjour", &[], None).await;
        assert!(!blank.index.contains("## Machine"), "{}", blank.index);

        let inv = crate::machine::Inventory {
            os: "macOS 27".into(),
            arch: "arm64".into(),
            sandbox_profile: "workspace-write".into(),
            shell_network: false,
            present: vec![crate::machine::Tool {
                name: "gh".into(),
                version: Some("gh version 2.62.0".into()),
                account: Some("edouard-claude".into()),
            }],
            missing: vec!["glab".into()],
            checked_at: "2026-09-21T09:00:00+04:00".into(),
        };
        s.kv_set(
            crate::machine::KV_KEY,
            &serde_json::to_string(&inv).unwrap(),
        )
        .await
        .unwrap();

        let v = status(&s, "s1", None, None, "machine").await.unwrap();
        assert_eq!(v["machine"]["inventory"]["present"][0]["name"], "gh");
        assert_eq!(
            v["machine"]["inventory"]["present"][0]["version"], "gh version 2.62.0",
            "la version sort ici, jamais dans le prompt"
        );
        assert_eq!(v["machine"]["inventory"]["missing"][0], "glab");

        let first = crate::conversation::build_tiers(&s, "bonjour", &[], None).await;
        assert!(first.index.contains("## Machine"), "{}", first.index);
        assert!(
            first.index.contains("gh (edouard-claude)"),
            "{}",
            first.index
        );
        assert!(
            !first.index.contains("2.62.0"),
            "aucune version dans le prompt : {}",
            first.index
        );
        let second = crate::conversation::build_tiers(&s, "autre message", &[], None).await;
        assert_eq!(
            first.prefix_hash(),
            second.prefix_hash(),
            "le préfixe tient d'un tour à l'autre"
        );
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
