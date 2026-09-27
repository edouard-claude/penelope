//! Opérations appliquées une à une au vault : exceptions et écarts d'une pratique,
//! liens, fiches d'entité, et leurs refus.

use super::*;

const PRACTICE: &str = "---\ntype: pratique\nid: langage-backend\nconfiance: 0.8\n---\n# Langage backend\n\n## Défaut\n- Go (stdlib) <!-- uid: DEF1 -->\n\n## Exceptions\n\n## Écarts observés\n";

async fn practice(s: &Services, vault: &Path) -> Practice {
    let _ = s;
    let raw = std::fs::read_to_string(vault.join("pratiques/langage-backend.md")).unwrap();
    Practice::parse(&raw, "langage-backend").unwrap()
}

#[tokio::test]
async fn a_practice_exception_is_updated_and_a_deviation_recorded() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(vault.join("pratiques/langage-backend.md"), PRACTICE).unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();

    let add = Operation::AddException {
        practice: "langage-backend".into(),
        text: "Rust".into(),
        quand: "tache=code".into(),
        confiance: Some(0.6),
    };
    apply(&d, &vault, &add, "r1").await.unwrap();
    let uid = practice(s, &vault).await.exceptions[0].uid.clone();

    let update = Operation::UpdateException {
        uid: uid.clone(),
        text: Some("Rust, avec tests de propriété".into()),
        quand: Some("tache=code; criticite=haute".into()),
        confiance: Some(1.7),
    };
    assert_eq!(
        apply(&d, &vault, &update, "r1").await.unwrap(),
        "pratiques/langage-backend.md"
    );
    let p = practice(s, &vault).await;
    assert_eq!(p.exceptions.len(), 1);
    let exc = &p.exceptions[0];
    assert_eq!(exc.text, "Rust, avec tests de propriété");
    assert_eq!(exc.annotations.confiance, Some(1.0), "confiance bornée");
    assert!(
        exc.annotations
            .quand
            .as_ref()
            .unwrap()
            .render()
            .contains("criticite=haute")
    );

    let ecart = Operation::RecordEcart {
        practice: "langage-backend".into(),
        text: "Script Python pour la migration".into(),
        quand: "tache=migration".into(),
    };
    apply(&d, &vault, &ecart, "r1").await.unwrap();
    let p = practice(s, &vault).await;
    assert_eq!(p.ecarts.len(), 1);
    assert_eq!(p.ecarts[0].text, "Script Python pour la migration");
    assert_eq!(p.default_entry.unwrap().text, "Go (stdlib)");

    let err = apply(
        &d,
        &vault,
        &Operation::UpdateException {
            uid: "DEF1".into(),
            text: None,
            quand: None,
            confiance: Some(0.1),
        },
        "r1",
    )
    .await
    .unwrap_err();
    assert!(err.contains("exception DEF1 absente"), "{err}");
}

#[tokio::test]
async fn an_entry_is_linked_to_an_entity_once() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Clients\n- Le client Martin est basé à Lyon <!-- uid: MAR1 -->\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    let link = Operation::Link {
        from_uid: "MAR1".into(),
        to_slug: "martin".into(),
    };
    assert_eq!(apply(&d, &vault, &link, "r1").await.unwrap(), "projets.md");
    apply(&d, &vault, &link, "r1").await.unwrap();
    let raw = std::fs::read_to_string(vault.join("projets.md")).unwrap();
    assert_eq!(raw.matches("[[martin]]").count(), 1, "{raw}");
    assert!(
        raw.contains("Le client Martin est basé à Lyon [[martin]]"),
        "{raw}"
    );
}

#[tokio::test]
async fn an_entity_card_is_created_never_overwritten() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let create = |body: &str| Operation::CreateEntity {
        kind: "client\nimportant".into(),
        slug: "Martin SA".into(),
        title: "Martin SA".into(),
        body: body.into(),
    };
    let file = apply(&d, &vault, &create("Client lyonnais depuis 2024."), "r1")
        .await
        .unwrap();
    assert_eq!(file, "entites/martin-sa.md");
    let raw = std::fs::read_to_string(vault.join(&file)).unwrap();
    assert!(raw.contains("type: entite"), "{raw}");
    assert!(raw.contains("genre: client important"), "{raw}");
    assert!(
        raw.contains("# Martin SA\n\nClient lyonnais depuis 2024."),
        "{raw}"
    );

    let err = apply(&d, &vault, &create("Autre texte."), "r1")
        .await
        .unwrap_err();
    assert!(err.contains("existe déjà"), "{err}");
    let err = apply(
        &d,
        &vault,
        &Operation::CreateEntity {
            kind: "client".into(),
            slug: "fuite".into(),
            title: "Fuite".into(),
            body: "clé sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        },
        "r1",
    )
    .await
    .unwrap_err();
    assert!(err.contains("refusé par le filtre"), "{err}");
    assert!(!vault.join("entites/fuite.md").exists());
}

/// Une opération qui vise un uid inconnu, ou un défaut de pratique, n'écrit rien.
#[tokio::test]
async fn operations_on_unknown_entries_are_refused() {
    let (_dir, d, _p) = daemon().await;
    let vault = crate::helpers::vault_dir(&d.services);
    for op in [
        Operation::Link {
            from_uid: "ZZZ".into(),
            to_slug: "x".into(),
        },
        Operation::RetireEntry {
            uid: "ZZZ".into(),
            reason: "r".into(),
        },
        Operation::UpdateException {
            uid: "ZZZ".into(),
            text: Some("t".into()),
            quand: None,
            confiance: None,
        },
    ] {
        let err = apply(&d, &vault, &op, "r1").await.unwrap_err();
        assert!(err.contains("uid inconnu de l'index : ZZZ"), "{err}");
        assert_eq!(target_file(&d.services, &op), "uid:ZZZ");
    }
    let default = Operation::UpdateDefault {
        practice: "langage-backend".into(),
        text: "Rust".into(),
    };
    assert!(
        apply(&d, &vault, &default, "r1")
            .await
            .unwrap_err()
            .contains("proposition")
    );
    assert_eq!(
        target_file(&d.services, &default),
        "pratiques/langage-backend.md"
    );
    assert_eq!(
        target_file(
            &d.services,
            &Operation::CreateEntity {
                kind: String::new(),
                slug: "martin".into(),
                title: String::new(),
                body: String::new(),
            }
        ),
        "entites/martin.md"
    );
}
