use super::subjects::{self, Subject};
use super::*;
use penelope_app::bus::{ChannelDelivery, Origin};
use penelope_app::helpers::vault_dir;
use penelope_app::outcome::TurnOutcome;
use penelope_kernel::clock::TestClock;
use penelope_kernel::session::SessionKind;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, Arc::new(s))
}

/// Le canal vu du cœur : il nomme le sujet de certaines sessions, c'est tout.
#[derive(Default)]
struct Subjects(Mutex<BTreeMap<String, String>>);

impl Subjects {
    fn name(&self, session: &str, subject: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(session.to_string(), subject.to_string());
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for Subjects {
    async fn deliver(&self, _: &str, _: &str, _: &Origin, _: &TurnOutcome) {}

    async fn subject_of(&self, session_id: &str) -> Option<String> {
        self.0.lock().unwrap().get(session_id).cloned()
    }
}

async fn session(s: &Services) -> String {
    s.sessions
        .create(SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string()
}

/// Une mémoire qui connaît deux projets : Fidelatoo (annotation) et LinkedIn (section).
async fn seed_memory(s: &Services) -> std::path::PathBuf {
    let vault = vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire de fond\n\n## Clients\n\
         - La base de Fidelatoo tourne sur Postgres <!-- projet: Fidelatoo --> ^01FIDBASE\n\
         - Le propriétaire préfère le tutoiement ^01GENERAL\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Fidelatoo\n- Pagination infinie à corriger ^01FIDPAG\n\n\
         ## LinkedIn\n- Trois publications par semaine ^01LINKED\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    vault
}

async fn project_events(s: &Services, sid: &str) -> usize {
    s.events
        .session_events_of_kind(sid, penelope_context::store::KIND_SESSION_PROJECT)
        .await
        .unwrap()
        .len()
}

/// #301 : le sujet que nomme le canal est le projet de la session, connu du vault ou non ;
/// sans sujet, le titre ou le message ne désignent qu'un projet connu, fiches comprises ;
/// un choix explicite prime sur tout.
#[tokio::test]
async fn the_subject_of_the_conversation_is_the_project_even_unknown() {
    let (_d, s) = services().await;
    let vault = seed_memory(&s).await;
    let port = Arc::new(Subjects::default());
    s.channel.delivery.set(Some(port.clone()));

    let dose = session(&s).await;
    port.name(&dose, "Dose");
    assert_eq!(resolve(&s, &dose, "bonjour").await, Some("dose".into()));
    assert_eq!(
        of_session(&s, &dose).await,
        (Some("dose".into()), Some("sujet".into()))
    );
    // Un projet inconnu de la mémoire : les entrées des autres projets restent au rappel.
    assert!(!keeps(
        &Scope::Session(Some("dose".into())),
        &s.memory.get("01FIDBASE").await.unwrap().unwrap()
    ));

    // Le sujet prime sur le titre.
    let titled = session(&s).await;
    s.sessions
        .set_title(&titled, "Fidelatoo : correctif", false)
        .await
        .unwrap();
    port.name(&titled, "Thaïlande");
    assert_eq!(resolve(&s, &titled, "x").await, Some("thailande".into()));

    // Sans sujet : le titre, parmi les projets connus.
    let plain = session(&s).await;
    s.sessions
        .set_title(&plain, "Fidelatoo : correctif", false)
        .await
        .unwrap();
    assert_eq!(resolve(&s, &plain, "x").await, Some("fidelatoo".into()));
    assert_eq!(of_session(&s, &plain).await.1, Some("titre".into()));

    // Une fiche de projet rend le projet connu : le premier message peut le nommer.
    subjects::ensure_note(&vault, "Dose", "dose", "2026-01-01").unwrap();
    assert!(known(&s).await.contains("dose"));
    let by_message = session(&s).await;
    assert_eq!(
        resolve(&s, &by_message, "où en est Dose ?").await,
        Some("dose".into())
    );
    assert_eq!(of_session(&s, &by_message).await.1, Some("message".into()));

    // L'explicite prime.
    let chosen = session(&s).await;
    port.name(&chosen, "Dose");
    set(&s, &chosen, Some("LinkedIn")).await;
    assert_eq!(resolve(&s, &chosen, "x").await, Some("linkedin".into()));

    // Sans canal, rien ne change pour une session sans titre ni message parlant.
    s.channel.delivery.set(None);
    let silent = session(&s).await;
    assert_eq!(resolve(&s, &silent, "bonjour").await, None);
}

/// #301 : la migration adopte chaque sujet nommé : fiche créée, sessions sans projet
/// rattachées, rattachements déduits (message faux, ancien nom) remplacés, explicites
/// conservés ; le préfixe des sessions actives est libéré une fois (`session.project`),
/// pas celui des sessions fermées ; un second passage ne change rien.
#[tokio::test]
async fn migration_adopts_named_subjects_once() {
    let (_d, s) = services().await;
    let vault = seed_memory(&s).await;
    let day = crate::vault_ops::day(&s);

    let fresh = session(&s).await;
    let chosen = session(&s).await;
    set(&s, &chosen, Some("autre")).await;
    let closed = session(&s).await;
    s.sessions.set_state(&closed, "closed").await.unwrap();
    let wrong = session(&s).await;
    s.kv_set(
        &format!("session.project.{wrong}"),
        r#"{"project":"helloasso","how":"message"}"#,
    )
    .await
    .unwrap();
    let stale = session(&s).await;
    s.kv_set(
        &format!("session.project.{stale}"),
        r#"{"project":"linkedin","how":"sujet"}"#,
    )
    .await
    .unwrap();
    let right = session(&s).await;
    s.kv_set(
        &format!("session.project.{right}"),
        r#"{"project":"westriders","how":"titre"}"#,
    )
    .await
    .unwrap();

    let subjects = vec![
        Subject {
            name: "Dose".into(),
            sessions: vec![fresh.clone(), chosen.clone(), closed.clone()],
        },
        Subject {
            name: "WestRiders".into(),
            sessions: vec![wrong.clone(), right.clone()],
        },
        Subject {
            name: "Posts LinkedIn".into(),
            sessions: vec![stale.clone()],
        },
    ];
    let r = subjects::migrate(&s, &vault, &subjects).await;
    assert_eq!(
        (r.subjects, r.created, r.attached, r.corrected, r.kept),
        (3, 3, 2, 2, 1),
        "{r:?}"
    );
    assert!(r.changed());
    assert_eq!(r.day, day);

    for (sid, want) in [
        (&fresh, ("dose", "sujet")),
        (&chosen, ("autre", "explicite")),
        (&closed, ("dose", "sujet")),
        (&wrong, ("westriders", "sujet")),
        (&right, ("westriders", "sujet")),
        (&stale, ("posts-linkedin", "sujet")),
    ] {
        assert_eq!(
            of_session(&s, sid).await,
            (Some(want.0.into()), Some(want.1.into())),
            "session {sid}"
        );
    }
    // Journal : une libération par session active changée ; aucune pour la fermée, pour
    // l'explicite (la sienne vient de `set`) ni pour le bon projet venu du titre.
    assert_eq!(project_events(&s, &fresh).await, 1);
    assert_eq!(project_events(&s, &closed).await, 0);
    assert_eq!(project_events(&s, &chosen).await, 1);
    assert_eq!(project_events(&s, &wrong).await, 1);
    assert_eq!(project_events(&s, &right).await, 0);
    assert_eq!(project_events(&s, &stale).await, 1);

    // Les fiches, au gabarit du wiki.
    let dose = std::fs::read_to_string(vault.join("projets/dose.md")).unwrap();
    assert!(dose.starts_with("---\n"), "{dose}");
    for want in [
        "type: projet\n",
        "nom: Dose\n",
        "aliases:\n  - Dose\n",
        "tags:\n  - projets\n",
        &format!("created: {day}\n"),
        "\n# Dose\n",
        "[[projets]]",
    ] {
        assert!(dose.contains(want), "{want:?} absent de {dose}");
    }
    assert!(vault.join("projets/westriders.md").exists());
    assert!(vault.join("projets/posts-linkedin.md").exists());
    assert_eq!(
        subjects::notes(&vault),
        vec!["dose", "posts-linkedin", "westriders"]
    );
    let log = std::fs::read_to_string(vault.join("log.md")).unwrap();
    assert!(log.contains("projet | Dose"), "{log}");
    // Hors de l'index : la fiche n'est pas une entrée de mémoire.
    assert!(crate::vault_inventory::excluded("projets/dose.md").is_some());
    assert!(crate::vault_inventory::excluded("projets.md").is_none());

    // Le digest du lendemain le dit.
    let note = subjects::digest_note(&s).await.unwrap();
    assert!(
        note.contains("3 sujet(s), 3 projet(s), 3 fiche(s) créée(s), 2 session(s) rattachée(s), 2 rattachement(s) corrigé(s)"),
        "{note}"
    );
    assert!(note.contains("1 choix explicite(s) conservé(s)"), "{note}");

    // Second passage : rien ne bouge.
    let again = subjects::migrate(&s, &vault, &subjects).await;
    assert!(!again.changed(), "{again:?}");
    assert_eq!((again.subjects, again.kept), (3, 1));
    assert_eq!(project_events(&s, &fresh).await, 1);
    assert_eq!(
        std::fs::read_to_string(vault.join("projets/dose.md")).unwrap(),
        dose
    );
    assert_eq!(of_session(&s, &chosen).await.0, Some("autre".into()));
}

/// #301 : un sujet renommé emporte son projet : la fiche est déplacée (ancien nom en
/// alias, wikilinks réécrits), les annotations et la section de la mémoire prennent le
/// nouveau nom, les sessions suivent sauf l'explicite ; pas de seconde fiche.
#[tokio::test]
async fn renaming_a_subject_moves_the_project_and_its_memory() {
    let (_d, s) = services().await;
    let vault = seed_memory(&s).await;
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire de fond\n\n## Clients\n\
         - Dose vend des capteurs, voir [[dose]] <!-- projet: Dose --> ^01DOSE\n\
         - Le propriétaire préfère le tutoiement ^01GENERAL\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Dose\n- Livraison du lot 2 en octobre <!-- importance: 6 --> ^01DOSELOT\n\n\
         ## LinkedIn\n- Trois publications par semaine ^01LINKED\n",
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();

    let sid = session(&s).await;
    let chosen = session(&s).await;
    set(&s, &chosen, Some("perso")).await;
    let born = Subject {
        name: "Dose".into(),
        sessions: vec![sid.clone(), chosen.clone()],
    };
    let a = subjects::adopt(&s, &vault, &born, None).await;
    assert!(a.created && a.attached == 1 && a.kept == 1, "{a:?}");

    let renamed = Subject {
        name: "Dose Capteurs".into(),
        sessions: vec![sid.clone(), chosen.clone()],
    };
    let a = subjects::adopt(&s, &vault, &renamed, Some("Dose")).await;
    assert_eq!(
        (a.project.as_str(), a.moved, a.created, a.corrected, a.kept),
        ("dose-capteurs", true, false, 1, 1),
        "{a:?}"
    );
    assert!(!vault.join("projets/dose.md").exists());
    let note = std::fs::read_to_string(vault.join("projets/dose-capteurs.md")).unwrap();
    for want in [
        "nom: Dose Capteurs\n",
        "aliases:\n  - Dose\n  - dose\n",
        "\n# Dose Capteurs\n",
    ] {
        assert!(note.contains(want), "{want:?} absent de {note}");
    }
    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    assert!(
        memoire.contains("[[dose-capteurs]] <!-- projet: Dose Capteurs --> ^01DOSE"),
        "{memoire}"
    );
    let projets = std::fs::read_to_string(vault.join("projets.md")).unwrap();
    assert!(projets.contains("\n## Dose Capteurs\n") && projets.contains("^01DOSELOT"));
    assert!(projets.contains("\n## LinkedIn\n"), "{projets}");
    let known = known(&s).await;
    assert!(
        known.contains("dose-capteurs") && !known.contains("dose"),
        "{known:?}"
    );
    assert_eq!(
        entry_project(&s.memory.get("01DOSE").await.unwrap().unwrap()),
        Some("dose-capteurs".into())
    );
    assert_eq!(
        entry_project(&s.memory.get("01DOSELOT").await.unwrap().unwrap()),
        Some("dose-capteurs".into())
    );
    assert_eq!(
        of_session(&s, &sid).await,
        (Some("dose-capteurs".into()), Some("sujet".into()))
    );
    assert_eq!(of_session(&s, &chosen).await.0, Some("perso".into()));
    assert_eq!(project_events(&s, &sid).await, 2);

    // Le même renommage une seconde fois : rien à déplacer, rien à corriger.
    let a = subjects::adopt(&s, &vault, &renamed, Some("Dose")).await;
    assert_eq!(
        (a.moved, a.created, a.corrected),
        (false, false, 0),
        "{a:?}"
    );
    assert_eq!(subjects::notes(&vault), vec!["dose-capteurs"]);
}
