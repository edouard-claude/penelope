//! Consolidation nocturne (« dreaming », §6.8), digest du matin et entretien du vault.
//!
//! Light : candidats depuis la dernière passe, regroupés et dédoublonnés. REM : thèmes
//! récurrents, écrits dans `DREAMS.md`. Deep : portes déterministes, puis le modèle du
//! rôle `compaction` propose des **opérations**, validées structurellement et appliquées
//! ligne par ligne, pré-image conservée dans `mem_history`. Une passe à la fois ; deux
//! passes sans nouvelle donnée ne changent rien.

use crate::ports::McpAdmin;
use crate::ports::Messenger;
use crate::ports::Slot;
use crate::{Context, DigestInputs, DigestSource};
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::{ChatMessage, ChatRequest, LlmError, LlmErrorKind};
use penelope_memory::candidates::{Candidate, CandidateGroup, group};
use penelope_memory::consolidation::{
    DreamReport, Gate, Operation, PromotionGates, ValidationContext, gate, validate,
};
use penelope_memory::edit;
use penelope_memory::vault::{Annotations, Practice, VaultEntry, When};
use penelope_memory::{IndexedEntry, Level, Origin, Provenance};
use penelope_store::rusqlite::params;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Verrou de passe : au-delà, une passe interrompue est considérée morte.
const LOCK_TTL_MS: i64 = 2 * 3_600_000;
const LOCK_KEY: &str = "dream.lock";
/// Débit prudent d'un modèle de consolidation, en tokens par seconde. Mesuré le 21/09 :
/// 15 344 tokens en 306 s (50/s), 19 700 en 110 s (179/s). Le plancher sert à *majorer*
/// le temps nécessaire, pas à le prédire.
const TOKENS_PER_SEC: f64 = 15.0;
/// Bornes du délai dérivé : assez long pour un lot qui réfléchit vraiment, assez court
/// pour ne pas immobiliser la nuit sur un seul appel.
const CALL_TIMEOUT_MIN: Duration = Duration::from_secs(240);
const CALL_TIMEOUT_MAX: Duration = Duration::from_secs(900);

/// Combien de temps laisser à un appel qui doit rendre `max_tokens` au plus.
///
/// 240 s fixes tuaient tout appel qui réfléchissait vraiment (issue #152) : avec 8 000 à
/// 16 000 tokens de raisonnement autorisés, un lot demande cinq à onze minutes. L'appel
/// était tué à quatre, classé « réseau coupé », rejoué après 300 s, indéfiniment.
fn call_timeout(max_tokens: u32, remaining: Duration) -> Duration {
    let derived = Duration::from_secs_f64(f64::from(max_tokens) / TOKENS_PER_SEC);
    derived
        .clamp(CALL_TIMEOUT_MIN, CALL_TIMEOUT_MAX)
        // Jamais au-delà de ce qu'il reste à la passe : un appel qui déborderait la nuit
        // ne sert à rien.
        .min(remaining.max(CALL_TIMEOUT_MIN))
}
/// Taille maximale d'un fichier cible montré au modèle, en caractères.
const FILE_EXCERPT_CHARS: usize = 6_000;
/// Fichiers où le modèle peut ajouter une entrée.
const WRITABLE_FILES: &[&str] = &["profil.md", "memoire.md", "projets.md", "notes.md"];

/// Issue d'une passe.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DreamOutcome {
    pub run_id: String,
    pub dry_run: bool,
    pub report: DreamReport,
}

// ------------------------------------------------------------------ passe

/// Lance une passe de consolidation. `dry_run` : rien n'est écrit, le rapport dit ce qui
/// serait fait.
/// Qui a lancé la passe : une passe planifiée ne répète pas le même message d'échec
/// chaque nuit, une passe lancée à la main rend toujours compte — le 21/09 elle a échoué
/// en silence, et le propriétaire a dû demander (issue #152).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Scheduled,
    Manual,
}

/// `messenger` : le canal où dire un échec, lu au moment de l'échec.
pub async fn run(
    d: &Context,
    messenger: &Slot<dyn Messenger>,
    dry_run: bool,
) -> anyhow::Result<DreamOutcome> {
    run_as(d, messenger, dry_run, Trigger::Manual).await
}

pub async fn run_as(
    d: &Context,
    messenger: &Slot<dyn Messenger>,
    dry_run: bool,
    trigger: Trigger,
) -> anyhow::Result<DreamOutcome> {
    let s = &d.services;
    let now = s.clock.now_ms();
    if !dry_run {
        if let Some(held) = s
            .kv_get(LOCK_KEY)
            .await?
            .and_then(|v| v.parse::<i64>().ok())
            && now - held < LOCK_TTL_MS
        {
            anyhow::bail!("une consolidation est déjà en cours");
        }
        s.kv_set(LOCK_KEY, &now.to_string()).await?;
    }
    let result = run_locked(d, dry_run).await;
    if !dry_run {
        let _ = s.kv_delete(LOCK_KEY).await;
    }
    match &result {
        Ok(_) if !dry_run => {
            let _ = s.kv_delete(FAILED_NIGHTS_KEY).await;
            let _ = s.kv_delete(FAILED_REASON_KEY).await;
        }
        // L'échec part au foyer d'où que vienne la passe : planifiée, il ne se répète pas
        // de nuit en nuit ; lancée à la main, il part toujours (issue #152).
        Err(e) if !dry_run => {
            failure_reported(d, messenger, &e.to_string(), trigger == Trigger::Manual).await;
        }
        _ => {}
    }
    result
}

#[allow(clippy::too_many_lines)] // gel 0.17 : phases de la nuit, lot G (dream/mod.rs)
async fn run_locked(d: &Context, dry_run: bool) -> anyhow::Result<DreamOutcome> {
    let s = &d.services;
    let cfg = s.config.config();
    let vault = crate::helpers::vault_dir(s);
    let run_id = format!("d_{}", penelope_kernel::ids::Ulid::new());
    let started = s.clock.now_rfc3339();
    let pass_started = std::time::Instant::now();
    // Temps de la nuit : au-delà, un lot coupé par le réseau n'est plus rejoué et la
    // passe rend la main avec ce qu'elle a écrit (issue #152). C'est la même borne que
    // le verrou de passe.
    let deadline = pass_started + Duration::from_millis(LOCK_TTL_MS as u64);
    let since = last_finished_start(s).await?;
    if !dry_run {
        record_run(s, &run_id, &started, "light", since.as_deref()).await?;
    }
    let mut report = DreamReport::default();
    // Passe précédente tuée en vol (machine endormie, démon arrêté) : elle est close en
    // `interrupted` avec ce qu'elle avait écrit, et la nuit reprend sur les candidats
    // restants — ils sont encore `new` ou `deferred`, rien n'est à rejouer (issue #152).
    if !dry_run && let Some((prev, lots, promoted)) = close_interrupted(s, &run_id).await? {
        report.warnings.push(format!(
            "reprise après la passe `{prev}` interrompue : {lots} lot(s) déjà écrit(s), \
             {promoted} entrée(s) gardée(s), les candidats restants repassent ici"
        ));
    }

    let outcome = async {
        // ---------------------------------------------------------------- Light
        // Décisions des notes de travail : candidats, jamais réécrits (issue #32).
        if !dry_run {
            let now = s.clock.now_rfc3339();
            let mut harvested: Vec<Candidate> = Vec::new();
            let mut sources: Vec<(String, String)> = Vec::new();
            for (session, text) in crate::session_notes::harvest(s).await? {
                // Genre de la session d'origine : une décision notée garde la provenance
                // de la session où elle a été prise, sinon elle est filtrée sans un mot
                // (issue #61).
                let kind = s
                    .sessions
                    .get(&session)
                    .await?
                    .map(|x| x.kind.as_str().to_string())
                    .unwrap_or_else(|| "interactive".into());
                // Une session qui n'autorise pas de candidat (planifiée, sous-agent) ne
                // passe pas par ce détour : la décision n'est ni enregistrée ni marquée.
                if !penelope_memory::provenance::session_allows_candidate(&kind, "decision", true) {
                    continue;
                }
                harvested.push(
                    Candidate::new(
                        penelope_memory::CandidateType::Decision,
                        &text,
                        Origin::Agent,
                        &kind,
                        &now,
                    )
                    .in_session(&session)
                    .with_importance(6),
                );
                sources.push((session, text));
            }
            if !harvested.is_empty() {
                // Pas de plafond de tour ici : ce sont les décisions de toute une journée.
                let all = harvested.len();
                s.candidates.record(harvested, all).await?;
                for (session, text) in &sources {
                    crate::session_notes::mark_harvested(s, session, text).await?;
                }
            }
        }
        let candidates = s.candidates.pending(None).await?;
        report.candidates_seen = candidates.len() as u32;
        let groups = group(candidates, cfg.memory.dedup_jaccard);
        report.groups = groups.len() as u32;
        if !dry_run {
            set_phase(s, &run_id, "rem").await?;
        }

        // ---------------------------------------------------------------- REM
        for g in &groups {
            if g.occurrences >= 3 && g.distinct_sessions >= 2 {
                report.reflections.push(format!(
                    "« {} » revient {} fois dans {} sessions",
                    g.representative.text, g.occurrences, g.distinct_sessions
                ));
            }
        }
        if !dry_run {
            set_phase(s, &run_id, "deep").await?;
        }

        // ---------------------------------------------------------------- Deep
        let gates = PromotionGates::from_config(&cfg.memory.promotion);
        let day = today(s);
        let mut admitted: Vec<&CandidateGroup> = Vec::new();
        // Verdicts de la grille : ils ne dépendent d'aucun appel au modèle, et sont
        // écrits avant le premier lot (issue #152).
        let mut gated: Vec<(Vec<String>, &'static str, Option<String>)> = Vec::new();
        for g in &groups {
            let ids: Vec<String> = g.members.iter().map(|m| m.id.clone()).collect();
            for name in crate::secret_shelf::references(&g.representative.text) {
                if !report.secrets.contains(&name) {
                    report.secrets.push(name);
                }
            }
            match gate(g, &gates) {
                Gate::Promote | Gate::Sort => admitted.push(g),
                Gate::Propose(reason) => {
                    report.proposals += 1;
                    report.questions.push(format!(
                        "Proposition : « {} » ({reason})",
                        short(&g.representative.text)
                    ));
                    gated.push((ids, "proposed", Some(reason)));
                }
                Gate::Reject(reason) => {
                    report
                        .rejected
                        .push(format!("« {} » : {reason}", short(&g.representative.text)));
                    gated.push((ids, "rejected", Some(reason)));
                }
            }
        }

        if !dry_run {
            for (ids, state, reason) in &gated {
                s.candidates
                    .set_state(ids, state, reason.as_deref())
                    .await?;
            }
        }

        if !admitted.is_empty() {
            let mut snapshot = VaultSnapshot::read(s, &vault).await?;
            // Souvenirs proches : un seul appel d'embeddings pour tout le lot (issue #59).
            let nearby_all = nearby_batch(
                d,
                &admitted
                    .iter()
                    .map(|g| g.representative.text.clone())
                    .collect::<Vec<_>>(),
            )
            .await;
            let mut items: Vec<Item<'_>> = Vec::new();
            for (i, g) in admitted.iter().enumerate() {
                items.push(Item {
                    group: g,
                    nearby: nearby_all.get(i).cloned().unwrap_or_default(),
                });
            }

            // Lots bornés : au-delà, la réponse du modèle ne tient pas et tout le lot
            // repart en attente, nuit après nuit (issue #59). La taille suit ce qui a
            // tenu, et la sortie demandée suit ce que le modèle écrit vraiment (#135).
            let mut sizer = BatchSizer::new(cfg.memory.dream_batch.max(1));
            let mut budget = OutputBudget::new(output_cap(d, &cfg));
            // Budget de raisonnement de la nuit : il part bas et monte quand le modèle
            // s'y heurte, plafonné par la configuration (issue #152). Réfléchir ne dépend
            // pas du nombre de candidats : c'est ce budget qu'on relève, pas le lot qu'on
            // réduit.
            let reasoning_cap = cfg
                .memory
                .consolidation_reasoning_tokens
                .max(REASONING_START);
            let mut reasoning = REASONING_START.min(reasoning_cap);
            // Famines survenues alors que le budget était déjà au plafond.
            let mut at_cap = 0usize;
            // Lots effectivement écrits : ce que la reprise n'aura pas à refaire.
            let mut lots_written = 0usize;
            // Candidats laissés par une passe arrêtée : ni jugés, ni reportés (#140).
            let mut unjudged: BTreeSet<String> = BTreeSet::new();
            let (mut lots, mut lone_lots) = (0usize, 0usize);
            // Appels « raisonnement plein » de la passe, et alias de repli une fois qu'on
            // y a basculé (issue #152).
            let mut fallback_alias: Option<String> = None;
            let mut i = 0usize;
            while i < items.len() {
                // Plus de la moitié des lots à un seul candidat : la passe ne converge
                // pas, elle s'arrête et le dit plutôt que de finir en un appel par
                // candidat (#140).
                if lots >= LONE_WATCH && lone_lots * 2 > lots {
                    let left = &items[i..];
                    report.warnings.push(format!(
                        "passe arrêtée : {lone_lots} lots sur {lots} n'ont tenu qu'à un seul \
                         candidat ; {} candidat(s) restent en attente, sans report consommé",
                        left.len()
                    ));
                    unjudged.extend(
                        left.iter()
                            .flat_map(|it| it.group.members.iter().map(|m| m.id.clone())),
                    );
                    break;
                }
                let mut size = budget.fit(sizer.size(items.len() - i));
                let mut lone_retried = false;
                loop {
                    let slice = &items[i..i + size];
                    let started = std::time::Instant::now();
                    let requested = budget.max_tokens(size);
                    let out = consolidate_retrying(
                        d,
                        slice,
                        &snapshot,
                        requested,
                        reasoning,
                        &mut report,
                        fallback_alias.as_deref(),
                        deadline,
                        &run_id,
                    )
                    .await?;
                    report.calls += 1;
                    batch_event(d, &run_id, size, started.elapsed(), &out).await;
                    // La sortie utile seule dimensionne les lots suivants : mesurer la
                    // complétion entière faisait apprendre le raisonnement comme si
                    // c'était du JSON (issue #152).
                    let (response, truncated, useful) = (out.parsed, out.truncated, out.useful);
                    // Raisonnement plein, sortie utile vide : réduire le lot n'y changerait
                    // rien, réfléchir ne dépend pas du nombre de candidats (issue #152).
                    // On relève le budget de réflexion et on rejoue le même lot ; au
                    // plafond deux fois, on passe à l'alias de repli pour la nuit.
                    if out.reasoning_starved {
                        report.wasted_calls += 1;
                        let on_fallback = fallback_alias.is_some();
                        let quiet = cfg.memory.consolidation_reasoning == "off";
                        report.warnings.push(format!(
                            "raisonnement plein sur {size} candidat(s) : {} tokens dépensés \
                             à réfléchir, aucune opération rendue",
                            out.reasoning
                        ));
                        // Raisonnement éteint par configuration et le modèle réfléchit
                        // quand même : relever un budget qu'on n'envoie pas ne sert à
                        // rien, c'est le modèle qu'il faut changer.
                        if !quiet && reasoning < reasoning_cap {
                            reasoning = reasoning.saturating_mul(2).min(reasoning_cap);
                            report.warnings.push(format!(
                                "budget de raisonnement relevé à {reasoning} tokens, même lot \
                                 rejoué"
                            ));
                            continue;
                        }
                        at_cap += 1;
                        if at_cap < 2 && !quiet {
                            report.warnings.push(format!(
                                "budget de raisonnement au plafond ({reasoning_cap} tokens) : \
                                 même lot rejoué une fois"
                            ));
                            continue;
                        }
                        if !on_fallback && let Some(next) = reasoning_fallback(&cfg) {
                            report.warnings.push(format!(
                                "bascule sur l'alias `{next}` pour le reste de la passe : le \
                                 modèle du rôle `compaction` dépense son budget en \
                                 raisonnement"
                            ));
                            fallback_alias = Some(next);
                            reasoning = REASONING_START.min(reasoning_cap);
                            at_cap = 0;
                            continue;
                        }
                        // Déjà sur le repli, ou pas de repli déclaré : on s'arrête là
                        // plutôt que de tourner, les candidats restants sont reportés.
                        report.warnings.push(
                            "plus rien à relever : passe arrêtée, candidats restants reportés"
                                .into(),
                        );
                        unjudged.extend(
                            items[i..]
                                .iter()
                                .flat_map(|it| it.group.members.iter().map(|m| m.id.clone())),
                        );
                        i = items.len();
                        break;
                    }
                    if truncated {
                        budget.cut(size, requested, useful);
                    }
                    if truncated && size > 1 {
                        // Sortie coupée : on rejoue tout de suite le même début, en lot deux
                        // fois plus petit, en le disant (#135).
                        report.wasted_calls += 1;
                        sizer.cut(size);
                        let next = budget.fit(sizer.size(items.len() - i));
                        report.warnings.push(format!(
                            "consolidation coupée sur {size} candidats : reprise par {next}"
                        ));
                        size = next;
                        continue;
                    }
                    if truncated && !lone_retried && budget.max_tokens(1) > requested {
                        // Un seul candidat coupé : une reprise avec une sortie doublée avant
                        // de garder une réponse tronquée (#140).
                        lone_retried = true;
                        report.wasted_calls += 1;
                        report.warnings.push(format!(
                            "consolidation coupée sur un seul candidat à {requested} tokens : \
                             reprise à {}",
                            budget.max_tokens(1)
                        ));
                        continue;
                    }
                    sizer.ok(size);
                    lots += 1;
                    if size == 1 {
                        lone_lots += 1;
                    }
                    if truncated {
                        // Gardée tronquée : ce qu'elle juge compte, l'appel est compté jeté.
                        report.wasted_calls += 1;
                        report.warnings.push(format!(
                            "consolidation coupée sur un seul candidat, même à {requested} \
                             tokens : réponse tronquée gardée"
                        ));
                    } else {
                        budget.observe(size, useful);
                    }
                    let (batch_ops, updates, batch_clashes) =
                        sort_and_plan(slice, &response, &snapshot, &day, &mut report);
                    // Le lot est une unité de travail complète : validé, écrit, ses
                    // candidats marqués, avant le suivant (issue #152). Accumuler
                    // jusqu'à la fin faisait tout perdre sur une coupure — 126 candidats
                    // traités puis jetés le 21/09.
                    write_batch(
                        d,
                        &vault,
                        &run_id,
                        &day,
                        &gates,
                        &snapshot,
                        slice,
                        batch_ops,
                        updates,
                        batch_clashes,
                        &mut report,
                        dry_run,
                    )
                    .await?;
                    if !dry_run {
                        // Ce qui vient d'être écrit est connu du lot suivant : les
                        // nouveaux UID, et les textes qui ne doivent pas se dédoubler.
                        snapshot = VaultSnapshot::read(s, &vault).await?;
                        save_stats(s, &run_id, &report).await?;
                    }
                    lots_written += 1;
                    report.lots = lots_written as u32;
                    break;
                }
                i += size;
            }
        }

        if dry_run {
            return Ok::<(), anyhow::Error>(());
        }

        // États passagers expirés : retirés du journal, sans question (issue #37).
        for op in expired_journal(s, &vault, &day).await {
            match apply(d, &vault, &op, &run_id).await {
                Ok(file) => {
                    report.journal_expired += 1;
                    if !report.files_touched.contains(&file) {
                        report.files_touched.push(file);
                    }
                }
                Err(e) => report.rejected.push(format!("{} : {e}", op.kind())),
            }
        }
        report.unused = unused_entries(d, &day).await;
        if let Some(w) = core_overflow(s, cfg.memory.core_budget_tokens as u64).await {
            report.warnings.push(w);
        }
        // Entrées ajoutées ou réécrites cette nuit, adressables `[[note#^uid]]`.
        let run = run_id.clone();
        let touched: Vec<(String, String)> = s
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT DISTINCT uid, file FROM mem_history
                     WHERE dream_run = ?1 AND uid IS NOT NULL
                       AND op IN ('add_entry', 'replace_entry', 'supersede_entry',
                                  'add_exception', 'record_ecart')",
                )?;
                let rows = st.query_map([&run], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let resolver = penelope_memory::wiki::Resolver::scan(&vault);
        report.promoted_refs = touched
            .iter()
            .filter(|(uid, _)| penelope_memory::vault::is_valid_block_id(uid))
            .map(|(uid, file)| format!("[[{}#^{uid}]]", resolver.link_target(file)))
            .collect();
        // Ce que la nuit a appris, en clair : le digest montre ces lignes à la place des
        // wikilinks bruts (issue #145).
        for (uid, file) in touched.iter().take(5) {
            if let Ok(Some(e)) = s.memory.get(uid).await {
                report
                    .promoted_examples
                    .push(format!("{} ({file})", short(&e.text.replace('\n', " "))));
            }
        }
        // Entrées fourre-tout : le digest propose le découpage, il ne le lance pas.
        report.cleanup = crate::mem_split::oversized(s)
            .await
            .iter()
            .map(|e| {
                format!(
                    "{} ({} caractères) — `penelope mem split {}`",
                    short(&e.text.replace('\n', " ")),
                    e.text.chars().count(),
                    e.uid
                )
            })
            .collect();
        let (lint, proposals) = wiki_review(s, &vault).await;
        report.lint = lint.summary();
        report.lint_problems = lint.problems() as u32;
        report.questions.extend(proposals);
        // Élagage (§6.8) : écarts non promus depuis longtemps.
        s.candidates
            .expire_stale(cfg.memory.expire_ecart_days.max(1))
            .await?;
        Ok(())
    }
    .await;
    report.duration_ms = pass_started.elapsed().as_millis() as u64;

    match outcome {
        Ok(()) => {
            if !dry_run {
                if !report.is_noop()
                    || !report.reflections.is_empty()
                    || !report.sorted.is_empty()
                    || !report.rejected.is_empty()
                    || report.journal_expired > 0
                {
                    append_dreams(s, &vault, &run_id, &report)?;
                    let message = format!(
                        "{}{} ({run_id}) : {} promues",
                        crate::vault_git::DREAM_PREFIX,
                        today(s),
                        report.promoted
                    );
                    if let Err(e) = vault_sync(&d.services, &message).await {
                        tracing::warn!(error = %e, "commit du vault après consolidation");
                    }
                }
                finish_run(s, &run_id, "done", &report, None).await?;
                let _ = s
                    .events
                    .append(EventDraft::new(
                        "memory.dreamed",
                        json!({"run": run_id, "report": report}),
                    ))
                    .await;
            }
            Ok(DreamOutcome {
                run_id,
                dry_run,
                report,
            })
        }
        Err(e) => {
            if !dry_run {
                let _ = finish_run(s, &run_id, "failed", &report, Some(&e.to_string())).await;
            }
            Err(e)
        }
    }
}

mod apply;
mod batches;
mod candidates;
mod clash;
mod consolidate;
mod digest;
mod nightly;
mod runs;
mod snapshot;

pub use apply::wiki_review;
use apply::{append_dreams, apply, target_file, today};
use batches::{
    BatchSizer, LONE_WATCH, OWN_TIMEOUT, OutputBudget, REASONING_START, batch_event,
    consolidate_retrying, output_cap, reasoning_fallback, write_batch,
};
#[cfg(test)]
use batches::{network_stall, own_timeout};
// Descendue avec les instantanés (T22) : le doctor la cite aussi.
pub use candidates::submission_order;
use candidates::{Clash, Item, ids_for, is_journal, nearby_batch, short, sort_and_plan};
#[cfg(test)]
use candidates::{Neighbour, contradiction};
pub use clash::file_unanswered_clash;
use clash::{ask_about_clash, expired_journal, unused_entries};
use consolidate::{CallOutcome, consolidate};
pub use digest::digest_text;
#[cfg(test)]
use digest::night_summary;
use digest::rejection_families;
#[cfg(test)]
use nightly::last_run;
use nightly::{FAILED_NIGHTS_KEY, FAILED_REASON_KEY, last_failure};
pub use nightly::{failure_reported, night_failed, nightly, system_crons, vault_check, vault_path};
pub(crate) use penelope_vault::snapshot::core_overflow;
// Descendue dans `vault_git` avec le vault (T22) : l'autocommit l'appelle.
pub use crate::vault_git::vault_sync;
use runs::{close_interrupted, finish_run, last_finished_start, record_run, save_stats, set_phase};
pub use runs::{history, last_report, learned, restore};
use snapshot::{VaultSnapshot, markdown_files};

#[cfg(test)]
mod tests;
