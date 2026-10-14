//! Instantanés du prompt système (issue #205) : le domaine vit dans
//! `penelope_conversation::prompt_snapshot` ; le daemon garde le port de la boucle.

use penelope_app::conversation::PromptPrefix;
use penelope_app::services::Services;
pub use penelope_conversation::prompt_snapshot::*;

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

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_app::engine::TurnIntake;
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
