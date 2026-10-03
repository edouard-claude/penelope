//! Place au niveau Cœur pendant la passe nocturne (issue #298).
//!
//! Le Cœur (`memoire.md`) est servi d'office sous `memory.core_budget_tokens` ; au-delà,
//! le bloc ne prend que les entrées les plus importantes, et la nuit continuait d'y écrire
//! sans regarder la place : jusqu'à 111 entrées hors du bloc le 22/09, et chaque matin le
//! même avertissement, sans effet.
//!
//! ```text
//!  add_entry → memoire.md
//!      │
//!      ├─ la place reste ───────────────────► memoire.md
//!      │
//!      └─ Cœur plein : son importance la classerait-elle dans le bloc servi ?
//!            ├─ non ───────────────────────► notes.md (fiche curée, rappelée à la demande)
//!            └─ oui ───────────────────────► memoire.md ; et, une fois par nuit au plus,
//!                                            la moins utile des entrées hors du bloc
//!                                            descend en notes.md : jamais la parole du
//!                                            propriétaire, jamais ce qui vient d'être écrit
//! ```
//!
//! Ce qui descend est hors du bloc servi : le préfixe stable (T2) ne change que par ce que
//! la nuit ajoute, et seulement la nuit.

use super::*;

/// Section de `notes.md` qui reçoit ce qui descend du Cœur.
pub(super) const DEMOTED_SECTION: &str = "Descendues du Cœur";

/// Fichier des fiches curées : rappelées à la demande, jamais servies d'office.
const NOTES_FILE: &str = "notes.md";

/// Coût d'une entrée dans le bloc, la formule de [`penelope_memory::recall::Snapshots`].
fn cost(text: &str) -> u64 {
    (text.chars().count() as u64 / 4).max(1)
}

/// Ce que la nuit sait du Cœur : son budget, ses entrées telles que l'index les donne, les
/// ajouts de la nuit au fil des lots, et si une entrée est déjà descendue.
pub(super) struct CoreBudget {
    budget: u64,
    /// Entrées actives du Cœur, hors expirées et contestées.
    entries: Vec<IndexedEntry>,
    /// Textes normalisés des candidats dits par le propriétaire (#245) : une entrée qui
    /// les reprend est sa parole.
    quoted: BTreeSet<String>,
    /// Une rétrogradation par nuit au plus.
    demoted: bool,
}

/// Décision pour un ajout au Cœur.
pub(super) enum CorePlacement {
    /// La place reste, ou l'entrée serait servie quand même : au Cœur.
    Core,
    /// Au Cœur, et cette entrée hors du bloc descend en notes (ses rappels, pour le dire).
    CoreAfterDemotion {
        entry: Box<IndexedEntry>,
        recalls: u32,
    },
    /// Hors du bloc quelle que soit la place : en note curée.
    Notes,
}

impl CoreBudget {
    pub(super) async fn read(s: &Services, budget: u64) -> anyhow::Result<CoreBudget> {
        let mut core = CoreBudget {
            budget,
            entries: Vec::new(),
            quoted: s.candidates.owner_quoted_texts().await?,
            demoted: false,
        };
        core.refresh(s).await?;
        Ok(core)
    }

    /// Relit le Cœur depuis l'index : après un lot écrit, ce qu'il contient vraiment.
    pub(super) async fn refresh(&mut self, s: &Services) -> anyhow::Result<()> {
        let hidden = s.memory.hidden_uids().await?;
        let mut entries = s.memory.by_level(Level::Coeur).await?;
        entries.retain(|e| !hidden.contains(&e.uid));
        self.entries = entries;
        Ok(())
    }

    /// Décide la place d'une opération : `None` si elle n'ajoute rien au Cœur.
    pub(super) async fn place(
        &mut self,
        s: &Services,
        op: &Operation,
        snap: &VaultSnapshot,
        day: &str,
    ) -> Option<CorePlacement> {
        let Operation::AddEntry {
            file,
            text,
            importance,
            ..
        } = op
        else {
            return None;
        };
        if file != "memoire.md" {
            return None;
        }
        // L'entrée telle que le bloc la verrait : la plus récente de son importance.
        let mut newcomer = penelope_memory::index::simple_entry(
            &penelope_kernel::ids::Ulid::new().to_string(),
            text,
            Level::Coeur,
            day,
        );
        newcomer.file = file.clone();
        newcomer.importance = *importance;
        newcomer.depuis = Some(day.to_string());
        let used: u64 = self.entries.iter().map(|e| cost(&e.text)).sum();
        if used + cost(text) <= self.budget {
            self.entries.push(newcomer);
            return Some(CorePlacement::Core);
        }
        let mut all = self.entries.clone();
        all.push(newcomer.clone());
        let (_, served) =
            penelope_memory::recall::Snapshots::build_block_with_uids(&all, self.budget);
        if !served.contains(&newcomer.uid) {
            return Some(CorePlacement::Notes);
        }
        self.entries.push(newcomer);
        if self.demoted {
            return Some(CorePlacement::Core);
        }
        match self.least_useful(s, &served, snap, day).await {
            Some((entry, recalls)) => {
                self.demoted = true;
                self.entries.retain(|e| e.uid != entry.uid);
                Some(CorePlacement::CoreAfterDemotion {
                    entry: Box::new(entry),
                    recalls,
                })
            }
            None => Some(CorePlacement::Core),
        }
    }

    /// La moins utile des entrées hors du bloc servi : importance la plus basse, puis la
    /// moins souvent utile, la moins rappelée, la plus ancienne. Jamais une entrée écrite
    /// par le propriétaire (provenance `owner`) ni une qui reprend sa phrase (#245), ni ce
    /// qui a été écrit cette nuit.
    async fn least_useful(
        &self,
        s: &Services,
        served: &[String],
        snap: &VaultSnapshot,
        day: &str,
    ) -> Option<(IndexedEntry, u32)> {
        use penelope_memory::grid::normalized;
        let mut candidates = Vec::new();
        for e in &self.entries {
            if served.contains(&e.uid)
                || snap.owner_uids.contains(&e.uid)
                || e.depuis.as_deref() == Some(day)
            {
                continue;
            }
            let t = normalized(&e.text);
            if self
                .quoted
                .iter()
                .any(|q| *q == t || q.contains(&t) || t.contains(q))
            {
                continue;
            }
            let sig = s.memory.signals_of(&e.uid).await.unwrap_or_default();
            candidates.push((e.clone(), sig.useful_recalls, sig.recalls));
        }
        candidates.sort_by(|(a, au, ar), (b, bu, br)| {
            a.importance
                .unwrap_or(5)
                .cmp(&b.importance.unwrap_or(5))
                .then_with(|| au.cmp(bu))
                .then_with(|| ar.cmp(br))
                .then_with(|| a.uid.cmp(&b.uid))
        });
        candidates
            .into_iter()
            .next()
            .map(|(e, _, recalls)| (e, recalls))
    }
}

/// Même opération, écrite en note curée plutôt qu'au Cœur.
pub(super) fn to_notes(op: &Operation) -> Operation {
    let mut op = op.clone();
    if let Operation::AddEntry { file, .. } = &mut op {
        *file = NOTES_FILE.into();
    }
    op
}

/// Descend une entrée du Cœur en note curée : la ligne quitte `memoire.md` telle quelle
/// (uid, annotations, signaux d'usage conservés) et rejoint `notes.md` ; l'index suit.
/// Deux pré-images dans `mem_history` (`demote_entry`), une par fichier.
pub(super) async fn demote(
    d: &Context,
    vault: &Path,
    run_id: &str,
    day: &str,
    entry: &IndexedEntry,
) -> Result<(), String> {
    let s = &d.services;
    let uid = entry.uid.as_str();
    let raw = std::fs::read_to_string(vault.join(&entry.file)).map_err(|e| e.to_string())?;
    let line =
        edit::line_of(&raw, uid).ok_or_else(|| format!("uid {uid} absent de {}", entry.file))?;
    mutate(
        s,
        vault,
        &entry.file,
        Some(uid),
        "demote_entry",
        run_id,
        |raw| {
            edit::remove_entry(raw, uid)
                .ok_or_else(|| format!("uid {uid} absent de {}", entry.file))
        },
    )
    .await?;
    mutate(
        s,
        vault,
        NOTES_FILE,
        Some(uid),
        "demote_entry",
        run_id,
        |raw| {
            Ok(edit::append_entry(
                raw,
                "# Notes",
                Some(DEMOTED_SECTION),
                &line,
            ))
        },
    )
    .await?;
    let mut moved = entry.clone();
    moved.file = NOTES_FILE.into();
    moved.level = Level::Cure;
    moved.pinned = false;
    moved.anchor = Some(DEMOTED_SECTION.into());
    moved.maj = day.to_string();
    // La provenance d'origine est gardée : `upsert` ne l'écrit qu'une fois.
    let prov = Provenance {
        origin: Origin::Agent,
        session_kind: "consolidation".into(),
        observed_at: s.clock.now_rfc3339(),
        supersedes_uid: None,
        source_ref: Some(format!("dream:{run_id}")),
        session_id: None,
    };
    s.memory
        .upsert(&moved, &prov)
        .await
        .map_err(|e| e.to_string())
}
