//! Lots de la phase Deep : taille, budget de sortie, écriture, nouvelles tentatives.

use super::*;

/// Taille des lots d'une passe (issues #135, #140). Après une sortie coupée, les lots
/// restent sous la taille accusée pour toute la passe ; après un lot qui tient, la taille
/// remonte à mi-chemin entre la plus grande taille qui a tenu depuis et la taille accusée,
/// jamais au-dessus. Sans ça, chaque lot repartait à `dream_batch` et quatre appels sur
/// cinq étaient jetés.
///
/// Un lot rejoué jusqu'à un seul candidat accuse ce candidat, pas la taille : seule la
/// première coupure compte. Deux fois de suite, c'est la taille : la plus petite coupure
/// compte. Une coupure à 2 candidats ou moins n'accuse jamais la taille (c'est la sortie
/// qui manque, `OutputBudget` s'en charge) : sans ces deux règles, une tête de lot
/// bavarde posait le plafond à 2, et toute la passe partait par lots d'un candidat.
#[derive(Debug, Clone)]
pub(crate) struct BatchSizer {
    max: usize,
    /// Taille accusée la plus petite de la passe : les lots restent dessous.
    ceiling: Option<usize>,
    /// Tailles coupées du lot en cours de rejeu, de la première à la dernière.
    descent: Vec<usize>,
    /// Le lot précédent n'a tenu qu'à un candidat, au bout d'un rejeu.
    lone_before: bool,
    /// Plus grande taille qui a tenu depuis que le plafond a baissé.
    held: usize,
    next: usize,
}

impl BatchSizer {
    pub(crate) fn new(max: usize) -> Self {
        BatchSizer {
            max: max.max(1),
            ceiling: None,
            descent: Vec::new(),
            lone_before: false,
            held: 0,
            next: max.max(1),
        }
    }

    pub(crate) fn size(&self, remaining: usize) -> usize {
        self.next.min(remaining).max(1)
    }

    pub(crate) fn cut(&mut self, size: usize) {
        self.descent.push(size);
        self.next = (size / 2).max(1);
    }

    pub(crate) fn ok(&mut self, size: usize) {
        let descent = std::mem::take(&mut self.descent);
        if descent.is_empty() {
            self.lone_before = false;
        } else {
            let lone = size == 1;
            let blamed = if lone && !self.lone_before {
                descent.first()
            } else {
                descent.iter().rev().find(|&&c| c > 2)
            };
            self.lone_before = lone;
            if let Some(&blamed) = blamed.filter(|&&c| c > 2)
                && self.ceiling.is_none_or(|c| blamed < c)
            {
                self.ceiling = Some(blamed);
                self.held = 0;
            }
        }
        self.held = self.held.max(size);
        self.next = match self.ceiling {
            // À mi-chemin de la taille accusée, sans l'atteindre.
            Some(c) => ((self.held + c) / 2).min(c - 1).max(1),
            None => self.max,
        }
        .min(self.max);
    }
}

/// Sortie demandée par lot (issues #135, #140) : estimée d'après les tokens réellement
/// écrits par candidat aux lots précédents (350 au départ, la grille de #37 en écrit
/// plusieurs centaines), avec une marge, dans la limite du modèle. Un lot plus grand que
/// ce que la limite permet est réduit avant l'appel.
///
/// Une coupure enseigne aussi : elle prouve que le modèle écrit plus que ce qui a été
/// demandé. Si c'est l'estimation par candidat qui a fixé la demande, elle ne redescend
/// plus sous cette preuve ; si c'est le plancher (2 000 tokens, ou un seul candidat), il
/// double. Avant #140, seuls les lots qui tenaient enseignaient, l'estimation tirait vers
/// le bas, et chaque lot rejoué recevait une demande juste sous ce que le modèle écrivait.
#[derive(Debug, Clone)]
pub(crate) struct OutputBudget {
    per_candidate: f64,
    /// Tokens par candidat prouvés par les coupures : l'estimation reste au-dessus.
    proven: f64,
    /// Sortie minimale d'un lot, relevée par ce que le modèle écrit pour un candidat.
    floor: u32,
    cap: u32,
}

impl OutputBudget {
    pub(crate) fn new(cap: u32) -> Self {
        OutputBudget {
            per_candidate: 350.0,
            proven: 0.0,
            floor: 2_000,
            cap: cap.max(2_000),
        }
    }

    pub(crate) fn max_tokens(&self, size: usize) -> u32 {
        ((size as f64 * self.per_candidate * 1.3) as u32)
            .max(self.floor)
            .min(self.cap)
    }

    /// Plus grand lot dont la sortie estimée tient dans la limite.
    pub(crate) fn fit(&self, size: usize) -> usize {
        let most = (self.cap as f64 / (self.per_candidate * 1.3)).floor() as usize;
        size.min(most.max(1))
    }

    /// Un lot qui a tenu.
    pub(crate) fn observe(&mut self, size: usize, completion: u64) {
        if completion == 0 || size == 0 {
            return;
        }
        let seen = (completion as f64 / size as f64).max(100.0);
        self.per_candidate = ((self.per_candidate + seen) / 2.0).max(self.proven);
        if size == 1 {
            self.floor = self
                .floor
                .max((completion as f64 * 1.3) as u32)
                .min(self.cap);
        }
    }

    /// Un lot coupé après `completion` tokens, pour `requested` demandés.
    pub(crate) fn cut(&mut self, size: usize, requested: u32, completion: u64) {
        if size == 0 {
            return;
        }
        if size == 1 || requested <= self.floor {
            // Le plancher a coupé : un seul candidat écrit plus que lui.
            self.floor = requested.saturating_mul(2).max(self.floor).min(self.cap);
        } else {
            self.proven = self.proven.max(completion as f64 / size as f64);
            self.per_candidate = self.per_candidate.max(self.proven);
        }
    }
}

/// Un lot jugé, rendu durable : opérations validées puis écrites, candidats marqués,
/// contradictions posées. Rien n'attend la fin de la passe (issue #152) — avant, une
/// coupure au 8ᵉ lot rendait les sept premiers à l'état d'avant, 126 candidats jetés.
#[allow(clippy::too_many_arguments)]
pub(super) async fn write_batch(
    d: &Arc<Daemon>,
    vault: &Path,
    run_id: &str,
    day: &str,
    gates: &PromotionGates,
    snap: &VaultSnapshot,
    slice: &[Item<'_>],
    ops: Vec<(Vec<String>, Operation)>,
    updates: Vec<(Vec<String>, &'static str, Option<String>)>,
    clashes: Vec<(Clash, Vec<String>)>,
    report: &mut DreamReport,
    dry_run: bool,
) -> anyhow::Result<()> {
    let s = &d.services;
    let mut state_updates = updates;
    let manually_modified = snap.changed_since_read(vault);
    let by_op = ops.clone();
    let ids_of = move |op: &Operation| -> Vec<String> { ids_for(&by_op, op) };
    let validation = validate(
        ops.iter().map(|(_, o)| o.clone()).collect(),
        &ValidationContext {
            known_uids: &snap.uids,
            entries_per_file: &snap.entries_per_file,
            uid_files: &snap.uid_file,
            manually_modified: &manually_modified,
            known_practices: &snap.practices,
            today: day,
        },
        gates,
    );
    report.deferred += validation.deferred.len() as u32;
    report.proposals += validation.proposals.len() as u32;
    // Opération refusée, reportée ou à confirmer : le candidat retourne en attente avec
    // la raison, au lieu d'être marqué promu sans écriture (#60).
    for (op, reason) in &validation.rejected {
        report.rejected.push(format!("{} : {reason}", op.kind()));
        state_updates.push((ids_of(op), "deferred", Some(reason.clone())));
    }
    for (op, reason) in &validation.deferred {
        state_updates.push((ids_of(op), "deferred", Some(reason.clone())));
    }
    for (op, reason) in &validation.proposals {
        report.questions.push(format!(
            "Proposition ({}) : {} ({reason})",
            op.kind(),
            op.text().unwrap_or_default()
        ));
        state_updates.push((ids_of(op), "question", Some(reason.clone())));
    }
    // Candidat retenu par la grille pour lequel le modèle n'a rien proposé : rien n'a été
    // écrit, il repasse la nuit prochaine.
    let served: BTreeSet<String> = ops.iter().flat_map(|(ids, _)| ids.clone()).collect();
    let decided: BTreeSet<String> = state_updates
        .iter()
        .flat_map(|(ids, _, _)| ids.clone())
        .collect();
    for it in slice {
        let ids: Vec<String> = it.group.members.iter().map(|m| m.id.clone()).collect();
        if ids
            .iter()
            .any(|id| served.contains(id) || decided.contains(id))
        {
            continue;
        }
        report.sorted.push(format!(
            "⏳ en attente « {} » : aucune opération proposée",
            short(&it.group.representative.text)
        ));
        state_updates.push((ids, "deferred", Some("aucune opération proposée".into())));
    }

    let applied: Vec<Operation> = validation.applied;
    if dry_run {
        // Rien n'est écrit : le rapport dit ce qui l'aurait été.
        report.promoted += applied.len() as u32;
        for op in &applied {
            let file = target_file(s, op);
            if !report.files_touched.contains(&file) {
                report.files_touched.push(file);
            }
        }
        return Ok(());
    }

    for op in &applied {
        let ids = ids_of(op);
        match apply(d, vault, op, run_id).await {
            Ok(file) => {
                report.promoted += 1;
                if is_journal(op) {
                    report.journal += 1;
                }
                if !report.files_touched.contains(&file) {
                    report.files_touched.push(file);
                }
                // Écrit : le candidat est traité (issue #60), et marqué tout de suite :
                // une passe arrêtée plus loin ne le repromouvra pas (issue #127).
                if !ids.is_empty() {
                    s.candidates.set_state(&ids, "promoted", None).await?;
                }
            }
            Err(e) => {
                report.rejected.push(format!("{} : {e}", op.kind()));
                // L'écriture a échoué : le candidat sera rejoué la nuit prochaine.
                if !ids.is_empty() {
                    state_updates.push((ids, "deferred", Some(e.to_string())));
                }
            }
        }
    }
    for (ids, state, reason) in state_updates {
        s.candidates
            .set_state(&ids, state, reason.as_deref())
            .await?;
    }
    // Une question sans bouton n'a pas de réponse possible : chaque contradiction devient
    // une carte, envoyée à part du digest (issue #145).
    for (clash, ids) in &clashes {
        if let Err(e) = ask_about_clash(s, clash, ids).await {
            tracing::warn!(error = %e, "carte de contradiction non posée");
        }
    }
    Ok(())
}

/// Budget de raisonnement au premier appel de la nuit : de quoi trier un lot en
/// réfléchissant, sans immobiliser la sortie utile (issue #152).
pub(super) const REASONING_START: u32 = 8_000;

/// Alias de repli du rôle de consolidation : le premier de la chaîne déclarée pour son
/// alias, sinon celui du rôle `memoire`.
pub(super) fn reasoning_fallback(cfg: &penelope_kernel::config::Config) -> Option<String> {
    let alias = cfg.role_alias("compaction");
    if let Some(next) = cfg
        .models
        .routing
        .fallback
        .get(&alias)
        .and_then(|chain| chain.first())
    {
        return Some(next.clone());
    }
    let memoire = cfg.role_alias("memoire");
    (memoire != alias).then_some(memoire)
}

/// Lots jugés avant qu'une passe faite surtout de lots d'un candidat soit arrêtée (#140).
pub(super) const LONE_WATCH: usize = 8;

/// Limite de sortie du modèle de consolidation : celle du catalogue, sinon 16 000.
pub(super) fn output_cap(d: &Arc<Daemon>, cfg: &penelope_kernel::config::Config) -> u32 {
    cfg.alias_model(&cfg.role_alias("compaction"))
        .and_then(|m| d.services.catalog.get(strip_provider(m)))
        .and_then(|i| i.max_output)
        .map(|m| m.min(32_000) as u32)
        .unwrap_or(16_000)
}

/// Un signe de vie par lot (issue #135) : événement et journal, taille, durée, sortie,
/// coupé ou non.
pub(super) async fn batch_event(
    d: &Arc<Daemon>,
    run_id: &str,
    size: usize,
    took: Duration,
    out: &CallOutcome,
) {
    // `coupe` reste ce qu'il a toujours été : la réponse n'a pas tenu dans le budget.
    // `raisonnement` dit l'autre échec, où rien n'a été écrit du tout (issue #152) : les
    // deux étaient comptés pareil, et un lot affamé passait pour un lot jugé.
    let starved = out.reasoning_starved;
    tracing::info!(
        run = run_id,
        lot = size,
        ms = took.as_millis() as u64,
        sortie = out.completion,
        raisonnement = out.reasoning,
        coupe = out.truncated,
        affame = starved,
        "lot de consolidation"
    );
    let _ = d
        .services
        .events
        .append(EventDraft::new(
            "memory.dream_batch",
            json!({"run": run_id, "size": size, "ms": took.as_millis() as u64,
                   "completion": out.completion, "reasoning": out.reasoning,
                   "truncated": out.truncated, "reasoning_starved": starved,
                   // Un lot affamé n'a rien jugé : il ne compte pas comme abouti.
                   "judged": !starved}),
        ))
        .await;
}

/// Un lot, repris après une erreur passagère du modèle (flux muet, 5xx, 429, délai) :
/// attente `memory.dream_retry_wait`, puis le double. À ce stade rien n'est écrit ni
/// marqué : la reprise ne peut rien appliquer deux fois, et le travail des lots déjà
/// faits n'est pas refait (issue #127).
#[allow(clippy::too_many_arguments)]
pub(super) async fn consolidate_retrying(
    d: &Arc<Daemon>,
    items: &[Item<'_>],
    snap: &VaultSnapshot,
    max_tokens: u32,
    reasoning_budget: u32,
    report: &mut DreamReport,
    alias_override: Option<&str>,
    deadline: std::time::Instant,
    run_id: &str,
) -> anyhow::Result<CallOutcome> {
    let wait = penelope_kernel::config::parse_duration(
        &d.services.config.config().memory.dream_retry_wait,
    )
    .unwrap_or(Duration::from_secs(120));
    let mut attempt = 0u32;
    let mut stalls = 0u32;
    loop {
        match consolidate(d, items, snap, max_tokens, reasoning_budget, alias_override).await {
            Err(e) if passing(&e) => {
                // Un appel tué par **notre** délai n'est pas une coupure réseau : c'est un
                // budget trop court. Une vraie coupure se prouve par une sonde, sans quoi
                // la passe attendait 300 s et rejouait sans fin (issue #152).
                let stall = network_stall(&e) || (own_timeout(&e) && !network_is_up().await);
                let delay = if stall {
                    STALL_WAIT
                } else {
                    wait * (attempt + 1)
                };
                // Un plafond **global** : quelle qu'en soit la cause, un lot ne monopolise
                // pas la nuit. Au-delà, il est reporté et la passe continue.
                let total = attempt + stalls + 1;
                let room = std::time::Instant::now() + delay + CALL_TIMEOUT_MIN < deadline;
                let over = total > MAX_ATTEMPTS
                    || if stall {
                        stalls >= RETRIES
                    } else {
                        attempt >= RETRIES
                    };
                if (over && !(stall && room && total <= MAX_ATTEMPTS)) || !room {
                    return Err(e);
                }
                report.calls += 1;
                report.wasted_calls += 1;
                let why = if stall {
                    stalls += 1;
                    format!(
                        "réseau coupé ou machine endormie ({e}), lot rejoué dans {} s",
                        delay.as_secs()
                    )
                } else {
                    attempt += 1;
                    format!(
                        "{e}, reprise {attempt}/{RETRIES} après {} s",
                        delay.as_secs()
                    )
                };
                report
                    .warnings
                    .push(format!("lot de {} candidat(s) : {why}", items.len()));
                // Chaque tentative laisse une trace : sans cela, la passe du 21/09 a
                // tourné une heure sans une ligne de journal ni un événement (issue #152).
                tracing::warn!(lot = items.len(), attempt = total, stall, "{why}");
                let _ = d
                    .services
                    .events
                    .append(EventDraft::new(
                        "memory.dream_retry",
                        json!({"run": run_id, "size": items.len(), "attempt": total,
                               "stall": stall, "delay_s": delay.as_secs(),
                               "error": e.to_string()}),
                    ))
                    .await;
                // Et l'état de la passe avance à chaque tentative, pas seulement après un
                // lot écrit : les avertissements restaient en mémoire, invisibles.
                let _ = save_stats(&d.services, run_id, report).await;
                tokio::time::sleep(delay).await;
            }
            other => return other,
        }
    }
}

/// Tentatives d'un même lot, toutes causes confondues. Au-delà, le lot est reporté et la
/// passe continue : une nuit entière sur un seul lot ne vaut pas mieux qu'un échec.
const MAX_ATTEMPTS: u32 = 3;

/// Attente entre deux reprises d'un lot coupé par le réseau ou la veille : assez longue
/// pour laisser la machine revenir, assez courte pour reprendre la nuit (issue #152).
const STALL_WAIT: Duration = Duration::from_secs(300);

/// Marque de notre propre délai, par opposition à une erreur venue du réseau.
pub(super) const OWN_TIMEOUT: &str = "appel trop long pour son budget";

/// Le réseau répond-il ? Sonde TCP vers le fournisseur, cinq secondes.
///
/// Sans elle, « notre appel a dépassé son délai » et « la machine dormait » se
/// confondaient, et un appel simplement trop lent partait pour une attente de cinq
/// minutes, sans fin (issue #152).
async fn network_is_up() -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(("openrouter.ai", 443)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Coupure réseau ou machine endormie, par opposition à une erreur passagère du
/// fournisseur : le lot est rejoué au retour, pas compté dans les deux reprises.
pub(super) fn network_stall(e: &anyhow::Error) -> bool {
    let Some(l) = e.downcast_ref::<LlmError>() else {
        return false;
    };
    if l.kind != LlmErrorKind::Transient {
        return false;
    }
    let m = l.to_string().to_lowercase();
    // Notre propre délai n'en est **pas** une : un appel trop long pour son budget est un
    // problème de budget, pas de réseau. Le confondre coûtait 300 s d'attente puis un
    // rejeu, sans fin (issue #152). Surtout pas « timeout » tout court non plus : un
    // `Upstream idle timeout` est une erreur du fournisseur (#127), qui se reprend vite.
    if m.contains(OWN_TIMEOUT) {
        return false;
    }
    m.contains("error sending request")
        || m.contains("connection refused")
        || m.contains("connection reset")
        || m.contains("connexion")
        || m.contains("dns")
        || m.contains("network is unreachable")
        || m.contains("réseau")
}

/// Notre appel a dépassé le temps qu'on lui avait accordé.
pub(super) fn own_timeout(e: &anyhow::Error) -> bool {
    e.downcast_ref::<LlmError>()
        .is_some_and(|l| l.to_string().contains(OWN_TIMEOUT))
}

/// Reprises d'un lot après une erreur passagère.
const RETRIES: u32 = 2;

/// Vrai pour une erreur passagère du fournisseur : un flux muet n'est pas un refus.
fn passing(e: &anyhow::Error) -> bool {
    e.downcast_ref::<LlmError>()
        .is_some_and(|l| l.kind.is_retryable())
}
