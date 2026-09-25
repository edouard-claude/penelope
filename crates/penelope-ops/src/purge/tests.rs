use super::*;
use penelope_kernel::session::SessionKind;
use penelope_llm::types::ChatMessage;
use std::sync::Arc;

async fn services() -> (
    tempfile::TempDir,
    Arc<Services>,
    penelope_kernel::clock::TestClock,
) {
    let dir = tempfile::tempdir().unwrap();
    let test_clock = penelope_kernel::clock::TestClock::default();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(test_clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (dir, s, test_clock)
}

/// Une session de conversation, comme celle qu'ouvre la CLI.
async fn chat(s: &Services) -> String {
    let sess = s
        .sessions
        .create(SessionKind::Chat, Some("CLI".into()))
        .await
        .unwrap();
    sess.id.to_string()
}

/// `history verify` : chaque session (ou `session`) dérivée du journal, comparée à ses
/// caches ; le rapport JSON de la méthode RPC.
async fn history_verify(s: &Services, session: Option<&str>) -> serde_json::Value {
    let report = s.context.history.verify(session, None).await.unwrap();
    serde_json::to_value(report).unwrap()
}

/// #148 : les lignes déjà en file sont repassées au rédacteur, une seule fois. Le
/// `Grant` du 20/09 y était en hexadécimal, forme que `redact` ignorait.
#[tokio::test]
async fn messages_already_queued_are_redacted_again() {
    let (_dir, s, _clock) = services().await;
    let grant_hex = "7b22616363657373".repeat(200);
    let payload = format!(
        r#"{{"chat_id":1,"text":"❌ Connexion abandonnée : security: unknown command \"{grant_hex}"}}"#
    );
    let now = s.clock.now_rfc3339();
    let p2 = payload.clone();
    s.store
        .write(move |tx| {
            for (id, body) in [
                ("o_1", p2.as_str()),
                ("o_2", r#"{"chat_id":1,"text":"bonjour"}"#),
            ] {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, attempts,
                        not_before, created_at)
                     VALUES(?1, 1, 'sendMessage', ?2, 'sent', 0, ?3, ?3)",
                    params![id, body, now],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

    assert!(
        reredact_outbox(&s).await.unwrap() >= 1,
        "la ligne fautive est réécrite"
    );
    let rows: Vec<String> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT payload FROM tg_outbox ORDER BY id")?;
            let r = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(r.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    // Ce sont **ces deux lignes** qui comptent, jamais un total : le rédacteur est un
    // état de processus, qu'un autre test du même binaire alimente (issue #151). Les
    // attentes passent donc par `redact` elles aussi : quoi qu'il ait appris, une
    // ligne en file doit valoir exactement ce que le rédacteur en fait.
    let ordinaire = r#"{"chat_id":1,"text":"bonjour"}"#;
    assert!(
        !rows[0].contains("7b22616363657373"),
        "hexadécimal resté en file : {}",
        rows[0]
    );
    assert_eq!(rows[0], penelope_observe::redact(&payload));
    assert_eq!(
        rows[1],
        penelope_observe::redact(ordinaire),
        "un message ordinaire n'est pas réécrit autrement que par le rédacteur"
    );

    // Une seule fois : la passe suivante ne relit rien.
    assert_eq!(reredact_outbox(&s).await.unwrap(), 0);
}

/// #205 : un prompt système contient le profil et la mémoire rappelée. La purge
/// d'une session emporte les instantanés qu'elle seule référençait, garde ceux
/// qu'une autre session utilise encore, et coupe le renvoi depuis `usage` — la
/// ligne comptable reste, la clé du texte part.
#[tokio::test]
async fn purging_a_session_takes_the_prompts_only_it_used() {
    let (_dir, s, _clock) = services().await;
    let mine = chat(&s).await;
    let other = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .0;
    let (a, b) = (mine.clone(), other.clone());
    s.store
        .write(move |tx| {
            for (h, text) in [("h_seul", "profil : Édouard"), ("h_partage", "règles")] {
                tx.execute(
                    "INSERT INTO prompt_snapshots(hash, rendered, first_seen_at,
                        last_seen_at, uses)
                     VALUES(?1,?2,'2026-06-01T00:00:00Z','2026-06-01T00:00:00Z',1)",
                    params![h, text],
                )?;
            }
            for (sid, h) in [(&a, "h_seul"), (&a, "h_partage"), (&b, "h_partage")] {
                tx.execute(
                    "INSERT INTO usage(ts, day, session_id, model, provider, prompt,
                        completion, cost_usd, system_hash)
                     VALUES('2026-06-01T00:00:00Z','2026-06-01',?1,'m','p',10,1,0.1,?2)",
                    params![sid, h],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

    session(&s, &mine, "essai").await.unwrap();

    let (kept, orphan, still_pointed) = s
        .store
        .read(move |c| {
            let kept: i64 = c.query_row(
                "SELECT count(*) FROM prompt_snapshots WHERE hash = 'h_partage'",
                [],
                |r| r.get(0),
            )?;
            let orphan: i64 = c.query_row(
                "SELECT count(*) FROM prompt_snapshots WHERE hash = 'h_seul'",
                [],
                |r| r.get(0),
            )?;
            let still: i64 = c.query_row(
                "SELECT count(*) FROM usage WHERE session_id = ?1 AND system_hash IS NOT NULL",
                [&mine],
                |r| r.get(0),
            )?;
            Ok((kept, orphan, still))
        })
        .await
        .unwrap();
    assert_eq!(orphan, 0, "l'instantané propre à la session est parti");
    assert_eq!(kept, 1, "celui qu'une autre session lit encore reste");
    assert_eq!(still_pointed, 0, "usage ne pointe plus vers le texte");
    let lines: i64 = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM usage WHERE session_id = ?1",
                [&other],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(lines, 1, "la comptabilité de l'autre session est intacte");
}

/// #205 : la rétention n'efface qu'un instantané que plus personne ne cite.
#[tokio::test]
async fn retention_only_drops_prompts_nothing_points_to() {
    let (_dir, s, clock) = services().await;
    clock.set_ms(
        chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .timestamp_millis(),
    );
    s.store
        .write(|tx| {
            for h in ["h_cite", "h_orphelin", "h_recent"] {
                let ts = if h == "h_recent" {
                    "2026-08-31T00:00:00Z"
                } else {
                    "2026-01-01T00:00:00Z"
                };
                tx.execute(
                    "INSERT INTO prompt_snapshots(hash, rendered, first_seen_at,
                        last_seen_at, uses)
                     VALUES(?1,'texte',?2,?2,1)",
                    params![h, ts],
                )?;
            }
            tx.execute(
                "INSERT INTO usage(ts, day, session_id, model, provider, prompt,
                    completion, cost_usd, system_hash)
                 VALUES('2026-01-01T00:00:00Z','2026-01-01','s1','m','p',10,1,0.1,'h_cite')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let report = retention(&s).await.unwrap();
    assert_eq!(report["prompt_snapshots"], 1, "{report}");
    let left: Vec<String> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT hash FROM prompt_snapshots ORDER BY hash")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            Ok(v)
        })
        .await
        .unwrap();
    assert_eq!(left, vec!["h_cite".to_string(), "h_recent".to_string()]);
}

/// Le mot du transcript ne doit plus exister nulle part : neuf tables, l'index plein
/// texte et les fichiers.
async fn word_is_gone(s: &Services, word: &str) -> bool {
    let w = format!("%{word}%");
    s.store
        .read(move |c| {
            let tables = [
                ("messages", "content"),
                ("message_context", "context"),
                ("lcm_nodes", "summary"),
                ("artifacts", "head"),
                ("prompt_snapshots", "rendered"),
                ("turn_queue", "payload"),
                ("tg_updates", "payload"),
                ("mem_candidates", "text"),
                ("sessions", "title"),
                ("events", "payload"),
                // #78 : ce que l'agent a fait et dit.
                ("effects", "request"),
                ("effects", "result"),
                ("tg_outbox", "payload"),
                ("approval_requests", "payload"),
                ("mcp_tasks", "request"),
                ("mcp_tasks", "result"),
                // #204 : les arguments et le résultat d'un job d'outil.
                ("tool_jobs", "request"),
                ("tool_jobs", "result"),
                ("workflow_step_log", "output"),
                ("workflow_runs", "params"),
                ("workflow_runs", "step_outputs"),
            ];
            for (table, column) in tables {
                let n: i64 = c.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {column} LIKE ?1"),
                    [&w],
                    |r| r.get(0),
                )?;
                if n > 0 {
                    return Ok(false);
                }
            }
            let fts: i64 = c.query_row(
                "SELECT count(*) FROM messages_fts WHERE content LIKE ?1",
                [&w],
                |r| r.get(0),
            )?;
            Ok(fts == 0)
        })
        .await
        .unwrap()
}

/// Payloads `conv.*` du journal qui contiennent le mot.
async fn conv_payloads_with(s: &Services, word: &str) -> i64 {
    let w = format!("%{word}%");
    s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM events WHERE kind LIKE 'conv.%' AND payload LIKE ?1",
                [&w],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

/// T17 : purger une session dont une autre est née d'un fork le dit (la fille perd le
/// préfixe qu'elle lit dans le journal de sa mère) ; la fille se relit sans ce préfixe.
#[tokio::test]
async fn purging_a_forked_session_warns_about_its_forks() {
    let (_dir, s, _clock) = services().await;
    let sid = chat(&s).await;
    let h = &s.context.history;
    h.append(&sid, &ChatMessage::user("Quetzal"), 5, 0, false, None)
        .await
        .unwrap();
    h.append(&sid, &ChatMessage::assistant("noté"), 5, 0, false, None)
        .await
        .unwrap();
    let forked = crate::session_ops::fork(&s, &sid, None).await.unwrap();
    let child = forked["session"].as_str().unwrap().to_string();
    // La lecture préalable (méthode RPC `session.purge_preview`) : le fork nommé, rien
    // d'effacé.
    let preview = preview(&s, &sid).await.expect("lecture préalable");
    assert_eq!(preview["forks"][0]["id"], child, "{preview}");
    let warning = preview["avertissement"].as_str().unwrap();
    assert!(
        warning.starts_with("Cette session a un fork, il perdra son début : "),
        "{warning}"
    );
    assert!(warning.contains(&child), "{warning}");
    let read = h.read_journal(&child).await.unwrap().unwrap().unwrap();
    assert!(
        !read.entries.is_empty(),
        "la lecture préalable n'efface rien"
    );
    let report = session(&s, &sid, "essai").await.unwrap();
    assert_eq!(report["forks"], serde_json::json!([child]), "{report}");
    assert!(
        report["avertissement"]
            .as_str()
            .unwrap()
            .contains("il perdra son début"),
        "{report}"
    );
    let read = h.read_journal(&child).await.unwrap().unwrap().unwrap();
    assert!(read.entries.is_empty(), "le préfixe hérité est parti");
    let alone = session(&s, &child, "essai").await.unwrap();
    assert!(alone.get("forks").is_none(), "{alone}");
}

/// Arbitrage 3 : le nombre de forks en toutes lettres jusqu'à dix, en chiffres au-delà.
#[test]
fn the_fork_warning_counts_in_words_up_to_ten() {
    let ids = |n: usize| (1..=n).map(|i| format!("s_{i}")).collect::<Vec<_>>();
    assert_eq!(
        forks_warning(&ids(1)),
        "Cette session a un fork, il perdra son début : s_1."
    );
    assert!(
        forks_warning(&ids(2))
            .starts_with("Cette session a deux forks, ils perdront leur début : s_1, s_2")
    );
    assert!(forks_warning(&ids(10)).starts_with("Cette session a dix forks,"));
    assert!(forks_warning(&ids(11)).starts_with("Cette session a 11 forks,"));
}

/// T17 : la rétention purge le payload des `conv.attempt` anciens (texte partiel
/// compris), garde les récents ; `history verify` reste à zéro, `audit verify` ok.
#[tokio::test]
async fn retention_purges_old_attempts_by_payload() {
    use penelope_context::journal::{AttemptCause, AttemptPayload, ConvEvent};
    let (_dir, s, clock) = services().await;
    let sid = chat(&s).await;
    let h = &s.context.history;
    let attempt = |text: &str| {
        let e = ConvEvent::Attempt(AttemptPayload {
            turn: None,
            step: 1,
            cause: AttemptCause::StreamCut,
            model: None,
            provider: None,
            upstream: None,
            error: None,
            partial_text: Some(text.to_string()),
            partial_reasoning: None,
            usage: None,
            cost_usd: None,
            llm_request_id: None,
            retry_prompt: None,
        });
        EventDraft::new(e.kind(), e.payload()).session(&sid)
    };
    h.append(&sid, &ChatMessage::user("question"), 5, 0, false, None)
        .await
        .unwrap();
    s.events.append(attempt("brouillon ancien")).await.unwrap();
    h.append(&sid, &ChatMessage::assistant("réponse"), 5, 0, false, None)
        .await
        .unwrap();
    clock.advance_ms(91 * 86_400_000);
    s.events.append(attempt("brouillon récent")).await.unwrap();

    let report = retention(&s).await.unwrap();
    assert_eq!(report["conv_attempts"], 1, "{report}");
    assert_eq!(conv_payloads_with(&s, "brouillon ancien").await, 0);
    assert_eq!(conv_payloads_with(&s, "brouillon récent").await, 1);
    let again = retention(&s).await.unwrap();
    assert_eq!(again["conv_attempts"], 0, "idempotent");
    let verified = s.events.verify().await.unwrap();
    assert!(verified.ok, "chaîne rompue : {verified:?}");
    let v = history_verify(&s, None).await;
    assert_eq!(v["ok"], true, "{v}");
    let read = h.read_journal(&sid).await.unwrap().unwrap().unwrap();
    assert_eq!(read.entries.len(), 2, "la surface ne perd rien");
}

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

/// #46 : la rétention efface ce qui a passé l'âge, et rien d'autre.
#[tokio::test]
async fn retention_removes_what_is_past_its_age_only() {
    let (_dir, s, clock) = services().await;
    // L'horloge de test démarre au 1er janvier 2026 : on avance de 91 jours et tout ce
    // qui date du jour 1 a dépassé les 90 jours de rétention.
    let vieux = "2026-01-01T00:00:00Z";
    clock.advance_ms(91 * 86_400_000);
    s.store
        .write(move |tx| {
            let p = penelope_store::rusqlite::params![vieux];
            tx.execute(
                "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at,
                    finished_at) VALUES('t_vieux','s','message','{}','done',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at)
                 VALUES('t_attente','s','message','{}','pending',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO llm_requests(id, session_id, model, provider, state, body_hash,
                    created_at, updated_at)
                 VALUES('r_vieux','s','m','p','completed','h',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                 VALUES(1,?1,1,'{\"text\":\"salut\"}')",
                p,
            )?;
            tx.execute(
                "INSERT INTO mem_history(uid, file, op, before, after, ts)
                 VALUES('u1','memoire.md','update','avant','apres',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO kv(k, v, ts) VALUES('turn.recorded.x','1',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO kv(k, v, ts) VALUES('upgrade.state','{}',?1)",
                p,
            )?;
            // #78 : un effet tranché perd son contenu, un effet incertain le garde.
            tx.execute(
                "INSERT INTO effects(id, session_id, idem_key, kind, tool, request, state,
                    result, created_at, updated_at)
                 VALUES('e_fait','s','k1','tool','fs_write','{\"c\":1}','completed',
                    '{\"ok\":true}',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO effects(id, session_id, idem_key, kind, tool, request, state,
                    created_at, updated_at)
                 VALUES('e_doute','s','k2','tool','git_push','{\"b\":1}','unknown',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                 VALUES('o_vieux',1,'sendMessage','{}','sent',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                 VALUES('o_attente',1,'sendMessage','{}','pending',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO approval_requests(id, kind, subject, risk, payload, created_at,
                    expires_at, state, decided_at)
                 VALUES('a_vieille','tool_call','x','write','{\"a\":1}',?1,?1,'approved',?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO mcp_tasks(id, server, task_ref, request, state, created_at,
                    updated_at) VALUES('mt_vieille','f','t','{}','completed',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO tool_jobs(id, session_id, tool, request, state, created_at,
                    updated_at)
                 VALUES('tj_vieux','s_1','shell_exec','{}','completed',?1,?1)",
                p,
            )?;
            tx.execute(
                "INSERT INTO tool_jobs(id, session_id, tool, request, state, created_at,
                    updated_at)
                 VALUES('tj_en_cours','s_1','shell_exec','{}','working',?1,?1)",
                p,
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let report = retention(&s).await.unwrap();
    assert_eq!(report["effects"], 1, "{report}");
    assert_eq!(report["tg_outbox"], 1);
    assert_eq!(report["approvals"], 1);
    assert_eq!(report["mcp_tasks"], 1);
    // #204 : un job terminé et vieux part, un job encore en cours reste.
    assert_eq!(report["tool_jobs"], 1, "{report}");
    let reste: i64 = s
        .store
        .read(|c| Ok(c.query_row("SELECT count(*) FROM tool_jobs", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(reste, 1, "le job en cours n'est pas ramassé");
    let (fait, doute, attente): (Option<String>, String, i64) = s
        .store
        .read(|c| {
            Ok((
                c.query_row("SELECT result FROM effects WHERE id='e_fait'", [], |r| {
                    r.get(0)
                })?,
                c.query_row("SELECT request FROM effects WHERE id='e_doute'", [], |r| {
                    r.get(0)
                })?,
                c.query_row(
                    "SELECT count(*) FROM tg_outbox WHERE id='o_attente'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(fait, None, "le résultat d'un effet tranché part");
    assert_eq!(doute, r#"{"b":1}"#, "un effet incertain garde tout");
    assert_eq!(attente, 1, "un message à envoyer reste");
    assert_eq!(report["turns"], 1, "{report}");
    assert_eq!(report["llm_requests"], 1);
    assert_eq!(report["tg_updates"], 1);
    assert_eq!(report["mem_history"], 1);
    assert_eq!(report["kv"], 1);

    let (turns, keys): (i64, i64) = s
        .store
        .read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM turn_queue", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM kv WHERE k='upgrade.state'", [], |r| {
                    r.get(0)
                })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(turns, 1, "le tour en attente reste");
    assert_eq!(keys, 1, "une clé durable n'est pas balayée");
}

#[test]
fn media_paths_are_read_out_of_a_message() {
    let root = "/data/media";
    let content = r#"[{"type":"text","text":"voici"},
        {"type":"image","path":"/data/media/photos/01J.jpg"},
        {"type":"audio","path":"/data/media/voice/01K.ogg"}]"#;
    assert_eq!(
        media_paths(content, root),
        vec!["/data/media/photos/01J.jpg", "/data/media/voice/01K.ogg"]
    );
    assert!(media_paths(r#"{"text":"rien"}"#, root).is_empty());
}
