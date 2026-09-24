//! Écritures du moteur de contexte dans le canonique : l'admission d'un groupe de
//! résultats d'outils (niveau 1) et la publication d'un résumé (niveau 3).

use crate::compaction::*;
use crate::engine::{ContextEngine, SummaryJob};
use crate::journal::{ConvEvent, SummaryPayload, SurfaceOp};
use crate::lcm::{NodeWrite, insert_leaf_in, replace_in};
use crate::store::mark_compacted_in;
use penelope_store::rusqlite::Transaction;

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
        self.publish_summary(job, validated, model_id, None).await
    }

    /// [`apply_summary`](Self::apply_summary), avec ce qui a déclenché la compaction
    /// (`manual`, `background`…), que porte le `conv.summary`.
    pub async fn apply_summary_as(
        &self,
        job: &SummaryJob,
        validated: &serde_json::Value,
        model_id: &str,
        trigger: &str,
    ) -> penelope_store::Result<String> {
        self.publish_summary(job, validated, model_id, Some(trigger))
            .await
    }

    /// La publication, avec ou sans déclencheur.
    ///
    /// Journal attaché, le résumé devient un `conv.summary` qui remplace la plage couverte
    /// (T7) : l'événement d'abord, puis, dans la seconde transaction, le nœud (qui cite
    /// l'événement par `event_id`) et le marquage de la plage, ensemble. Un événement
    /// déjà écrit pour ce travail (`SummaryJob::idempotency_key`) n'est pas réécrit : un
    /// arrêt entre l'événement et le nœud se répare en écrivant le nœud qu'il annonce.
    async fn publish_summary(
        &self,
        job: &SummaryJob,
        validated: &serde_json::Value,
        model_id: &str,
        trigger: Option<&str>,
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

        match &job.previous_node_id {
            Some(prev) => {
                if !existing.iter().any(|n| &n.id == prev) {
                    return Err(penelope_store::StoreError::other(format!(
                        "résumé périmé : le nœud {prev} a changé depuis la préparation"
                    )));
                }
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
            }
        }

        let sid = job.session_id.clone();
        let mut node = self
            .lcm
            .node(&sid, &rendered, &job.anchors, job.tokens_src, tokens_self);
        let key = job.idempotency_key();
        let place = Placement {
            session_id: sid.clone(),
            previous: job.previous_node_id.clone(),
            chunk_from: job.chunk_from_seq,
            to: job.to_seq,
            added_src: job.tokens_src,
        };
        if let Some((event_id, node_id)) = self.history.summary_event(&sid, &key).await? {
            if self.lcm.get(&node_id).await?.is_none() {
                node.id = node_id.clone();
                node.event_id = Some(event_id);
                self.history
                    .store()
                    .write(move |tx| place.write(tx, &node))
                    .await?;
            }
            return Ok(node_id);
        }
        let event = match self.history.journals() {
            true => Some(ConvEvent::Summary(SummaryPayload {
                surface: SurfaceOp::Replace {
                    from: self.history.address(&sid, job.from_seq).await?,
                    to: self.history.address(&sid, job.to_seq).await?,
                },
                node_id: node.id.clone(),
                previous_node_id: job.previous_node_id.clone(),
                summary: rendered,
                anchors: job.anchors.clone(),
                verbatim_users: job.verbatim_users.clone(),
                model: Some(model_id.to_string()),
                tokens_src: job.tokens_src,
                tokens_self,
                batches_left: job.remaining_batches() as u64,
                trigger: trigger.map(String::from),
                idempotency_key: Some(key),
            })),
            false => None,
        };
        let id = node.id.clone();
        self.history
            .journaled(&sid, event, move |tx, event_id| {
                node.event_id = event_id;
                place.write(tx, &node)
            })
            .await?;
        Ok(id)
    }
}

/// Où un résumé se range : prolongation du nœud précédent, ou feuille neuve.
struct Placement {
    session_id: String,
    previous: Option<String>,
    chunk_from: i64,
    to: i64,
    added_src: u64,
}

impl Placement {
    /// Le nœud et le marquage de la plage, dans une seule transaction.
    fn write(&self, tx: &Transaction<'_>, node: &NodeWrite) -> penelope_store::Result<()> {
        match &self.previous {
            Some(prev) => replace_in(tx, prev, Some((self.to, self.added_src)), node)?,
            None => insert_leaf_in(tx, node, self.chunk_from, self.to)?,
        }
        mark_compacted_in(tx, &self.session_id, self.chunk_from, self.to)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
