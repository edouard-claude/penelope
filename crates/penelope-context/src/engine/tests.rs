use super::*;
use crate::tiers::TiersBuilder;
use crate::transcript::pairs_are_valid;
use penelope_kernel::clock::TestClock;
use penelope_llm::types::ToolCall;
use penelope_store::Store;
use serde_json::json;
use std::sync::Arc;

async fn engine() -> ContextEngine {
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    ContextEngine::new(
        HistoryStore::new(store.clone(), clock.clone()),
        Lcm::new(store, clock.clone()),
        TokenEstimator::new(),
        Catalog::new(),
        clock,
    )
}

fn params(window: u64) -> CompactionParams {
    CompactionParams {
        window,
        threshold: 0.70,
        tail_ratio: 0.025,
        tail_min_tokens: 1_000,
        tail_max_tokens: 25_000,
        min_tail_user_messages: 2,
        max_tool_result_share: 0.25,
        large_payload_tokens: 25_000,
        background_margin: 0.10,
        max_prompt_tokens: 0,
    }
}

fn tiers() -> Tiers {
    TiersBuilder::new().soul("Pénélope.").build()
}

fn tool_pair(seq: i64, id: &str, body: &str, tokens: u64) -> Vec<Entry> {
    vec![
        Entry::new(
            seq,
            ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: id.into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }]),
            10,
        ),
        Entry::new(
            seq + 1,
            ChatMessage::tool_result(id, "fs_read", body),
            tokens,
        )
        .eager(true),
    ]
}

/// `ctx-safety` — niveau 0 : les résultats volatils anciens deviennent des stubs,
/// le canonique est intact, les paires restent valides.
#[tokio::test]
async fn ctx_safety_level0() {
    let e = engine().await;
    let big = "x".repeat(200_000);
    let mut entries = vec![Entry::new(1, ChatMessage::user("analyse"), 10)];
    entries.extend(tool_pair(2, "c1", &big, 50_000));
    entries.push(Entry::new(4, ChatMessage::user("et ensuite ?"), 10));
    entries.push(Entry::new(5, ChatMessage::user("alors ?"), 10));

    let ctx = e.build_from_entries(&entries, &tiers(), &params(20_000), "m", false);
    assert!(
        ctx.steps.iter().any(|s| s.level == 0),
        "le niveau 0 doit s'appliquer : {:?}",
        ctx.steps
    );
    assert!(pairs_are_valid(&ctx.messages[1..]));
    assert_eq!(
        entries[2].message.text().len(),
        200_000,
        "le canonique n'est pas modifié"
    );
}

/// `ctx-safety` — niveau 1 : admission sous budget, externalisation en artefact.
#[tokio::test]
async fn ctx_safety_level1() {
    let e = engine().await;
    let big = "y".repeat(500_000);
    e.history
        .append(
            "s1",
            &ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }]),
            10,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    let seq = e
        .history
        .append(
            "s1",
            &ChatMessage::tool_result("c1", "fs_read", &big),
            140_000,
            0,
            false,
            None,
        )
        .await
        .unwrap();

    let steps = e
        .admit_tool_group("s1", &[(seq, big.clone())], &params(100_000), "m")
        .await
        .unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].level, 1);
    assert!(steps[0].after_tokens < steps[0].before_tokens);

    let entries = e.history.load("s1", 0).await.unwrap();
    let body = entries[1].message.text();
    assert!(body.contains("artifact_read"), "{body:.200}");
    assert!(entries[1].artifact_id.is_some());

    // Le contenu complet reste lisible depuis l'artefact.
    let id = entries[1].artifact_id.clone().unwrap();
    let (chunk, _, _) = e.history.read_artifact(&id, 0, 100).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 100);
}

/// `ctx-safety` — niveau 2 : dégradation progressive, queue protégée.
#[tokio::test]
async fn ctx_safety_level2() {
    let e = engine().await;
    let mut entries = Vec::new();
    for i in 0..12 {
        entries.push(Entry::new(
            i * 2 + 1,
            ChatMessage::user(format!("q{i}")),
            20,
        ));
        entries.push(Entry::new(
            i * 2 + 2,
            ChatMessage::tool_result(format!("c{i}"), "t", "z".repeat(8000)),
            2200,
        ));
    }
    // Les résultats sans appel sont réparés ; on vérifie surtout la réduction.
    let ctx = e.build_from_entries(&entries, &tiers(), &params(10_000), "m", false);
    assert!(
        ctx.steps.iter().any(|s| s.level == 2),
        "le niveau 2 doit s'appliquer : {:?}",
        ctx.steps
    );
    assert!(pairs_are_valid(&ctx.messages[1..]));
}

/// `ctx-safety` — niveau 3 : résumé publié une seule fois, même republié.
#[tokio::test]
async fn ctx_safety_level3_publication_is_idempotent() {
    let e = engine().await;
    for i in 0..30 {
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("message {i}")),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
        e.history
            .append(
                "s1",
                &ChatMessage::assistant("réponse"),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    let p = params(40_000);
    let job = e
        .prepare_summary("s1", &p, 128_000, "m", false)
        .await
        .unwrap()
        .expect("un travail de résumé");
    assert!(job.from_seq >= 1 && job.to_seq > job.from_seq);
    assert_eq!(job.batches.len(), 1, "un seul lot suffit au résumeur");

    let validated = json!({
        "objectif": "suivre la conversation",
        "fait": "30 échanges",
        "en_cours": "rien",
        "prochaines_etapes": "continuer"
    });
    let a = e.apply_summary(&job, &validated, "m").await.unwrap();
    let b = e.apply_summary(&job, &validated, "m").await.unwrap();
    assert_eq!(a, b, "republier le même résumé ne crée pas un second nœud");
    assert_eq!(e.lcm.count("s1").await.unwrap(), 1);

    let active = e.active_context("s1", &p).await.unwrap();
    assert!(active[0].text().contains("### Objectif"));
    assert!(pairs_are_valid(&active));
}

/// Niveau 3 incrémental : le résumé suivant **met à jour** le précédent et prolonge
/// sa couverture ; un travail préparé avant cette mise à jour est périmé.
#[tokio::test]
async fn recompaction_extends_the_previous_summary() {
    let e = engine().await;
    let say = |i: usize| ChatMessage::user(format!("demande {i} sur PROJ-{i}"));
    for i in 0..30 {
        e.history
            .append("s1", &say(i), 500, 0, false, None)
            .await
            .unwrap();
    }
    let p = params(40_000);
    let first = e
        .prepare_summary("s1", &p, 128_000, "m", false)
        .await
        .unwrap()
        .unwrap();
    assert!(first.previous_node_id.is_none());
    let summary = json!({"objectif": "suivre", "fait": "première partie"});
    e.apply_summary(&first, &summary, "m").await.unwrap();

    // Rien de neuf au-delà de la queue : pas d'appel au résumeur.
    assert!(
        e.prepare_summary("s1", &p, 128_000, "m", false)
            .await
            .unwrap()
            .is_none()
    );

    for i in 30..60 {
        e.history
            .append("s1", &say(i), 500, 0, false, None)
            .await
            .unwrap();
    }
    let second = e
        .prepare_summary("s1", &p, 128_000, "m", false)
        .await
        .unwrap()
        .expect("un second lot");
    assert!(
        second.previous_node_id.is_some(),
        "le résumé précédent est repris"
    );
    assert_eq!(second.from_seq, first.from_seq);
    assert_eq!(second.chunk_from_seq, first.to_seq + 1);
    assert!(
        second
            .previous_summary
            .as_deref()
            .unwrap()
            .contains("première partie"),
        "le résumeur reçoit le résumé à mettre à jour"
    );
    let prompt = second.summarizer_messages()[1].text();
    assert!(prompt.contains("Résumé précédent"));
    assert!(
        !prompt.contains("verbatim"),
        "seules les sections rédigées sont reprises"
    );
    assert!(
        second.anchors.iter().any(|a| a.value == "PROJ-1"),
        "les ancres du résumé prolongé survivent"
    );
    assert!(
        second
            .anchors
            .iter()
            .any(|a| a.value == format!("PROJ-{}", second.to_seq - 1))
    );

    let node = e
        .apply_summary(&second, &json!({"objectif": "suivre", "fait": "tout"}), "m")
        .await
        .unwrap();
    let active = e.lcm.active_nodes("s1").await.unwrap();
    assert_eq!(active.len(), 1, "un seul résumé vivant");
    assert_eq!(active[0].id, node);
    assert_eq!(
        (active[0].from_seq, active[0].to_seq),
        (Some(first.from_seq), Some(second.to_seq))
    );
    assert_eq!(active[0].tokens_src, first.tokens_src + second.tokens_src);
    let history = e.history.load("s1", 0).await.unwrap();
    assert!(
        history
            .iter()
            .all(|m| m.compacted == (m.seq <= second.to_seq)),
        "le canonique est marqué exactement sur la couverture"
    );

    // Republier le premier travail ne défait rien.
    assert!(e.apply_summary(&first, &summary, "m").await.is_err());
    assert_eq!(e.lcm.active_nodes("s1").await.unwrap()[0].id, node);
}

#[tokio::test]
async fn only_a_forced_compaction_summarises_a_short_history() {
    let e = engine().await;
    for i in 0..6 {
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("m{i}")),
                100,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    let p = params(40_000);
    assert!(
        e.prepare_summary("s1", &p, 128_000, "m", false)
            .await
            .unwrap()
            .is_none()
    );
    let job = e
        .prepare_summary("s1", &p, 128_000, "m", true)
        .await
        .unwrap()
        .expect("`/compact` force un lot");
    assert!(job.to_seq < 6, "la queue reste verbatim");
}

#[tokio::test]
async fn a_crash_between_node_and_marking_is_repaired() {
    let e = engine().await;
    for i in 0..30 {
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("m{i}")),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    // Nœud écrit, canonique pas encore marqué.
    e.lcm
        .insert_leaf("s1", 1, 20, "résumé", &[], 10_000, 50)
        .await
        .unwrap();
    let _ = e
        .prepare_summary("s1", &params(40_000), 128_000, "m", false)
        .await
        .unwrap();
    let history = e.history.load("s1", 0).await.unwrap();
    assert!(history.iter().filter(|m| m.seq <= 20).all(|m| m.compacted));
    assert!(history.iter().filter(|m| m.seq > 20).all(|m| !m.compacted));
}

/// `ctx-safety` — reprise après crash : l'historique persistant suffit à reconstruire
/// exactement la même projection.
#[tokio::test]
async fn ctx_safety_recovery_after_crash() {
    let e = engine().await;
    for i in 0..10 {
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("m{i}")),
                100,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    let p = params(100_000);
    let t = tiers();
    let a = e.build_request("s1", &t, &p, "m", false).await.unwrap();

    // « kill -9 » : nouveau moteur sur la même base.
    let e2 = ContextEngine::new(
        HistoryStore::new(e.history.store().clone(), Arc::new(TestClock::default())),
        Lcm::new(e.history.store().clone(), Arc::new(TestClock::default())),
        TokenEstimator::new(),
        Catalog::new(),
        Arc::new(TestClock::default()),
    );
    let b = e2.build_request("s1", &t, &p, "m", false).await.unwrap();
    assert_eq!(
        a.messages, b.messages,
        "la projection doit être reproductible"
    );
    assert_eq!(a.prefix_hash, b.prefix_hash);
}

/// `ctx-safety` — niveau 4 : preuve locale avant envoi.
#[tokio::test]
async fn ctx_safety_level4() {
    let e = engine().await;
    let long = "w".repeat(80_000);
    let mut msgs = vec![ChatMessage::system("règles")];
    for i in 0..6 {
        msgs.push(ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
            id: format!("c{i}"),
            name: "t".into(),
            arguments: json!({}),
        }]));
        msgs.push(ChatMessage::tool_result(format!("c{i}"), "t", long.clone()));
    }
    msgs.push(ChatMessage::user("dernière question"));

    let (out, tokens) = e.emergency(msgs, 3_000, "m").unwrap();
    assert!(tokens <= 3_000);
    assert!(pairs_are_valid(&out));
    assert!(out.iter().any(|m| m.text() == "dernière question"));
}

#[tokio::test]
async fn summary_job_batches_when_summarizer_window_is_too_small() {
    let e = engine().await;
    for i in 0..40 {
        e.history
            .append(
                "s1",
                &ChatMessage::user("x".repeat(2000)),
                600,
                0,
                false,
                None,
            )
            .await
            .unwrap();
        let _ = i;
    }
    let job = e
        .prepare_summary("s1", &params(40_000), 4_000, "m", false)
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.batches.len() > 1,
        "une fenêtre de résumeur trop petite impose un découpage explicite"
    );
    assert_eq!(
        (job.chunk_from_seq, job.to_seq),
        job.batches[0],
        "le travail ne couvre que le premier lot"
    );
    for w in job.batches.windows(2) {
        assert_eq!(
            w[1].0,
            w[0].1 + 1,
            "aucun message n'est abandonné entre deux lots"
        );
    }
    assert_eq!(job.remaining_batches(), job.batches.len() - 1);
}

#[tokio::test]
async fn summary_job_key_is_stable() {
    let e = engine().await;
    for i in 0..10 {
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("m{i}")),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    let a = e
        .prepare_summary("s1", &params(6_000), 128_000, "m", false)
        .await
        .unwrap();
    let b = e
        .prepare_summary("s1", &params(6_000), 128_000, "m", false)
        .await
        .unwrap();
    assert!(a.is_some());
    assert_eq!(
        a.as_ref().map(|j| j.idempotency_key()),
        b.as_ref().map(|j| j.idempotency_key())
    );
}

#[tokio::test]
async fn short_session_has_nothing_to_summarise() {
    let e = engine().await;
    e.history
        .append("s1", &ChatMessage::user("bonjour"), 5, 0, false, None)
        .await
        .unwrap();
    assert!(
        e.prepare_summary("s1", &params(100_000), 128_000, "m", true)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn background_compaction_flag_trips_ten_points_early() {
    let e = engine().await;
    let entries: Vec<Entry> = (0..40)
        .map(|i| Entry::new(i, ChatMessage::user("x".repeat(1200)), 350))
        .collect();
    let ctx = e.build_from_entries(&entries, &tiers(), &params(20_000), "m", false);
    assert!(
        ctx.needs_background_compaction,
        "à 60 % de la fenêtre, la compaction de fond doit être demandée ({} tokens)",
        ctx.tokens
    );
}

#[test]
fn transcript_rendering_marks_roles_and_calls() {
    let entries = vec![
        Entry::new(1, ChatMessage::user("fais X"), 5),
        Entry::new(
            2,
            ChatMessage::assistant("ok").with_tool_calls(vec![ToolCall {
                id: "c".into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }]),
            5,
        ),
    ];
    let t = render_transcript(&entries);
    assert!(t.contains("[UTILISATEUR #1]"));
    assert!(t.contains("(appelle fs_read)"));
}

#[test]
fn huge_messages_are_sampled_for_the_summarizer() {
    let body = format!("DEBUT{}FIN", "y".repeat(50_000));
    let e = Entry::new(3, ChatMessage::tool_result("c", "shell_exec", body), 12_000);
    let line = render_entry(&e);
    assert!(line.chars().count() < crate::render::TOOL_RESULT_MAX_CHARS + 200);
    assert!(
        line.contains("DEBUT") && line.contains("FIN"),
        "tête et queue gardées"
    );
    assert!(
        line.contains("caractères non montrés"),
        "l'échantillonnage est dit"
    );
}
