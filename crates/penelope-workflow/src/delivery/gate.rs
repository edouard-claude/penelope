//! Gate de production (T5 de #185, issue #193).
//!
//! Après l'E2E de dev, le run ne propose rien seul : il présente au propriétaire un
//! **bilan vérifiable** (plan et run exacts, PR dev, commit, CI, E2E et leurs preuves),
//! puis attend son approbation explicite. Même tout vert, sans clic, aucune PR vers la
//! production.
//!
//! ```text
//!  livraison-e2e ─passed─► livraison-bilan ─passed─► gate-prod ──Proposer──► livraison-prod ─passed─► fin
//!                               │                       │ Re-vérifier ─► livraison-pr      │ stale ─► livraison-pr
//!                               │ superseded ─► $blocked│ Refuser ────► $blocked           │ superseded ─► $blocked
//!                               └─► carte bloquée       └ (attend, durablement)            └─► carte bloquée
//! ```
//!
//! Le clic n'est pas cru sur parole : juste avant la PR, l'étape relit ce que le bilan
//! affirmait ([`freshness`]). Un plan révisé depuis arrête le run ; un bilan trop vieux,
//! une PR dev qui porte un autre commit ou une CI qui n'est plus verte le rendent périmé,
//! et le run repart vérifier PR, CI et E2E avant de reposer une carte neuve. Le bouton de
//! l'ancienne carte ne vaut plus rien : sa visite d'étape est passée.
//!
//! La PR dev → prod est ouverte au plus une fois par run ; elle n'est ni fusionnée ni
//! déployée : le déploiement suit la politique du projet, hors de cet automatisme.

use super::config::{FileConfig, Missing, non_empty};
use super::verdicts::CiVerdict;
use super::{ENTRY, PASSED, blocked_card};
use crate::model::{BLOCKED, Phase as RunPhase, Step, Transition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Étapes du gate, dans l'ordre.
pub const REPORT_STEP: &str = "livraison-bilan";
pub const GATE_STEP: &str = "gate-prod";
pub const PROD_STEP: &str = "livraison-prod";

/// Choix de la carte d'approbation.
pub const APPROVE: &str = "Proposer la PR prod";
pub const RECHECK: &str = "Re-vérifier";
pub const REFUSE: &str = "Refuser";

/// Le bilan ne tient plus : le run repart vérifier.
pub const STALE: &str = "stale";
/// Le plan a une révision plus récente : ce run n'est plus la version approuvable.
pub const SUPERSEDED: &str = "superseded";

/// Âge maximal par défaut d'un bilan, en minutes.
pub const MAX_AGE_MINUTES: u64 = 60;

const REFUSED_TAG: &str = "PR prod refusée par le propriétaire";
const SUPERSEDED_TAG: &str = "plan révisé depuis le bilan : ce run n'est plus la version approuvée";

/// Les étapes du gate, la dernière menant à `end`. Aucune n'a de variante « laisse
/// filer » : l'approbation est un point dur, jamais sauté.
pub fn steps(end: &str) -> Vec<Step> {
    let superseded = Transition {
        tag: SUPERSEDED_TAG.into(),
        ..Transition::on_result(BLOCKED, SUPERSEDED)
    };
    let report_name = "Production · bilan vérifié";
    let prod_name = "Production · PR dev → prod";
    vec![
        Step {
            id: REPORT_STEP.into(),
            name: report_name.into(),
            kind: "delivery".into(),
            delivery: super::PROD_REPORT.into(),
            phase: RunPhase::Deploy,
            transitions: vec![
                Transition::on_result(GATE_STEP, PASSED),
                superseded.clone(),
                Transition::always(&format!("{REPORT_STEP}-bloquee")),
            ],
            ..Default::default()
        },
        blocked_card(REPORT_STEP, report_name),
        Step {
            id: GATE_STEP.into(),
            name: "Production · approbation humaine".into(),
            kind: "user".into(),
            phase: RunPhase::Waiting,
            template: "question".into(),
            choices: vec![APPROVE.into(), RECHECK.into(), REFUSE.into()],
            transitions: vec![
                Transition::on_result(PROD_STEP, APPROVE),
                Transition::on_result(ENTRY, RECHECK),
                Transition {
                    tag: REFUSED_TAG.into(),
                    ..Transition::on_result(BLOCKED, REFUSE)
                },
            ],
            ..Default::default()
        },
        Step {
            id: PROD_STEP.into(),
            name: prod_name.into(),
            kind: "delivery".into(),
            delivery: super::PROD_PULL_REQUEST.into(),
            phase: RunPhase::Deploy,
            transitions: vec![
                Transition::on_result(end, PASSED),
                Transition::on_result(ENTRY, STALE),
                superseded,
                Transition::always(&format!("{PROD_STEP}-bloquee")),
            ],
            ..Default::default()
        },
        blocked_card(PROD_STEP, prod_name),
    ]
}

/// Où proposer la PR de production, et combien de temps un bilan tient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProdTarget {
    pub dev_branch: String,
    pub prod_branch: String,
    pub max_age_ms: u64,
}

/// Résout la cible de production : la branche de prod ne se suppose pas plus que celle de
/// dev, et ne peut pas être la même.
pub fn resolve_prod(cfg: &FileConfig) -> Result<ProdTarget, Vec<Missing>> {
    let dev = non_empty(&cfg.branches.dev);
    let prod = non_empty(&cfg.branches.prod);
    let mut missing = Vec::new();
    if dev.is_none() {
        missing.push(Missing::new(
            "branches.dev",
            "la branche de développement, tête de la PR vers la production",
        ));
    }
    match (&prod, &dev) {
        (None, _) => missing.push(Missing::new(
            "branches.prod",
            "la branche de production vers laquelle proposer la PR",
        )),
        (Some(p), Some(d)) if p == d => missing.push(Missing::new(
            "branches.prod",
            format!(
                "`{p}` est aussi la branche de dev : une PR ne relie pas une branche à elle-même"
            ),
        )),
        _ => {}
    }
    match (dev, prod) {
        (Some(dev_branch), Some(prod_branch)) if missing.is_empty() => Ok(ProdTarget {
            dev_branch,
            prod_branch,
            max_age_ms: cfg.prod.max_age_minutes.unwrap_or(MAX_AGE_MINUTES).max(1) * 60_000,
        }),
        _ => Err(missing),
    }
}

/// La révision exacte d'un plan : sa version et son empreinte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRef {
    pub version: u64,
    pub fingerprint: String,
}

/// Ce que le propriétaire approuve : tout ce qui a été vérifié, et quand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bilan {
    pub run: String,
    /// Le plan exécuté par le run ; `None` pour un run qui n'en vient pas.
    pub plan: Option<PlanRef>,
    pub forge: String,
    pub repo: String,
    /// Branche de travail et commit vérifié.
    pub head: String,
    pub sha: String,
    pub dev_branch: String,
    pub prod_branch: String,
    pub dev_pr_number: u64,
    pub dev_pr_url: String,
    /// `green`, ou `none` pour un projet sans CI.
    pub ci: String,
    pub ci_detail: String,
    pub e2e_url: String,
    pub e2e_checks: usize,
    /// Fichier des preuves de l'E2E, dans l'espace du run.
    pub evidence: String,
    pub verified_at_ms: u64,
}

fn short(s: &str, n: usize) -> &str {
    &s[..s.len().min(n)]
}

impl Bilan {
    /// Empreinte du bilan : un autre commit, une autre CI, un autre E2E ou un autre plan
    /// en donnent une autre. Elle reste dans la trace du run (sorties du bilan et de la PR
    /// de production), pas sur la carte.
    pub fn fingerprint(&self) -> String {
        let raw = serde_json::to_vec(self).unwrap_or_default();
        Sha256::digest(&raw)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Ce qui a été vérifié, une ligne par preuve : la carte et la PR de production le
    /// reprennent.
    pub fn summary(&self) -> String {
        let plan = match &self.plan {
            Some(p) => format!(
                "plan v{} (empreinte {}), run `{}`",
                p.version,
                short(&p.fingerprint, 12),
                self.run
            ),
            None => format!("run `{}`", self.run),
        };
        let ci = if self.ci == "none" {
            "pas de CI déclarée (`ci.provider = \"none\"`)".to_string()
        } else {
            format!("verte : {}", self.ci_detail)
        };
        format!(
            "- Version : {plan}\n\
             - PR dev : {} (`{}` → `{}`, commit `{}`)\n\
             - CI : {ci}\n\
             - E2E : vert sur {}, {} contrôle(s), preuves `{}`",
            self.dev_pr_url,
            self.head,
            self.dev_branch,
            short(&self.sha, 8),
            self.e2e_url,
            self.e2e_checks,
            self.evidence,
        )
    }

    /// Le bilan tel que la carte d'approbation le montre.
    pub fn render(&self, max_age_ms: u64) -> String {
        format!(
            "Bilan vérifié avant la production\n{}\n\nValable {} min, puis re-vérifié avant \
             toute PR.\n\n\
             « {APPROVE} » ouvre la PR `{}` → `{}` sur {} `{}`, une seule fois. Elle n'est ni \
             fusionnée ni déployée : le déploiement suit la politique du projet. La PR dev doit \
             être fusionnée dans `{}` avant. « {RECHECK} » refait PR, CI et E2E ; « {REFUSE} » \
             arrête le run sans rien proposer.",
            self.summary(),
            max_age_ms / 60_000,
            self.dev_branch,
            self.prod_branch,
            self.forge,
            self.repo,
            self.dev_branch,
        )
    }
}

/// Le run exécute-t-il encore le plan actif de sa conversation ? `Some(raison)` sinon.
pub fn supersession(plan: &PlanRef, active: Option<&PlanRef>) -> Option<String> {
    match active {
        Some(active) if active == plan => None,
        Some(active) => Some(format!(
            "la conversation en est au plan v{} (empreinte {}), ce run exécute le plan v{} \
             (empreinte {})",
            active.version,
            short(&active.fingerprint, 12),
            plan.version,
            short(&plan.fingerprint, 12)
        )),
        None => Some("la conversation n'a plus de plan actif pour ce run".into()),
    }
}

/// Ce que l'étape de production relit au moment de l'approbation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub now_ms: u64,
    /// Le plan actif de la conversation du run.
    pub active_plan: Option<PlanRef>,
    /// Le commit que porte la PR dev sur le forgeur.
    pub dev_pr_head: Option<String>,
    /// La CI relue sur le commit vérifié ; `None` si elle n'a pas été relue.
    pub ci: Option<CiVerdict>,
}

/// Le bilan tient-il encore ?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    /// Il faut re-vérifier : la raison.
    Stale(String),
    /// Le run n'est plus la version approuvable : la raison.
    Superseded(String),
}

/// Juge le bilan contre ce qui est relu. L'ordre compte : un plan révisé ne se rattrape
/// pas par une nouvelle vérification, un bilan périmé si.
pub fn freshness(b: &Bilan, seen: &Seen, max_age_ms: u64) -> Freshness {
    if let Some(why) = b
        .plan
        .as_ref()
        .and_then(|plan| supersession(plan, seen.active_plan.as_ref()))
    {
        return Freshness::Superseded(why);
    }
    let age = seen.now_ms.saturating_sub(b.verified_at_ms);
    if age > max_age_ms {
        return Freshness::Stale(format!(
            "bilan vieux de {} min (au plus {})",
            age / 60_000,
            max_age_ms / 60_000
        ));
    }
    match seen.dev_pr_head.as_deref() {
        Some(head) if head == b.sha => {}
        Some(head) => {
            return Freshness::Stale(format!(
                "la PR dev porte maintenant le commit `{}`, le bilan vérifiait `{}`",
                short(head, 8),
                short(&b.sha, 8)
            ));
        }
        None => {
            return Freshness::Stale("le forgeur ne dit plus quel commit porte la PR dev".into());
        }
    }
    if b.ci != "none" {
        match &seen.ci {
            Some(CiVerdict::Green(_)) => {}
            Some(CiVerdict::Red(d)) => {
                return Freshness::Stale(format!("la CI n'est plus verte : {d}"));
            }
            Some(CiVerdict::Pending(d)) => {
                return Freshness::Stale(format!("la CI est relancée, sans verdict : {d}"));
            }
            None => return Freshness::Stale("la CI n'a pas pu être relue".into()),
        }
    }
    Freshness::Fresh
}

#[cfg(test)]
mod tests;
