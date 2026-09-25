//! Clés kv, paramètres et petits utilitaires de la passerelle (chats vus, modèles, journaux).

use super::*;

pub(super) fn approval_reason_key(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(t) => format!("tg.await_reason.{chat_id}.{t}"),
        None => format!("tg.await_reason.{chat_id}"),
    }
}

/// `clé=valeur clé2=valeur2` : chaque valeur est lue en JSON si possible (nombres,
/// booléens), sinon gardée en texte.
pub fn parse_params(raw: &str) -> Value {
    let mut out = serde_json::Map::new();
    for pair in raw.split_whitespace() {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let value = serde_json::from_str::<Value>(v)
            .ok()
            .filter(|x| !x.is_string())
            .unwrap_or_else(|| json!(v));
        out.insert(k.to_string(), value);
    }
    Value::Object(out)
}

/// `openrouter:` est implicite : `/model main z-ai/glm-5.3` suffit.
pub(super) fn normalise_model_id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_string()
    } else {
        format!("openrouter:{raw}")
    }
}

/// Remplace les `{{variables}}` d'un gabarit, en un seul passage (issue #129).
pub(super) fn substitute(body: &str, vars: &BTreeMap<String, String>) -> String {
    penelope_telegram::templates::substitute(body, vars)
}

/// `openrouter:z-ai/glm-5.3` devient `glm-5.3` : assez pour reconnaître un modèle.
pub(super) fn short_model(id: &str) -> String {
    penelope_llm::catalog::strip_provider(id)
        .rsplit('/')
        .next()
        .unwrap_or(id)
        .to_string()
}

/// Réponse à `/model <alias>` ou `/model auto`.
pub(super) fn model_pin_notice(view: &Value) -> String {
    match view["pinned"].as_str() {
        Some(alias) => format!(
            "📌 Session épinglée sur `{alias}` · `{}`. Retour à l'automatique : `/model auto`.",
            short_model(view["pinned_model"].as_str().unwrap_or("?"))
        ),
        None => "🔀 Session en automatique.".into(),
    }
}

/// Dernières lignes du journal JSON du jour (le plus récent à défaut), filtrées par
/// composant (cible `tracing` ou texte), rendues `HH:MM:SS NIVEAU cible : message`.
pub(super) fn recent_log_lines(dir: &Path, component: &str, n: usize) -> Vec<String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy())
                .is_some_and(|f| f.starts_with("penelope-") && f.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let Some(latest) = files.last() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(latest) else {
        return Vec::new();
    };
    let wanted = component.to_lowercase();
    let mut out: Vec<String> = raw
        .lines()
        .rev()
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line).ok()?;
            let target = v["target"].as_str().unwrap_or_default();
            let message = v["fields"]["message"].as_str().unwrap_or_default();
            if !wanted.is_empty()
                && !target.to_lowercase().contains(&wanted)
                && !message.to_lowercase().contains(&wanted)
            {
                return None;
            }
            let time = v["timestamp"]
                .as_str()
                .and_then(|t| t.get(11..19))
                .unwrap_or("");
            let short_target = target.rsplit("::").next().unwrap_or(target);
            Some(format!(
                "{time} {} {short_target} : {}",
                v["level"].as_str().unwrap_or("?"),
                message.chars().take(200).collect::<String>()
            ))
        })
        .take(n)
        .collect();
    out.reverse();
    out
}

/// Titre du groupe d'un message (un chat privé n'en a pas).
pub(super) fn chat_title_of(update: &Value) -> Option<(i64, String)> {
    let chat = &update.get("message")?["chat"];
    let title = chat["title"].as_str().filter(|t| !t.trim().is_empty())?;
    Some((chat["id"].as_i64()?, title.to_string()))
}

/// Nom du sujet d'un message de forum : à sa création, à son renommage, ou dans le
/// message de création auquel chaque message du sujet répond.
pub(super) fn topic_name_of(update: &Value) -> Option<(i64, i64, String)> {
    let msg = update.get("message")?;
    let chat = msg["chat"]["id"].as_i64()?;
    let topic = msg["message_thread_id"].as_i64()?;
    let name = msg["forum_topic_created"]["name"]
        .as_str()
        .or(msg["forum_topic_edited"]["name"].as_str())
        .or(msg["reply_to_message"]["forum_topic_created"]["name"].as_str())?;
    Some((chat, topic, name.to_string()))
}

const SEEN_CHATS_MAX: usize = 20;

/// Une conversation refusée : son identifiant, son type, son titre et la dernière fois.
pub async fn record_seen_chat(
    s: &penelope_app::services::Services,
    chat_id: i64,
    kind: &str,
    title: &str,
) {
    let mut seen = seen_chats(s).await;
    seen.retain(|c| c["id"].as_i64() != Some(chat_id));
    seen.insert(
        0,
        json!({"id": chat_id, "type": kind, "title": title, "last_seen": s.clock.now_rfc3339()}),
    );
    seen.truncate(SEEN_CHATS_MAX);
    let v = serde_json::to_string(&seen).unwrap_or_default();
    let _ = s
        .store
        .write(move |tx| penelope_store::kv_set(tx, SEEN_CHATS_KEY, &v))
        .await;
}
