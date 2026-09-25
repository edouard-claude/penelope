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
//! la donnée personnelle. Purge et rétention sont livrées dans le même lot ([`penelope_ops::purge`]).

use penelope_app::services::Services;
use penelope_context::tiers::TileMap;
use penelope_store::rusqlite::{OptionalExtension, params};

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
    let (hash, rendered, ts) = (
        hash.to_string(),
        prefix.rendered.clone(),
        s.clock.now_rfc3339(),
    );
    let tiles = prefix
        .tiles
        .as_ref()
        .and_then(|t| serde_json::to_string(t).ok());
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at,
                    last_seen_at, uses)
                 VALUES(?1,?2,?3,?4,?4,1)
                 ON CONFLICT(hash) DO UPDATE SET last_seen_at = ?4, uses = uses + 1",
                params![hash, rendered, tiles, ts],
            )?;
            Ok(())
        })
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

/// Le port `PromptSnapshots` de la boucle : la table `prompt_snapshots` (épopée #208,
/// T09).
pub struct StoredSnapshots(pub std::sync::Arc<Services>);

#[async_trait::async_trait]
impl penelope_agent::PromptSnapshots for StoredSnapshots {
    async fn record(&self, hash: &str, prefix: &PromptPrefix) -> anyhow::Result<bool> {
        record(&self.0, hash, prefix).await
    }

    async fn prefix_cause(&self, before: Option<&str>, after: &str) -> String {
        prefix_cause(&self.0, before, after).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_context::tiers::{Tiers, TiersBuilder};
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), Arc::new(TestClock::default()))
                .await
                .unwrap(),
        );
        (dir, s)
    }

    fn tiers(skill: &str) -> Tiers {
        TiersBuilder::new()
            .soul("Je suis Pénélope.")
            .skill(skill, "une skill")
            .memory_snapshot("- Répondre en français.", "", "")
            .volatile("Date et heure : lundi")
            .build()
    }

    /// Le même prompt, deux fois : une seule ligne, `uses` à 2.
    #[tokio::test]
    async fn the_same_prompt_is_written_once() {
        let (_d, s) = services().await;
        let t = tiers("revue");
        let prefix = PromptPrefix::of(&t);
        let hash = prefix.hash();
        assert!(record(&s, &hash, &prefix).await.unwrap());
        assert!(record(&s, &hash, &prefix).await.unwrap());
        let snap = get(&s, &hash).await.unwrap().unwrap();
        assert_eq!(snap.uses, 2);
        assert_eq!(snap.rendered, t.prefix());
        let n: i64 = s
            .store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM prompt_snapshots", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    /// Le volatile (T4) n'entre pas dans l'instantané : sinon la déduplication s'effondre.
    #[tokio::test]
    async fn the_volatile_tier_never_enters_the_snapshot() {
        let (_d, s) = services().await;
        let a = TiersBuilder::new()
            .soul("x")
            .volatile("Date : lundi")
            .build();
        let b = TiersBuilder::new()
            .soul("x")
            .volatile("Date : mardi")
            .build();
        let (pa, pb) = (PromptPrefix::of(&a), PromptPrefix::of(&b));
        assert_eq!(pa.hash(), pb.hash());
        record(&s, &pa.hash(), &pa).await.unwrap();
        record(&s, &pb.hash(), &pb).await.unwrap();
        let snap = get(&s, &pa.hash()).await.unwrap().unwrap();
        assert_eq!(snap.uses, 2);
        assert!(!snap.rendered.contains("lundi"), "{}", snap.rendered);
    }

    /// Une skill rechargée : deux instantanés, et le diagnostic nomme la tuile T1.
    #[tokio::test]
    async fn a_reloaded_skill_names_the_index_tile() {
        let (_d, s) = services().await;
        let (a, b) = (tiers("revue"), tiers("autre"));
        let (pa, pb) = (PromptPrefix::of(&a), PromptPrefix::of(&b));
        record(&s, &pa.hash(), &pa).await.unwrap();
        record(&s, &pb.hash(), &pb).await.unwrap();
        assert_ne!(pa.hash(), pb.hash());
        assert_eq!(
            changed_tiles(&s, Some(&pa.hash()), &pb.hash()).await,
            vec!["T1"]
        );
    }

    /// #205 : la cause « préfixe » nomme la tuile qui a bougé, et son libellé le dit en
    /// clair dans `penelope usage --by miss`.
    #[tokio::test]
    async fn the_prefix_miss_names_the_tile_that_moved() {
        let (_d, s) = services().await;
        let (a, b) = (tiers("revue"), tiers("autre"));
        let (pa, pb) = (PromptPrefix::of(&a), PromptPrefix::of(&b));
        record(&s, &pa.hash(), &pa).await.unwrap();
        record(&s, &pb.hash(), &pb).await.unwrap();
        let cause = prefix_cause(&s, Some(&pa.hash()), &pb.hash()).await;
        assert_eq!(cause, "prefixe:T1");
        let label = penelope_kernel::budget::miss_label(&cause);
        assert!(label.contains("index des capacités"), "{label}");
        // Sans instantané d'avant, la cause reste celle d'hier : rien n'est inventé.
        assert_eq!(prefix_cause(&s, None, &pb.hash()).await, "prefixe");
    }

    /// Deux tours d'une session sans rechargement : un seul instantané, deux usages.
    #[tokio::test]
    async fn two_turns_without_a_reload_share_one_snapshot() {
        use crate::runtime::Daemon;
        use penelope_app::bus::Origin;
        use penelope_llm::mock::MockProvider;

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

        for text in ["où en es-tu ?", "merci"] {
            p.reply(r#"{"complexity":"medium"}"#);
            p.reply("Voilà.");
            d.enqueue_message(&sid, text, &Origin::Cli, None)
                .await
                .unwrap();
            let t = s.turns.claim("test").await.unwrap().unwrap();
            d.run_turn(&t).await;
            s.turns.complete(&t).await.unwrap();
        }

        let (rows, _bytes) = weight_bytes(&s).await.unwrap();
        assert_eq!(rows, 1, "un seul prompt distinct");
        let hash = previous_system_hash(&s, &sid).await;
        let snap = get(&s, &hash).await.unwrap().unwrap();
        assert_eq!(snap.uses, 2);
        assert!(
            snap.rendered.contains("agent personnel autonome"),
            "le prompt système est relisible : {}",
            &snap.rendered[..snap.rendered.len().min(120)]
        );
        assert!(snap.tiles.is_some(), "la découpe accompagne le prompt");
        // T6 : le journal porte le même préfixe, une seule fois.
        let events = s.events.session_events(&sid, 0).await.unwrap();
        let systems: Vec<_> = events.iter().filter(|e| e.kind == "conv.system").collect();
        assert_eq!(systems.len(), 1);
        assert_eq!(systems[0].payload["hash"], hash.as_str());
    }

    async fn previous_system_hash(s: &Services, session_id: &str) -> String {
        crate::cache_audit::previous_call(s, session_id)
            .await
            .unwrap()
            .unwrap()
            .system_hash
            .unwrap()
    }

    /// Requête réduite en urgence : le préfixe ne correspond plus à l'empreinte, on
    /// n'écrit rien plutôt qu'un texte que le modèle n'a pas lu.
    #[tokio::test]
    async fn a_prefix_that_does_not_match_its_fingerprint_is_not_written() {
        let (_d, s) = services().await;
        let prefix = PromptPrefix::plain("prompt réduit");
        assert!(!record(&s, "une_autre_empreinte", &prefix).await.unwrap());
        assert_eq!(weight_bytes(&s).await.unwrap().0, 0);
    }

    /// Un transcript sans tuiles (sous-agent, workflow) est gardé quand même : le texte
    /// seul suffit à relire ce que le modèle avait sous les yeux.
    #[tokio::test]
    async fn a_prompt_without_tiles_is_still_kept() {
        let (_d, s) = services().await;
        let prefix = PromptPrefix::plain("Tu es un sous-agent.");
        assert!(record(&s, &prefix.hash(), &prefix).await.unwrap());
        let snap = get(&s, &prefix.hash()).await.unwrap().unwrap();
        assert_eq!(snap.rendered, "Tu es un sous-agent.");
        assert!(snap.tiles.is_none());
        assert!(
            changed_tiles(&s, Some(&prefix.hash()), &prefix.hash())
                .await
                .is_empty()
        );
    }
}
