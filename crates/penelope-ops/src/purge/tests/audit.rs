use super::*;

/// #46 : après la purge, plus un mot du transcript dans la base, les fichiers sont
/// partis, la chaîne d'audit tient toujours.
#[tokio::test]
async fn purging_a_session_leaves_the_audit_chain_and_nothing_else() {
    let (dir, s, _clock) = services().await;
    let sid = chat(&s).await;
    s.sessions.bind_telegram(&sid, 4242, None).await.unwrap();
    const SECRET: &str = "Zéphyrine";

    // Un transcript, son contexte figé, un résumé, un artefact avec son fichier.
    let h = &s.context.history;
    h.append(
        &sid,
        &ChatMessage::user(format!("ma carte est au nom de {SECRET}")),
        10,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    h.append(&sid, &ChatMessage::assistant("noté"), 5, 0, false, None)
        .await
        .unwrap();
    h.freeze_context(&sid, 1, SECRET).await.unwrap();
    let art = h
        .put_artifact(Some(&sid), None, "text", None, &format!("dossier {SECRET}"))
        .await
        .unwrap();
    let media = s.platform.dirs.data().join("media").join("voice");
    std::fs::create_dir_all(&media).unwrap();
    let vocal = media.join("01J.ogg");
    std::fs::write(&vocal, b"OggS").unwrap();
    h.append(
        &sid,
        &ChatMessage::user(format!("{} (vocal)", vocal.display())),
        5,
        0,
        false,
        None,
    )
    .await
    .unwrap();

    // Une trace dans chacune des autres tables.
    let sid2 = sid.clone();
    let (secret_owned, path_owned) = (SECRET.to_string(), art.id.clone());
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO lcm_nodes(id, session_id, kind, level, summary, created_at)
                 VALUES('n1',?1,'condensed',1,?2,'2026-01-01T00:00:00Z')",
                penelope_store::rusqlite::params![sid2, format!("résumé {secret_owned}")],
            )?;
            tx.execute(
                "INSERT INTO llm_requests(id, session_id, model, provider, state, body_hash,
                    created_at, updated_at, system_hash)
                 VALUES('r1',?1,'m','p','completed','h','2026-01-01T00:00:00Z',
                    '2026-01-01T00:00:00Z','h_prompt')",
                penelope_store::rusqlite::params![sid2],
            )?;
            // #205 : le prompt système rendu, gardé sous son empreinte.
            tx.execute(
                "INSERT INTO prompt_snapshots(hash, rendered, first_seen_at, last_seen_at,
                    uses)
                 VALUES('h_prompt',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',1)",
                [format!("profil du propriétaire : {secret_owned}")],
            )?;
            tx.execute(
                "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                 VALUES(77,'2026-06-01T00:00:00Z',1,?1)",
                [format!(
                    r#"{{"update_id":77,"message":{{"chat":{{"id":4242}},"text":"{secret_owned}"}}}}"#
                )],
            )?;
            tx.execute(
                "INSERT INTO mem_candidates(id, ctype, text, importance, origin, session_id,
                    session_kind, observed_at, day, state)
                 VALUES('c1','fait',?2,5,'owner',?1,'chat','2026-06-01T00:00:00Z',
                    '2026-06-01','new')",
                penelope_store::rusqlite::params![sid2, format!("{secret_owned} paie comptant")],
            )?;
            tx.execute(
                "UPDATE artifacts SET path = ?2 WHERE id = ?1",
                penelope_store::rusqlite::params![path_owned, "art.txt"],
            )?;
            // #78 : réponse partie sur Telegram, demande d'approbation, tâche MCP,
            // run de workflow et sa journalisation d'étape.
            let dit = format!(r#"{{"text":"{secret_owned}"}}"#);
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                 VALUES('o1',4242,'sendMessage',?1,'sent','2026-06-01T00:00:00Z')",
                [&dit],
            )?;
            tx.execute(
                "INSERT INTO approval_requests(id, kind, subject, risk, payload, session_id,
                    created_at, expires_at, state)
                 VALUES('a1','tool_call','fs_write','write',?2,?1,'2026-06-01T00:00:00Z',
                    '2026-06-02T00:00:00Z','pending')",
                penelope_store::rusqlite::params![sid2, dit],
            )?;
            tx.execute(
                "INSERT INTO workflow_runs(id, workflow_id, session_id, params, state,
                    step_outputs, started_at, updated_at, finished_at)
                 VALUES('wr1','demo',?1,?2,'done',?2,'2026-06-01T00:00:00Z',
                    '2026-06-01T00:00:00Z','2026-06-01T00:00:00Z')",
                penelope_store::rusqlite::params![sid2, dit],
            )?;
            tx.execute(
                "INSERT INTO workflow_step_log(run_id, step_id, started_at, output)
                 VALUES('wr1','un','2026-06-01T00:00:00Z',?1)",
                [&dit],
            )?;
            tx.execute(
                "INSERT INTO mcp_tasks(id, server, task_ref, run_id, request, state, result,
                    created_at, updated_at)
                 VALUES('mt1','forge','t','wr1',?1,'completed',?1,'2026-06-01T00:00:00Z',
                    '2026-06-01T00:00:00Z')",
                [&dit],
            )?;
            // #204 : un job d'outil de la session porte la commande du propriétaire.
            tx.execute(
                "INSERT INTO tool_jobs(id, session_id, tool, request, state, result,
                    created_at, updated_at)
                 VALUES('tj1',?1,'shell_exec',?2,'completed',?2,'2026-06-01T00:00:00Z',
                    '2026-06-01T00:00:00Z')",
                penelope_store::rusqlite::params![sid2, dit],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Un effet mené à terme dans la session : arguments et résultat portent le mot.
    let effet = || {
        penelope_kernel::effects::EffectSpec::new(
            penelope_kernel::effects::EffectKind::Tool,
            "fs_write",
            serde_json::json!({"path": "notes.md", "content": SECRET}),
        )
        .session(&sid)
    };
    let eid = match s.effects.plan(effet()).await.unwrap() {
        penelope_kernel::effects::Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    s.effects.dispatching(&eid).await.unwrap();
    s.effects
        .complete(&eid, serde_json::json!({"written": SECRET}))
        .await
        .unwrap();
    let art_file = s.platform.dirs.artifacts().join("art.txt");
    std::fs::create_dir_all(s.platform.dirs.artifacts()).unwrap();
    std::fs::write(&art_file, format!("dossier {SECRET}")).unwrap();
    s.turns
        .enqueue(
            &sid,
            penelope_kernel::turn::TurnKind::Message,
            serde_json::json!({"text": format!("ma carte est au nom de {SECRET}")}),
            None,
            0,
        )
        .await
        .unwrap();
    s.events
        .append(
            penelope_kernel::event::EventDraft::new(
                "message.received",
                serde_json::json!({"text": SECRET}),
            )
            .session(&sid),
        )
        .await
        .unwrap();

    assert!(!word_is_gone(&s, SECRET).await, "le mot doit être là avant");
    // T17 : le journal de la conversation (`conv.*`) porte le mot, il doit partir.
    assert!(
        conv_payloads_with(&s, SECRET).await > 0,
        "conv.user le porte"
    );
    assert!(!h.grep(SECRET, None, 10).await.unwrap().is_empty());

    let report = session(&s, &sid, "essai").await.unwrap();
    assert!(report["events"].as_u64().unwrap() > 0);
    assert_eq!(report["files"], 2, "vocal et artefact effacés : {report}");

    assert!(
        word_is_gone(&s, SECRET).await,
        "purge incomplète : {report}"
    );
    assert_eq!(conv_payloads_with(&s, SECRET).await, 0);
    // T17 : refondue depuis son journal purgé, la session n'a plus de surface.
    s.context.history.reindex(Some(&sid)).await.unwrap();
    assert_eq!(history_verify(&s, Some(&sid)).await["ok"], true);
    assert!(h.load(&sid, 0).await.unwrap().is_empty());
    assert!(word_is_gone(&s, SECRET).await, "la refonte ne ramène rien");
    assert!(h.grep(SECRET, None, 10).await.unwrap().is_empty());
    // L'idempotence survit : l'effet purgé est rejoué, jamais ré-exécuté.
    assert!(matches!(
        s.effects.plan(effet()).await.unwrap(),
        penelope_kernel::effects::Planned::Replayed(_)
    ));
    // La demande en attente de la session ne peut plus être décidée.
    assert!(s.approvals.pending(10).await.unwrap().is_empty());
    assert!(!vocal.exists());
    assert!(!art_file.exists());
    // La chaîne d'audit tient : les lignes sont là, leurs hash d'origine aussi.
    let verified = s.events.verify().await.unwrap();
    assert!(verified.ok, "chaîne rompue : {verified:?}");
    drop(dir);
}

/// #46 : le payload d'un update traité est vidé, mais son `update_id` reste : rejoué,
/// il n'est pas traité deux fois.
#[tokio::test]
async fn a_replayed_update_is_still_deduplicated_without_its_payload() {
    let (_dir, s, _clock) = services().await;
    s.store
        .write(|tx| {
            tx.execute(
                "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                 VALUES(12,'2026-06-01T00:00:00Z',1,'{}')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let already: bool = s
        .store
        .write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO tg_updates(update_id, received_at, processed, payload)
                 VALUES(12,'2026-06-02T00:00:00Z',0,'{\"update_id\":12}')",
                [],
            )?;
            let processed: i64 = tx.query_row(
                "SELECT processed FROM tg_updates WHERE update_id = 12",
                [],
                |r| r.get(0),
            )?;
            Ok(processed == 1)
        })
        .await
        .unwrap();
    assert!(already, "un update rejoué reste dédoublonné sans payload");
}
