//! Contrôles de la mémoire : entrées trop longues, fichiers hors index, embeddings.

use super::*;
use penelope_llm::mock::MockProvider;

fn embedder(s: &Arc<Services>, p: &Arc<MockProvider>) -> crate::embeddings::Embedder {
    crate::embeddings::Embedder {
        services: s.clone(),
        providers: penelope_app::testing::MockProviders::new(p.clone()),
        state: Arc::default(),
    }
}

/// #145 : une entrée au-delà de la taille permise est nommée, avec la commande qui la
/// découpe.
#[tokio::test]
async fn an_oversized_entry_is_named() {
    let (_d, s) = services().await;
    assert!(memory_size_check(&s).await.ok);
    let vault = crate::helpers::vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    let long = "Le serveur Atlas tourne sous Debian. ".repeat(12);
    std::fs::write(
        vault.join("projets.md"),
        format!("# Projets\n\n## Infrastructure\n- {long} <!-- uid: LONG1 -->\n"),
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();
    let c = memory_size_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert!(
        c.detail
            .starts_with("1 entrée(s) au-delà de 300 caractères : `LONG1`"),
        "{}",
        c.detail
    );
    assert!(c.fix.unwrap().contains("penelope mem split"));
}

/// #15 : un fichier du vault que l'index ignore est signalé.
#[tokio::test]
async fn a_vault_file_outside_the_index_is_reported() {
    let (_d, s) = services().await;
    let vault = crate::helpers::vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("profil.md"),
        "# Profil\n\n## Préférences\n- Café le matin <!-- uid: CAF1 -->\n",
    )
    .unwrap();
    let c = vault_index_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert!(c.detail.contains("hors index : profil.md"), "{}", c.detail);
    crate::vault_ops::reindex(&s, &vault).await.unwrap();
    let c = vault_index_check(&s).await;
    assert!(c.ok, "{c:?}");
}

/// #11 : le rôle `embedding` refusé, muet ou joignable, chacun a son verdict.
#[tokio::test]
async fn the_embedding_probe_says_why_search_stays_lexical() {
    let (_d, s) = services().await;
    let p = Arc::new(MockProvider::new());
    let emb = embedder(&s, &p);

    let c = embedding_check(&emb).await;
    assert!(!c.ok && c.detail.contains("injoignable"), "{c:?}");

    // Un vecteur vide est gardé en cache comme un autre : la sonde le montrerait encore
    // une fois le fournisseur réparé. D'où une instance à part.
    {
        let (_d2, s2) = services().await;
        let p2 = Arc::new(MockProvider::new());
        p2.set_embedder(Some(Arc::new(|_| Vec::new())));
        let c = embedding_check(&embedder(&s2, &p2)).await;
        assert!(!c.ok && c.detail.contains("vecteur vide"), "{c:?}");
    }

    p.set_embedder(Some(Arc::new(|_| vec![0.5; 8])));
    let c = embedding_check(&emb).await;
    assert!(c.ok && c.detail.contains("8 dimensions"), "{c:?}");
}
