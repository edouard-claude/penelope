use super::*;
use crate::runtime::Daemon;
use penelope_context::derive::derive;
use penelope_context::journal::KIND_IMPORT;
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_llm::types::{ChatMessage, Role};
use penelope_store::rusqlite::params;
use std::sync::Arc;

const PLAIN: &str = "s_v0_simple";
const COMPACTED: &str = "s_v0_compactee";
const EMPTY: &str = "s_v0_vide";

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, Arc::new(s))
}

fn content(text: &str) -> String {
    serde_json::json!({"blocks": [{"type": "text", "text": text}], "tool_calls": []}).to_string()
}

/// La base d'une 0.17 : trois sessions écrites sans journal, dont une compactée (nœud
/// actif sur 1..4, lignes marquées) et une vide.
async fn seed_v0(s: &Services) {
    s.store
        .write(|tx| {
            for sid in [PLAIN, COMPACTED, EMPTY] {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at, state)
                     VALUES(?1, 'chat', '2026-09-01T10:00:00Z', '2026-09-01T10:00:00Z', 'active')",
                    [sid],
                )?;
            }
            let plain = [
                ("user", "bonjour"),
                ("assistant", "salut"),
                ("user", "et ensuite ?"),
            ];
            for (i, (role, text)) in plain.iter().enumerate() {
                tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, tokens_est, ts, episode)
                     VALUES(?1, ?2, ?3, ?4, 3, '2026-09-01T10:00:00Z', 1)",
                    params![PLAIN, i as i64 + 1, role, content(text)],
                )?;
            }
            tx.execute(
                "INSERT INTO message_context(session_id, seq, context) VALUES(?1, 3, '<ctx>')",
                [PLAIN],
            )?;
            for seq in 1..=6_i64 {
                let role = if seq % 2 == 1 { "user" } else { "assistant" };
                tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, tokens_est, ts, episode,
                        compacted)
                     VALUES(?1, ?2, ?3, ?4, 3, '2026-09-02T10:00:00Z', 1, ?5)",
                    params![
                        COMPACTED,
                        seq,
                        role,
                        content(&format!("m{seq}")),
                        (seq <= 4) as i64
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO message_context(session_id, seq, context) VALUES(?1, 5, '<c5>')",
                [COMPACTED],
            )?;
            tx.execute(
                "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary,
                    tokens_self, created_at)
                 VALUES('n_v0', ?1, 'leaf', 0, 1, 4, 'quatre messages résumés', 7,
                    '2026-09-02T10:00:00Z')",
                [COMPACTED],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn imports(s: &Services) -> Vec<String> {
    s.store
        .read(|c| {
            let mut st = c.prepare("SELECT session_id FROM events WHERE kind = ?1 ORDER BY id")?;
            let rows = st.query_map([KIND_IMPORT], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

fn texts(messages: impl IntoIterator<Item = ChatMessage>) -> Vec<String> {
    messages.into_iter().map(|m| m.text()).collect()
}

/// Critère de fin de T11 : premier démarrage, deux `conv.import` (la session vide et la
/// session déjà journalisée n'en reçoivent pas) ; second démarrage, aucun ; la chaîne
/// reste vérifiée.
#[tokio::test]
async fn each_v0_session_is_sealed_once_at_boot() {
    let (_dir, s) = services().await;
    seed_v0(&s).await;
    let v1 = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap();
    s.context
        .history
        .append(
            v1.id.as_str(),
            &ChatMessage::user("déjà journalisé"),
            3,
            0,
            false,
            None,
        )
        .await
        .unwrap();

    let d = Daemon::from_services(s.clone());
    d.recover().await.unwrap();
    assert_eq!(imports(&s).await, [COMPACTED, PLAIN]);
    d.recover().await.unwrap();
    assert_eq!(
        imports(&s).await,
        [COMPACTED, PLAIN],
        "le second démarrage ne scelle rien"
    );
    assert!(seal_legacy(&s).await.unwrap() == SealReport::default());

    let unsealed: i64 = s
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM messages WHERE event_id IS NULL AND sealed = 0",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(unsealed, 0);
    assert!(s.events.verify().await.unwrap().ok);
}

/// Critère de fin de T11 : la dérivation d'une session scellée redonne son préfixe, la
/// projection V0 comprise (résumé actif, puis ce qu'il ne couvre pas, contexte figé en
/// tête du message utilisateur) ; un message écrit après le scellement vient ensuite,
/// à une adresse au-delà de l'`offset`.
#[tokio::test]
async fn a_sealed_session_derives_back_to_its_prefix() {
    let (_dir, s) = services().await;
    seed_v0(&s).await;
    seal_legacy(&s).await.unwrap();
    let history = &s.context.history;

    for sid in [PLAIN, COMPACTED] {
        let (import, prefix) = history.sealed_prefix(sid).await.unwrap().expect("scellée");
        assert_eq!(import.digest, prefix.digest(), "{sid}");
        let events = s.events.session_events(sid, 0).await.unwrap();
        let surface = derive(&prefix.sealed(), &events).unwrap();
        assert_eq!(
            surface.entries(),
            history.load(sid, 0).await.unwrap(),
            "{sid}"
        );
    }

    let (import, prefix) = history.sealed_prefix(COMPACTED).await.unwrap().unwrap();
    assert_eq!((import.messages, import.contexts), (6, 1));
    assert_eq!(import.lcm_active.len(), 1);
    history
        .append(COMPACTED, &ChatMessage::user("après"), 3, 1, false, None)
        .await
        .unwrap();
    let events = s.events.session_events(COMPACTED, 0).await.unwrap();
    let surface = derive(&prefix.sealed(), &events).unwrap();
    assert_eq!(
        texts(surface.projected_entries().into_iter().map(|e| e.message)),
        [
            "Résumé de la conversation antérieure (nœud n_v0) :\nquatre messages résumés",
            "<c5>m5",
            "m6",
            "après"
        ]
    );
    let last = surface.entries().last().cloned().unwrap();
    assert_eq!(last.message.role, Role::User);
    assert!(last.seq > 6, "adresse {} au-delà de l'offset", last.seq);
    assert!(surface.entries()[..4].iter().all(|e| e.compacted));
}

/// Pour `history verify` (T12) : une ligne scellée modifiée change l'empreinte ; une
/// compaction ultérieure du préfixe (drapeau `compacted`) ne la change pas.
#[tokio::test]
async fn the_digest_follows_content_not_compaction() {
    let (_dir, s) = services().await;
    seed_v0(&s).await;
    seal_legacy(&s).await.unwrap();
    let history = &s.context.history;

    history.mark_compacted(PLAIN, 1, 2).await.unwrap();
    let (import, prefix) = history.sealed_prefix(PLAIN).await.unwrap().unwrap();
    assert_eq!(import.digest, prefix.digest());

    s.store
        .write(|tx| {
            tx.execute(
                "UPDATE messages SET content = ?2 WHERE session_id = ?1 AND seq = 2",
                params![PLAIN, content("salut !")],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (import, prefix) = history.sealed_prefix(PLAIN).await.unwrap().unwrap();
    assert_ne!(import.digest, prefix.digest());
}

/// T12 : la base d'une 0.17 scellée, puis un message journalisé dans une session scellée,
/// se vérifie sans divergence ; une ligne falsifiée est nommée par `history.verify` et
/// par la ligne `doctor`.
#[tokio::test]
async fn verify_names_a_tampered_row_through_rpc_and_doctor() {
    let (_dir, s) = services().await;
    seed_v0(&s).await;
    seal_legacy(&s).await.unwrap();
    let history = &s.context.history;
    let seq = history
        .append(PLAIN, &ChatMessage::user("nouveau"), 2, 1, false, None)
        .await
        .unwrap();
    history.freeze_context(PLAIN, seq, "<ctx2>").await.unwrap();
    s.store
        .write(|tx| {
            tx.execute(
                "UPDATE sessions SET updated_at = '2026-01-01T00:00:00Z'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let clean = verify(&s, &serde_json::json!({})).await.unwrap();
    assert_eq!(clean["ok"], true, "{clean:#}");
    assert!(doctor_check(&s).await.ok);

    s.store
        .write(move |tx| {
            tx.execute(
                "UPDATE messages SET content = ?3 WHERE session_id = ?1 AND seq = ?2",
                params![PLAIN, seq, content("falsifié")],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let report = verify(&s, &serde_json::json!({"session": PLAIN}))
        .await
        .unwrap();
    assert_eq!(report["ok"], false);
    let d = &report["divergences"][0];
    assert_eq!(
        (d["session"].as_str(), d["what"].as_str()),
        (Some(PLAIN), Some("content"))
    );
    assert_eq!(d["seq"], seq);
    assert!(
        d["node"].as_i64().unwrap() > 3,
        "adresse après l'offset du scellement"
    );
    let check = doctor_check(&s).await;
    assert!(!check.ok);
    assert!(check.detail.contains(PLAIN), "{}", check.detail);
}

/// T13 : `history.reindex` refait les lignes effacées d'une session scellée, et le
/// rattrapage d'ouverture refait la ligne qu'une seconde transaction n'a pas écrite.
#[tokio::test]
async fn reindex_and_catch_up_rebuild_what_the_journal_says() {
    let (_dir, s) = services().await;
    seed_v0(&s).await;
    seal_legacy(&s).await.unwrap();
    let history = &s.context.history;
    history
        .append(PLAIN, &ChatMessage::user("nouveau"), 2, 1, false, None)
        .await
        .unwrap();
    s.store
        .write(|tx| {
            tx.execute("DELETE FROM messages WHERE sealed = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let report = reindex(&s, &serde_json::json!({"session": PLAIN}))
        .await
        .unwrap();
    assert_eq!(report["ok"], true, "{report:#}");
    assert_eq!(report["sessions"][0]["rows"], 1);
    assert_eq!(
        verify(&s, &serde_json::json!({})).await.unwrap()["ok"],
        true
    );

    // L'événement est commité, sa ligne jamais écrite.
    let lost = penelope_context::journal::message_event(
        &ChatMessage::user("perdu"),
        2,
        1,
        false,
        &penelope_context::journal::Provenance::default(),
    )
    .unwrap();
    s.events
        .append(penelope_kernel::event::EventDraft::new(lost.kind(), lost.payload()).session(PLAIN))
        .await
        .unwrap();
    assert_eq!(
        verify(&s, &serde_json::json!({})).await.unwrap()["ok"],
        false
    );
    catch_up(&s, PLAIN).await;
    assert_eq!(
        verify(&s, &serde_json::json!({})).await.unwrap()["ok"],
        true
    );
    assert!(doctor_check(&s).await.ok);
}
