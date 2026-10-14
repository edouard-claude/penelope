//! Instantanés du prompt système (issue #205).
//!
//! Le préfixe stable (T0 à T2) est réassemblé à chaque tour et n'existait nulle part :
//! `HistoryStore` ne persiste que `user`, `assistant` et `tool`, et le seul vestige était
//! une empreinte dans `usage`. Une réponse surprenante n'était donc pas auditable, et
//! `penelope usage --by miss` savait dire « le préfixe a changé » sans jamais dire **quoi**.
//!
//! ```text
//!   prompt rendu ──sha256──▶ system_hash ──▶ prompt_snapshots(hash) ──▶ rendered + tuiles
//!        (usage.system_hash, llm_requests.system_hash, turn.started les citent déjà)
//! ```
//!
//! Trois règles :
//!
//! - **une ligne par prompt distinct** : le même préfixe sur cinq cents tours n'écrit
//!   qu'une fois, `uses` compte les appels ;
//! - **jamais sur le chemin de la réponse** : l'écriture suit l'appel, et son échec ne
//!   fait pas échouer le tour (§1, premier retour en moins de 1,5 s) ;
//! - **le volatile n'y entre pas** : T4 vit dans `message_context` depuis #17, et le
//!   figer ici ferait s'effondrer la déduplication.
//!
//! Le prompt contient le profil, la mémoire rappelée et les notes de session : c'est de
//! la donnée personnelle. Purge et rétention sont livrées dans le même lot (`penelope_ops::purge`).

use penelope_app::services::Services;
use penelope_context::tiers::TileMap;
use penelope_store::rusqlite::OptionalExtension;

/// Un prompt système gardé sous son empreinte.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub hash: String,
    pub rendered: String,
    pub tiles: Option<TileMap>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub uses: i64,
}

use penelope_app::conversation::PromptPrefix;

/// Enregistre le prompt qui vient d'être envoyé, sous l'empreinte déjà calculée.
///
/// `hash` est le `system_hash` de l'empreinte : si le préfixe fourni ne lui correspond
/// pas (requête réduite en urgence, §5.4 niveau 4), rien n'est écrit plutôt qu'un texte
/// qui ne serait pas celui qu'a lu le modèle.
pub async fn record(s: &Services, hash: &str, prefix: &PromptPrefix) -> anyhow::Result<bool> {
    if prefix.hash() != hash {
        tracing::debug!("préfixe et empreinte divergent : pas d'instantané pour cette requête");
        return Ok(false);
    }
    s.context
        .history
        .prompt_sent(hash, &prefix.rendered, prefix.tiles.as_ref())
        .await?;
    Ok(true)
}

/// Relit un instantané par son empreinte.
pub async fn get(s: &Services, hash: &str) -> anyhow::Result<Option<Snapshot>> {
    let hash = hash.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT hash, rendered, tiers, first_seen_at, last_seen_at, uses
                 FROM prompt_snapshots WHERE hash = ?1",
                [hash],
                |r| {
                    let tiers: Option<String> = r.get(2)?;
                    Ok(Snapshot {
                        hash: r.get(0)?,
                        rendered: r.get(1)?,
                        tiles: tiers.and_then(|t| serde_json::from_str(&t).ok()),
                        first_seen_at: r.get(3)?,
                        last_seen_at: r.get(4)?,
                        uses: r.get(5)?,
                    })
                },
            )
            .optional()?)
        })
        .await?)
}

/// Les tuiles qui ont changé entre deux prompts, nommées : c'est ce que
/// `penelope usage --by miss` ajoute à la cause « préfixe » (issue #17).
pub async fn changed_tiles(s: &Services, before: Option<&str>, after: &str) -> Vec<&'static str> {
    let (Some(before), Ok(Some(a))) = (before, get(s, after).await) else {
        return Vec::new();
    };
    let Ok(Some(b)) = get(s, before).await else {
        return Vec::new();
    };
    match (b.tiles, a.tiles) {
        (Some(b), Some(a)) => b.changed(&a),
        _ => Vec::new(),
    }
}

/// Cause « préfixe » précisée par les tuiles qui ont bougé : `prefixe:T1`,
/// `prefixe:T1+T2`, ou `prefixe` quand aucun des deux instantanés n'est relisible.
///
/// C'est la colonne « ce qui a changé » de `penelope usage --by miss` : le diagnostic
/// s'arrêtait jusqu'ici à l'empreinte (issue #17).
pub async fn prefix_cause(s: &Services, before: Option<&str>, after: &str) -> String {
    match changed_tiles(s, before, after).await {
        tiles if tiles.is_empty() => "prefixe".to_string(),
        tiles => format!("prefixe:{}", tiles.join("+")),
    }
}
/// Poids de la table, pour `doctor`.
pub async fn weight_bytes(s: &Services) -> anyhow::Result<(i64, i64)> {
    Ok(s.store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*), coalesce(sum(length(rendered)), 0) FROM prompt_snapshots",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await?)
}
