//! Plan conversationnel versionné et gate de lancement (#186).

use penelope_store::{Store, StoreError, rusqlite::params};
use serde::{Deserialize, Serialize};

/// La phase donne au moteur un sens stable, même si l'ordre des pas est proposé
/// librement par l'orchestrateur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Specification,
    Tests,
    Implementation,
    Review,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub phase: Phase,
    pub title: String,
}

impl PlanStep {
    pub fn new(phase: Phase, title: impl Into<String>) -> Self {
        Self {
            phase,
            title: title.into(),
        }
    }
}

/// Le seul passage vers l'exécution est l'approbation explicite du propriétaire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanGate {
    Review,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevision {
    pub version: u64,
    pub goal: String,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    goal: String,
    version: u64,
    steps: Vec<PlanStep>,
    gate: PlanGate,
    history: Vec<PlanRevision>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("le but du plan est vide")]
    EmptyGoal,
    #[error("le plan n'a aucune étape")]
    EmptySteps,
    #[error("une étape du plan n'a pas de titre")]
    EmptyTitle,
    #[error("le plan a déjà été approuvé")]
    AlreadyApproved,
    #[error("la version {0} du plan est introuvable")]
    UnknownVersion(u64),
    #[error("la version du plan ne peut plus être incrémentée")]
    VersionOverflow,
}

impl Plan {
    pub fn new(goal: impl Into<String>, steps: Vec<PlanStep>) -> Result<Self, PlanError> {
        let goal = goal.into();
        Self::validate(&goal, &steps)?;
        let history = vec![PlanRevision {
            version: 1,
            goal: goal.clone(),
            steps: steps.clone(),
        }];
        Ok(Self {
            goal,
            version: 1,
            steps,
            gate: PlanGate::Review,
            history,
        })
    }

    pub fn goal(&self) -> &str {
        &self.goal
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn gate(&self) -> PlanGate {
        self.gate
    }

    pub fn history(&self) -> &[PlanRevision] {
        &self.history
    }

    pub fn can_execute(&self) -> bool {
        self.gate == PlanGate::Ready
    }

    pub fn revise(
        &mut self,
        goal: impl Into<String>,
        steps: Vec<PlanStep>,
    ) -> Result<(), PlanError> {
        self.require_review()?;
        let goal = goal.into();
        Self::validate(&goal, &steps)?;
        self.version = self
            .version
            .checked_add(1)
            .ok_or(PlanError::VersionOverflow)?;
        self.history.push(PlanRevision {
            version: self.version,
            goal: goal.clone(),
            steps: steps.clone(),
        });
        self.goal = goal;
        self.steps = steps;
        Ok(())
    }

    /// Revenir au contenu ancien crée une nouvelle version et garde la trace du retour.
    pub fn restore(&mut self, version: u64) -> Result<(), PlanError> {
        self.require_review()?;
        let old = self
            .history
            .iter()
            .find(|revision| revision.version == version)
            .ok_or(PlanError::UnknownVersion(version))?
            .clone();
        self.revise(old.goal, old.steps)
    }

    pub fn approve(&mut self) -> Result<(), PlanError> {
        self.require_review()?;
        self.gate = PlanGate::Ready;
        Ok(())
    }

    fn require_review(&self) -> Result<(), PlanError> {
        (self.gate == PlanGate::Review)
            .then_some(())
            .ok_or(PlanError::AlreadyApproved)
    }

    fn validate(goal: &str, steps: &[PlanStep]) -> Result<(), PlanError> {
        if goal.trim().is_empty() {
            return Err(PlanError::EmptyGoal);
        }
        if steps.is_empty() {
            return Err(PlanError::EmptySteps);
        }
        if steps.iter().any(|step| step.title.trim().is_empty()) {
            return Err(PlanError::EmptyTitle);
        }
        Ok(())
    }
}

/// Écriture durable du brouillon à chaque transition de gate ou de version.
#[derive(Clone)]
pub struct PlanStore {
    store: Store,
}

impl PlanStore {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub async fn save(&self, session: &str, plan: &Plan) -> penelope_store::Result<()> {
        let key = format!("workflow.plan.{session}");
        let value = serde_json::to_string(plan).map_err(|e| StoreError::other(e.to_string()))?;
        self.store
            .write_durable(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v, ts) VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                    params![key, value],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn get(&self, session: &str) -> penelope_store::Result<Option<Plan>> {
        let key = format!("workflow.plan.{session}");
        let value = self
            .store
            .read(move |db| {
                let mut stmt = db.prepare("SELECT v FROM kv WHERE k = ?1")?;
                let mut rows = stmt.query([key])?;
                Ok(match rows.next()? {
                    Some(row) => Some(row.get::<_, String>(0)?),
                    None => None,
                })
            })
            .await?;
        value
            .map(|json| serde_json::from_str(&json).map_err(|e| StoreError::other(e.to_string())))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(title: &str) -> Vec<PlanStep> {
        vec![
            PlanStep::new(Phase::Specification, "Définir le contrat"),
            PlanStep::new(Phase::Tests, title),
            PlanStep::new(Phase::Implementation, "Coder le changement"),
        ]
    }

    #[test]
    fn execution_waits_for_the_explicit_go_gate() {
        let mut plan = Plan::new("Corriger la commande", steps("Écrire les tests")).unwrap();
        assert_eq!(plan.version(), 1);
        assert_eq!(plan.gate(), PlanGate::Review);
        assert!(!plan.can_execute());
        plan.approve().unwrap();
        assert_eq!(plan.gate(), PlanGate::Ready);
        assert!(plan.can_execute());
    }

    #[test]
    fn revisions_are_unbounded_and_preserve_their_history() {
        let mut plan = Plan::new("Corriger la commande", steps("Tests initiaux")).unwrap();
        for n in 2..=25 {
            plan.revise(
                "Corriger la commande",
                steps(&format!("Tests révision {n}")),
            )
            .unwrap();
            assert_eq!(plan.version(), n);
        }
        assert_eq!(plan.history().len(), 25);
        assert_eq!(plan.history()[0].version, 1);
        assert_eq!(plan.history()[24].version, 25);
        assert!(!plan.can_execute());
    }

    #[test]
    fn restoring_an_old_revision_creates_a_new_version() {
        let mut plan = Plan::new("Corriger la commande", steps("Tests initiaux")).unwrap();
        plan.revise(
            "Corriger la commande et sa documentation",
            steps("Tests révisés"),
        )
        .unwrap();
        plan.restore(1).unwrap();
        assert_eq!(plan.version(), 3);
        assert_eq!(plan.goal(), "Corriger la commande");
        assert_eq!(plan.steps(), plan.history()[0].steps);
        assert_eq!(plan.history().len(), 3);
        assert!(!plan.can_execute());
    }

    #[test]
    fn an_approved_plan_is_immutable_and_serializable() {
        let mut plan = Plan::new("Corriger la commande", steps("Écrire les tests")).unwrap();
        plan.approve().unwrap();
        assert!(plan.revise("Autre but", steps("Autres tests")).is_err());
        assert!(plan.restore(1).is_err());
        let raw = serde_json::to_string(&plan).unwrap();
        let restored: Plan = serde_json::from_str(&raw).unwrap();
        assert_eq!(restored, plan);
        assert!(restored.can_execute());
    }

    #[tokio::test]
    async fn a_plan_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut plan = Plan::new("Corriger la commande", steps("Tests initiaux")).unwrap();
        plan.revise("Corriger la commande", steps("Tests révisés"))
            .unwrap();
        let first = PlanStore::new(penelope_store::Store::open(&path).unwrap());
        first.save("topic:21", &plan).await.unwrap();
        drop(first);
        let reopened = PlanStore::new(penelope_store::Store::open(&path).unwrap());
        assert_eq!(reopened.get("topic:21").await.unwrap(), Some(plan));
    }
}
