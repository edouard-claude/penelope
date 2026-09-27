//! Retour d'usage de la mémoire (issue #105) : un souvenir servi au modèle compte comme
//! rappelé, et comme utile seulement si la réponse s'en sert.
//!
//! ```text
//!  tour de conversation
//!    rappel automatique, mem_search ── served ──► rappels + 1, uid en attente
//!    réponse ────────────────────────── judge ──► reprend le souvenir ? utiles + 1
//!  message suivant du propriétaire ─── settle ──► succès ou contradiction des souvenirs
//!                                                  utiles d'avant, ou rien (#230)
//!  hors conversation (workflow, sous-agent) : date de rappel seulement, rien à juger
//! ```
//!
//! Les compteurs pèsent sur le classement par [`penelope_memory::index::usage_factor`].
//! Les succès et contradictions décident en plus d'une règle **contestée** : plus
//! servie d'office, soumise au propriétaire (`memory.promotion.contested_*`, #230).

use penelope_app::services::Services;

fn key(session_id: &str) -> String {
    format!("session.served.{session_id}")
}

/// Souvenirs dont la dernière réponse s'est servie, en attente du message suivant du
/// propriétaire qui les jugera (#230). Présente, même vide : un tour répondu attend son
/// verdict.
fn judged_key(session_id: &str) -> String {
    format!("session.judged.{session_id}")
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
/// utile. Renvoie ceux-là ; la liste d'attente est vidée. `owner` : le tour répond à un
/// message du propriétaire, qui juge d'abord la réponse d'avant ([`settle`]).
pub async fn judge(
    s: &Services,
    session_id: &str,
    owner: bool,
    asked: &str,
    answer: &str,
) -> Vec<String> {
    if owner {
        settle(s, session_id, asked).await;
    }
    let waiting = pending(s, session_id).await;
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
    if !session_id.is_empty() {
        let (k, v) = (
            judged_key(session_id),
            serde_json::to_string(&useful).unwrap_or_default(),
        );
        let _ = s
            .store
            .write(move |tx| penelope_store::kv_set(tx, &k, &v))
            .await;
    }
    useful
}

/// Le message du propriétaire qui suit une réponse la juge (#230). Pour le tour : accepté
/// (aucun marqueur de correction) ou repris, noté par session ([`turn_outcomes`]) ; c'est
/// ce que la promotion d'un écart compte. Pour chaque souvenir dont cette réponse s'est servie :
/// succès, contradiction, ou rien dans le doute ([`penelope_memory::recall::outcome_of`]),
/// chacun journalisé `memory.outcome`. Sans réponse d'avant en attente, rien.
pub async fn settle(s: &Services, session_id: &str, reply: &str) {
    let k = judged_key(session_id);
    let raw = {
        let k = k.clone();
        s.store
            .read(move |c| penelope_store::kv_get(c, &k))
            .await
            .ok()
            .flatten()
    };
    let Some(raw) = raw else { return };
    let _ = s
        .store
        .write(move |tx| {
            tx.execute("DELETE FROM kv WHERE k = ?1", [&k])?;
            Ok(())
        })
        .await;
    if reply.trim().is_empty() {
        return;
    }
    let accepted = !penelope_memory::candidates::looks_like_correction(reply);
    note_turn(s, session_id, accepted).await;
    let uids: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    for uid in uids {
        let Ok(Some(e)) = s.memory.get(&uid).await else {
            continue;
        };
        let Some(success) = penelope_memory::recall::outcome_of(&e.text, reply) else {
            tracing::debug!(%uid, "réponse du propriétaire sans verdict net : aucun signal");
            continue;
        };
        if s.memory.record_outcome(&uid, success).await.is_err() {
            continue;
        }
        tracing::info!(%uid, success, "signal d'usage de la mémoire");
        record(
            s,
            session_id,
            "memory.outcome",
            serde_json::json!({"uid": uid, "success": success}),
        )
        .await;
    }
}

fn outcomes_key(session_id: &str) -> String {
    format!("memory.turn_outcomes.{session_id}")
}

/// Verdicts des tours répondus d'une session, dans l'ordre : `(date, accepté)`. Les
/// 50 derniers ; au-delà, un écart est vieux et sa session a déjà parlé.
pub async fn turn_outcomes(s: &Services, session_id: &str) -> Vec<(String, bool)> {
    let k = outcomes_key(session_id);
    s.store
        .read(move |c| penelope_store::kv_get(c, &k))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

async fn note_turn(s: &Services, session_id: &str, accepted: bool) {
    let mut all = turn_outcomes(s, session_id).await;
    all.push((s.clock.now_rfc3339(), accepted));
    if all.len() > 50 {
        all.remove(0);
    }
    tracing::info!(session = %session_id, accepted, "verdict du propriétaire sur la réponse d'avant");
    let (k, v) = (
        outcomes_key(session_id),
        serde_json::to_string(&all).unwrap_or_default(),
    );
    let _ = s
        .store
        .write(move |tx| penelope_store::kv_set(tx, &k, &v))
        .await;
}

async fn record(s: &Services, session_id: &str, kind: &str, payload: serde_json::Value) {
    let draft = penelope_kernel::event::EventDraft::new(kind, payload).session(session_id);
    if let Err(e) = s.events.append(draft).await {
        tracing::warn!(error = %e, kind, "signal d'usage non journalisé");
    }
}

/// Tour échoué ou annulé : les souvenirs servis restent comptés, sans utilité, et la
/// réponse d'avant ne sera pas jugée : le message suivant répondrait à autre chose.
pub async fn forget(s: &Services, session_id: &str) {
    let (k, j) = (key(session_id), judged_key(session_id));
    let _ = s
        .store
        .write(move |tx| {
            tx.execute("DELETE FROM kv WHERE k = ?1 OR k = ?2", [&k, &j])?;
            Ok(())
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_memory::Level;
    use std::sync::Arc;

    async fn services() -> (tempfile::TempDir, Services, String) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        let vault = penelope_app::helpers::vault_dir(&s);
        let uid = crate::vault_ops::remember(
            &s,
            &vault,
            Level::Coeur,
            "Le client Martin est basé à Grenoble",
            "s1",
        )
        .await
        .unwrap();
        (dir, s, uid)
    }

    /// Un tour qui se sert du souvenir, puis le message du propriétaire.
    async fn exchange(s: &Services, uid: &str, reply: &str, owner: bool) {
        let q = "où est Martin ?";
        served(s, "s1", true, &[uid.to_string()], q).await;
        judge(s, "s1", true, q, "Martin est à Grenoble.").await;
        judge(s, "s1", owner, reply, "D'accord.").await;
    }

    /// #230 : la réponse suivante du propriétaire juge le souvenir dont la réponse s'est
    /// servie ; le verdict du tour est gardé pour la promotion des écarts.
    #[tokio::test]
    async fn the_owner_reply_records_a_success_or_a_contradiction() {
        let (_dir, s, uid) = services().await;
        exchange(&s, &uid, "parfait, merci", true).await;
        exchange(&s, &uid, "Non, Martin est à Lyon maintenant.", true).await;
        let sig = s.memory.signals_of(&uid).await.unwrap();
        assert_eq!((sig.successes, sig.contradictions), (1, 1), "{sig:?}");
        let verdicts: Vec<bool> = turn_outcomes(&s, "s1")
            .await
            .into_iter()
            .map(|(_, ok)| ok)
            .collect();
        assert_eq!(verdicts, vec![true, true, false]);
        let logged = s.events.range(0, 1000).await.unwrap();
        assert_eq!(
            logged.iter().filter(|e| e.kind == "memory.outcome").count(),
            2
        );
    }

    /// Dans le doute, rien : une relance qui n'est pas du propriétaire, un tour échoué
    /// entre la réponse et le message, une correction qui parle d'autre chose.
    #[tokio::test]
    async fn no_signal_without_a_clear_owner_verdict() {
        let (_dir, s, uid) = services().await;
        exchange(&s, &uid, "[relance] le job est fini", false).await;
        exchange(&s, &uid, "Non, je parlais du serveur de build.", true).await;
        let q = "où est Martin ?";
        served(&s, "s1", true, std::slice::from_ref(&uid), q).await;
        judge(&s, "s1", true, q, "Martin est à Grenoble.").await;
        forget(&s, "s1").await;
        judge(&s, "s1", true, "parfait", "D'accord.").await;
        let sig = s.memory.signals_of(&uid).await.unwrap();
        assert_eq!((sig.successes, sig.contradictions), (0, 0), "{sig:?}");
    }
}
