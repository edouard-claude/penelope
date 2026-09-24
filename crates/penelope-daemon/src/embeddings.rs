//! Embeddings (§6.11, issue #11) : vecteurs des entrées mémoire (`mem_vec`), des intentions
//! (`intent_vec`) et des outils MCP (`mcp_tools_vec`), calculés par le rôle `embedding` et
//! mis en cache par contenu (`embeddings_cache`). Un rattrapage de fond comble les vecteurs
//! manquants après chaque écriture ; au tour, le vecteur du message a un délai borné et la
//! recherche redevient lexicale s'il manque.

use crate::ports::ProviderSource;
use crate::runtime::Services;
use penelope_kernel::canonical::sha256_hex;
use penelope_store::rusqlite::params;
use penelope_store::{decode_embedding, encode_embedding};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

/// Délai accordé au vecteur du message d'un tour.
pub const TURN_BUDGET: Duration = Duration::from_millis(1_500);
/// Textes par requête d'embeddings.
const BATCH: usize = 64;
/// Après un échec, pas de nouvel essai avant ce délai (un alias cassé ne ralentit pas
/// chaque tour).
const RETRY_AFTER_MS: i64 = 10 * 60_000;
/// Longueur maximale d'un texte envoyé (les modèles tronquent au-delà de 8 k tokens).
const MAX_CHARS: usize = 16_000;

/// État partagé du calcul : dernier échec, rattrapage en cours.
#[derive(Default)]
pub struct State {
    failed_at_ms: AtomicI64,
    last_error: std::sync::Mutex<Option<String>>,
    backfilling: AtomicBool,
}

/// Ce que le calcul reçoit au lieu du daemon : services, providers et état partagé.
#[derive(Clone)]
pub struct Embedder {
    pub services: Arc<Services>,
    pub providers: Arc<dyn ProviderSource>,
    pub state: Arc<State>,
}

/// Modèle du rôle `embedding`, s'il est défini.
pub fn model(s: &Services) -> Option<String> {
    let cfg = s.config.config();
    cfg.alias_model(&cfg.role_alias("embedding"))
        .map(String::from)
        .filter(|m| !m.is_empty())
}

/// Vecteurs de `texts`, dans l'ordre, depuis le cache ou le provider.
pub async fn embed_texts(
    emb: &Embedder,
    texts: &[String],
) -> anyhow::Result<(String, Vec<Vec<f32>>)> {
    let model = model(&emb.services)
        .ok_or_else(|| anyhow::anyhow!("aucun modèle pour le rôle `embedding`"))?;
    let s = &*emb.services;
    let hashes: Vec<String> = texts
        .iter()
        .map(|t| sha256_hex(clip(t).as_bytes()))
        .collect();
    let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
    {
        let (hs, m) = (hashes.clone(), model.clone());
        let cached: Vec<(String, Vec<u8>)> = s
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT content_hash, embedding FROM embeddings_cache
                     WHERE model = ?1 AND content_hash = ?2",
                )?;
                let mut found = Vec::new();
                for h in &hs {
                    let mut rows = st.query(params![m, h])?;
                    if let Some(r) = rows.next()? {
                        found.push((r.get(0)?, r.get(1)?));
                    }
                }
                Ok(found)
            })
            .await?;
        for (h, blob) in cached {
            for (i, hi) in hashes.iter().enumerate() {
                if *hi == h {
                    out[i] = Some(decode_embedding(&blob));
                }
            }
        }
    }
    let missing: Vec<usize> = (0..texts.len()).filter(|i| out[*i].is_none()).collect();
    if !missing.is_empty() {
        let model = crate::codex_scope::background(&emb.services, &model, "embeddings").await;
        let provider = emb
            .providers
            .provider_for(&model)
            .await
            .map_err(anyhow::Error::msg)?;
        for chunk in missing.chunks(BATCH) {
            let inputs: Vec<String> = chunk.iter().map(|i| clip(&texts[*i]).to_string()).collect();
            let vectors = provider.embed(&model, &inputs).await.inspect_err(|e| {
                emb.state
                    .failed_at_ms
                    .store(s.clock.now_ms(), Ordering::SeqCst);
                if let Ok(mut g) = emb.state.last_error.lock() {
                    *g = Some(e.to_string());
                }
            })?;
            let now = s.clock.now_rfc3339();
            let rows: Vec<(String, Vec<f32>)> = chunk
                .iter()
                .zip(vectors)
                .map(|(i, v)| (hashes[*i].clone(), v))
                .collect();
            for ((h, v), i) in rows.iter().zip(chunk) {
                debug_assert_eq!(*h, hashes[*i]);
                out[*i] = Some(v.clone());
            }
            let m = model.clone();
            s.store
                .write(move |tx| {
                    for (h, v) in &rows {
                        tx.execute(
                            "INSERT OR REPLACE INTO embeddings_cache(content_hash, model, dim,
                                embedding, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
                            params![h, m, v.len() as i64, encode_embedding(v), now],
                        )?;
                    }
                    Ok(())
                })
                .await?;
        }
        emb.state.failed_at_ms.store(0, Ordering::SeqCst);
        if let Ok(mut g) = emb.state.last_error.lock() {
            *g = None;
        }
    }
    Ok((
        model,
        out.into_iter().map(Option::unwrap_or_default).collect(),
    ))
}

fn clip(t: &str) -> &str {
    match t.char_indices().nth(MAX_CHARS) {
        Some((i, _)) => &t[..i],
        None => t,
    }
}

/// Vecteur d'un message au tour, en moins de [`TURN_BUDGET`] ; `None` : recherche lexicale.
pub async fn query_vector(emb: &Embedder, text: &str) -> Option<Vec<f32>> {
    if text.trim().is_empty() || model(&emb.services).is_none() {
        return None;
    }
    let failed = emb.state.failed_at_ms.load(Ordering::SeqCst);
    if failed > 0 && emb.services.clock.now_ms() - failed < RETRY_AFTER_MS {
        return None;
    }
    match tokio::time::timeout(TURN_BUDGET, embed_texts(emb, &[text.to_string()])).await {
        Ok(Ok((_, mut v))) => v.pop().filter(|v| !v.is_empty()),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "embedding du message indisponible : recherche lexicale");
            None
        }
        Err(_) => {
            tracing::debug!("embedding du message trop lent : recherche lexicale");
            None
        }
    }
}

/// Bilan d'un rattrapage.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Backfill {
    pub model: String,
    pub memory: usize,
    pub intents: usize,
    pub tools: usize,
    pub error: Option<String>,
}

/// Calcule les vecteurs manquants (ou tous, avec `force`).
pub async fn backfill(emb: &Embedder, force: bool) -> anyhow::Result<Backfill> {
    let Some(model) = model(&emb.services) else {
        anyhow::bail!("aucun modèle pour le rôle `embedding`");
    };
    let s = &*emb.services;
    let mut report = Backfill {
        model: model.clone(),
        ..Default::default()
    };

    // Entrées mémoire actives sans vecteur de ce modèle.
    let m = model.clone();
    let entries: Vec<(String, String)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT e.uid, e.text FROM mem_entries e
                 LEFT JOIN mem_vec v ON v.uid = e.uid
                 WHERE e.statut != 'retiree' AND (?1 OR v.uid IS NULL OR v.model != ?2)
                 LIMIT 10000",
            )?;
            let rows = st.query_map(params![force, m], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    for chunk in entries.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let (_, vectors) = embed_texts(emb, &texts).await?;
        for ((uid, _), v) in chunk.iter().zip(vectors) {
            s.memory.put_embedding(uid, &model, &v).await?;
            report.memory += 1;
        }
    }

    // Intentions armées sans vecteur.
    let intents: Vec<(String, String)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT i.id, i.texte || ' ' || i.declencheurs FROM intents i
                 LEFT JOIN intent_vec v ON v.id = i.id
                 WHERE i.etat = 'armee' AND (?1 OR v.id IS NULL)",
            )?;
            let rows = st.query_map(params![force], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    for chunk in intents.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let (_, vectors) = embed_texts(emb, &texts).await?;
        for ((id, _), v) in chunk.iter().zip(vectors) {
            s.intents.put_embedding(id, &v).await?;
            report.intents += 1;
        }
    }

    // Outils MCP sans vecteur.
    let tools: Vec<(String, String)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT t.qualified, t.name || ' ' || COALESCE(t.title, '') || ' ' || t.description
                 FROM mcp_tools t LEFT JOIN mcp_tools_vec v ON v.qualified = t.qualified
                 WHERE ?1 OR v.qualified IS NULL",
            )?;
            let rows = st.query_map(params![force], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    for chunk in tools.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let (_, vectors) = embed_texts(emb, &texts).await?;
        let rows: Vec<(String, Vec<f32>)> =
            chunk.iter().map(|(q, _)| q.clone()).zip(vectors).collect();
        report.tools += rows.len();
        s.store
            .write(move |tx| {
                for (q, v) in &rows {
                    tx.execute(
                        "INSERT OR REPLACE INTO mcp_tools_vec(qualified, dim, embedding)
                         VALUES(?1, ?2, ?3)",
                        params![q, v.len() as i64, encode_embedding(v)],
                    )?;
                }
                Ok(())
            })
            .await?;
    }
    if report.memory + report.intents + report.tools > 0 {
        tracing::info!(?report, "embeddings calculés");
    }
    Ok(report)
}

/// Rattrapage en tâche de fond, un seul à la fois ; sans effet si le dernier calcul a
/// échoué il y a moins de dix minutes.
pub fn spawn_backfill(emb: Embedder) {
    if model(&emb.services).is_none() {
        return;
    }
    let failed = emb.state.failed_at_ms.load(Ordering::SeqCst);
    if failed > 0 && emb.services.clock.now_ms() - failed < RETRY_AFTER_MS {
        return;
    }
    if emb.state.backfilling.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        emb.state.backfilling.store(false, Ordering::SeqCst);
        return;
    };
    rt.spawn(async move {
        if let Err(e) = backfill(&emb, false).await {
            tracing::warn!(error = %e, "embeddings non calculés : recherche lexicale seule");
        }
        emb.state.backfilling.store(false, Ordering::SeqCst);
    });
}

/// Mode de recherche, pour `self_status` et `doctor`.
pub async fn search_mode(emb: &Embedder) -> anyhow::Result<Value> {
    let s = &*emb.services;
    let counts: (i64, i64, i64, i64, i64, i64) = s
        .store
        .read(|c| {
            let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
            Ok((
                n("SELECT COUNT(*) FROM mem_entries WHERE statut != 'retiree'")?,
                n(
                    "SELECT COUNT(*) FROM mem_vec v JOIN mem_entries e ON e.uid = v.uid
                   WHERE e.statut != 'retiree'",
                )?,
                n("SELECT COUNT(*) FROM intents WHERE etat = 'armee'")?,
                n("SELECT COUNT(*) FROM intent_vec")?,
                n("SELECT COUNT(*) FROM mcp_tools")?,
                n("SELECT COUNT(*) FROM mcp_tools_vec")?,
            ))
        })
        .await?;
    let failed = emb.state.failed_at_ms.load(Ordering::SeqCst);
    let error = emb.state.last_error.lock().ok().and_then(|g| g.clone());
    let model = model(&emb.services);
    let hybrid = model.is_some() && counts.1 > 0 && failed == 0;
    Ok(json!({
        "mode": if hybrid { "hybride (mots-clés et vecteurs)" } else { "lexicale seule" },
        "model": model,
        "last_error": error,
        "vectors": {
            "memory": format!("{}/{}", counts.1, counts.0),
            "intents": format!("{}/{}", counts.3, counts.2),
            "mcp_tools": format!("{}/{}", counts.5, counts.4),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::session::SessionKind;
    use penelope_llm::mock::MockProvider;
    use penelope_memory::Level;

    /// Faux modèle : un axe par famille de synonymes.
    fn embedder() -> penelope_llm::mock::Embedder {
        Arc::new(|text: &str| {
            let t = text.to_lowercase();
            let has = |words: &[&str]| words.iter().any(|w| t.contains(w)) as i32 as f32;
            vec![
                has(&["voiture", "automobile", "véhicule"]),
                has(&["facture", "devis"]),
                0.1,
            ]
        })
    }

    /// Issue #11 : une entrée écrite reçoit son vecteur, et une recherche formulée avec un
    /// synonyme la retrouve ; sans embeddings, la recherche reste lexicale et le dit.
    #[tokio::test]
    async fn a_memory_entry_is_found_by_a_synonym() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let p = Arc::new(MockProvider::new());
        let emb = Embedder {
            services: s.clone(),
            providers: crate::testing::MockProviders::new(p.clone()),
            state: Arc::default(),
        };
        let sid = s
            .sessions
            .create(SessionKind::Chat, Some("CLI".into()))
            .await
            .unwrap()
            .id
            .to_string();
        let vault = crate::helpers::vault_dir(&s);
        crate::vault_ops::remember(
            &s,
            &vault,
            Level::Coeur,
            "Le propriétaire roule en automobile électrique",
            &sid,
        )
        .await
        .unwrap();

        // Sans modèle d'embeddings joignable : lexical seul, dit comme tel.
        assert!(backfill(&emb, false).await.is_err());
        assert_eq!(search_mode(&emb).await.unwrap()["mode"], "lexicale seule");

        p.set_embedder(Some(embedder()));
        emb.state.failed_at_ms.store(0, Ordering::SeqCst);
        let report = backfill(&emb, false).await.unwrap();
        assert!(report.memory >= 1, "{report:?}");
        let vectors: i64 = s
            .store
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM mem_vec", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert!(vectors >= 1);
        assert_eq!(
            backfill(&emb, false).await.unwrap().memory,
            0,
            "rien à recalculer"
        );

        let filter = penelope_memory::SearchFilter {
            limit: 5,
            ..Default::default()
        };
        let lexical = s
            .memory
            .search("voiture", None, &filter, &[])
            .await
            .unwrap();
        assert!(
            lexical.is_empty(),
            "les mots seuls ne trouvent pas le synonyme"
        );
        let vector = query_vector(&emb, "Quelle voiture ai-je ?").await;
        assert!(vector.is_some());
        let hits = s
            .memory
            .search("Quelle voiture ai-je ?", vector, &filter, &[])
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.entry.text.contains("automobile")),
            "{hits:?}"
        );
        assert_eq!(
            search_mode(&emb).await.unwrap()["mode"],
            "hybride (mots-clés et vecteurs)"
        );
    }
}
