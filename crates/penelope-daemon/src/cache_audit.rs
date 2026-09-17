//! Cache de prompt (§5, issue #17) : empreinte de chaque requête de conversation et cause
//! probable d'un raté, comparée à l'appel précédent de la session. `penelope usage --by
//! miss` en fait le bilan ; le fournisseur amont qui a servi reste épinglé tant que la
//! session est active.

use penelope_kernel::canonical::sha256_hex;
use penelope_llm::catalog::strip_provider;
use penelope_llm::types::{ChatMessage, ToolDef};
use serde_json::json;

/// Au-delà, le cache des fournisseurs a expiré.
pub const CACHE_TTL_MS: i64 = 5 * 60_000;
/// Durée pendant laquelle le fournisseur amont d'une session reste épinglé.
pub const STICKY_MS: i64 = 10 * 60_000;
/// En dessous, les fournisseurs ne mettent pas en cache : pas de raté à expliquer.
const MIN_CACHEABLE_TOKENS: u64 = 2_048;

/// Empreinte d'une requête.
#[derive(Debug, Clone, PartialEq)]
pub struct Fingerprint {
    /// Hachage chaîné : `chain[i]` couvre les messages `0..=i`.
    pub chain: Vec<String>,
    pub system_hash: String,
    pub tools_hash: String,
}

impl Fingerprint {
    pub fn of(messages: &[ChatMessage], tools: &[ToolDef]) -> Fingerprint {
        let mut chain = Vec::with_capacity(messages.len());
        let mut previous = String::new();
        for m in messages {
            // Le marqueur de cache se déplace d'un appel à l'autre sans changer le texte.
            let canonical = json!({
                "role": m.role.as_str(),
                "content": m.content,
                "tool_calls": m.tool_calls,
                "tool_call_id": m.tool_call_id,
                "name": m.name,
            })
            .to_string();
            previous = sha256_hex(format!("{previous}\n{canonical}").as_bytes());
            chain.push(previous.clone());
        }
        let system = messages
            .first()
            .filter(|m| m.role.as_str() == "system")
            .map(|m| m.text())
            .unwrap_or_default();
        Fingerprint {
            chain,
            system_hash: sha256_hex(system.as_bytes()),
            tools_hash: sha256_hex(serde_json::to_string(tools).unwrap_or_default().as_bytes()),
        }
    }

    pub fn request_hash(&self) -> Option<String> {
        self.chain.last().cloned()
    }
}

/// Dernier appel de conversation d'une session.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousCall {
    pub ts_ms: i64,
    pub model: String,
    pub upstream: Option<String>,
    pub msg_count: Option<i64>,
    pub request_hash: Option<String>,
    pub system_hash: Option<String>,
    pub tools_hash: Option<String>,
}

pub async fn previous_call(
    s: &crate::runtime::Services,
    session_id: &str,
) -> anyhow::Result<Option<PreviousCall>> {
    use penelope_store::rusqlite::OptionalExtension;
    let sid = session_id.to_string();
    let row = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT ts, model, upstream, msg_count, request_hash, system_hash, tools_hash
                 FROM usage WHERE session_id = ?1 AND COALESCE(role, 'chat') = 'chat'
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        PreviousCall {
                            ts_ms: 0,
                            model: r.get(1)?,
                            upstream: r.get(2)?,
                            msg_count: r.get(3)?,
                            request_hash: r.get(4)?,
                            system_hash: r.get(5)?,
                            tools_hash: r.get(6)?,
                        },
                    ))
                },
            )
            .optional()?)
        })
        .await?;
    Ok(row.map(|(ts, mut p)| {
        p.ts_ms = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        p
    }))
}

/// Fournisseur amont à garder pour l'appel suivant : celui du dernier appel du même
/// modèle, s'il date de moins de dix minutes.
pub fn sticky_upstream(
    previous: Option<&PreviousCall>,
    model_id: &str,
    now_ms: i64,
) -> Option<String> {
    let p = previous?;
    (strip_provider(&p.model) == strip_provider(model_id) && now_ms - p.ts_ms < STICKY_MS)
        .then(|| p.upstream.clone())
        .flatten()
}

/// Entrées de la comparaison, une fois la réponse connue.
pub struct Observed<'a> {
    pub fingerprint: &'a Fingerprint,
    pub model: &'a str,
    pub upstream: Option<&'a str>,
    pub prompt: u64,
    pub cached: u64,
    pub now_ms: i64,
}

/// Cause probable d'un raté de cache (moins de la moitié du prompt servi par le cache),
/// dans l'ordre où elle se vérifie. `None` : le cache a servi.
pub fn miss_cause(previous: Option<&PreviousCall>, o: &Observed<'_>) -> Option<&'static str> {
    if o.prompt < MIN_CACHEABLE_TOKENS || o.cached * 2 >= o.prompt {
        return None;
    }
    let Some(p) = previous else {
        return Some("premier_appel");
    };
    if o.now_ms - p.ts_ms > CACHE_TTL_MS {
        return Some("pause");
    }
    if p.system_hash.as_deref() != Some(o.fingerprint.system_hash.as_str()) {
        return Some("prefixe");
    }
    if p.tools_hash.as_deref() != Some(o.fingerprint.tools_hash.as_str()) {
        return Some("outils");
    }
    if strip_provider(&p.model) != strip_provider(o.model) {
        return Some("modele");
    }
    let kept = match (p.msg_count, &p.request_hash) {
        (Some(n), Some(h)) if n > 0 => o
            .fingerprint
            .chain
            .get(n as usize - 1)
            .is_some_and(|c| c == h),
        _ => false,
    };
    if !kept {
        return Some("historique");
    }
    if p.upstream.is_some() && p.upstream.as_deref() != o.upstream {
        return Some("fournisseur");
    }
    Some("fournisseur_sans_cache")
}

/// Bloc de contexte volatil, tel qu'il précède le texte d'un message utilisateur.
pub fn context_block(volatile: &str) -> String {
    format!("<contexte>\n{}\n</contexte>\n\n", volatile.trim())
}

/// Fige le contexte volatil (T4) avec le dernier message utilisateur de la session, avant
/// son premier envoi : il l'accompagnera dans toutes les requêtes suivantes, reprises
/// après approbation et tours suivants compris, au lieu de se déplacer à chaque tour et
/// de réécrire l'historique.
pub async fn freeze_volatile(
    s: &crate::runtime::Services,
    session_id: &str,
    tiers: &mut penelope_context::Tiers,
) -> anyhow::Result<()> {
    let sid = session_id.to_string();
    let last_user: Option<i64> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT MAX(seq) FROM messages WHERE session_id = ?1 AND role = 'user'",
                [sid],
                |r| r.get(0),
            )?)
        })
        .await?;
    let Some(seq) = last_user else {
        return Ok(());
    };
    if !tiers.volatile.trim().is_empty() {
        s.context
            .history
            .freeze_context(session_id, seq, &context_block(&tiers.volatile))
            .await?;
    }
    tiers.volatile.clear();
    Ok(())
}

pub fn prefix_key(session_id: &str) -> String {
    format!("prompt.prefix.{session_id}")
}

/// Préfixe stable (T0 à T2) : tant que le cache de la session est chaud, un préfixe
/// modifié (nouvel instantané mémoire, skill ou serveur MCP) attend la prochaine pause ou
/// la prochaine compaction, qui le cassent de toute façon.
pub async fn stable_prefix(
    d: &crate::runtime::Daemon,
    session_id: &str,
    tiers: &mut penelope_context::Tiers,
) -> anyhow::Result<()> {
    let s = &d.services;
    let key = prefix_key(session_id);
    let warm = previous_call(s, session_id)
        .await?
        .is_some_and(|p| s.clock.now_ms() - p.ts_ms < CACHE_TTL_MS);
    let built = json!([tiers.identity, tiers.index, tiers.context]);
    if warm
        && let Some(stored) = d
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str::<[String; 3]>(&raw).ok())
    {
        if built != json!(stored) {
            tracing::debug!(
                session = session_id,
                "préfixe modifié : attend un cache froid"
            );
            let [identity, index, context] = stored;
            tiers.identity = identity;
            tiers.index = index;
            tiers.context = context;
        }
        return Ok(());
    }
    d.kv_set(&key, &built.to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Origin;
    use crate::runtime::{Daemon, Services};
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use penelope_llm::types::ToolCall;
    use std::sync::Arc;

    /// CA 5 étendu au transcript (issue #17) : d'un appel au suivant, dans un tour comme
    /// d'un tour à l'autre, la requête envoyée reprend la précédente octet pour octet ; le
    /// contexte volatil reste avec son message et l'empreinte est enregistrée.
    #[tokio::test]
    async fn ca_5_4_each_request_extends_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();

        let turn = |text: &'static str| {
            let d = d.clone();
            let sid = sid.clone();
            async move {
                d.enqueue_message(&sid, text, &Origin::Cli, None)
                    .await
                    .unwrap();
                let t = d.services.turns.claim("test").await.unwrap().unwrap();
                let out = d.run_turn(&t).await;
                d.services.turns.complete(&t).await.unwrap();
                out
            }
        };
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "self_status".into(),
                arguments: json!({"section": "model"}),
            }],
        ));
        p.reply("Voici l'état.");
        turn("où en es-tu ?").await;
        // Deux minutes plus tard : l'heure du contexte volatil a changé, le cache est chaud.
        clock.advance_secs(120);
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("D'accord.");
        turn("merci").await;

        let chats: Vec<serde_json::Value> = p
            .requests()
            .iter()
            .filter(|r| {
                r.messages
                    .first()
                    .is_some_and(|m| m.text().contains("agent personnel autonome"))
            })
            .map(|r| penelope_llm::provider::to_openai_body(r)["messages"].clone())
            .collect();
        assert_eq!(chats.len(), 3, "deux appels au premier tour, un au second");
        for pair in chats.windows(2) {
            let (before, after) = (pair[0].as_array().unwrap(), pair[1].as_array().unwrap());
            assert!(after.len() > before.len());
            assert_eq!(
                &after[..before.len()],
                &before[..],
                "la requête suivante réécrit la précédente"
            );
        }
        let first_user = chats[2].as_array().unwrap()[1]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(first_user.starts_with("<contexte>"), "{first_user}");
        assert!(first_user.ends_with("où en es-tu ?"), "{first_user}");

        let rows = s.budget.report("miss", Some(&sid), None, 10).await.unwrap();
        assert!(!rows.is_empty());
        let fp = previous_call(&s, &sid).await.unwrap().unwrap();
        assert!(
            fp.request_hash.is_some() && fp.msg_count.unwrap() >= 4,
            "{fp:?}"
        );
    }

    fn previous(fp: &Fingerprint, n: usize, upstream: &str, ts_ms: i64) -> PreviousCall {
        PreviousCall {
            ts_ms,
            model: "z-ai/glm-5.3".into(),
            upstream: Some(upstream.into()),
            msg_count: Some(n as i64),
            request_hash: fp.chain.get(n - 1).cloned(),
            system_hash: Some(fp.system_hash.clone()),
            tools_hash: Some(fp.tools_hash.clone()),
        }
    }

    #[test]
    fn a_miss_is_explained_by_what_changed() {
        let tools = vec![ToolDef::new("fs_read", "lire", json!({"type": "object"}))];
        let first = vec![
            ChatMessage::system("Tu es Pénélope."),
            ChatMessage::user("question"),
        ];
        let mut second = first.clone();
        second.push(ChatMessage::assistant("réponse"));
        second.push(ChatMessage::user("suite"));
        let fp1 = Fingerprint::of(&first, &tools);
        let fp2 = Fingerprint::of(&second, &tools);
        let prev = previous(&fp1, 2, "Together", 0);
        let obs = |fp, upstream, cached, now_ms| Observed {
            fingerprint: fp,
            model: "openrouter:z-ai/glm-5.3",
            upstream,
            prompt: 100_000,
            cached,
            now_ms,
        };

        assert_eq!(
            miss_cause(Some(&prev), &obs(&fp2, Some("Together"), 90_000, 1_000)),
            None
        );
        assert_eq!(
            miss_cause(None, &obs(&fp2, None, 0, 0)),
            Some("premier_appel")
        );
        assert_eq!(
            miss_cause(
                Some(&prev),
                &obs(&fp2, Some("Together"), 0, CACHE_TTL_MS + 1)
            ),
            Some("pause")
        );
        assert_eq!(
            miss_cause(Some(&prev), &obs(&fp2, Some("Alibaba"), 0, 1_000)),
            Some("fournisseur")
        );
        assert_eq!(
            miss_cause(Some(&prev), &obs(&fp2, Some("Together"), 0, 1_000)),
            Some("fournisseur_sans_cache")
        );

        let mut rewritten = second.clone();
        rewritten[1] = ChatMessage::user("question reformulée");
        let fp3 = Fingerprint::of(&rewritten, &tools);
        assert_eq!(
            miss_cause(Some(&prev), &obs(&fp3, Some("Together"), 0, 1_000)),
            Some("historique")
        );
        let mut system = second.clone();
        system[0] = ChatMessage::system("Tu es Pénélope, version 2.");
        assert_eq!(
            miss_cause(
                Some(&prev),
                &obs(
                    &Fingerprint::of(&system, &tools),
                    Some("Together"),
                    0,
                    1_000
                )
            ),
            Some("prefixe")
        );

        // Le marqueur de cache ne compte pas.
        let mut marked = second.clone();
        marked[1].cache_marker = true;
        assert_eq!(Fingerprint::of(&marked, &tools), fp2);

        assert_eq!(
            sticky_upstream(Some(&prev), "openrouter:z-ai/glm-5.3", 60_000).as_deref(),
            Some("Together")
        );
        assert!(sticky_upstream(Some(&prev), "openrouter:z-ai/glm-5.3", STICKY_MS + 1).is_none());
        assert!(sticky_upstream(Some(&prev), "openrouter:deepseek/v4", 60_000).is_none());
    }
}
