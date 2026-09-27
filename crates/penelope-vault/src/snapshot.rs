//! Instantanés mémoire T2 (§6.6) : profil, cœur et projets injectés d'office, figés par
//! épisode, et la mesure du budget du niveau Cœur. Venus de `conversation.rs` et de
//! `dream` du daemon (épopée #208, T22).

use penelope_app::services::Services;
use penelope_memory::Level;

/// uid servis d'office dans l'instantané T2 : ni rappelés une seconde fois, ni comptés
/// « jamais rappelés » (issue #62).
pub async fn snapshot_uids(s: &Services, scope: &crate::session_project::Scope) -> Vec<String> {
    let cfg = s.config.config();
    let hidden = s.memory.hidden_uids().await.unwrap_or_default();
    let mut out = Vec::new();
    for (level, budget) in [
        (
            penelope_memory::Level::Profil,
            cfg.memory.profile_budget_tokens,
        ),
        (penelope_memory::Level::Coeur, cfg.memory.core_budget_tokens),
        (
            penelope_memory::Level::Projet,
            cfg.memory.project_budget_tokens,
        ),
    ] {
        let mut entries = s.memory.by_level(level).await.unwrap_or_default();
        entries.retain(|e| !hidden.contains(&e.uid) && crate::session_project::keeps(scope, e));
        let (_, uids) =
            penelope_memory::recall::Snapshots::build_block_with_uids(&entries, budget as u64);
        out.extend(uids);
    }
    out
}

/// Profil, cœur et projets tels que l'index les donne maintenant, pour cette portée : le
/// cœur et les projets d'un autre sujet restent au rappel (issue #119).
pub async fn fresh_snapshot(s: &Services, scope: &crate::session_project::Scope) -> [String; 3] {
    let cfg = s.config.config();
    let mut out: [String; 3] = Default::default();
    // Entrées expirées : jamais injectées d'office ; `sensible` n'est qu'un marqueur
    // (issues #25 et #37).
    let hidden = s.memory.hidden_uids().await.unwrap_or_default();
    for (i, (level, budget)) in [
        (
            penelope_memory::Level::Profil,
            cfg.memory.profile_budget_tokens,
        ),
        (penelope_memory::Level::Coeur, cfg.memory.core_budget_tokens),
        (
            penelope_memory::Level::Projet,
            cfg.memory.project_budget_tokens,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entries = s.memory.by_level(level).await.unwrap_or_default();
        entries.retain(|e| !hidden.contains(&e.uid) && crate::session_project::keeps(scope, e));
        out[i] = penelope_memory::recall::Snapshots::build_block(&entries, budget as u64);
    }
    out
}

/// Budget du niveau Cœur, mesuré sur ce qui est réellement injecté (hors entrées
/// expirées) : un dépassement est signalé dans `DREAMS.md` (issue #25).
pub async fn core_overflow(s: &Services, budget: u64) -> Option<String> {
    let hidden = s.memory.hidden_uids().await.ok()?;
    let mut entries = s.memory.by_level(Level::Coeur).await.ok()?;
    entries.retain(|e| !hidden.contains(&e.uid));
    let (total, left_out) = penelope_memory::recall::Snapshots::budget_use(&entries, budget);
    (left_out > 0).then(|| {
        format!(
            "niveau Cœur à ~{total} jetons pour un budget de {budget} ({left_out} entrée(s) \
             non injectée(s)) : alléger memoire.md ou relever core_budget_tokens"
        )
    })
}

/// Instantané de l'épisode : calculé au premier tour, relu ensuite.
pub(crate) async fn frozen_snapshot(
    s: &Services,
    session_id: &str,
    episode: i64,
    user_text: &str,
) -> [String; 3] {
    let key = crate::episodes::snapshot_key(session_id, episode);
    let k = key.clone();
    let stored: Option<String> = s
        .store
        .read(move |c| {
            let mut st = c.prepare("SELECT v FROM kv WHERE k = ?1")?;
            let mut rows = st.query([&k])?;
            Ok(match rows.next()? {
                Some(r) => Some(r.get::<_, String>(0)?),
                None => None,
            })
        })
        .await
        .ok()
        .flatten();
    if let Some(blocks) = stored.and_then(|raw| serde_json::from_str::<[String; 3]>(&raw).ok()) {
        return blocks;
    }
    // Le sujet se fixe ici, avec l'instantané de l'épisode (#119).
    let project = crate::session_project::resolve(s, session_id, user_text).await;
    let blocks = fresh_snapshot(s, &crate::session_project::Scope::Session(project)).await;
    if let Ok(raw) = serde_json::to_string(&blocks) {
        let _ = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v, ts)
                     VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                    penelope_store::rusqlite::params![key, raw],
                )?;
                Ok(())
            })
            .await;
    }
    blocks
}
