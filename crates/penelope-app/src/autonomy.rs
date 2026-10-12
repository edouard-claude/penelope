//! Runs autonomes (#346) : une série lancée le matin pour la semaine ne s'arrête sur
//! aucune approbation.
//!
//! ```text
//!  run ──► wf.autonomous.<run> = 1 | 0 ?   le propriétaire l'a décidé (`wf control`)
//!   │            sinon
//!   ├──► settings.autonomous du workflow ?   déclaré (`backlog`)
//!   │            sinon
//!   └──► même question pour le parent        un sous-run `tache` suit sa série
//! ```
//!
//! Relu à chaque appel d'outil : un run déjà lancé le devient sans relance. Le cœur ne
//! fait qu'en décider ; la politique (`penelope-agent`) et l'étape `tool` (orchestrateur)
//! en tirent `auto` à la place de `ask`.

use crate::services::Services;

/// Profondeur maximale suivie dans la chaîne des parents ; celle des sous-workflows est
/// bornée bien en dessous.
const MAX_CHAIN: usize = 8;

/// Le choix du propriétaire pour un run : `"1"` autonome, `"0"` supervisé.
pub fn key(run_id: &str) -> String {
    format!("wf.autonomous.{run_id}")
}

/// Le run `run_id` est-il autonome : par décision du propriétaire, par son workflow, ou
/// par celui d'un parent. Une lecture qui échoue vaut « non » : le doute demande.
pub async fn is_autonomous(s: &Services, run_id: &str) -> bool {
    let mut current = Some(run_id.to_string());
    for _ in 0..MAX_CHAIN {
        let Some(id) = current.take() else {
            return false;
        };
        match s.kv_get(&key(&id)).await.ok().flatten().as_deref() {
            Some("1") => return true,
            Some("0") => return false,
            _ => {}
        }
        let Ok(Some(run)) = s.runs.get(&id).await else {
            return false;
        };
        if s.workflows
            .get(&run.workflow_id)
            .is_some_and(|w| w.settings.autonomous)
        {
            return true;
        }
        current = run.parent_run;
    }
    false
}

/// Fixe le choix du propriétaire pour un run (et, par la chaîne, ses sous-runs).
pub async fn set(s: &Services, run_id: &str, autonomous: bool) -> anyhow::Result<()> {
    s.kv_set(&key(run_id), if autonomous { "1" } else { "0" })
        .await
}
