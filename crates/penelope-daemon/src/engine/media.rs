//! Port `Transcriber` et photos d'un tour : un vocal transcrit, des images montrées au
//! modèle ou décrites par le rôle `image_describe`.

use super::*;

impl Core {
    /// Message utilisateur d'un tour avec photos.
    pub(super) async fn photo_message(
        &self,
        text: &str,
        images: &[std::path::PathBuf],
        model_id: &str,
        session_id: &str,
        turn_id: &str,
    ) -> ChatMessage {
        let s = &self.services;
        let sees = s
            .catalog
            .get(penelope_llm::catalog::strip_provider(model_id))
            .map(|i| i.accepts_images())
            .unwrap_or(false);
        // L'image est réduite pour le modèle qui la lira (issue #242).
        let (cfg, describe) = (s.config.config(), penelope_executor::vision::Task::Describe);
        let describer = penelope_executor::vision::alias_for(&cfg, &s.catalog, describe);
        let describer = cfg.alias_model(&describer);
        let reader = if sees {
            model_id
        } else {
            describer.unwrap_or(model_id)
        };
        let mut urls = Vec::new();
        let mut unreadable = 0;
        for p in images {
            match penelope_app::media::model_data_url(s, p, reader, session_id).await {
                Ok(u) => urls.push(u),
                Err(e) => {
                    tracing::warn!(error = %e, "photo illisible");
                    unreadable += 1;
                }
            }
        }
        let count = if urls.len() > 1 {
            format!("{} photos", urls.len())
        } else {
            "une photo".to_string()
        };
        let mut request = if text.trim().is_empty() {
            format!("(Le propriétaire envoie {count}, sans légende.)")
        } else {
            text.trim().to_string()
        };
        if unreadable > 0 {
            request.push_str(&format!(
                "
({unreadable} photo(s) illisible(s) ignorée(s).)"
            ));
        }
        // Le chemin reste : `image_inspect` y relit un texte ou y pointe un élément (#125).
        let saved: Vec<String> = images.iter().map(|p| p.display().to_string()).collect();
        if !saved.is_empty() {
            request.push_str(&format!(
                "\n(photo enregistrée : {} ; `image_inspect` pour y lire le texte ou y \
                 pointer un élément)",
                saved.join(", ")
            ));
        }
        if sees && !urls.is_empty() {
            let mut content = vec![penelope_llm::types::Content::text(request)];
            content.extend(
                urls.into_iter()
                    .map(|url| penelope_llm::types::Content::ImageUrl { url, detail: None }),
            );
            return ChatMessage {
                content,
                ..ChatMessage::user("")
            };
        }
        if urls.is_empty() {
            return ChatMessage::user(request);
        }
        let description = match self.describe_images(&urls, text, session_id, turn_id).await {
            Ok(d) => d,
            Err(e) => format!("(description impossible : {e})"),
        };
        ChatMessage::user(format!(
            "{request}

[{count} : description par le modèle de vision, le modèle de la              conversation ne lit pas les images]
{description}"
        ))
    }
}

#[async_trait::async_trait]
impl Transcriber for Core {
    /// Un modèle local sans endpoint actif qui le sert (`providers.extra.*` ou
    /// `providers.local`) est dit, plutôt que d'envoyer l'audio ailleurs (#335).
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        filename: &str,
        session_id: &str,
    ) -> Result<String, String> {
        let s = &self.services;
        let cfg = s.config.config();
        let model = cfg
            .role_model("stt")
            .ok_or_else(|| "aucun modèle pour le rôle `stt`".to_string())?;
        if self.provider_override_active().is_none() {
            cfg.providers.check_local(&model)?;
        }
        let model = codex_scope::background(&self.services, &model, "transcription").await;
        let provider = self.provider_for(&model).await?;
        let language = Some(cfg.owner.language.clone()).filter(|l| !l.is_empty());
        let t = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            provider.transcribe(&model, audio, filename, language.as_deref()),
        )
        .await
        .map_err(|_| "transcription trop longue (plus de 3 min)".to_string())?
        .map_err(|e| format!("{} ({model})", e.message))?;
        let _ = s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(session_id.to_string()),
                model: penelope_llm::catalog::strip_provider(&model).to_string(),
                provider: provider.name().to_string(),
                role: Some("stt".into()),
                cost_usd: t.cost_usd.unwrap_or(0.0),
                estimated: t.cost_usd.is_none(),
                ..Default::default()
            })
            .await;
        Ok(t.text)
    }

    async fn describe_images(
        &self,
        urls: &[String],
        caption: &str,
        session_id: &str,
        turn_id: &str,
    ) -> Result<String, String> {
        let request = if caption.trim().is_empty() {
            "Décris ces images.".to_string()
        } else {
            format!("Légende du propriétaire : {}", caption.trim())
        };
        let (s, p) = (&self.services, self.providers.as_ref());
        let task = penelope_executor::vision::Task::Describe;
        penelope_executor::vision::ask(s, p, task, urls, &request, None, session_id, turn_id)
            .await
            .map(|a| a.text)
    }
}
