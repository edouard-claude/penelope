//! Écritures du moteur de contexte dans le canonique : l'admission d'un groupe de
//! résultats d'outils (niveau 1) et la publication d'un résumé (niveau 3).

use crate::compaction::*;
use crate::engine::{ContextEngine, SummaryJob};

impl ContextEngine {
    /// Niveau 1 — admission : applique le budget à un groupe de résultats d'outils et
    /// externalise le surplus. C'est une modification du **canonique**.
    pub async fn admit_tool_group(
        &self,
        session_id: &str,
        results: &[(i64, String)],
        params: &CompactionParams,
        model_id: &str,
    ) -> penelope_store::Result<Vec<AppliedStep>> {
        let sized: Vec<(usize, String, u64)> = results
            .iter()
            .enumerate()
            .map(|(i, (_, text))| (i, text.clone(), self.estimator.text_tokens(model_id, text)))
            .collect();
        let decisions = level1_admission(&sized, params);
        let mut steps = Vec::new();

        for (i, decision) in decisions {
            let Admission::Externalise {
                head,
                tail,
                original_tokens,
            } = decision
            else {
                continue;
            };
            let (seq, full) = &results[i];
            let kind = crate::store::guess_kind(full);
            let artifact = self
                .history
                .put_artifact(Some(session_id), None, kind, None, full)
                .await?;
            let body = externalised_body(&artifact.id, &head, &tail, original_tokens, kind);
            let new_tokens = self.estimator.text_tokens(model_id, &body);
            self.history
                .externalise(session_id, *seq, &body, &artifact.id, new_tokens)
                .await?;
            steps.push(AppliedStep {
                level: 1,
                label: format!("résultat externalisé en artefact {}", artifact.id),
                before_tokens: original_tokens,
                after_tokens: new_tokens,
                touched: 1,
            });
        }
        Ok(steps)
    }

    /// Publie un résumé validé. **Idempotent** : republier le même travail ne crée pas un
    /// second nœud (CA 5). Un travail préparé avant qu'un autre ne mette à jour le même
    /// résumé est périmé : erreur, rien n'est écrit.
    pub async fn apply_summary(
        &self,
        job: &SummaryJob,
        validated: &serde_json::Value,
        model_id: &str,
    ) -> penelope_store::Result<String> {
        // Un nœud vivant couvrant déjà exactement cet intervalle vaut publication faite.
        let existing = self.lcm.active_nodes(&job.session_id).await?;
        if let Some(n) = existing
            .iter()
            .find(|n| n.from_seq == Some(job.from_seq) && n.to_seq == Some(job.to_seq))
        {
            self.history
                .mark_compacted(&job.session_id, job.chunk_from_seq, job.to_seq)
                .await?;
            return Ok(n.id.clone());
        }

        let rendered = render_summary(
            validated,
            &crate::anchors::render(&job.anchors),
            &job.verbatim_users,
        );
        let tokens_self = self.estimator.text_tokens(model_id, &rendered);

        let id = match &job.previous_node_id {
            Some(prev) => {
                if !existing.iter().any(|n| &n.id == prev) {
                    return Err(penelope_store::StoreError::other(format!(
                        "résumé périmé : le nœud {prev} a changé depuis la préparation"
                    )));
                }
                self.lcm
                    .extend(
                        prev,
                        job.to_seq,
                        job.tokens_src,
                        &rendered,
                        &job.anchors,
                        tokens_self,
                    )
                    .await?
            }
            None => {
                if existing
                    .iter()
                    .any(|n| n.to_seq.is_some_and(|t| t >= job.chunk_from_seq))
                {
                    return Err(penelope_store::StoreError::other(
                        "résumé périmé : un autre résumé couvre déjà ces messages",
                    ));
                }
                self.lcm
                    .insert_leaf(
                        &job.session_id,
                        job.chunk_from_seq,
                        job.to_seq,
                        &rendered,
                        &job.anchors,
                        job.tokens_src,
                        tokens_self,
                    )
                    .await?
            }
        };

        self.history
            .mark_compacted(&job.session_id, job.chunk_from_seq, job.to_seq)
            .await?;
        Ok(id)
    }
}
