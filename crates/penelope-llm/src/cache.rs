//! Empreinte d'une requête et cause probable d'un raté de cache (§5, issue #17) : des
//! fonctions pures sur la requête et le dernier appel de la session, que la boucle
//! calcule sans le daemon (épopée #208, T09, T27).

use crate::catalog::strip_provider;
use crate::types::{ChatMessage, ToolDef};
pub use penelope_kernel::budget::PreviousCall;
use penelope_kernel::canonical::sha256_hex;
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
            tools_hash: Fingerprint::tools_hash_of(tools),
        }
    }

    pub fn request_hash(&self) -> Option<String> {
        self.chain.last().cloned()
    }

    /// Empreinte du seul message système, telle que l'attend `prompt_snapshots`.
    pub fn system_hash_of(rendered: &str) -> String {
        sha256_hex(rendered.as_bytes())
    }

    /// Empreinte de la liste d'outils, calculée comme dans [`Fingerprint::of`].
    pub fn tools_hash_of(tools: &[ToolDef]) -> String {
        sha256_hex(serde_json::to_string(tools).unwrap_or_default().as_bytes())
    }

    /// Les trois clés qui rendent la requête retrouvable (issue #205) : c'est ce que
    /// `llm_requests` garde à la place du corps, jamais écrit depuis l'origine.
    pub fn keys(&self) -> crate::RequestKeys {
        crate::RequestKeys {
            system_hash: Some(self.system_hash.clone()),
            tools_hash: Some(self.tools_hash.clone()),
            request_hash: self.request_hash(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
