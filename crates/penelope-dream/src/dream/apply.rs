//! Application des opérations validées au vault, `DREAMS.md`, revue du wiki.

use super::*;

pub(super) fn target_file(s: &Services, op: &Operation) -> String {
    let _ = s;
    match op {
        Operation::AddEntry { file, .. } => file.clone(),
        Operation::AddException { practice, .. }
        | Operation::RecordEcart { practice, .. }
        | Operation::UpdateDefault { practice, .. } => format!("pratiques/{practice}.md"),
        Operation::CreateEntity { slug, .. } => format!("entites/{slug}.md"),
        Operation::ReplaceEntry { uid, .. }
        | Operation::SupersedeEntry { uid, .. }
        | Operation::RetireEntry { uid, .. }
        | Operation::UpdateException { uid, .. } => format!("uid:{uid}"),
        Operation::Link { from_uid, .. } => format!("uid:{from_uid}"),
    }
}

fn level_of(file: &str) -> Level {
    if file == "projets.md" {
        Level::Projet
    } else {
        Level::from_path(file)
    }
}

pub(super) fn today(s: &Services) -> String {
    s.clock.now_rfc3339()[..10].to_string()
}

/// Applique une opération validée ; renvoie le fichier touché.
pub(super) async fn apply(
    d: &Context,
    vault: &Path,
    op: &Operation,
    run_id: &str,
) -> Result<String, String> {
    let s = &d.services;
    let day = today(s);
    let prov = Provenance {
        origin: Origin::Agent,
        session_kind: "consolidation".into(),
        observed_at: s.clock.now_rfc3339(),
        supersedes_uid: None,
        source_ref: Some(format!("dream:{run_id}")),
        session_id: None,
    };
    let cx = ApplyCtx {
        s,
        vault,
        op,
        run_id,
        prov: &prov,
        day: &day,
    };
    match op {
        Operation::AddEntry {
            file,
            section,
            text,
            importance,
            declencheurs,
            expire,
            sensible,
        } => {
            if !WRITABLE_FILES.contains(&file.as_str()) {
                return Err(format!("fichier non autorisé : {file}"));
            }
            crate::vault_ops::write_filter(text)?;
            let uid = penelope_kernel::ids::Ulid::new().to_string();
            let annotations = Annotations {
                uid: Some(uid.clone()),
                importance: *importance,
                declencheurs: declencheurs.clone().unwrap_or_default(),
                depuis: Some(day.clone()),
                source: Some("consolidation".into()),
                expire: expire.clone(),
                sensible: sensible.unwrap_or(false),
                ..Default::default()
            };
            let line = edit::entry_line(text, &annotations);
            let title = title_of(file);
            mutate(s, vault, file, Some(&uid), op.kind(), run_id, |raw| {
                Ok(edit::append_entry(raw, title, section.as_deref(), &line))
            })
            .await?;
            let entry = VaultEntry {
                uid: uid.clone(),
                text: text.trim().to_string(),
                annotations,
                section: section.clone().unwrap_or_default(),
                line: 0,
            };
            let etype = if file == "profil.md" {
                "preference"
            } else {
                "fait"
            };
            let indexed = IndexedEntry::from_vault(&entry, file, level_of(file), etype, None, &day);
            s.memory
                .upsert(&indexed, &prov)
                .await
                .map_err(|e| e.to_string())?;
            if expire.is_some() || sensible.unwrap_or(false) {
                s.memory
                    .set_flags(&uid, sensible.unwrap_or(false), expire.as_deref())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(file.clone())
        }
        Operation::ReplaceEntry { uid, text } => {
            crate::vault_ops::write_filter(text)?;
            let mut entry = s
                .memory
                .get(uid)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("uid inconnu de l'index : {uid}"))?;
            let file = entry.file.clone();
            mutate(s, vault, &file, Some(uid), op.kind(), run_id, |raw| {
                edit::replace_entry_text(raw, uid, text)
                    .ok_or_else(|| format!("uid {uid} absent de {file}"))
            })
            .await?;
            entry.text = text.trim().to_string();
            entry.maj = day.clone();
            entry.content_hash = penelope_kernel::canonical::sha256_hex(entry.text.as_bytes());
            s.memory
                .upsert(&entry, &prov)
                .await
                .map_err(|e| e.to_string())?;
            Ok(file)
        }
        Operation::SupersedeEntry { uid, text, .. } => supersede_entry(&cx, uid, text).await,
        Operation::RetireEntry { uid, .. } => {
            let entry = s
                .memory
                .get(uid)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("uid inconnu de l'index : {uid}"))?;
            let file = entry.file.clone();
            mutate(s, vault, &file, Some(uid), op.kind(), run_id, |raw| {
                edit::remove_entry(raw, uid).ok_or_else(|| format!("uid {uid} absent de {file}"))
            })
            .await?;
            s.memory.retire(uid).await.map_err(|e| e.to_string())?;
            Ok(file)
        }
        Operation::Link { from_uid, to_slug } => {
            let entry = s
                .memory
                .get(from_uid)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("uid inconnu de l'index : {from_uid}"))?;
            let file = entry.file.clone();
            mutate(s, vault, &file, Some(from_uid), op.kind(), run_id, |raw| {
                edit::link_entry(raw, from_uid, to_slug)
                    .ok_or_else(|| format!("uid {from_uid} absent de {file}"))
            })
            .await?;
            Ok(file)
        }
        Operation::AddException {
            practice,
            text,
            quand,
            confiance,
        } => {
            add_to_practice(
                d, vault, op, practice, text, quand, *confiance, run_id, &prov,
            )
            .await
        }
        Operation::RecordEcart {
            practice,
            text,
            quand,
        } => add_to_practice(d, vault, op, practice, text, quand, None, run_id, &prov).await,
        Operation::UpdateException {
            uid,
            text,
            quand,
            confiance,
        } => update_exception(&cx, uid, text.as_deref(), quand.as_deref(), *confiance).await,
        Operation::CreateEntity {
            kind,
            slug,
            title,
            body,
        } => create_entity(&cx, kind, slug, title, body).await,
        Operation::UpdateDefault { .. } => {
            Err("une modification de défaut reste une proposition".into())
        }
    }
}

/// Ce que toute opération appliquée partage : le vault, le run, la provenance, le jour.
#[derive(Clone, Copy)]
struct ApplyCtx<'a> {
    s: &'a Services,
    vault: &'a Path,
    op: &'a Operation,
    run_id: &'a str,
    prov: &'a Provenance,
    day: &'a String,
}

/// Un fait corrigé : l'ancienne entrée est retirée, la nouvelle la cite (issue #37).
async fn supersede_entry(cx: &ApplyCtx<'_>, uid: &str, text: &str) -> Result<String, String> {
    let ApplyCtx {
        s,
        vault,
        op,
        run_id,
        prov,
        day,
    } = *cx;
    crate::vault_ops::write_filter(text)?;
    let old = s
        .memory
        .get(uid)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("uid inconnu de l'index : {uid}"))?;
    let file = old.file.clone();
    if !WRITABLE_FILES.contains(&file.as_str()) {
        return Err(format!(
            "remplacement hors des fichiers de mémoire ({file}) : exception ou pratique"
        ));
    }
    let new_uid = penelope_kernel::ids::Ulid::new().to_string();
    let annotations = Annotations {
        uid: Some(new_uid.clone()),
        importance: old.importance,
        declencheurs: old.declencheurs.clone(),
        projet: old.projet.clone(),
        depuis: Some(day.clone()),
        source: Some("consolidation".into()),
        quand: old.quand.clone(),
        remplace: Some(uid.to_string()),
        ..Default::default()
    };
    let line = edit::entry_line(text, &annotations);
    mutate_as(
        s,
        vault,
        &file,
        Some(uid),
        Some(&new_uid),
        op.kind(),
        run_id,
        |raw| {
            edit::replace_entry_line(raw, uid, &line)
                .ok_or_else(|| format!("uid {uid} absent de {file}"))
        },
    )
    .await?;
    s.memory.retire(uid).await.map_err(|e| e.to_string())?;
    let mut entry = old.clone();
    entry.uid = new_uid.clone();
    entry.text = text.trim().to_string();
    entry.depuis = Some(day.clone());
    entry.maj = day.clone();
    entry.retired_at = None;
    entry.statut = "active".into();
    entry.content_hash = penelope_kernel::canonical::sha256_hex(entry.text.as_bytes());
    let prov = Provenance {
        supersedes_uid: Some(uid.to_string()),
        ..prov.clone()
    };
    s.memory
        .upsert(&entry, &prov)
        .await
        .map_err(|e| e.to_string())?;
    Ok(file)
}

/// Une exception existante change de texte, de condition ou de confiance.
async fn update_exception(
    cx: &ApplyCtx<'_>,
    uid: &str,
    text: Option<&str>,
    quand: Option<&str>,
    confiance: Option<f64>,
) -> Result<String, String> {
    let ApplyCtx {
        s,
        vault,
        op,
        run_id,
        day,
        ..
    } = *cx;
    let entry = s
        .memory
        .get(uid)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("uid inconnu de l'index : {uid}"))?;
    if let Some(t) = text {
        crate::vault_ops::write_filter(t)?;
    }
    let file = entry.file.clone();
    let practice_id = entry.slug.clone().unwrap_or_default();
    mutate(s, vault, &file, Some(uid), op.kind(), run_id, |raw| {
        let mut p = Practice::parse(raw, &practice_id)?;
        let target = p
            .exceptions
            .iter_mut()
            .find(|e| e.uid == uid)
            .ok_or_else(|| format!("exception {uid} absente de {file}"))?;
        if let Some(t) = text {
            target.text = t.trim().to_string();
        }
        if let Some(q) = quand {
            target.annotations.quand = Some(When::parse(q)?);
        }
        if let Some(c) = confiance {
            target.annotations.confiance = Some(c.clamp(0.0, 1.0));
        }
        p.maj = day.clone();
        Ok(p.render())
    })
    .await?;
    Ok(file)
}

/// Une fiche d'entité neuve, jamais par-dessus une fiche existante.
async fn create_entity(
    cx: &ApplyCtx<'_>,
    kind: &str,
    slug: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let ApplyCtx {
        s,
        vault,
        op,
        run_id,
        day,
        ..
    } = *cx;
    crate::vault_ops::write_filter(title)?;
    if penelope_observe::contains_secret(body) || penelope_observe::is_suspicious(body) {
        return Err("contenu d'entité refusé par le filtre".into());
    }
    let slug = penelope_platform::slugify(slug);
    let file = format!("entites/{slug}.md");
    if vault.join(&file).exists() {
        return Err(format!("{file} existe déjà"));
    }
    let mut fields = BTreeMap::new();
    use penelope_kernel::frontmatter::FmValue;
    fields.insert("type".to_string(), FmValue::Str("entite".into()));
    fields.insert("genre".to_string(), FmValue::Str(kind.replace('\n', " ")));
    fields.insert("maj".to_string(), FmValue::Str(day.clone()));
    let content = penelope_kernel::frontmatter::render(
        &fields,
        &format!("# {}\n\n{}\n", title.replace('\n', " "), body.trim()),
    );
    mutate(s, vault, &file, None, op.kind(), run_id, |_| {
        Ok(content.clone())
    })
    .await?;
    Ok(file)
}

/// Exception (ou écart observé) ajoutée à une pratique existante.
#[allow(clippy::too_many_arguments)]
async fn add_to_practice(
    d: &Context,
    vault: &Path,
    op: &Operation,
    practice: &str,
    text: &str,
    quand: &str,
    confiance: Option<f64>,
    run_id: &str,
    prov: &Provenance,
) -> Result<String, String> {
    let s = &d.services;
    let day = today(s);
    crate::vault_ops::write_filter(text)?;
    let when = When::parse(quand)?;
    let file = format!("pratiques/{practice}.md");
    let uid = penelope_kernel::ids::Ulid::new().to_string();
    let is_exception = matches!(op, Operation::AddException { .. });
    let annotations = Annotations {
        uid: Some(uid.clone()),
        quand: Some(when),
        confiance: confiance.map(|c| c.clamp(0.0, 1.0)),
        depuis: Some(day.clone()),
        source: Some("consolidation".into()),
        occurrences: (!is_exception).then_some(1),
        ..Default::default()
    };
    let text_owned = text.trim().to_string();
    let entry = VaultEntry {
        uid: uid.clone(),
        text: text_owned.clone(),
        annotations,
        section: if is_exception {
            "Exceptions".into()
        } else {
            "Écarts observés".into()
        },
        line: 0,
    };
    let for_file = entry.clone();
    mutate(s, vault, &file, Some(&uid), op.kind(), run_id, |raw| {
        let mut p = Practice::parse(raw, practice)?;
        if is_exception {
            p.exceptions.push(for_file.clone());
        } else {
            p.ecarts.push(for_file.clone());
        }
        p.maj = day.clone();
        Ok(p.render())
    })
    .await?;
    let etype = if is_exception { "exception" } else { "ecart" };
    let indexed =
        IndexedEntry::from_vault(&entry, &file, Level::Cure, etype, Some(practice), &today(s));
    s.memory
        .upsert(&indexed, prov)
        .await
        .map_err(|e| e.to_string())?;
    Ok(file)
}

fn title_of(file: &str) -> &'static str {
    match file {
        "profil.md" => "# Profil du propriétaire",
        "memoire.md" => "# Mémoire de fond",
        "projets.md" => "# Projets",
        _ => "# Notes",
    }
}

/// Lit, transforme, écrit atomiquement sans écraser une édition concurrente, et garde la
/// pré-image dans `mem_history`.
async fn mutate(
    s: &Services,
    vault: &Path,
    rel: &str,
    uid: Option<&str>,
    op: &str,
    run_id: &str,
    f: impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    mutate_as(s, vault, rel, uid, uid, op, run_id, f).await
}

/// [`mutate`], l'historique rattaché à `history_uid` (la nouvelle entrée d'un
/// remplacement) plutôt qu'à la ligne visée.
#[allow(clippy::too_many_arguments)]
async fn mutate_as(
    s: &Services,
    vault: &Path,
    rel: &str,
    uid: Option<&str>,
    history_uid: Option<&str>,
    op: &str,
    run_id: &str,
    f: impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    if rel.contains("..") || rel.starts_with('/') {
        return Err(format!("chemin refusé : {rel}"));
    }
    let Some((before, after)) = crate::vault_ops::update_note(vault, rel, uid, &today(s), f)?
    else {
        return Ok(());
    };
    let (uid, rel, op, run, ts) = (
        history_uid.map(String::from),
        rel.to_string(),
        op.to_string(),
        run_id.to_string(),
        s.clock.now_rfc3339(),
    );
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO mem_history(uid, file, op, before, after, ts, dream_run)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![uid, rel, op, before, after, ts, run],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn append_dreams(
    s: &Services,
    vault: &Path,
    run_id: &str,
    report: &DreamReport,
) -> anyhow::Result<()> {
    let day = today(s);
    crate::vault_ops::update_note(vault, "DREAMS.md", None, &day, |raw| {
        let mut body = if raw.trim().is_empty() {
            "# Revue\n".to_string()
        } else {
            raw.to_string()
        };
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!(
            "\n## Rêve du {day} (`{run_id}`)\n\n{}\n",
            report.render()
        ));
        if !report.sorted.is_empty() {
            body.push_str("\n### Tri\n");
            for line in &report.sorted {
                body.push_str(&format!("- {line}\n"));
            }
        }
        // Motifs d'écart regroupés : un motif qui revient vingt fois est un réglage à
        // revoir (issue #109).
        let families = rejection_families(&report.rejected);
        if !families.is_empty() {
            body.push_str(&format!(
                "\n### Motifs d'écart ({} candidat(s) examiné(s), {} écarté(s))\n",
                report.candidates_seen,
                report.rejected.len()
            ));
            for (family, n) in &families {
                body.push_str(&format!("- {family} : {n}\n"));
            }
        }
        if !report.reflections.is_empty() {
            body.push_str("\n### Réflexions\n");
            for r in &report.reflections {
                body.push_str(&format!("- {r}\n"));
            }
        }
        Ok(body)
    })
    .map_err(anyhow::Error::msg)?;
    let mut details: Vec<String> = report.promoted_refs.clone();
    details.extend(report.lint.iter().cloned());
    crate::vault_ops::log(
        vault,
        &day,
        "dream",
        &format!("{} promues", report.promoted),
        &details,
    )
    .map_err(anyhow::Error::msg)?;
    if !report.lint.is_empty() {
        crate::vault_ops::log(
            vault,
            &day,
            "lint",
            &format!("{} problème(s)", report.lint_problems),
            &report.lint,
        )
        .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

/// Passe de lint du rêve (issue #29) : graphe et propriétés du wiki, puis ce qui se
/// propose sans se corriger en silence (entrées expirées, contradictions).
pub async fn wiki_review(
    s: &Services,
    vault: &Path,
) -> (penelope_memory::wiki::LintReport, Vec<String>) {
    let report = penelope_memory::wiki::lint(vault);
    let mut proposals = Vec::new();
    let day = today(s);
    let resolver = penelope_memory::wiki::Resolver::scan(vault);
    let refer = |file: &str, uid: &str| format!("[[{}#^{uid}]]", resolver.link_target(file));
    let hidden_expired: Vec<(String, String)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT f.uid, f.expire FROM mem_flags f JOIN mem_entries e ON e.uid = f.uid
                 WHERE e.statut != 'retiree' AND f.expire IS NOT NULL AND f.expire < ?1
                 ORDER BY f.expire",
            )?;
            let rows = st.query_map([&day], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    for (uid, expire) in hidden_expired.into_iter().take(5) {
        if let Ok(Some(e)) = s.memory.get(&uid).await {
            proposals.push(format!(
                "« {} » ({}) a expiré le {expire} : la retirer ou la prolonger ?",
                short(&e.text),
                refer(&e.file, &uid)
            ));
        }
    }
    let mut entries = s.memory.by_level(Level::Profil).await.unwrap_or_default();
    entries.extend(s.memory.by_level(Level::Coeur).await.unwrap_or_default());
    let mut contradictions = 0;
    'outer: for (i, a) in entries.iter().enumerate() {
        for b in &entries[i + 1..] {
            if penelope_memory::consolidation::contradicts(&a.text, &b.text) {
                proposals.push(format!(
                    "« {} » ({}) et « {} » ({}) se contredisent : laquelle garder ?",
                    short(&a.text),
                    refer(&a.file, &a.uid),
                    short(&b.text),
                    refer(&b.file, &b.uid)
                ));
                contradictions += 1;
                if contradictions >= 5 {
                    break 'outer;
                }
            }
        }
    }
    (report, proposals)
}
