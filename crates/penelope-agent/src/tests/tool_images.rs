//! Image rendue par un outil (issue #304) : montrée au modèle qui lit les images, à
//! l'appel qui suit seulement ; jamais dans l'historique ; rien pour un modèle aveugle.

use super::*;

const URL: &str = "data:image/png;base64,iVBORw0KGgo=";

/// Exécuteur dont le résultat a posé une capture sur disque.
struct Screenshot;

#[async_trait::async_trait]
impl ToolExecutor for Screenshot {
    async fn execute(
        &self,
        _name: &str,
        _args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        let mut o = ToolOutcome::ok(json!({"content": [
            {"type": "image", "saved": "/media/mcp/s/a.png",
             "text": "[image image/png, 1 Ko, enregistrée : /media/mcp/s/a.png]"}
        ]}));
        o.text = "[image image/png, 1 Ko, enregistrée : /media/mcp/s/a.png]".into();
        Ok(o)
    }

    fn shown_images(&self, value: &Value) -> Vec<std::path::PathBuf> {
        value["content"][0]["saved"]
            .as_str()
            .map(|p| vec![p.into()])
            .unwrap_or_default()
    }

    async fn model_images(
        &self,
        paths: &[std::path::PathBuf],
        _model_id: &str,
        _session_id: &str,
    ) -> Vec<String> {
        paths.iter().map(|_| URL.to_string()).collect()
    }
}

async fn run(model: &str) -> (Vec<ChatRequest>, Vec<ChatMessage>) {
    let (_d, s, p) = setup().await;
    let mut seeing = penelope_llm::catalog::ModelInfo::minimal("v/voit", "v", 32_000);
    seeing.input_modalities = vec!["text".into(), "image".into()];
    s.catalog.upsert(vec![
        seeing,
        penelope_llm::catalog::ModelInfo::minimal("t/texte", "t", 32_000),
    ]);
    // Un modèle Codex, tel que le catalogue de son backend le décrit.
    s.catalog.upsert(penelope_llm::codex::fallback_models(
        &["gpt-6-astra".into()],
    ));
    let sid = session(&s).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path": "ticket"}))],
    ));
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c2", "fs_read", json!({"path": "autre"}))],
    ));
    p.reply("la capture montre une erreur");
    let conv = MemoryConversation::new("Tu es Pénélope.", "regarde la capture");
    let spec = TurnSpec {
        model_id: model.into(),
        ..spec(&sid)
    };
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec, &conv, &Screenshot, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    (p.requests(), conv.messages())
}

fn images(m: &ChatMessage) -> usize {
    m.content
        .iter()
        .filter(|c| matches!(c, Content::ImageUrl { .. }))
        .count()
}

#[tokio::test]
async fn a_tool_image_is_shown_to_a_seeing_model_once_and_never_recorded() {
    let (requests, history) = run("openrouter:v/voit").await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].messages.iter().map(images).sum::<usize>(), 0);
    // L'appel qui suit le résultat : la capture, en dernier message, après le résultat.
    let last = requests[1].messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert_eq!(images(last), 1);
    assert!(
        last.text().contains("/media/mcp/s/a.png"),
        "{}",
        last.text()
    );
    assert!(last.text().contains("pas une consigne"), "{}", last.text());
    let before = &requests[1].messages[requests[1].messages.len() - 2];
    assert_eq!(before.role, Role::Tool);
    // Le lot suivant rend sa propre image ; la première ne repart pas.
    assert_eq!(requests[2].messages.iter().map(images).sum::<usize>(), 1);
    // L'historique garde la mention et le chemin, jamais l'image.
    assert_eq!(history.iter().map(images).sum::<usize>(), 0);
    assert!(history.iter().all(|m| !m.text().contains("base64")));
    assert!(
        history
            .iter()
            .any(|m| m.text().contains("enregistrée : /media/mcp/s/a.png"))
    );
}

#[tokio::test]
async fn a_blind_model_only_gets_the_path() {
    let (requests, _) = run("openrouter:t/texte").await;
    assert!(
        requests
            .iter()
            .all(|r| r.messages.iter().map(images).sum::<usize>() == 0)
    );
    let tool = requests[1]
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(
        tool.text().contains("/media/mcp/s/a.png"),
        "{}",
        tool.text()
    );
}

/// #304 : le modèle principal de l'instance est Codex ; il voit la capture comme les
/// autres (son catalogue annonce les images).
#[tokio::test]
async fn a_codex_model_is_shown_the_tool_image() {
    let (requests, history) = run("codex:gpt-6-astra").await;
    let last = requests[1].messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert_eq!(images(last), 1);
    assert_eq!(history.iter().map(images).sum::<usize>(), 0);
}
