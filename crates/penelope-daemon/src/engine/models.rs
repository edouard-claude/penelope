//! Port `SessionModels` et choix du modèle d'un tour : alias collant, épinglage,
//! frontières (#82), classifieur de complexité.

use super::*;

/// Frontière qui a fait reclasser le dernier message (#82), vide sinon.
fn last_model_why_key(session_id: &str) -> String {
    format!("session.model_last.why.{session_id}")
}

/// Libellé d'une frontière, pour `/model`.
fn boundary_label(b: &str) -> &'static str {
    match b {
        "compaction" => "contexte compacté",
        "episode" => "nouvel épisode",
        _ => "pause au-delà de la durée du cache",
    }
}

impl Core {
    /// Choisit l'alias et le modèle d'un tour (§10.3).
    ///
    /// L'alias est collant pour la session, mais le modèle est **relu** dans la
    /// configuration à chaque tour : `penelope model set main …` s'applique partout.
    pub async fn select_model(
        &self,
        session: &penelope_kernel::session::Session,
        text: &str,
        turn_id: &str,
    ) -> (String, String) {
        let s = &self.services;
        let cfg = s.config.config();
        let router = Router::new(s.catalog.clone());
        let routing = &cfg.models.routing;

        // Sans classifieur, pas d'alias collant : `main` (rôle `chat_default`) s'applique
        // aussitôt à toutes les sessions, y compris celles routées avant le changement.
        // Avec lui, l'alias « léger » (`low`) ne colle jamais : un « bonjour » ne doit pas
        // enfermer la session sur le petit modèle, le message suivant est reclassé.
        let sticky = session
            .model_alias
            .as_ref()
            .filter(|_| routing.classifier)
            .filter(|a| **a != routing.low)
            .and_then(|a| {
                cfg.alias_model(a).map(|id| StickyModel {
                    alias: a.clone(),
                    model_id: id.to_string(),
                })
            });
        let pinned = self.pinned_model(session.id.as_str()).await;
        // Frontière (#82) : cache froid, compaction ou nouvel épisode depuis le dernier
        // appel. Le préfixe change de toute façon, le collant n'y gagne rien : le message
        // repasse par le classifieur, qui peut monter ou descendre d'étage.
        let boundary = match (&sticky, &pinned) {
            (Some(_), None) => self.model_boundary(session.id.as_str()).await,
            _ => None,
        };
        let input = RouteInput {
            message: text.to_string(),
            pinned,
            sticky,
            at_boundary: boundary.is_some(),
            ..Default::default()
        };

        let decision = match router.route_deterministic(&cfg, &input) {
            Some(d) => d,
            None => match self.classify(text, session.id.as_str(), turn_id).await {
                Some(c) => router.route_with_classification(&cfg, &c),
                None => router.default_decision(&cfg),
            },
        };

        tracing::info!(
            session = %session.id,
            alias = %decision.alias,
            model = %decision.model_id,
            reason = ?decision.reason,
            frontiere = boundary.unwrap_or("aucune"),
            "modèle choisi"
        );
        // Seul le choix « de conversation » devient collant, pas un détour ponctuel
        // (image, vision) ni le petit modèle.
        let persist = match decision.reason {
            RouteReason::Default => true,
            RouteReason::Classifier => decision.alias != routing.low,
            _ => false,
        };
        if persist && session.model_alias.as_deref() != Some(decision.alias.as_str()) {
            let _ = s
                .sessions
                .set_model(session.id.as_str(), &decision.alias, &decision.model_id)
                .await;
        } else if !persist && boundary.is_some() && decision.reason == RouteReason::Classifier {
            // Reclassé en « simple » à une frontière : l'ancien collant ne revient pas au
            // message suivant.
            let _ = s.sessions.clear_model(session.id.as_str()).await;
        }
        // Dernier choix, pour que `/model` dise qui a répondu en dernier, et pourquoi il
        // a pu changer.
        let _ = s
            .kv_set(&last_model_key(session.id.as_str()), &decision.alias)
            .await;
        let _ = s
            .kv_set(
                &last_model_why_key(session.id.as_str()),
                boundary.map(boundary_label).unwrap_or(""),
            )
            .await;
        (decision.alias, decision.model_id)
    }

    /// Frontière franchie depuis le dernier appel de conversation de la session (#82) :
    /// cache du fournisseur expiré, contexte compacté, ou épisode clos.
    async fn model_boundary(&self, session_id: &str) -> Option<&'static str> {
        let s = &self.services;
        let previous = crate::cache_audit::previous_call(s, session_id)
            .await
            .ok()?;
        let Some(previous) = previous else {
            return Some("cache");
        };
        if s.clock.now_ms() - previous.ts_ms >= penelope_llm::cache::CACHE_TTL_MS {
            return Some("cache");
        }
        let since = chrono::DateTime::from_timestamp_millis(previous.ts_ms)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let sid = session_id.to_string();
        let kinds: Vec<String> = s
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT DISTINCT kind FROM events WHERE session_id = ?1 AND ts >= ?2
                       AND kind IN ('context.compacted', 'memory.episode_closed')",
                )?;
                let rows = st.query_map(penelope_store::rusqlite::params![sid, since], |r| {
                    r.get::<_, String>(0)
                })?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .ok()?;
        if kinds.iter().any(|k| k == "context.compacted") {
            Some("compaction")
        } else if kinds.iter().any(|k| k == "memory.episode_closed") {
            Some("episode")
        } else {
            None
        }
    }

    /// Classifieur de complexité : un petit modèle, une réponse JSON, 8 s au plus.
    ///
    /// Raisonnement réduit au minimum que le modèle accepte, sortie structurée quand le
    /// modèle la supporte, et coût enregistré comme celui de la conversation.
    async fn classify(
        &self,
        text: &str,
        session_id: &str,
        turn_id: &str,
    ) -> Option<Classification> {
        if text.trim().is_empty() {
            return None;
        }
        let s = &self.services;
        let cfg = s.config.config();
        let alias = cfg.role_alias("classifier");
        let model_id = cfg.alias_model(&alias)?.to_string();
        let model_id = codex_scope::background(s, &model_id, "classifieur").await;
        let provider = self.provider_for(&model_id).await.ok()?;
        let info = s
            .catalog
            .get(penelope_llm::catalog::strip_provider(&model_id));
        let effort = info.as_ref().and_then(|i| i.lightest_effort());
        let structured = info
            .as_ref()
            .map(|i| i.supports_structured_output())
            .unwrap_or(false);
        let req = ChatRequest {
            model: model_id.clone(),
            messages: vec![
                ChatMessage::system(CLASSIFIER_PROMPT),
                ChatMessage::user(text.chars().take(2_000).collect::<String>()),
            ],
            stream: true,
            // Sans raisonnement, 300 tokens suffisent ; avec, il lui faut de la marge
            // pour ne pas finir à vide (`finish_reason: length`).
            max_tokens: Some(if effort.as_deref() == Some("none") {
                300
            } else {
                2_000
            }),
            reasoning_effort: effort,
            response_format: structured.then(classification_schema),
            session_id: Some(session_id.to_string()),
            ..Default::default()
        };
        let call = async {
            let rx = provider.chat_stream(req, CancelToken::new()).await.ok()?;
            collect_stream(rx, &model_id, provider.name(), &s.catalog)
                .await
                .ok()
        };
        let response = tokio::time::timeout(std::time::Duration::from_secs(8), call)
            .await
            .ok()
            .flatten()?;
        let _ = s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
                model: response.model.clone(),
                provider: response.provider.clone(),
                role: Some("classifier".into()),
                generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
                upstream: response.upstream.clone(),
                finish: Some(format!("{:?}", response.finish).to_lowercase()),
                prompt: response.usage.prompt,
                completion: response.usage.completion,
                cached: response.usage.cached,
                cache_write: response.usage.cache_write,
                reasoning: response.usage.reasoning,
                cost_usd: response.cost_usd,
                estimated: response.cost_estimated,
                ..Default::default()
            })
            .await;
        parse_classification(&response.message.text())
    }
}

#[async_trait::async_trait]
impl SessionModels for Core {
    async fn pinned_model(&self, session_id: &str) -> Option<StickyModel> {
        penelope_app::helpers::pinned_model(&self.services, session_id).await
    }

    async fn pin_model(&self, session_id: &str, alias: Option<&str>) -> anyhow::Result<()> {
        if let Some(a) = alias {
            let cfg = self.services.config.config();
            if cfg.alias_model(a).is_none() {
                anyhow::bail!(
                    "alias inconnu `{a}` ; alias disponibles : {}",
                    conversation_aliases(&cfg).join(", ")
                );
            }
        }
        self.services
            .kv_set(&pin_key(session_id), alias.unwrap_or(""))
            .await
    }

    async fn session_model_view(&self, session_id: &str) -> anyhow::Result<Value> {
        let cfg = self.services.config.config();
        let model_of = |a: &str| cfg.alias_model(a).map(String::from);
        let pinned = self.pinned_model(session_id).await;
        let last = self
            .services
            .kv_get(&last_model_key(session_id))
            .await?
            .filter(|a| !a.is_empty());
        let why = self
            .services
            .kv_get(&last_model_why_key(session_id))
            .await?
            .filter(|a| !a.is_empty());
        let choices: Vec<Value> = conversation_aliases(&cfg)
            .into_iter()
            .map(|a| json!({"alias": a, "model": model_of(&a)}))
            .collect();
        Ok(json!({
            "session": session_id,
            "mode": if pinned.is_some() { "épinglé" } else { "automatique" },
            "pinned": pinned.as_ref().map(|p| p.alias.clone()),
            "pinned_model": pinned.as_ref().map(|p| p.model_id.clone()),
            "last_alias": last,
            "last_model": last.as_deref().and_then(model_of),
            "last_boundary": why,
            "classifier": cfg.models.routing.classifier,
            "choices": choices,
        }))
    }
}
