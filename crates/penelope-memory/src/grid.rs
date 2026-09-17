//! Grille de tri du rêve (issue #37) : un verdict par critère plutôt qu'une importance
//! auto-attribuée ou un comptage d'occurrences.
//!
//! ```text
//!  candidat ─► modèle : durable ? utile ? précis ? introuvable ailleurs ? endossé ?
//!                  │
//!                  ▼  décision calculée ici, pas par le modèle
//!   introuvable ✗ ─────────────────────────► ignoré (retrouvable ailleurs)
//!   précis ✗ ──────────────────────────────► ignoré
//!   endossé ✗ ─────────────────────────────► ignoré
//!   utile ✗ ───────────────────────────────► ignoré
//!   durable ✗ ─────────────────────────────► journal (projets.md, expire)
//!   tout ✓ ────────────────────────────────► mémoire durable (add, update, supersede, noop)
//! ```
//!
//! Les secrets ne passent pas par la grille : ils sont rangés dans le magasin de secrets
//! dès la relecture, et le candidat ne porte plus qu'une référence `${SECRET:nom}`.

use crate::consolidation::Operation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Section de `projets.md` qui reçoit le journal des états passagers.
pub const JOURNAL_SECTION: &str = "États en cours";
/// Durée de vie d'un état passager quand le modèle n'en donne pas.
pub const JOURNAL_DAYS: i64 = 14;
/// Durée de vie maximale d'un état passager.
pub const JOURNAL_MAX_DAYS: i64 = 90;
/// Une entrée durable jamais rappelée pendant ce délai est proposée au retrait.
pub const UNUSED_DAYS: i64 = 60;

/// Verdict du modèle sur un candidat, critère par critère.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// Numéro du candidat dans la liste envoyée (à partir de 1).
    pub candidat: usize,
    #[serde(default)]
    pub durable: bool,
    #[serde(default)]
    pub utile: bool,
    #[serde(default)]
    pub precis: bool,
    #[serde(default)]
    pub introuvable: bool,
    #[serde(default)]
    pub endosse: bool,
    /// Justification d'une ligne, recopiée dans `DREAMS.md`.
    #[serde(default)]
    pub justification: String,
    /// Fin de validité d'un état passager (`AAAA-MM-JJ`).
    #[serde(default)]
    pub expire: Option<String>,
}

/// Où va un candidat.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// Mémoire durable : `profil.md`, `memoire.md`, `projets.md`, pratiques.
    Durable,
    /// Vrai aujourd'hui, pas dans un mois : journal des états en cours, avec expiration.
    Journal { expire: String },
    /// Écarté, avec la raison.
    Ignored(String),
}

impl Placement {
    pub fn label(&self) -> &'static str {
        match self {
            Placement::Durable => "gardé",
            Placement::Journal { .. } => "journal",
            Placement::Ignored(_) => "ignoré",
        }
    }
}

fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

impl Verdict {
    /// Décision déterministe à partir des critères : le modèle juge, le code décide.
    pub fn placement(&self, today: &str) -> Placement {
        if !self.introuvable {
            return Placement::Ignored("retrouvable ailleurs (code, docs, tracker, git)".into());
        }
        if !self.precis {
            return Placement::Ignored("imprécis : sujet ou phrase incomplets".into());
        }
        if !self.endosse {
            return Placement::Ignored(
                "ni dit ni confirmé par le propriétaire, ni constaté par un outil".into(),
            );
        }
        if !self.utile {
            return Placement::Ignored("ne change rien à ce que Pénélope fera".into());
        }
        if !self.durable {
            return Placement::Journal {
                expire: journal_expiry(self.expire.as_deref(), today),
            };
        }
        Placement::Durable
    }

    /// Les cinq critères, pour le rapport.
    pub fn criteria_line(&self) -> String {
        format!(
            "durable {} · utile {} · précis {} · introuvable ailleurs {} · endossé {}",
            mark(self.durable),
            mark(self.utile),
            mark(self.precis),
            mark(self.introuvable),
            mark(self.endosse)
        )
    }
}

fn add_days(today: &str, days: i64) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .ok()
        .map(|d| d + chrono::Duration::days(days))
}

/// Expiration d'un état passager : la date demandée si elle est dans les 90 jours, sinon
/// dans 14 jours.
pub fn journal_expiry(requested: Option<&str>, today: &str) -> String {
    let fmt = |d: chrono::NaiveDate| d.format("%Y-%m-%d").to_string();
    let Some(default) = add_days(today, JOURNAL_DAYS) else {
        return today.to_string();
    };
    let max = add_days(today, JOURNAL_MAX_DAYS).unwrap_or(default);
    let start = add_days(today, 0).unwrap_or(default);
    match requested.and_then(|r| chrono::NaiveDate::parse_from_str(r.trim(), "%Y-%m-%d").ok()) {
        Some(d) if d >= start && d <= max => fmt(d),
        _ => fmt(default),
    }
}

/// Date limite du retour d'usage : `today` moins [`UNUSED_DAYS`].
pub fn unused_cutoff(today: &str) -> String {
    add_days(today, -UNUSED_DAYS)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| today.to_string())
}

/// Réponse du modèle de consolidation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Consolidation {
    pub verdicts: Vec<Verdict>,
    /// Opérations et candidat auquel chacune se rattache.
    pub operations: Vec<(Option<usize>, Operation)>,
    /// `noop` : rien à écrire (déjà en mémoire), avec la raison.
    pub noops: Vec<(Option<usize>, String)>,
}

impl Consolidation {
    pub fn verdict(&self, candidat: usize) -> Option<&Verdict> {
        self.verdicts.iter().find(|v| v.candidat == candidat)
    }
}

/// Lit `{"tri": [...], "operations": [...]}` ; un élément malformé est ignoré, pas le lot.
pub fn parse(raw: &str) -> Consolidation {
    let Some(v) = raw
        .find('{')
        .zip(raw.rfind('}'))
        .filter(|(a, b)| a < b)
        .and_then(|(a, b)| serde_json::from_str::<Value>(&raw[a..=b]).ok())
    else {
        return Consolidation::default();
    };
    let mut out = Consolidation::default();
    for t in v["tri"].as_array().cloned().unwrap_or_default() {
        if let Ok(verdict) = serde_json::from_value::<Verdict>(t) {
            out.verdicts.push(verdict);
        }
    }
    for op in v["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(50)
    {
        let candidat = op["candidat"].as_u64().map(|n| n as usize);
        if op["op"] == "noop" {
            let reason = op["reason"]
                .as_str()
                .or_else(|| op["raison"].as_str())
                .unwrap_or("déjà en mémoire")
                .to_string();
            out.noops.push((candidat, reason));
            continue;
        }
        if let Ok(parsed) = serde_json::from_value::<Operation>(op) {
            out.operations.push((candidat, parsed));
        }
    }
    out
}

/// Texte normalisé pour repérer un doublon : minuscules, lettres et chiffres.
pub fn normalized(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(durable: bool, utile: bool, precis: bool, introuvable: bool, endosse: bool) -> Verdict {
        Verdict {
            candidat: 1,
            durable,
            utile,
            precis,
            introuvable,
            endosse,
            justification: String::new(),
            expire: None,
        }
    }

    /// Le tableau de l'issue #37 : règle gardée, état passager au journal, élément
    /// retrouvable ailleurs ignoré, déduction non endossée ignorée.
    #[test]
    fn the_grid_decides_from_the_criteria() {
        let today = "2026-09-17";
        assert_eq!(
            v(true, true, true, true, true).placement(today),
            Placement::Durable
        );
        assert_eq!(
            v(false, true, true, true, true).placement(today),
            Placement::Journal {
                expire: "2026-10-01".into()
            }
        );
        assert!(matches!(
            v(true, true, true, false, true).placement(today),
            Placement::Ignored(r) if r.contains("ailleurs")
        ));
        assert!(matches!(
            v(true, true, true, true, false).placement(today),
            Placement::Ignored(r) if r.contains("propriétaire")
        ));
        assert!(matches!(
            v(true, false, true, true, true).placement(today),
            Placement::Ignored(_)
        ));
        // Passager et sans utilité : ignoré, pas même au journal.
        assert!(matches!(
            v(false, false, true, true, true).placement(today),
            Placement::Ignored(_)
        ));
        let line = v(true, false, true, true, true).criteria_line();
        assert_eq!(
            line,
            "durable ✓ · utile ✗ · précis ✓ · introuvable ailleurs ✓ · endossé ✓"
        );
    }

    #[test]
    fn journal_expiry_is_bounded() {
        let today = "2026-09-17";
        assert_eq!(journal_expiry(Some("2026-09-20"), today), "2026-09-20");
        assert_eq!(journal_expiry(Some("2027-09-20"), today), "2026-10-01");
        assert_eq!(journal_expiry(Some("2026-09-01"), today), "2026-10-01");
        assert_eq!(journal_expiry(Some("demain"), today), "2026-10-01");
        assert_eq!(journal_expiry(None, today), "2026-10-01");
        assert_eq!(unused_cutoff(today), "2026-07-19");
    }

    #[test]
    fn verdicts_operations_and_noops_are_read_leniently() {
        let raw = r#"Voici : {"tri": [
            {"candidat": 1, "durable": true, "utile": true, "precis": true, "introuvable": true,
             "endosse": true, "justification": "règle dite par le propriétaire"},
            {"candidat": "deux"}
          ],
          "operations": [
            {"op": "add_entry", "candidat": 1, "file": "profil.md", "text": "Toujours pousser sur dev"},
            {"op": "supersede_entry", "candidat": 2, "uid": "01OLD", "text": "Base dev sur le port 40000", "reason": "corrigé"},
            {"op": "noop", "candidat": 3, "reason": "déjà en mémoire"},
            {"op": "inconnue", "candidat": 4}
          ]}"#;
        let c = parse(raw);
        assert_eq!(c.verdicts.len(), 1);
        assert_eq!(
            c.verdict(1).unwrap().justification,
            "règle dite par le propriétaire"
        );
        assert_eq!(c.operations.len(), 2);
        assert_eq!(c.operations[0].0, Some(1));
        assert!(matches!(
            c.operations[1].1,
            Operation::SupersedeEntry { .. }
        ));
        assert_eq!(c.noops, vec![(Some(3), "déjà en mémoire".to_string())]);
        assert_eq!(parse("rien").verdicts.len(), 0);
        assert_eq!(normalized("Base  dev : 127.0.0.1"), "base dev 127 0 0 1");
    }
}
