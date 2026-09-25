use super::harness::Harness;
use super::*;
use penelope_app::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;

struct Env {
    d: Harness,
    p: Arc<MockProvider>,
    clock: TestClock,
    r: Arc<RecordingMessenger>,
}

async fn env() -> Env {
    let clock = TestClock::new(1_789_516_800_000);
    let d = Harness::new(Arc::new(clock.clone())).await;
    let p = d.provider.clone();
    let r = RecordingMessenger::with_cards();
    d.set_messenger(r.clone());
    Env { d, p, clock, r }
}

/// Installe un workflow de test, validé comme un fichier déposé par l'utilisateur.
async fn install(d: &Context, raw: Value) {
    let s = &d.services;
    let wf = Workflow::from_json(&raw.to_string()).expect("JSON de workflow");
    let known =
        penelope_app::services::workflow_known_with(&s.config.config(), &s.mcp_tools, &s.workflows)
            .await;
    let dir = s.platform.dirs.workflows();
    std::fs::create_dir_all(&dir).unwrap();
    s.workflows
        .write(&dir, &wf, &known)
        .expect("workflow valide");
    s.workflows
        .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);
}

fn wf(id: &str, entry: &str, steps: Value) -> Value {
    json!({
        "metadata": {"id": id, "name": format!("Essai {id}"), "parameters": []},
        "entryStep": entry,
        "settings": {"maxIterations": 10, "budget": {"maxUsd": 1.0, "maxTokens": 100000, "maxWallMs": 3600000}},
        "steps": steps,
    })
}

fn owner() -> Origin {
    Origin::Telegram {
        chat_id: 42,
        topic_id: None,
        message_id: None,
    }
}

mod runs;
mod steps;
