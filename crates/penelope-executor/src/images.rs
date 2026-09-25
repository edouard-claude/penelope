//! Génération d'images (§10.4) : rôle `image_generate`, alias `image`.
//!
//! La requête demande les modalités `image` et `text` ; les images reviennent en URI
//! `data:` et sont enregistrées sous `{data}/media/generated`, d'où elles partent vers le
//! propriétaire.

use base64::Engine;
use penelope_app::ports::ProviderSource;
use penelope_app::services::Services;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::{ChatMessage, ChatRequest, Content};
use serde_json::{Value, json};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(180);

/// Génère une ou plusieurs images ; renvoie les fichiers écrits.
pub async fn generate(
    s: &Services,
    providers: &dyn ProviderSource,
    prompt: &str,
    size: Option<&str>,
) -> Result<Value, String> {
    let cfg = s.config.config();
    let alias = cfg.role_alias("image_generate");
    let model = cfg
        .alias_model(&alias)
        .ok_or_else(|| format!("aucun modèle pour l'alias `{alias}` du rôle `image_generate`"))?
        .to_string();
    if let Some(info) = s.catalog.get(strip_provider(&model))
        && !info.output_modalities.iter().any(|m| m == "image")
    {
        return Err(format!(
            "`{model}` ne produit pas d'images : `penelope model set {alias} \
                 openrouter:google/gemini-3.1-flash-image`, par exemple"
        ));
    }
    let provider = providers.provider_for(&model).await?;
    let mut text = prompt.trim().to_string();
    if let Some(size) = size.filter(|s| !s.trim().is_empty()) {
        text.push_str(&format!("\n\nFormat demandé : {size}."));
    }
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::user(text)],
        stream: true,
        modalities: vec!["image".into(), "text".into()],
        ..Default::default()
    };
    let call = async {
        let rx = provider
            .chat_stream(request, CancelToken::new())
            .await
            .map_err(|e| e.to_string())?;
        collect_stream(rx, &model, provider.name(), &s.catalog)
            .await
            .map_err(|e| e.to_string())
    };
    let response = tokio::time::timeout(TIMEOUT, call)
        .await
        .map_err(|_| "génération d'image trop longue".to_string())??;
    let _ = s
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            model: response.model.clone(),
            provider: response.provider.clone(),
            role: Some("image_generate".into()),
            generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
            upstream: response.upstream.clone(),
            prompt: response.usage.prompt,
            completion: response.usage.completion,
            cost_usd: response.cost_usd,
            estimated: response.cost_estimated,
            ..Default::default()
        })
        .await;

    let dir = s.platform.dirs.data().join("media").join("generated");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for c in &response.message.content {
        let Content::ImageUrl { url, .. } = c else {
            continue;
        };
        let (bytes, ext) = decode_data_url(url)?;
        let path = dir.join(format!("{}.{ext}", penelope_kernel::ids::Ulid::new()));
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        files.push(path.to_string_lossy().to_string());
    }
    if files.is_empty() {
        let said = response.message.text();
        return Err(if said.trim().is_empty() {
            format!("`{model}` n'a renvoyé aucune image")
        } else {
            format!("`{model}` n'a renvoyé aucune image : {}", said.trim())
        });
    }
    Ok(json!({
        "files": files,
        "model": response.model,
        "text": response.message.text(),
        "cost_usd": response.cost_usd,
    }))
}

/// Octets et extension d'une URI `data:image/<type>;base64,…`.
pub fn decode_data_url(url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or("image distante non prise en charge : URI `data:` attendue")?;
    let (meta, payload) = rest.split_once(',').ok_or("URI `data:` malformée")?;
    if !meta.ends_with(";base64") {
        return Err("URI `data:` non encodée en base64".into());
    }
    let ext = match meta.trim_end_matches(";base64") {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        other => return Err(format!("type d'image inattendu : {other}")),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("image illisible : {e}"))?;
    Ok((bytes, ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_app::testing::MockProviders;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use std::sync::Arc;

    #[tokio::test]
    async fn generated_images_are_written_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let p = Arc::new(MockProvider::new());
        let providers = MockProviders::new(p.clone());
        p.push(Scripted::Images(
            "Voici un phare.".into(),
            vec!["data:image/png;base64,iVBORw0KGgo=".into()],
        ));

        let v = generate(
            &s,
            providers.as_ref(),
            "un phare breton au crépuscule",
            Some("1024x1024"),
        )
        .await
        .unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        let bytes = std::fs::read(files[0].as_str().unwrap()).unwrap();
        assert_eq!(&bytes[..4], b"\x89PNG");
        let req = p.requests().pop().unwrap();
        assert_eq!(req.modalities, vec!["image", "text"]);
        assert!(req.messages[0].text().contains("1024x1024"));
        let roles = s.budget.report("role", None, None, 10).await.unwrap();
        assert!(roles.iter().any(|r| r.key == "image_generate"));

        p.reply("Désolé, pas d'image.");
        let err = generate(&s, providers.as_ref(), "rien", None)
            .await
            .unwrap_err();
        assert!(err.contains("aucune image"), "{err}");
    }

    #[test]
    fn data_urls_are_decoded_with_their_type() {
        let (bytes, ext) = decode_data_url("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(ext, "png");
        assert_eq!(&bytes[..4], b"\x89PNG");
        assert!(decode_data_url("https://exemple.org/a.png").is_err());
        assert!(decode_data_url("data:text/plain;base64,AA==").is_err());
    }
}
