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
