//! Tests du harnais : le patch de configuration par chemin pointé et la transcription
//! d'un flux en ligne de script.

use super::lifecycle::{fold, set_path, shut_down};
use crate::scenario::ScriptLine;
use penelope_llm::types::FinishReason;
use serde_json::json;

#[test]
fn set_path_creates_missing_tables() {
    let mut tree = json!({"context": {"tail_ratio": 0.1}});
    set_path(&mut tree, "context.large_payload_tokens", json!(300)).unwrap();
    set_path(&mut tree, "nouveau.sous.cle", json!("x")).unwrap();
    assert_eq!(tree["context"]["large_payload_tokens"], 300);
    assert_eq!(tree["context"]["tail_ratio"], 0.1);
    assert_eq!(tree["nouveau"]["sous"]["cle"], "x");
    assert!(set_path(&mut tree, "context.tail_ratio.x", json!(1)).is_err());
}

#[test]
fn folding_a_stream_gives_the_script_vocabulary() {
    assert_eq!(
        fold(
            "ok".into(),
            vec![],
            vec![],
            None,
            Some(FinishReason::Stop),
            None
        ),
        ScriptLine::Text("ok".into())
    );
    assert!(matches!(
        fold(
            String::new(),
            vec![],
            vec![],
            None,
            None,
            Some("coupé".into())
        ),
        ScriptLine::MidStreamError { .. }
    ));
    let cut = fold(
        "long".into(),
        vec![],
        vec![],
        Some(penelope_llm::types::Usage {
            completion: 9,
            ..Default::default()
        }),
        Some(FinishReason::Length),
        None,
    );
    assert!(matches!(
        cut,
        ScriptLine::Written {
            cut: true,
            completion: 9,
            ..
        }
    ));
}

fn event(seq: i64, kind: &str, payload: serde_json::Value) -> penelope_kernel::event::Event {
    penelope_kernel::event::Event {
        id: seq,
        session_id: Some("s".into()),
        run_id: None,
        seq,
        ts: String::new(),
        kind: kind.into(),
        payload,
        hash: String::new(),
        prev_hash: String::new(),
    }
}

/// Un tour : réponse avec appel d'outil (seq 2), résultat (seq 3), puis `between`, puis
/// l'appel suivant.
fn turn_with(
    first_call: &str,
    between: Vec<penelope_kernel::event::Event>,
) -> Vec<penelope_kernel::event::Event> {
    let call = |seq, kind: &str| match kind {
        "conv.attempt" => event(
            seq,
            kind,
            json!({"cause": "before_stream", "llm_request_id": "r"}),
        ),
        _ => event(
            seq,
            kind,
            json!({"request_hash": "h", "surface": {"op": "append"}}),
        ),
    };
    let mut out = vec![
        event(1, "turn.started", json!({})),
        call(2, first_call),
        event(3, "conv.tool_result", json!({"surface": {"op": "append"}})),
    ];
    out.extend(between);
    out.push(call(10, "conv.assistant"));
    out.push(event(11, "turn.finished", json!({})));
    out
}

fn replace(
    seq: i64,
    kind: &str,
    from: i64,
    to: i64,
    trigger: &str,
) -> penelope_kernel::event::Event {
    event(
        seq,
        kind,
        json!({"surface": {"op": "replace", "from": from, "to": to}, "trigger": trigger}),
    )
}

#[test]
fn only_two_replaces_may_fall_between_two_calls() {
    let within =
        |events: &[penelope_kernel::event::Event]| super::visible::replaces_within_turns(events, 0);
    // Niveau 1 d'un résultat ajouté depuis l'appel précédent : admis.
    let fresh = turn_with(
        "conv.assistant",
        vec![replace(4, "conv.tool_result", 3, 3, "")],
    );
    assert_eq!(within(&fresh).unwrap().get("conv.tool_result"), Some(&1));
    // Niveau 1 d'un nœud déjà envoyé : refusé.
    let sent = turn_with(
        "conv.assistant",
        vec![replace(4, "conv.tool_result", 2, 2, "")],
    );
    assert!(within(&sent).is_err());
    // Résumé après une tentative refusée pour dépassement : admis ; après une réponse, ou
    // pour une autre raison : refusé.
    let overflow = vec![replace(4, "conv.summary", 1, 2, "overflow")];
    assert_eq!(
        within(&turn_with("conv.attempt", overflow.clone()))
            .unwrap()
            .get("conv.summary"),
        Some(&1)
    );
    assert!(within(&turn_with("conv.assistant", overflow)).is_err());
    let manual = vec![replace(4, "conv.summary", 1, 2, "manual")];
    assert!(within(&turn_with("conv.attempt", manual)).is_err());
    // Prompt système remplacé, coupe : refusés.
    assert!(
        within(&turn_with(
            "conv.assistant",
            vec![replace(4, "conv.system", 0, 0, "")]
        ))
        .is_err()
    );
    let cut = event(
        4,
        "conv.rewind",
        json!({"surface": {"op": "cut", "after": 1}}),
    );
    assert!(within(&turn_with("conv.assistant", vec![cut])).is_err());
    // Hors d'un tour, ou avant le premier appel : frontière de tour, rien à dire.
    let mut edge = vec![replace(1, "conv.summary", 1, 2, "manual")];
    edge.extend(turn_with("conv.assistant", vec![]));
    edge.insert(2, replace(2, "conv.system", 0, 0, ""));
    assert!(within(&edge).unwrap().is_empty());
}

async fn life(dir: &std::path::Path) -> super::Life {
    let clock: penelope_kernel::clock::SharedClock =
        std::sync::Arc::new(penelope_kernel::clock::TestClock::default());
    let services = std::sync::Arc::new(
        penelope_app::services::Services::for_tests(dir.to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let daemon = std::sync::Arc::new(penelope_daemon::runtime::Daemon::from_services(
        services.clone(),
    ));
    super::Life {
        services,
        daemon,
        gateway: None,
        mcp: None,
    }
}

/// Une vie sans fuite se ferme sans attendre l'échéance. Avant, `shut_down` attendait
/// deux copies des services là où le daemon en tient trois (cœur et `Providers`) :
/// chaque vie de chaque scénario attendait cinq secondes, puis fermait « sans elles ».
#[tokio::test]
async fn a_life_without_leaks_closes_without_waiting() {
    let dir = tempfile::tempdir().unwrap();
    assert!(shut_down(life(dir.path()).await, "sans-fuite").await);
}

/// Une copie des services gardée hors de la vie : la fermeture le dit et rend la main à
/// l'échéance, au lieu de pendre.
#[tokio::test]
async fn a_leaked_reference_is_reported_and_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let life = life(dir.path()).await;
    let leak = life.services.clone();
    assert!(!shut_down(life, "fuite").await);
    assert_eq!(std::sync::Arc::strong_count(&leak), 1);
}

/// Un clic sur une opération répond au toast puis travaille dans une tâche détachée
/// (issue #73) ; l'étape `telegram` attend cette tâche, même quand sa carte met 300 ms à
/// partir, plus que le silence qui concluait l'étape (#269). Le bouton « Annuler »
/// (`noop`) dit son issue dans la conversation, une fois le clavier retiré : c'est ce
/// retrait que le transport fait traîner.
#[tokio::test]
async fn a_telegram_step_waits_for_the_detached_work_of_a_click() {
    use penelope_telegram::api::{BotTransport as _, method};
    let scenario = crate::scenario::Scenario {
        dir: std::path::PathBuf::new(),
        spec: toml::from_str("name = \"clic-lent\"\nsteps = []\n").unwrap(),
        script: Vec::new(),
    };
    let mut h = super::Harness::start(&scenario, crate::scenario::Mode::Replay)
        .await
        .unwrap();
    let transport = h.telegram.transport.clone();
    let action = {
        // Rien de la vie n'est gardé au-delà : sa fermeture n'attend personne.
        let d = h.daemon().unwrap();
        penelope_telegram::ActionStore::new(d.services.store.clone(), d.services.clock.clone(), 42)
            .create(
                penelope_telegram::actions::kind::SCREEN_DO,
                "noop",
                json!({}),
                60_000,
                true,
            )
            .await
            .unwrap()
    };
    transport
        .call(
            method::SEND_MESSAGE,
            json!({"chat_id": 42, "text": "Écran", "reply_markup": {"inline_keyboard":
                [[{"text": "Annuler", "callback_data": action.token}]]}}),
        )
        .await
        .unwrap();
    transport
        .set_call_delay(
            method::EDIT_MESSAGE_REPLY_MARKUP,
            std::time::Duration::from_millis(300),
        )
        .await;

    let v = h.telegram(None, Some("Annuler")).await.unwrap();

    let texts: Vec<&str> = v["screens"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["text"].as_str())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("Annulé")),
        "l'étape s'est conclue sans la carte du clic : {texts:?}"
    );
    if let Some(life) = h.life.take() {
        shut_down(life, "clic-lent").await;
    }
}
