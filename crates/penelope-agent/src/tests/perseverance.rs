//! Expérience persévérance (#291) : l'indice de contexte en fin de requête et le rappel de
//! délégation sans chiffres. Deux interrupteurs, éteints par défaut.

use super::*;
use penelope_kernel::config::NudgeStyle;
use penelope_kernel::event::Event;
use penelope_llm::cache::Fingerprint;

/// Le texte de l'expérience, tel que la source le rapporte efficace.
const HINT: &str = "You have 500000 tokens context window.";

fn codex(session_id: &str) -> TurnSpec {
    TurnSpec {
        model_id: "codex:gpt-6-astra".into(),
        ..spec(session_id)
    }
}

fn set_hint(s: &AgentServices, text: &str) {
    let text = text.to_string();
    s.config
        .mutate("test", move |c| {
            c.agent.context_hint = text;
            Ok(vec!["agent.context_hint".into()])
        })
        .unwrap();
}

/// Un tour d'un seul appel, sur un fournisseur neuf : la requête vue par le modèle et la
/// conversation telle qu'elle reste.
async fn one_turn(s: &Arc<AgentServices>, sp: &TurnSpec) -> (ChatRequest, MemoryConversation) {
    let p = Arc::new(MockProvider::new());
    p.reply("fait");
    let conv = MemoryConversation::new("Tu es Pénélope.", "corrige le bug");
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    let mut requests = p.requests();
    assert_eq!(requests.len(), 1);
    (requests.remove(0), conv)
}

/// Les événements d'une session, d'un kind.
async fn events_of(s: &AgentServices, session_id: &str, kind: &str) -> Vec<Event> {
    s.events
        .range(0, 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind && e.session_id.as_deref() == Some(session_id))
        .collect()
}

/// Éteint par défaut (chaîne vide), et une chaîne blanche vaut vide : rien ne part, rien
/// n'est journalisé, même pour `codex:`.
#[tokio::test]
async fn the_hint_is_off_by_default() {
    let (_d, s, _) = setup().await;
    for blank in ["", "   "] {
        set_hint(&s, blank);
        let sid = session(&s).await;
        let (req, _) = one_turn(&s, &codex(&sid)).await;
        assert_eq!(req.developer_note, None, "{blank:?}");
        let llm = events_of(&s, &sid, "runtime.llm").await;
        assert!(!llm.is_empty());
        assert!(
            llm.iter().all(|e| e.payload.get("context_hint").is_none()),
            "{llm:?}"
        );
    }
}

/// Renseigné, pour `codex:` : le texte exact est dans la requête envoyée, hors
/// `messages` ; la requête est sinon la même qu'avant, octet pour octet, donc même
/// empreinte ; rien dans la conversation ni dans le journal, sauf le booléen de
/// `runtime.llm`.
#[tokio::test]
async fn the_hint_rides_outside_the_history_and_leaves_the_fingerprint_alone() {
    let (_d, s, _) = setup().await;
    let sid_plain = session(&s).await;
    let (plain, _) = one_turn(&s, &codex(&sid_plain)).await;

    set_hint(&s, HINT);
    let sid = session(&s).await;
    let (hinted, conv) = one_turn(&s, &codex(&sid)).await;

    assert_eq!(hinted.developer_note.as_deref(), Some(HINT));
    assert_eq!(
        hinted.messages, plain.messages,
        "l'historique envoyé ne change pas"
    );
    assert_eq!(
        Fingerprint::of(&hinted.messages, &hinted.tools),
        Fingerprint::of(&plain.messages, &plain.tools)
    );
    let before = s.budget.previous_call(&sid_plain).await.unwrap().unwrap();
    let after = s.budget.previous_call(&sid).await.unwrap().unwrap();
    assert_eq!(before.system_hash, after.system_hash);
    assert_eq!(before.tools_hash, after.tools_hash);
    assert_eq!(before.request_hash, after.request_hash);

    // Jamais persisté : ni transcript, ni journal.
    assert!(
        conv.messages()
            .iter()
            .all(|m| !m.text().contains("500000 tokens")),
        "{:?}",
        conv.messages()
    );
    let all = s.events.range(0, 10_000).await.unwrap();
    assert!(
        all.iter()
            .all(|e| !e.payload.to_string().contains("500000 tokens")),
        "l'indice ne doit apparaître dans aucun événement"
    );
    // Sa présence, elle, se mesure.
    let llm = events_of(&s, &sid, "runtime.llm").await;
    assert!(!llm.is_empty());
    assert!(
        llm.iter().all(|e| e.payload["context_hint"] == json!(true)),
        "{llm:?}"
    );
    let plain_llm = events_of(&s, &sid_plain, "runtime.llm").await;
    assert!(
        plain_llm
            .iter()
            .all(|e| e.payload.get("context_hint").is_none())
    );
}

/// Renseigné, pour un modèle qui n'est pas `codex:` : rien ne part. DeepSeek via
/// OpenRouter ne connaît pas le rôle `developer`.
#[tokio::test]
async fn the_hint_is_not_sent_to_other_providers() {
    let (_d, s, _) = setup().await;
    set_hint(&s, HINT);
    let sid = session(&s).await;
    let deepseek = TurnSpec {
        model_id: "openrouter:deepseek/deepseek-v4-pro".into(),
        ..spec(&sid)
    };
    let (req, _) = one_turn(&s, &deepseek).await;
    assert_eq!(req.developer_note, None);
    let llm = events_of(&s, &sid, "runtime.llm").await;
    assert!(!llm.is_empty());
    assert!(llm.iter().all(|e| e.payload.get("context_hint").is_none()));
}

/// Un tour qui lit un fichier puis répond : le résultat d'outil, et l'événement
/// `tool.result` qui l'accompagne.
async fn nudged_result(s: &Arc<AgentServices>, sid: &str) -> (String, Event) {
    let p = Arc::new(MockProvider::new());
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("lu");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis a.rs");
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    let result = conv
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("un résultat d'outil")
        .text();
    let mut events = events_of(s, sid, "tool.result").await;
    assert_eq!(events.len(), 1);
    (result, events.remove(0))
}

/// La note du tour, après le résultat de l'outil.
fn note_of(result: &str) -> &str {
    result.split_once("[Harnais").map(|(_, n)| n).unwrap_or("")
}

/// Les deux styles du rappel : le classique cite le compte et le coût, le doux ne cite
/// rien de chiffré et dit de continuer ; l'événement dit le style, et rien quand il n'y a
/// pas de note.
#[tokio::test]
async fn the_delegation_nudge_has_two_styles_and_the_event_names_it() {
    let (_d, s, _) = setup().await;

    // Par défaut, tous les dix appels : un tour d'un appel ne porte rien.
    let sid = session(&s).await;
    let (result, event) = nudged_result(&s, &sid).await;
    assert!(!result.contains("[Harnais"), "{result}");
    assert!(event.payload.get("nudge_style").is_none(), "{event:?}");

    s.config
        .mutate("test", |c| {
            c.budget.delegate_after_calls = 1;
            Ok(vec!["budget.delegate_after_calls".into()])
        })
        .unwrap();
    let sid = session(&s).await;
    let (result, event) = nudged_result(&s, &sid).await;
    let note = note_of(&result);
    assert!(note.contains("appels au modèle dans ce tour"), "{note}");
    assert!(
        note.contains('$'),
        "le style classique cite le coût : {note}"
    );
    assert!(note.contains("`sub_agent_spawn`") && note.contains("`session_notes`"));
    assert_eq!(event.payload["nudge_style"], json!("classique"));

    s.config
        .mutate("test", |c| {
            c.budget.delegation_nudge_style = NudgeStyle::Doux;
            Ok(vec!["budget.delegation_nudge_style".into()])
        })
        .unwrap();
    let sid = session(&s).await;
    let (result, event) = nudged_result(&s, &sid).await;
    let note = note_of(&result);
    assert!(!note.is_empty(), "{result}");
    assert!(
        !note.chars().any(|c| c.is_ascii_digit()) && !note.contains('$'),
        "le style doux ne cite ni compte ni coût : {note}"
    );
    assert!(note.contains("`sub_agent_spawn`") && note.contains("`session_notes`"));
    assert!(
        note.contains("ce rappel ne demande pas de conclure"),
        "{note}"
    );
    assert_eq!(event.payload["nudge_style"], json!("doux"));
}
