//! Retour d'usage de la mémoire (issue #105) : un souvenir servi au modèle compte comme
//! rappelé, et comme utile seulement si la réponse s'en sert.
//!
//! ```text
//!  tour de conversation
//!    rappel automatique, mem_search ── served ──► rappels + 1, uid en attente
//!    réponse ────────────────────────── judge ──► reprend le souvenir ? utiles + 1
//!  hors conversation (workflow, sous-agent) : date de rappel seulement, rien à juger
//! ```
//!
//! Les compteurs pèsent sur le classement par [`penelope_memory::index::usage_factor`] ;
//! ils ne font entrer ni sortir aucun souvenir.

use penelope_app::services::Services;

fn key(session_id: &str) -> String {
    format!("session.served.{session_id}")
}

async fn pending(s: &Services, session_id: &str) -> Vec<String> {
    let k = key(session_id);
    s.store
        .read(move |c| penelope_store::kv_get(c, &k))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// Souvenirs servis au modèle pour `query`. `judged` : la session est une conversation
/// dont la réponse sera jugée ; sinon seules la date et la requête sont notées. Un tour
/// rejoué (reprise après approbation) ne compte pas deux fois le même souvenir.
pub async fn served(s: &Services, session_id: &str, judged: bool, uids: &[String], query: &str) {
    if uids.is_empty() {
        return;
    }
    if !judged || session_id.is_empty() {
        for uid in uids {
            let _ = s.memory.record_served(uid, query, false).await;
        }
        return;
    }
    let mut waiting = pending(s, session_id).await;
    let before = waiting.len();
    for uid in uids {
        if waiting.contains(uid) {
            continue;
        }
        let _ = s.memory.record_served(uid, query, true).await;
        waiting.push(uid.clone());
    }
    if waiting.len() != before {
        let (k, v) = (
            key(session_id),
            serde_json::to_string(&waiting).unwrap_or_default(),
        );
        let _ = s
            .store
            .write(move |tx| penelope_store::kv_set(tx, &k, &v))
            .await;
    }
}

/// Fin d'un tour répondu : chaque souvenir servi que la réponse reprend compte comme
/// utile. Renvoie ceux-là ; la liste d'attente est vidée.
pub async fn judge(s: &Services, session_id: &str, asked: &str, answer: &str) -> Vec<String> {
    let waiting = pending(s, session_id).await;
    if waiting.is_empty() {
        return Vec::new();
    }
    let mut useful = Vec::new();
    for uid in waiting {
        if let Ok(Some(e)) = s.memory.get(&uid).await
            && penelope_memory::recall::used_in_answer(&e.text, asked, answer)
        {
            let _ = s.memory.mark_useful(&uid).await;
            useful.push(uid);
        }
    }
    forget(s, session_id).await;
    useful
}

/// Tour échoué ou annulé : les souvenirs servis restent comptés, sans utilité.
pub async fn forget(s: &Services, session_id: &str) {
    let k = key(session_id);
    let _ = s
        .store
        .write(move |tx| {
            tx.execute("DELETE FROM kv WHERE k = ?1", [&k])?;
            Ok(())
        })
        .await;
}
