//! Le juge d'approbation sur un modèle du rôle `approval_judge` (issue #203) : l'appel
//! au provider. Hors du module `judge`, que la boucle lit : celui-ci nomme `Services`.

use crate::judge::*;
use crate::ports::ProviderSource;
use crate::services::Services;
use penelope_kernel::config::APPROVAL_JUDGE_ROLE;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::{ChatMessage, ChatRequest, ToolChoice};
use serde_json::{Value, json};
use std::sync::Arc;

/// Échecs de suite qui ouvrent le disjoncteur (Hermès, `approval.py`), et durée pendant
/// laquelle il reste ouvert : un modèle en panne ne coûte pas dix secondes par carte.
const BREAKER_FAILURES: i64 = 3;
const BREAKER_OPEN_MS: i64 = 10 * 60_000;
/// État du disjoncteur, dans `kv` : il survit au juge de chaque tour.
const BREAKER_KEY: &str = "approval.judge_breaker";

/// Jetons de sortie : l'objet tient en quelques centaines.
const MAX_TOKENS: u32 = 600;

/// Le juge branché sur les providers du daemon.
pub struct ModelJudge {
    services: Arc<Services>,
    providers: Arc<dyn ProviderSource>,
}

impl ModelJudge {
    pub fn new(services: Arc<Services>, providers: Arc<dyn ProviderSource>) -> Self {
        ModelJudge {
            services,
            providers,
        }
    }

    /// `(échecs de suite, ouvert jusqu'à)`.
    async fn breaker(&self) -> (i64, i64) {
        let v: Value = self
            .services
            .kv_get(BREAKER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        (
            v["failures"].as_i64().unwrap_or(0),
            v["open_until_ms"].as_i64().unwrap_or(0),
        )
    }

    async fn settle<T>(&self, r: Result<T, JudgeFailure>) -> Result<T, JudgeFailure> {
        let (failures, _) = self.breaker().await;
        let next = match &r {
            Ok(_) if failures == 0 => None,
            Ok(_) => Some(json!({"failures": 0, "open_until_ms": 0})),
            Err(_) if failures + 1 >= BREAKER_FAILURES => Some(json!({
                "failures": 0,
                "open_until_ms": self.services.clock.now_ms() + BREAKER_OPEN_MS,
            })),
            Err(_) => Some(json!({"failures": failures + 1, "open_until_ms": 0})),
        };
        if let Some(v) = next {
            let _ = self.services.kv_set(BREAKER_KEY, &v.to_string()).await;
        }
        r
    }

    async fn call(&self, req: &JudgeRequest<'_>) -> Result<Judgement, JudgeFailure> {
        let s = &self.services;
        let started = std::time::Instant::now();
        let cfg = s.config.config();
        let alias = cfg.judge_alias();
        let model_id = cfg
            .alias_model(&alias)
            .ok_or_else(|| JudgeFailure::Unavailable(format!("alias `{alias}` sans modèle")))?
            .to_string();
        let model_id = crate::codex_scope::background(s, &model_id, "juge").await;
        let provider = self
            .providers
            .provider_for(&model_id)
            .await
            .map_err(JudgeFailure::Unavailable)?;
        let info = s
            .catalog
            .get(penelope_llm::catalog::strip_provider(&model_id));
        let effort = info.as_ref().and_then(|i| i.lightest_effort());
        let structured = info
            .as_ref()
            .is_some_and(|i| i.supports_structured_output());
        let command = hostile_text(req.command);
        let marker = penelope_kernel::ids::Ulid::new().to_string().to_lowercase();
        let (system, user) = judge_messages(&command, req.cwd, req.workspaces, &marker);
        let request = ChatRequest {
            model: model_id.clone(),
            messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
            // Aucun outil : le juge décrit, il n'agit pas.
            tools: Vec::new(),
            tool_choice: Some(ToolChoice::None),
            stream: true,
            max_tokens: Some(if effort.as_deref() == Some("none") {
                MAX_TOKENS
            } else {
                MAX_TOKENS * 4
            }),
            reasoning_effort: effort,
            response_format: structured.then(judgement_schema),
            session_id: Some(req.session_id.to_string()),
            ..Default::default()
        };
        let call = async {
            let rx = provider
                .chat_stream(request, CancelToken::new())
                .await
                .map_err(|e| JudgeFailure::Unavailable(e.to_string()))?;
            collect_stream(rx, &model_id, provider.name(), &s.catalog)
                .await
                .map_err(|e| JudgeFailure::Unavailable(e.to_string()))
        };
        let response = tokio::time::timeout(JUDGE_TIMEOUT, call)
            .await
            .map_err(|_| JudgeFailure::Timeout)??;
        // Le coût est compté même quand la sortie est rejetée : il a été payé.
        let _ = s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(req.session_id.to_string()),
                turn_id: req.turn_id.map(String::from),
                model: response.model.clone(),
                provider: response.provider.clone(),
                role: Some(APPROVAL_JUDGE_ROLE.into()),
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
        if !response.message.tool_calls.is_empty() {
            return Err(JudgeFailure::Schema("appel d'outil".into()));
        }
        let out = parse_judgement(&response.message.text()).map_err(JudgeFailure::Schema)?;
        Ok(Judgement {
            powers: out.powers,
            paths: out.paths,
            hosts: out.hosts,
            verdict: out.verdict,
            why: out.why,
            model: response.model,
            cost_usd: response.cost_usd,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }
}

#[async_trait::async_trait]
impl Judge for ModelJudge {
    async fn judge(&self, req: JudgeRequest<'_>) -> Result<Judgement, JudgeFailure> {
        if self.services.clock.now_ms() < self.breaker().await.1 {
            return Err(JudgeFailure::Unavailable("disjoncteur ouvert".into()));
        }
        if req.command.chars().count() > MAX_JUDGED_CHARS {
            return Err(JudgeFailure::Unavailable("commande trop longue".into()));
        }
        let r = self.call(&req).await;
        self.settle(r).await
    }
}

#[cfg(test)]
mod tests;
