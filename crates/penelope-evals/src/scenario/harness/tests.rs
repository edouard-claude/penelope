//! Tests du harnais : le patch de configuration par chemin pointé et la transcription
//! d'un flux en ligne de script.

use super::lifecycle::{fold, set_path};
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
    let hashes = [("r".to_string(), "h".to_string())].into();
    let within = |events: &[penelope_kernel::event::Event]| {
        super::visible::replaces_within_turns(events, 0, &hashes)
    };
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
