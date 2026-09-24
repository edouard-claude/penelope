//! Candidats de la phase Deep : ordre de soumission, voisins, contradictions, plan.

use super::*;

/// Textes des candidats dans l'ordre où la passe les soumet au modèle : les verdicts se
/// rattachent à ces numéros (tests et banc d'essai de la mémoire).
pub async fn submission_order(s: &Services) -> anyhow::Result<Vec<String>> {
    let cfg = s.config.config();
    let gates = PromotionGates::from_config(&cfg.memory.promotion);
    let groups = group(s.candidates.pending(None).await?, cfg.memory.dedup_jaccard);
    Ok(groups
        .iter()
        .filter(|g| matches!(gate(g, &gates), Gate::Promote | Gate::Sort))
        .map(|g| g.representative.text.clone())
        .collect())
}

pub(super) fn short(text: &str) -> String {
    let t: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        format!("{t}…")
    } else {
        t
    }
}

/// Contradiction trouvée : de quoi poser une carte à trois boutons (issue #145), et la
/// phrase qui la résume.
pub(crate) struct Clash {
    pub question: String,
    /// Entrée déjà en mémoire, et son identifiant : « Remplacer » la retire.
    pub existing_uid: String,
    pub existing: String,
    pub proposed: String,
    /// Contexte du candidat, s'il en a un : « Exception » s'en sert.
    pub quand: Option<String>,
}

/// Contradiction avec un souvenir proche, sans contexte distinct : une question (§6.8),
/// jamais un doublon (issue #37).
pub(super) fn contradiction(
    candidate: &Candidate,
    text: &str,
    nearby: &[Neighbour],
) -> Option<Clash> {
    use penelope_memory::consolidation::CONTRADICTION_SIMILARITY;
    let mut probe = candidate.clone();
    probe.text = text.to_string();
    for n in nearby {
        // Le voisin lointain ne contredit pas : le 5ᵉ résultat était comparé comme le
        // premier, sans seuil (issue #145). Sans vecteur des deux côtés, la similarité
        // n'est pas mesurable et le Jaccard décide seul.
        if n.similarity.is_some_and(|s| s < CONTRADICTION_SIMILARITY) {
            continue;
        }
        let e = &n.entry;
        if let Some(penelope_memory::consolidation::Contradiction::NeedsQuestion {
            existing,
            candidate: proposed,
        }) =
            penelope_memory::consolidation::detect_contradiction(&probe, &e.text, e.quand.as_ref())
        {
            // Les deux entrées **tronquées** : une question qui recopie un dossier de
            // 3 000 caractères n'est pas une question (issue #145).
            return Some(Clash {
                question: format!(
                    "Tu as dit « {} », j'avais « {} » : je remplace, j'ajoute une \
                     exception, ou j'ignore ?",
                    short(&proposed),
                    short(&existing)
                ),
                existing_uid: e.uid.clone(),
                existing: existing.clone(),
                proposed: proposed.clone(),
                quand: candidate.quand.as_ref().map(|w| w.render()),
            });
        }
    }
    None
}

/// Candidat soumis au modèle, avec ses souvenirs proches.
pub(super) struct Item<'a> {
    pub(super) group: &'a CandidateGroup,
    pub(super) nearby: Vec<Neighbour>,
}

/// Souvenir proche d'un candidat, avec la similarité qui l'a rapproché quand elle a pu
/// être mesurée (issue #145) : `None` quand la recherche est restée lexicale.
#[derive(Debug, Clone)]
pub(super) struct Neighbour {
    pub(super) entry: IndexedEntry,
    pub(super) similarity: Option<f64>,
}

/// Souvenirs proches de chaque candidat, avec **un seul** appel d'embeddings pour tout le
/// lot (issue #59).
pub(super) async fn nearby_batch(d: &Arc<Daemon>, texts: &[String]) -> Vec<Vec<Neighbour>> {
    let vectors = match crate::embeddings::embed_texts(&d.embedder(), texts).await {
        Ok((_, v)) => v,
        Err(e) => {
            tracing::debug!(error = %e, "embeddings du lot indisponibles : recherche lexicale");
            Vec::new()
        }
    };
    let mut out = Vec::with_capacity(texts.len());
    for (i, text) in texts.iter().enumerate() {
        let vector = vectors.get(i).filter(|v| !v.is_empty()).cloned();
        out.push(nearby_with(d, text, vector).await);
    }
    out
}

/// Souvenirs proches d'un candidat : recherche par le sens quand les embeddings répondent,
/// sinon lexicale ; ni journal, ni documents ingérés.
async fn nearby_with(d: &Arc<Daemon>, text: &str, vector: Option<Vec<f32>>) -> Vec<Neighbour> {
    let s = &d.services;
    let filter = penelope_memory::SearchFilter {
        limit: 8,
        ..Default::default()
    };
    // Sans vecteur de requête, aucune similarité n'est mesurable : le voisin est retenu
    // sans seuil, comme avant (issue #145).
    let measured = vector.is_some();
    s.memory
        .search(text, vector, &filter, &[])
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|h| {
            h.entry.level != Level::Episodic
                && h.entry.etype != penelope_memory::ingest::SOURCE_ETYPE
        })
        .map(|h| Neighbour {
            // Une entrée jamais vectorisée sort à 0 : la similarité reste inconnue.
            similarity: (measured && h.similarity > 0.0).then_some(h.similarity),
            entry: h.entry,
        })
        .take(5)
        .collect()
}

/// Candidats servis par une opération, après le passage de `validate` qui peut l'avoir
/// retouchée (texte rogné, fichier changé, découpée en morceaux) : égalité d'abord, puis
/// texte normalisé, puis inclusion, puis uid visé (issue #60).
pub(super) fn ids_for(ops: &[(Vec<String>, Operation)], op: &Operation) -> Vec<String> {
    use penelope_memory::grid::normalized;
    if let Some((ids, _)) = ops.iter().find(|(_, o)| o == op) {
        return ids.clone();
    }
    if let Some(text) = op.text().map(normalized).filter(|t| !t.is_empty()) {
        let same = ops.iter().find(|(_, o)| {
            o.text()
                .map(normalized)
                .is_some_and(|t| t == text || t.contains(&text) || text.contains(&t))
        });
        if let Some((ids, _)) = same {
            return ids.clone();
        }
    }
    if let Some(uid) = op.target_uid()
        && let Some((ids, _)) = ops
            .iter()
            .find(|(_, o)| o.target_uid() == Some(uid) && o.kind() == op.kind())
    {
        return ids.clone();
    }
    Vec::new()
}

/// Opération d'ajout au journal des états en cours.
pub(super) fn is_journal(op: &Operation) -> bool {
    matches!(op, Operation::AddEntry { file, section: Some(section), .. }
        if file == "projets.md" && section == penelope_memory::grid::JOURNAL_SECTION)
}

/// Décide la place de chaque candidat à partir des verdicts, puis ne garde que les
/// opérations des candidats retenus ; le journal est écrit par le harnais, pas par le
/// modèle. Renvoie les opérations à valider et les changements d'état des candidats.
#[allow(clippy::type_complexity)]
pub(super) fn sort_and_plan(
    items: &[Item<'_>],
    response: &penelope_memory::grid::Consolidation,
    snap: &VaultSnapshot,
    day: &str,
    report: &mut DreamReport,
) -> (
    Vec<(Vec<String>, Operation)>,
    Vec<(Vec<String>, &'static str, Option<String>)>,
    Vec<(Clash, Vec<String>)>,
) {
    use penelope_memory::grid::{JOURNAL_SECTION, Placement, normalized};
    let mut updates = Vec::new();
    let mut clashes: Vec<(Clash, Vec<String>)> = Vec::new();
    let mut placements: Vec<Option<Placement>> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let g = item.group;
        let ids: Vec<String> = g.members.iter().map(|m| m.id.clone()).collect();
        let text = short(&g.representative.text);
        // Un écart a passé ses seuils de répétition : pas de verdict à attendre. Son
        // état se décide après l'écriture, comme les autres (issue #60).
        if g.ctype == penelope_memory::CandidateType::Ecart {
            let _ = &ids;
            placements.push(Some(Placement::Durable));
            continue;
        }
        let Some(v) = response.verdict(i + 1) else {
            report.sorted.push(format!(
                "⏳ en attente « {text} » : pas de verdict, retenté la nuit prochaine"
            ));
            updates.push((ids, "deferred", Some("sans verdict de la grille".into())));
            placements.push(None);
            continue;
        };
        let place = v.placement(day);
        let (icon, detail) = match &place {
            Placement::Durable => ("✅", String::new()),
            Placement::Journal { expire } => ("🗓", format!(" jusqu'au {expire}")),
            Placement::Ignored(why) => ("⏭", format!(" ({why})")),
        };
        let why = if v.justification.trim().is_empty() {
            "sans justification".to_string()
        } else {
            v.justification.trim().replace('\n', " ")
        };
        report.sorted.push(format!(
            "{icon} {} « {text} »{detail} : {} · {why}",
            place.label(),
            v.criteria_line()
        ));
        // `promoted` seulement après une écriture réussie (issue #60) ; un candidat
        // écarté par la grille, lui, est tranché ici.
        if let Placement::Ignored(reason) = &place {
            report.rejected.push(format!("« {text} » : {reason}"));
            updates.push((ids, "rejected", Some(reason.clone())));
        }
        placements.push(Some(place));
    }

    let mut ops = Vec::new();
    let mut journal_text: BTreeMap<usize, String> = BTreeMap::new();
    let mut seen: BTreeSet<String> = snap.texts.clone();
    for (candidat, op) in &response.operations {
        let Some(n) = candidat.filter(|n| *n >= 1 && *n <= items.len()) else {
            report
                .rejected
                .push(format!("{} : opération sans candidat", op.kind()));
            continue;
        };
        let item = &items[n - 1];
        match &placements[n - 1] {
            Some(Placement::Durable) => {
                if let Operation::AddEntry { text, .. } = op {
                    // Jamais doublée : un texte déjà en mémoire ne s'ajoute pas.
                    if !seen.insert(normalized(text)) {
                        report
                            .sorted
                            .push(format!("＝ déjà en mémoire « {} »", short(text)));
                        continue;
                    }
                    if let Some(clash) =
                        contradiction(&item.group.representative, text, &item.nearby)
                    {
                        let ids: Vec<String> =
                            item.group.members.iter().map(|m| m.id.clone()).collect();
                        report.questions.push(clash.question.clone());
                        // Une carte à trois boutons, à part du digest : une question sans
                        // bouton n'a pas de réponse possible (issue #145).
                        clashes.push((clash, ids.clone()));
                        updates.push((ids, "question", None));
                        continue;
                    }
                }
                ops.push((
                    item.group.members.iter().map(|m| m.id.clone()).collect(),
                    op.clone(),
                ));
            }
            Some(Placement::Journal { .. }) => {
                if let Operation::AddEntry { text, .. } = op {
                    journal_text.entry(n).or_insert_with(|| text.clone());
                }
            }
            Some(Placement::Ignored(_)) => report
                .rejected
                .push(format!("{} : candidat {n} écarté par la grille", op.kind())),
            None => {}
        }
    }
    for (candidat, reason) in &response.noops {
        if let Some(n) = candidat.filter(|n| *n >= 1 && *n <= items.len()) {
            // Rien à écrire parce que c'est déjà en mémoire : le candidat est traité.
            updates.push((
                items[n - 1]
                    .group
                    .members
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
                "promoted",
                None,
            ));
            report.sorted.push(format!(
                "＝ déjà en mémoire « {} » : {reason}",
                short(&items[n - 1].group.representative.text)
            ));
        }
    }
    for (i, place) in placements.iter().enumerate() {
        if let Some(Placement::Journal { expire }) = place {
            let text = journal_text
                .remove(&(i + 1))
                .unwrap_or_else(|| items[i].group.representative.text.clone());
            ops.push((
                items[i]
                    .group
                    .members
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
                Operation::AddEntry {
                    file: "projets.md".into(),
                    section: Some(JOURNAL_SECTION.into()),
                    text,
                    importance: None,
                    declencheurs: None,
                    expire: Some(expire.clone()),
                    sensible: None,
                },
            ));
        }
    }
    (ops, updates, clashes)
}
