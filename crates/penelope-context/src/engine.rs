//! Moteur de contexte : orchestration des niveaux 0 à 4 et du LCM (§5).

use crate::compaction::*;
use crate::lcm::Lcm;
use crate::store::HistoryStore;
use crate::tiers::Tiers;
use crate::transcript::{Entry, repair_pairs};
use penelope_kernel::clock::SharedClock;
use penelope_llm::catalog::Catalog;
use penelope_llm::tokens::TokenEstimator;
use penelope_llm::types::{ChatMessage, Role};
use serde::{Deserialize, Serialize};

/// Contexte assemblé pour un tour.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub messages: Vec<ChatMessage>,
    pub tokens: u64,
    pub steps: Vec<AppliedStep>,
    pub prefix_hash: String,
    /// La compaction de fond (niveau 3) devrait démarrer en parallèle.
    pub needs_background_compaction: bool,
    /// La requête tient dans la fenêtre.
    pub fits: bool,
}

/// Travail de résumé à confier au modèle (rôle `summarizer`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryJob {
    pub session_id: String,
    pub from_seq: i64,
    pub to_seq: i64,
    /// Transcript à résumer, déjà mis en forme.
    pub source_text: String,
    /// Résumé précédent à **mettre à jour** plutôt qu'à refaire (§5.4).
    pub previous_summary: Option<String>,
    pub previous_node_id: Option<String>,
    pub anchors: Vec<crate::anchors::Anchor>,
    pub verbatim_users: Vec<String>,
    pub tokens_src: u64,
    /// Découpage en lots si la fenêtre du résumeur est trop petite.
    pub batches: Vec<(i64, i64)>,
}

impl SummaryJob {
    /// Clé d'idempotence : la publication d'un résumé pour la même couverture et le même
    /// contenu source ne doit créer qu'un seul nœud.
    pub fn idempotency_key(&self) -> String {
        penelope_kernel::canonical::sha256_hex(
            format!(
                "{}|{}|{}|{}",
                self.session_id,
                self.from_seq,
                self.to_seq,
                penelope_kernel::canonical::sha256_hex(self.source_text.as_bytes())
            )
            .as_bytes(),
        )
    }
}

#[derive(Clone)]
pub struct ContextEngine {
    pub history: HistoryStore,
    pub lcm: Lcm,
    pub estimator: TokenEstimator,
    pub catalog: Catalog,
    #[allow(dead_code)]
    clock: SharedClock,
}

impl ContextEngine {
    pub fn new(
        history: HistoryStore,
        lcm: Lcm,
        estimator: TokenEstimator,
        catalog: Catalog,
        clock: SharedClock,
    ) -> Self {
        ContextEngine {
            history,
            lcm,
            estimator,
            catalog,
            clock,
        }
    }

    /// Construit la projection de requête d'un tour.
    ///
    /// Ordre : historique canonique → niveaux 0 et 2 si ça ne tient pas → réparation des
    /// paires → assemblage des tiers. Le canonique n'est **jamais** modifié ici.
    pub async fn build_request(
        &self,
        session_id: &str,
        tiers: &Tiers,
        params: &CompactionParams,
        model_id: &str,
        anthropic_cache: bool,
    ) -> penelope_store::Result<TurnContext> {
        let entries = self.history.load(session_id, 0).await?;
        Ok(self.build_from_entries(&entries, tiers, params, model_id, anthropic_cache))
    }

    /// Variante pure, sans base : c'est elle que testent les suites déterministes.
    pub fn build_from_entries(
        &self,
        entries: &[Entry],
        tiers: &Tiers,
        params: &CompactionParams,
        model_id: &str,
        anthropic_cache: bool,
    ) -> TurnContext {
        let est_one = |m: &ChatMessage| self.estimator.message_tokens(model_id, m);
        let est_all = |ms: &[ChatMessage]| ms.iter().map(&est_one).sum::<u64>();

        let mut projection: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
        let prefix_tokens = self.estimator.text_tokens(model_id, &tiers.prefix())
            + self.estimator.text_tokens(model_id, &tiers.volatile);
        let mut steps = Vec::new();
        let mut body_tokens = est_all(&projection);
        let limit = params.window.saturating_sub(reserved_output(params.window));

        // Frontière protégée : la queue verbatim n'est jamais dégradée.
        let (protect_from, _) = split_for_summary(entries, params);

        if prefix_tokens + body_tokens > limit {
            let before = body_tokens;
            let touched = level0_micro(entries, &mut projection, protect_from);
            body_tokens = est_all(&projection);
            if touched > 0 {
                steps.push(AppliedStep {
                    level: 0,
                    label: "micro-compaction des résultats volatils".into(),
                    before_tokens: before,
                    after_tokens: body_tokens,
                    touched,
                });
            }
        }

        if prefix_tokens + body_tokens > limit {
            let before = body_tokens;
            let target = limit.saturating_sub(prefix_tokens);
            let (after, touched) = level2_degrade(
                entries,
                &mut projection,
                protect_from,
                target,
                body_tokens,
                &est_one,
            );
            body_tokens = after;
            if touched > 0 {
                steps.push(AppliedStep {
                    level: 2,
                    label: "dégradation des anciens résultats".into(),
                    before_tokens: before,
                    after_tokens: body_tokens,
                    touched,
                });
            }
        }

        // Invariant protocolaire : toujours des paires valides dans la requête.
        let repaired = repair_pairs(projection);
        let messages = tiers.assemble(repaired, anthropic_cache);
        let tokens = est_all(&messages);

        TurnContext {
            fits: tokens <= limit,
            needs_background_compaction: tokens >= params.background_threshold_tokens(0.10),
            tokens,
            steps,
            prefix_hash: tiers.prefix_hash(),
            messages,
        }
    }

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

    /// Prépare le travail de résumé (niveau 3).
    pub async fn prepare_summary(
        &self,
        session_id: &str,
        params: &CompactionParams,
        summarizer_window: u64,
        model_id: &str,
    ) -> penelope_store::Result<Option<SummaryJob>> {
        let entries = self.history.load(session_id, 0).await?;
        if entries.len() < 4 {
            return Ok(None);
        }
        let (boundary, _) = split_for_summary(&entries, params);
        if boundary == 0 {
            return Ok(None);
        }
        let to_summarise = &entries[..boundary];
        let from_seq = to_summarise.first().map(|e| e.seq).unwrap_or(1);
        let to_seq = to_summarise.last().map(|e| e.seq).unwrap_or(1);

        let source_text = render_transcript(to_summarise);
        let texts: Vec<&str> = to_summarise
            .iter()
            .map(|e| e.message.text())
            .collect::<Vec<String>>()
            .iter()
            .map(|s| Box::leak(s.clone().into_boxed_str()) as &str)
            .collect();
        let anchors = crate::anchors::extract_many(&texts, 120);
        let verbatim_users = select_verbatim_users(to_summarise, params.tail_budget() / 8);
        let tokens_src: u64 = to_summarise.iter().map(|e| e.tokens).sum();

        // §5.4 : la fenêtre du résumeur DOIT être ≥ à l'entrée. Sinon, découpage en lots
        // avec échantillonnage explicite, jamais d'abandon silencieux.
        let source_tokens = self.estimator.text_tokens(model_id, &source_text);
        let batches = if source_tokens > summarizer_window.saturating_sub(2_000) {
            batch_ranges(
                to_summarise,
                summarizer_window.saturating_sub(2_000),
                model_id,
                &self.estimator,
            )
        } else {
            Vec::new()
        };

        // Résumé précédent à mettre à jour, s'il existe.
        let active = self.lcm.active_nodes(session_id).await?;
        let previous = active.last().cloned();

        Ok(Some(SummaryJob {
            session_id: session_id.to_string(),
            from_seq,
            to_seq,
            source_text,
            previous_summary: previous.as_ref().map(|n| n.summary.clone()),
            previous_node_id: previous.map(|n| n.id),
            anchors,
            verbatim_users,
            tokens_src,
            batches,
        }))
    }

    /// Publie un résumé validé. **Idempotent** : republier le même travail ne crée pas un
    /// second nœud (CA 5).
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
            return Ok(n.id.clone());
        }

        let rendered = render_summary(
            validated,
            &crate::anchors::render(&job.anchors),
            &job.verbatim_users,
        );
        let tokens_self = self.estimator.text_tokens(model_id, &rendered);

        let id = match &job.previous_node_id {
            // Re-compaction : on met à jour le nœud précédent s'il couvre le même début.
            Some(prev)
                if existing
                    .iter()
                    .any(|n| &n.id == prev && n.from_seq == Some(job.from_seq)) =>
            {
                self.lcm
                    .supersede(prev, &rendered, &job.anchors, tokens_self)
                    .await?
            }
            _ => {
                self.lcm
                    .insert_leaf(
                        &job.session_id,
                        job.from_seq,
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
            .mark_compacted(&job.session_id, job.from_seq, job.to_seq)
            .await?;
        Ok(id)
    }

    /// Niveau 4 — urgence, après une erreur `context_length` du provider.
    pub fn emergency(
        &self,
        messages: Vec<ChatMessage>,
        limit_tokens: u64,
        model_id: &str,
    ) -> Result<(Vec<ChatMessage>, u64), String> {
        let est_all = |ms: &[ChatMessage]| {
            ms.iter()
                .map(|m| self.estimator.message_tokens(model_id, m))
                .sum::<u64>()
        };
        let (out, fits) = level4_emergency(messages, limit_tokens, &est_all);
        if !fits {
            return Err("impossible de faire tenir la requête, même réduite au minimum".into());
        }
        let tokens = proof_of_fit(&out, limit_tokens, &est_all)?;
        Ok((out, tokens))
    }

    /// Reconstruit le contexte actif : résumés de plus haut niveau + queue verbatim.
    pub async fn active_context(
        &self,
        session_id: &str,
        params: &CompactionParams,
    ) -> penelope_store::Result<Vec<ChatMessage>> {
        let entries = self.history.load(session_id, 0).await?;
        let nodes = self.lcm.active_nodes(session_id).await?;
        let covered_to = nodes.iter().filter_map(|n| n.to_seq).max().unwrap_or(0);

        let mut out = Vec::new();
        for n in &nodes {
            out.push(ChatMessage::system(n.summary.clone()));
        }
        let (boundary, _) = split_for_summary(&entries, params);
        let start = entries
            .iter()
            .position(|e| e.seq > covered_to)
            .unwrap_or(boundary);
        out.extend(entries[start..].iter().map(|e| e.message.clone()));
        Ok(repair_pairs(out))
    }
}

/// Tokens réservés à la sortie : on ne remplit jamais la fenêtre jusqu'au bord.
fn reserved_output(window: u64) -> u64 {
    (window / 10).clamp(1_000, 32_000)
}

/// Met le transcript en forme pour le résumeur.
pub fn render_transcript(entries: &[Entry]) -> String {
    let mut s = String::new();
    for e in entries {
        let who = match e.message.role {
            Role::User => "UTILISATEUR",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "OUTIL",
            Role::System => "SYSTÈME",
        };
        s.push_str(&format!("[{} #{}] ", who, e.seq));
        if !e.message.tool_calls.is_empty() {
            let names: Vec<&str> = e
                .message
                .tool_calls
                .iter()
                .map(|t| t.name.as_str())
                .collect();
            s.push_str(&format!("(appelle {}) ", names.join(", ")));
        }
        s.push_str(&e.message.text());
        s.push('\n');
    }
    s
}

/// Découpe en lots quand la fenêtre du résumeur est plus petite que l'entrée.
fn batch_ranges(
    entries: &[Entry],
    budget: u64,
    model_id: &str,
    est: &TokenEstimator,
) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut start = entries.first().map(|e| e.seq).unwrap_or(1);
    let mut acc = 0u64;
    for e in entries {
        let t = est.message_tokens(model_id, &e.message);
        if acc + t > budget && acc > 0 {
            out.push((start, e.seq - 1));
            start = e.seq;
            acc = 0;
        }
        acc += t;
    }
    if let Some(last) = entries.last() {
        out.push((start, last.seq));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiers::TiersBuilder;
    use crate::transcript::pairs_are_valid;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::types::ToolCall;
    use penelope_store::Store;
    use serde_json::json;
    use std::sync::Arc;

    async fn engine() -> ContextEngine {
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','chat','t','t')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let clock: SharedClock = Arc::new(TestClock::default());
        ContextEngine::new(
            HistoryStore::new(store.clone(), clock.clone()),
            Lcm::new(store, clock.clone()),
            TokenEstimator::new(),
            Catalog::new(),
            clock,
        )
    }

    fn params(window: u64) -> CompactionParams {
        CompactionParams {
            window,
            threshold: 0.70,
            tail_ratio: 0.025,
            tail_min_tokens: 1_000,
            tail_max_tokens: 25_000,
            min_tail_user_messages: 2,
            max_tool_result_share: 0.25,
            large_payload_tokens: 25_000,
        }
    }

    fn tiers() -> Tiers {
        TiersBuilder::new().soul("Pénélope.").build()
    }

    fn tool_pair(seq: i64, id: &str, body: &str, tokens: u64) -> Vec<Entry> {
        vec![
            Entry::new(
                seq,
                ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                    id: id.into(),
                    name: "fs_read".into(),
                    arguments: json!({}),
                }]),
                10,
            ),
            Entry::new(
                seq + 1,
                ChatMessage::tool_result(id, "fs_read", body),
                tokens,
            )
            .eager(true),
        ]
    }

    /// `ctx-safety` — niveau 0 : les résultats volatils anciens deviennent des stubs,
    /// le canonique est intact, les paires restent valides.
    #[tokio::test]
    async fn ctx_safety_level0() {
        let e = engine().await;
        let big = "x".repeat(200_000);
        let mut entries = vec![Entry::new(1, ChatMessage::user("analyse"), 10)];
        entries.extend(tool_pair(2, "c1", &big, 50_000));
        entries.push(Entry::new(4, ChatMessage::user("et ensuite ?"), 10));
        entries.push(Entry::new(5, ChatMessage::user("alors ?"), 10));

        let ctx = e.build_from_entries(&entries, &tiers(), &params(20_000), "m", false);
        assert!(
            ctx.steps.iter().any(|s| s.level == 0),
            "le niveau 0 doit s'appliquer : {:?}",
            ctx.steps
        );
        assert!(pairs_are_valid(&ctx.messages[1..]));
        assert_eq!(
            entries[2].message.text().len(),
            200_000,
            "le canonique n'est pas modifié"
        );
    }

    /// `ctx-safety` — niveau 1 : admission sous budget, externalisation en artefact.
    #[tokio::test]
    async fn ctx_safety_level1() {
        let e = engine().await;
        let big = "y".repeat(500_000);
        e.history
            .append(
                "s1",
                &ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    arguments: json!({}),
                }]),
                10,
                0,
                false,
                None,
            )
            .await
            .unwrap();
        let seq = e
            .history
            .append(
                "s1",
                &ChatMessage::tool_result("c1", "fs_read", &big),
                140_000,
                0,
                false,
                None,
            )
            .await
            .unwrap();

        let steps = e
            .admit_tool_group("s1", &[(seq, big.clone())], &params(100_000), "m")
            .await
            .unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].level, 1);
        assert!(steps[0].after_tokens < steps[0].before_tokens);

        let entries = e.history.load("s1", 0).await.unwrap();
        let body = entries[1].message.text();
        assert!(body.contains("artifact_read"), "{body:.200}");
        assert!(entries[1].artifact_id.is_some());

        // Le contenu complet reste lisible depuis l'artefact.
        let id = entries[1].artifact_id.clone().unwrap();
        let (chunk, _, _) = e.history.read_artifact(&id, 0, 100).await.unwrap().unwrap();
        assert_eq!(chunk.len(), 100);
    }

    /// `ctx-safety` — niveau 2 : dégradation progressive, queue protégée.
    #[tokio::test]
    async fn ctx_safety_level2() {
        let e = engine().await;
        let mut entries = Vec::new();
        for i in 0..12 {
            entries.push(Entry::new(
                i * 2 + 1,
                ChatMessage::user(format!("q{i}")),
                20,
            ));
            entries.push(Entry::new(
                i * 2 + 2,
                ChatMessage::tool_result(format!("c{i}"), "t", "z".repeat(8000)),
                2200,
            ));
        }
        // Les résultats sans appel sont réparés ; on vérifie surtout la réduction.
        let ctx = e.build_from_entries(&entries, &tiers(), &params(10_000), "m", false);
        assert!(
            ctx.steps.iter().any(|s| s.level == 2),
            "le niveau 2 doit s'appliquer : {:?}",
            ctx.steps
        );
        assert!(pairs_are_valid(&ctx.messages[1..]));
    }

    /// `ctx-safety` — niveau 3 : résumé publié une seule fois, même republié.
    #[tokio::test]
    async fn ctx_safety_level3_publication_is_idempotent() {
        let e = engine().await;
        for i in 0..30 {
            e.history
                .append(
                    "s1",
                    &ChatMessage::user(format!("message {i}")),
                    500,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
            e.history
                .append(
                    "s1",
                    &ChatMessage::assistant("réponse"),
                    500,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        let p = params(40_000);
        let job = e
            .prepare_summary("s1", &p, 128_000, "m")
            .await
            .unwrap()
            .expect("un travail de résumé");
        assert!(job.from_seq >= 1 && job.to_seq > job.from_seq);

        let validated = json!({
            "objectif": "suivre la conversation",
            "fait": "30 échanges",
            "en_cours": "rien",
            "prochaines_etapes": "continuer"
        });
        let a = e.apply_summary(&job, &validated, "m").await.unwrap();
        let b = e.apply_summary(&job, &validated, "m").await.unwrap();
        assert_eq!(a, b, "republier le même résumé ne crée pas un second nœud");
        assert_eq!(e.lcm.count("s1").await.unwrap(), 1);

        let active = e.active_context("s1", &p).await.unwrap();
        assert!(active[0].text().contains("### Objectif"));
        assert!(pairs_are_valid(&active));
    }

    /// `ctx-safety` — reprise après crash : l'historique persistant suffit à reconstruire
    /// exactement la même projection.
    #[tokio::test]
    async fn ctx_safety_recovery_after_crash() {
        let e = engine().await;
        for i in 0..10 {
            e.history
                .append(
                    "s1",
                    &ChatMessage::user(format!("m{i}")),
                    100,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        let p = params(100_000);
        let t = tiers();
        let a = e.build_request("s1", &t, &p, "m", false).await.unwrap();

        // « kill -9 » : nouveau moteur sur la même base.
        let e2 = ContextEngine::new(
            HistoryStore::new(e.history.store().clone(), Arc::new(TestClock::default())),
            Lcm::new(e.history.store().clone(), Arc::new(TestClock::default())),
            TokenEstimator::new(),
            Catalog::new(),
            Arc::new(TestClock::default()),
        );
        let b = e2.build_request("s1", &t, &p, "m", false).await.unwrap();
        assert_eq!(
            a.messages, b.messages,
            "la projection doit être reproductible"
        );
        assert_eq!(a.prefix_hash, b.prefix_hash);
    }

    /// `ctx-safety` — niveau 4 : preuve locale avant envoi.
    #[tokio::test]
    async fn ctx_safety_level4() {
        let e = engine().await;
        let long = "w".repeat(80_000);
        let mut msgs = vec![ChatMessage::system("règles")];
        for i in 0..6 {
            msgs.push(ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: format!("c{i}"),
                name: "t".into(),
                arguments: json!({}),
            }]));
            msgs.push(ChatMessage::tool_result(format!("c{i}"), "t", long.clone()));
        }
        msgs.push(ChatMessage::user("dernière question"));

        let (out, tokens) = e.emergency(msgs, 3_000, "m").unwrap();
        assert!(tokens <= 3_000);
        assert!(pairs_are_valid(&out));
        assert!(out.iter().any(|m| m.text() == "dernière question"));
    }

    #[tokio::test]
    async fn summary_job_batches_when_summarizer_window_is_too_small() {
        let e = engine().await;
        for i in 0..40 {
            e.history
                .append(
                    "s1",
                    &ChatMessage::user("x".repeat(2000)),
                    600,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
            let _ = i;
        }
        let job = e
            .prepare_summary("s1", &params(40_000), 4_000, "m")
            .await
            .unwrap()
            .unwrap();
        assert!(
            !job.batches.is_empty(),
            "une fenêtre de résumeur trop petite impose un découpage explicite"
        );
        let covered: i64 = job.batches.iter().map(|(a, b)| b - a + 1).sum();
        assert!(
            covered >= job.to_seq - job.from_seq,
            "aucun lot n'est abandonné"
        );
    }

    #[tokio::test]
    async fn summary_job_key_is_stable() {
        let e = engine().await;
        for i in 0..10 {
            e.history
                .append(
                    "s1",
                    &ChatMessage::user(format!("m{i}")),
                    500,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        let a = e
            .prepare_summary("s1", &params(6_000), 128_000, "m")
            .await
            .unwrap();
        let b = e
            .prepare_summary("s1", &params(6_000), 128_000, "m")
            .await
            .unwrap();
        assert_eq!(
            a.as_ref().map(|j| j.idempotency_key()),
            b.as_ref().map(|j| j.idempotency_key())
        );
    }

    #[tokio::test]
    async fn short_session_has_nothing_to_summarise() {
        let e = engine().await;
        e.history
            .append("s1", &ChatMessage::user("bonjour"), 5, 0, false, None)
            .await
            .unwrap();
        assert!(
            e.prepare_summary("s1", &params(100_000), 128_000, "m")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn background_compaction_flag_trips_ten_points_early() {
        let e = engine().await;
        let entries: Vec<Entry> = (0..40)
            .map(|i| Entry::new(i, ChatMessage::user("x".repeat(1200)), 350))
            .collect();
        let ctx = e.build_from_entries(&entries, &tiers(), &params(20_000), "m", false);
        assert!(
            ctx.needs_background_compaction,
            "à 60 % de la fenêtre, la compaction de fond doit être demandée ({} tokens)",
            ctx.tokens
        );
    }

    #[test]
    fn transcript_rendering_marks_roles_and_calls() {
        let entries = vec![
            Entry::new(1, ChatMessage::user("fais X"), 5),
            Entry::new(
                2,
                ChatMessage::assistant("ok").with_tool_calls(vec![ToolCall {
                    id: "c".into(),
                    name: "fs_read".into(),
                    arguments: json!({}),
                }]),
                5,
            ),
        ];
        let t = render_transcript(&entries);
        assert!(t.contains("[UTILISATEUR #1]"));
        assert!(t.contains("(appelle fs_read)"));
    }
}
