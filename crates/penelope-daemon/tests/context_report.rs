//! `penelope context` (#322), sur la composition réelle : des tours joués par le moteur,
//! une compaction, puis la méthode RPC. Le prompt et le cache viennent de la ligne
//! d'`usage`, la découpe par tuile de l'instantané du prompt et de la requête repliée ;
//! aucun texte de la conversation ne sort.

use penelope_app::bus::Origin;
use penelope_app::engine::TurnIntake;
use penelope_app::services::Services;
use penelope_daemon::rpc::Rpc;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::api::{RpcRequest, method};
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::MockProvider;
use penelope_llm::types::ChatMessage;
use serde_json::{Value, json};
use std::sync::Arc;

async fn turn(d: &Arc<Daemon>, p: &MockProvider, sid: &str, text: &str) {
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Voilà.");
    d.enqueue_message(sid, text, &Origin::Cli, None)
        .await
        .unwrap();
    let s = &d.services;
    let t = s.turns.claim("test").await.unwrap().unwrap();
    d.run_turn(&t).await;
    s.turns.complete(&t).await.unwrap();
}

async fn context(rpc: &Rpc, params: Value) -> Value {
    let r = rpc
        .handle(RpcRequest::new(1, method::CONTEXT, params))
        .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    r.result.unwrap()
}

#[tokio::test]
async fn context_measures_tiles_compactions_and_session_totals() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let rpc = Rpc::new(d.core.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();

    // Session neuve : rien à mesurer, et la CLI retrouve sa session sans identifiant.
    let fresh = context(&rpc, json!({})).await;
    assert_eq!(fresh["session"], sid.as_str());
    assert!(fresh["last"].is_null(), "{fresh}");
    assert_eq!(fresh["totals"]["calls"], 0);
    assert!(fresh["text"].as_str().unwrap().contains("Aucun appel"));

    let h = &s.context.history;
    for i in 0..20 {
        let q = format!("question {i} sur PROJ-7 : {}", "détail ".repeat(340));
        let a = format!("réponse {i} : {}", "analyse ".repeat(300));
        h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
            .await
            .unwrap();
        h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
            .await
            .unwrap();
    }
    turn(&d, &p, &sid, "où en es-tu ?").await;
    p.reply(r#"{"objectif": "migrer PROJ-7"}"#);
    let r = penelope_conversation::compaction::compact(
        &penelope_daemon::compaction::context_of(&d.core),
        &sid,
        penelope_conversation::compaction::Trigger::Manual,
        None,
    )
    .await
    .unwrap();
    assert_eq!(r.published, 1, "{r:?}");
    turn(&d, &p, &sid, "et maintenant ?").await;

    let v = context(&rpc, json!({"session": sid})).await;
    assert_eq!(v["compactions"], 1, "{v}");
    let prompt = v["last"]["prompt"].as_u64().unwrap();
    assert!(prompt > 0, "{v}");
    let names: Vec<&str> = v["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in ["T0", "T1", "T2", "T3", "T4"] {
        assert!(names.contains(&want), "{names:?}");
    }
    assert!(v["reserves"].as_array().unwrap().is_empty(), "{v}");
    let tile = |n: &str| {
        v["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == n)
            .unwrap()["tokens"]
            .as_u64()
            .unwrap()
    };
    assert!(tile("T0") > 100, "T0 porte les règles du harnais : {v}");
    assert!(tile("T3") > 0, "{v}");
    // Quatre appels par tour joué, classifieur compris, plus le résumé.
    let totals = &v["totals"];
    assert!(totals["calls"].as_u64().unwrap() >= 5, "{totals}");
    assert!(totals["prompt"].as_u64().unwrap() >= prompt, "{totals}");

    let text = v["text"].as_str().unwrap();
    assert!(text.contains("Par tuile (estimation locale)"), "{text}");
    assert!(text.contains("compactions : 1"), "{text}");
    assert!(text.contains("Libre"), "{text}");
    assert!(
        !text.contains("PROJ-7"),
        "aucun texte de conversation : {text}"
    );
    assert!(!text.contains("où en es-tu"), "{text}");
}
