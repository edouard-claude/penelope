use super::*;
use penelope_kernel::clock::TestClock;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, Arc::new(s))
}

/// #119 : deux sessions de sujets différents ne reçoivent pas le même bloc Projet ;
/// une entrée d'un projet n'entre pas dans la tuile T2 d'une session d'un autre projet
/// mais ressort par `mem_search` ; une entrée sans projet entre partout ; le préfixe
/// reste identique d'un tour à l'autre ; le sujet se déduit du titre ou du sujet
/// Telegram quand il nomme un projet connu.
#[tokio::test]
async fn the_injected_memory_follows_the_session_subject() {
    let (_dir, s) = services().await;
    let vault = vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire de fond\n\n## Clients\n\
         - La base de Fidelatoo tourne sur Postgres chez Scaleway <!-- projet: Fidelatoo --> ^01FIDBASE\n\
         - Le propriétaire préfère le tutoiement ^01GENERAL\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Fidelatoo\n- Pagination infinie à corriger ^01FIDPAG\n\n\
         ## LinkedIn\n- Trois publications par semaine ^01LINKED\n",
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();
    let d = crate::runtime::Daemon::from_services(s.clone());

    let fid = session(&s).await;
    let lnk = session(&s).await;
    crate::session_project::set(&d, &fid, Some("Fidelatoo")).await;
    crate::session_project::set(&d, &lnk, Some("linkedin")).await;
    let t2 = |sid: String| {
        let s = s.clone();
        async move {
            build_tiers_in(&s, "bonjour", &[], None, Some((sid.as_str(), 0)), None)
                .await
                .context
        }
    };
    let (a, b) = (t2(fid.clone()).await, t2(lnk.clone()).await);
    assert!(
        a.contains("Postgres chez Scaleway") && a.contains("Pagination infinie"),
        "{a}"
    );
    assert!(!a.contains("Trois publications"), "{a}");
    assert!(b.contains("Trois publications"), "{b}");
    assert!(!b.contains("Scaleway") && !b.contains("Pagination"), "{b}");
    assert!(
        a.contains("tutoiement") && b.contains("tutoiement"),
        "sans projet : partout"
    );

    // Ce qui est écarté reste atteignable.
    let hits = s
        .memory
        .search(
            "Postgres Scaleway",
            None,
            &penelope_memory::SearchFilter::explicit(),
            &[],
        )
        .await
        .unwrap();
    assert!(hits.iter().any(|h| h.entry.uid == "01FIDBASE"));
    let uids = snapshot_uids(
        &s,
        &crate::session_project::Scope::Session(Some("linkedin".into())),
    )
    .await;
    assert!(
        !uids.contains(&"01FIDBASE".to_string()),
        "le rappel peut la servir"
    );

    // Préfixe stable d'un tour à l'autre dans la même session.
    let p1 = build_tiers_in(&s, "et ensuite ?", &[], None, Some((fid.as_str(), 0)), None)
        .await
        .prefix_hash();
    let p2 = build_tiers_in(&s, "autre chose", &[], None, Some((fid.as_str(), 0)), None)
        .await
        .prefix_hash();
    assert_eq!(p1, p2);

    // Déduit du titre, puis du nom du sujet Telegram.
    let titled = session(&s).await;
    s.sessions
        .set_title(&titled, "Fidelatoo : correctif de la pagination", false)
        .await
        .unwrap();
    let c = t2(titled.clone()).await;
    assert!(
        c.contains("Pagination infinie") && !c.contains("Trois publications"),
        "{c}"
    );
    assert_eq!(
        crate::session_project::of_session(&s, &titled).await,
        (Some("fidelatoo".into()), Some("titre".into()))
    );
    let topic = session(&s).await;
    s.sessions
        .bind_telegram(&topic, -10_042, Some(21))
        .await
        .unwrap();
    let key = crate::helpers::topic_name_key(-10_042, 21);
    d.services.kv_set(&key, "Posts LinkedIn").await.unwrap();
    let t = t2(topic.clone()).await;
    assert!(
        t.contains("Trois publications") && !t.contains("Scaleway"),
        "{t}"
    );
    let plain = session(&s).await;
    let p = t2(plain).await;
    assert!(
        p.contains("tutoiement") && !p.contains("Scaleway") && !p.contains("Trois"),
        "{p}"
    );
}

async fn session(s: &Services) -> String {
    s.sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string()
}

const PRATIQUE: &str = "---\n\
    type: pratique\n\
    id: langage-backend\n\
    scope: global\n\
    confiance: 0.8\n\
    preuves: 9\n\
    statut: active\n\
    maj: 2026-09-17\n\
    declencheurs: [langage, backend]\n\
    ---\n\
    # Langage backend\n\n\
    ## Défaut\n\
    - Go (stdlib, architecture hexagonale). <!-- uid: 01J9A -->\n\n\
    ## Exceptions\n\
    - **Rust** si l'agent code seul sur un projet critique. <!-- uid: 01J9B --> \
      <!-- quand: tache=code; criticite=haute; codeur=agent --> <!-- confiance: 0.9 -->\n\n\
    ## Écarts observés\n\
    - 2026-09-12 · [[client-x]] : langage imposé par l'existant. <!-- uid: 01J9C --> \
      <!-- quand: client=client-x --> <!-- occurrences: 1 -->\n";

/// #62 : une entrée du Cœur servie d'office dans T2 n'est pas répétée dans T4 quand
/// le message la déclenche.
#[tokio::test]
async fn an_injected_entry_is_never_recalled_twice() {
    let (_d, s) = services().await;
    let vault = vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    crate::vault_ops::remember(
        &s,
        &vault,
        penelope_memory::Level::Coeur,
        "Le centre de calcul de Gravelines héberge les sauvegardes",
        "s1",
    )
    .await
    .unwrap();

    let tiers = build_tiers(&s, "où sont les sauvegardes de Gravelines ?", &[], None).await;
    let whole = format!("{}\n{}", tiers.context, tiers.volatile);
    assert_eq!(
        whole.matches("Gravelines").count(),
        1,
        "une seule fois dans T2 + T4 : {whole}"
    );
}

/// #58 : une pratique est rappelée avec son défaut quand le message la déclenche, et
/// son écart observé n'est jamais injecté d'office.
#[tokio::test]
async fn a_practice_is_recalled_with_its_default_and_never_its_deviations() {
    let (_d, s) = services().await;
    let vault = vault_dir(&s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(vault.join("pratiques/langage-backend.md"), PRATIQUE).unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();

    let tiers = build_tiers(&s, "quel langage pour ce backend ?", &[], None).await;
    let t4 = tiers.volatile.clone();
    assert!(
        t4.contains("[pratique: langage-backend"),
        "la pratique doit être rappelée : {t4}"
    );
    assert!(t4.contains("Défaut : Go"), "{t4}");
    assert!(
        !t4.contains("langage imposé par l'existant"),
        "un écart observé n'est jamais injecté d'office : {t4}"
    );
    assert!(
        !t4.contains("Rust"),
        "l'exception ne vaut que sous son `quand` : {t4}"
    );

    // Même en citant ses mots, l'écart ne remonte pas dans le rappel automatique.
    let tiers = build_tiers(
        &s,
        "langage imposé par l'existant chez le client",
        &[],
        None,
    )
    .await;
    let t4 = tiers.volatile.clone();
    assert!(!t4.contains("2026-09-12"), "{t4}");
}

/// #58 : le projet actif du workspace de la session compte dans le classement : à
/// texte égal, l'entrée du projet passe devant.
#[tokio::test]
async fn an_entry_of_the_open_project_ranks_first() {
    let (_d, s) = services().await;
    let sess = s
        .sessions
        .create_with(
            penelope_kernel::session::SessionKind::Chat,
            None,
            None,
            Some("/Users/edouard/Code/atlas".into()),
        )
        .await
        .unwrap();
    let sid = sess.id.to_string();
    let key = penelope_memory::recall::project_key(None, "/Users/edouard/Code/atlas");

    let vault = vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("projets.md"),
        format!(
            "# Projets\n\n\
             - Les migrations passent par sqlx. <!-- uid: 01PROJ --> \
               <!-- projet: {key} -->\n\
             - Les migrations passent par sqlx ailleurs. <!-- uid: 01AUTRE -->\n"
        ),
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();

    let ctx = current_context(&s, "comment fait-on les migrations ?", Some(&sid)).await;
    assert_eq!(
        ctx.active_projects,
        vec![key.clone()],
        "projet actif du tour"
    );

    let hits = s
        .memory
        .search(
            "migrations sqlx",
            None,
            &penelope_memory::index::SearchFilter {
                limit: 5,
                automatic: true,
                ..Default::default()
            },
            &ctx.active_projects,
        )
        .await
        .unwrap();
    assert_eq!(
        hits.first().map(|h| h.entry.uid.as_str()),
        Some("01PROJ"),
        "l'entrée du projet ouvert passe devant : {hits:?}"
    );
}

/// #58 : l'index donne aux sections d'une pratique leur vrai type.
#[tokio::test]
async fn practice_sections_are_indexed_with_their_own_type() {
    let (_d, s) = services().await;
    let vault = vault_dir(&s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(vault.join("pratiques/langage-backend.md"), PRATIQUE).unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();

    let types = s
        .memory
        .store()
        .read(|c| {
            let mut st = c.prepare("SELECT uid, etype FROM mem_entries ORDER BY uid")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .unwrap();
    let etype = |uid: &str| {
        types
            .iter()
            .find(|(u, _)| u == uid)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    };
    assert_eq!(etype("01J9A"), "fait", "{types:?}");
    assert_eq!(etype("01J9B"), "exception", "{types:?}");
    assert_eq!(etype("01J9C"), "ecart", "{types:?}");
}

/// #55 : après une compaction, la projection ne relit plus les messages couverts par
/// un résumé, et la queue ne lit que ses dernières entrées.
#[tokio::test]
async fn the_projection_only_reads_what_is_not_summarised() {
    let (_d, s) = services().await;
    let sid = session(&s).await;
    let tiers = build_tiers(&s, "bonjour", &[], None).await;
    let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
    for i in 0..200 {
        conv.record(&ChatMessage::user(format!("question {i}")), false)
            .await
            .unwrap();
        conv.record(&ChatMessage::assistant(format!("réponse {i}")), false)
            .await
            .unwrap();
    }
    // Un résumé couvre les 300 premières entrées.
    s.context
        .lcm
        .insert_leaf(&sid, 1, 300, "résumé des débuts", &[], 1_000, 40)
        .await
        .unwrap();
    s.context
        .history
        .mark_compacted(&sid, 1, 300)
        .await
        .unwrap();

    let msgs = conv.request_messages().await.unwrap();
    let joined: String = msgs.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n");
    assert!(
        joined.contains("résumé des débuts"),
        "le résumé est projeté"
    );
    assert!(
        !joined.contains("question 10\n") && !joined.contains("question 100"),
        "les messages couverts ne sont pas relus"
    );
    assert!(joined.contains("question 199"), "la suite est là");

    let tail = conv.tail().await.unwrap();
    assert_eq!(tail.len(), TAIL_ENTRIES, "la queue est bornée");
    assert_eq!(tail.last().unwrap().text(), "réponse 199");
    assert!(
        tail.first().unwrap().text().contains("question 1"),
        "dans l'ordre : {}",
        tail.first().unwrap().text()
    );
}

/// #52 : cinq résultats d'outils parallèles de 20 k tokens chacun ne passent pas
/// entiers parce qu'aucun ne dépasse le seuil à lui seul : le groupe est admis sous le
/// budget, têtes et queues gardées, le reste lisible en artefact.
#[tokio::test]
async fn parallel_tool_results_are_admitted_as_one_group() {
    let (_d, s) = services().await;
    let sid = session(&s).await;
    let tiers = build_tiers(&s, "lis ces fichiers", &[], None).await;
    let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
    let params = conv.params();
    let budget = params.tool_group_budget();

    // ~20 k tokens chacun : sous le seuil d'un résultat isolé, cinq fois trop à cinq.
    let body = "donnée ".repeat(12_000);
    for i in 0..5 {
        conv.record(
            &ChatMessage::tool_result(format!("c{i}"), "fs_read", &body),
            false,
        )
        .await
        .unwrap();
    }
    conv.admit_tool_results(5).await.unwrap();

    let entries = s.context.history.load(&sid, 0).await.unwrap();
    let tools: Vec<_> = entries
        .iter()
        .filter(|e| e.message.role == Role::Tool)
        .collect();
    assert_eq!(tools.len(), 5);
    let externalised = tools.iter().filter(|e| e.artifact_id.is_some()).count();
    assert!(externalised > 0, "le groupe doit être admis sous budget");
    let total: u64 = tools
        .iter()
        .map(|e| {
            s.context
                .estimator
                .text_tokens("openrouter:mock/model", &e.message.text())
        })
        .sum();
    // La découpe se fait en caractères (~3,6 par token) : on vise le budget à 20 %
    // près, loin des 100 k tokens qui entraient avant.
    assert!(
        total <= budget + budget / 5,
        "{total} tokens admis pour un budget de {budget}"
    );
    let first = tools.iter().find(|e| e.artifact_id.is_some()).unwrap();
    assert!(first.message.text().contains("artifact_read"));
    let id = first.artifact_id.clone().unwrap();
    let (chunk, _, _) = s
        .context
        .history
        .read_artifact(&id, 0, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chunk.len(), 50, "le contenu complet reste lisible");

    // Idempotent : une seconde passe ne réexternalise rien.
    conv.admit_tool_results(5).await.unwrap();
    let after = s.context.history.load(&sid, 0).await.unwrap();
    let again = after
        .iter()
        .filter(|e| e.message.role == Role::Tool && e.artifact_id.is_some())
        .count();
    assert_eq!(again, externalised, "admission déjà faite, rien ne bouge");
}

/// #52 : un résultat seul, même gros, reste entier tant qu'il tient dans le budget.
#[tokio::test]
async fn a_lone_tool_result_is_left_whole() {
    let (_d, s) = services().await;
    let sid = session(&s).await;
    let tiers = build_tiers(&s, "lis ce fichier", &[], None).await;
    let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
    conv.record(
        &ChatMessage::tool_result("c1", "fs_read", "donnée ".repeat(12_000)),
        false,
    )
    .await
    .unwrap();
    conv.admit_tool_results(1).await.unwrap();
    let entries = s.context.history.load(&sid, 0).await.unwrap();
    assert!(
        entries.iter().all(|e| e.artifact_id.is_none()),
        "un résultat isolé sous le seuil reste entier"
    );
}

#[tokio::test]
async fn recorded_messages_come_back_in_the_request() {
    let (_d, s) = services().await;
    let sid = session(&s).await;
    let tiers = build_tiers(&s, "bonjour", &[], None).await;
    let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);

    conv.record(&ChatMessage::user("bonjour"), false)
        .await
        .unwrap();
    conv.record(&ChatMessage::assistant("salut !"), false)
        .await
        .unwrap();

    let msgs = conv.request_messages().await.unwrap();
    assert_eq!(msgs[0].role, Role::System, "le préfixe vient en tête");
    assert!(msgs[0].text().contains("Tu es Pénélope"));
    let texts: Vec<String> = msgs.iter().map(|m| m.text()).collect();
    assert!(texts.iter().any(|t| t.ends_with("bonjour")));
    assert!(texts.contains(&"salut !".to_string()));
    // T4 en tête du dernier message utilisateur : la date locale.
    let last_user = msgs.iter().rev().find(|m| m.role == Role::User).unwrap();
    assert!(last_user.text().starts_with("<contexte>"));
    assert!(last_user.text().contains("Date et heure"));

    // Relu depuis la base, par une autre instance : rien n'était en mémoire.
    let tiers = build_tiers(&s, "", &[], None).await;
    let again = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
    assert_eq!(again.tail().await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_stable_prefix_does_not_move_between_turns() {
    let (_d, s) = services().await;
    let a = build_tiers(&s, "premier message", &[], None).await;
    let b = build_tiers(&s, "second message, tout autre", &[], None).await;
    assert_eq!(a.prefix_hash(), b.prefix_hash());
}

#[tokio::test]
async fn soul_and_mcp_servers_enter_the_prefix() {
    let (_d, s) = services().await;
    let vault = vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("SOUL.md"), "Tu tutoies ton propriétaire.").unwrap();
    let t = build_tiers(&s, "", &["forge : 12 outils".into()], None).await;
    assert!(t.identity.contains("Tu tutoies"));
    assert!(t.index.contains("forge : 12 outils"));
    assert!(t.index.contains("tool_search"));
}

#[tokio::test]
async fn huge_tool_results_are_externalised() {
    let (_d, s) = services().await;
    let sid = session(&s).await;
    let tiers = build_tiers(&s, "", &[], None).await;
    let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
    let big = "ligne de journal très bavarde\n".repeat(20_000);
    conv.record(
        &ChatMessage::tool_result("c1", "shell_exec", big.clone()),
        false,
    )
    .await
    .unwrap();
    let stored = s.context.history.load(&sid, 0).await.unwrap();
    assert!(
        stored[0].artifact_id.is_some(),
        "le corps doit partir en artefact"
    );
    assert!(stored[0].message.text().len() < big.len() / 4);
}
