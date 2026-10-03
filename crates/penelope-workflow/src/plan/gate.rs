//! Le gate « vas-y » vu depuis la conversation (issue #302).
//!
//! Le bouton de la carte est le seul passage vers l'exécution ; la conversation, elle,
//! doit savoir où en est le plan. Le 03/10, après le clic, la session a appelé
//! `workflow_start` quatre fois : chaque appel était refusé par un texte qui renvoyait à
//! `workflow_plan`, comme si rien n'avait été approuvé, et le propriétaire avait validé
//! quatre cartes pour rien. Ici, le refus dit l'état réel : plan en revue, approuvé en
//! attente du clic, ou lancé, avec le run et son état.

use super::{PlanDraft, PlanStore, plan_run_id};
use crate::runs::{Run, RunState, RunStore};

/// Un plan approuvé de la session et le run que « vas-y » lui a donné.
#[derive(Debug, Clone)]
pub struct PlanRun {
    pub draft: PlanDraft,
    pub run: Run,
}

impl PlanRun {
    /// L'état du run en une proposition : « en pause à l'étape `e1-spec` ».
    pub fn state_line(&self) -> String {
        state_line(&self.run)
    }

    /// Vrai tant que le run peut encore avancer (en cours, en pause, bloqué).
    pub fn is_live(&self) -> bool {
        !self.run.state.is_terminal()
    }
}

/// L'état d'un run en une proposition, avec l'étape quand elle a un sens.
pub fn state_line(run: &Run) -> String {
    let step = run
        .current_step
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| format!(" à l'étape `{s}`"))
        .unwrap_or_default();
    let error = run
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| format!(" ({e})"))
        .unwrap_or_default();
    match run.state {
        RunState::Running => format!("en cours{step}"),
        RunState::Paused => format!("en pause{step}"),
        RunState::Blocked => format!("bloqué{step}{error}"),
        RunState::Done => "terminé".to_string(),
        RunState::Failed => format!("en échec{error}"),
        RunState::Cancelled => "annulé".to_string(),
    }
}

/// Ce que la conversation sait du gate de son plan.
#[derive(Debug, Clone)]
pub enum Gate {
    /// Aucun plan dans cette conversation.
    NoPlan,
    /// Un plan en revue : le propriétaire ne l'a pas validé. `previous` : le run encore
    /// vivant d'un plan approuvé plus tôt dans la même conversation, s'il en reste un.
    InReview {
        draft: Box<PlanDraft>,
        previous: Option<Box<PlanRun>>,
    },
    /// Approuvé, mais sans run : le lancement s'est arrêté entre l'approbation et la
    /// ligne du run ; le clic rejoué le reprend.
    Approved { draft: Box<PlanDraft> },
    /// Approuvé et lancé.
    Launched(Box<PlanRun>),
}

impl Gate {
    /// Le refus de `workflow_start` depuis un canal : l'état réel, et ce que le modèle
    /// peut faire à la place.
    pub fn refusal(&self) -> String {
        match self {
            Gate::NoPlan => "`workflow_start` ne lance rien depuis cette conversation : propose \
                             d'abord un plan avec `workflow_plan` ; seul le propriétaire lance \
                             le run, par le bouton « vas-y » de la carte"
                .to_string(),
            Gate::InReview { draft, previous } => {
                let mut out = format!(
                    "le plan v{} (« {} ») est en revue : le propriétaire le valide par le \
                     bouton « vas-y » de la carte, `workflow_start` ne le remplace pas. \
                     Corrige-le avec `workflow_plan` s'il le demande, sinon attends son clic",
                    draft.plan.version(),
                    draft.plan.goal()
                );
                if let Some(p) = previous {
                    out.push_str(&format!(
                        " ; le run `{}` d'un plan précédent est {}",
                        p.run.id,
                        p.state_line()
                    ));
                }
                out
            }
            Gate::Approved { draft } => format!(
                "le plan v{} (« {} ») est approuvé et attend son run : seul le clic « vas-y » \
                 du propriétaire le lance, `workflow_start` ne le remplace pas ; dis-le-lui",
                draft.plan.version(),
                draft.plan.goal()
            ),
            Gate::Launched(p) => {
                let mut out = format!(
                    "le plan v{} (« {} ») est déjà lancé : run `{}`, {}. Ne relance rien : \
                     `workflow_status` (run_id `{}`) donne son état",
                    p.draft.plan.version(),
                    p.draft.plan.goal(),
                    p.run.id,
                    p.state_line(),
                    p.run.id
                );
                if p.run.state == RunState::Paused {
                    out.push_str(&format!(
                        ", `workflow_control` (run_id `{}`, op `resume`) le reprend",
                        p.run.id
                    ));
                }
                out
            }
        }
    }
}

/// Les runs des plans approuvés de la session, du plus ancien au plus récent.
pub async fn plan_runs(
    plans: &PlanStore,
    runs: &RunStore,
    session: &str,
) -> penelope_store::Result<Vec<PlanRun>> {
    let mut out = Vec::new();
    for draft in plans.approved(session).await? {
        if let Some(run) = runs
            .get(&plan_run_id(session, &draft.fingerprint()))
            .await?
        {
            out.push(PlanRun { draft, run });
        }
    }
    Ok(out)
}

/// Le run encore vivant le plus récent parmi les plans de la session.
pub async fn live_run(
    plans: &PlanStore,
    runs: &RunStore,
    session: &str,
) -> penelope_store::Result<Option<PlanRun>> {
    let mut live: Vec<PlanRun> = plan_runs(plans, runs, session)
        .await?
        .into_iter()
        .filter(PlanRun::is_live)
        .collect();
    live.sort_by(|a, b| a.run.started_at.cmp(&b.run.started_at));
    Ok(live.pop())
}

/// L'état du gate du plan actif de la session.
pub async fn gate(
    plans: &PlanStore,
    runs: &RunStore,
    session: &str,
) -> penelope_store::Result<Gate> {
    let Some(draft) = plans.get(session).await? else {
        return Ok(Gate::NoPlan);
    };
    if !draft.plan.can_execute() {
        let previous = live_run(plans, runs, session).await?.map(Box::new);
        return Ok(Gate::InReview {
            draft: Box::new(draft),
            previous,
        });
    }
    Ok(
        match runs
            .get(&plan_run_id(session, &draft.fingerprint()))
            .await?
        {
            Some(run) => Gate::Launched(Box::new(PlanRun { draft, run })),
            None => Gate::Approved {
                draft: Box::new(draft),
            },
        },
    )
}

/// « vas-y » tapé en texte : le message entier, à la casse, aux espaces, aux accents et
/// à la ponctuation près (« Vas-y ! », « vasy », « ok vas-y »). Jamais un mot plus
/// général (« go », « ok ») : tapé pour répondre au modèle, il doit lui parvenir.
pub fn is_go_text(text: &str) -> bool {
    let folded: String = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ù' | 'û' | 'ü' => 'u',
            other => other,
        })
        .collect();
    matches!(
        folded.as_str(),
        "vasy" | "okvasy" | "ouivasy" | "allezvasy" | "vasygo" | "govasy"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Phase, Plan, PlanStep};
    use serde_json::json;

    fn draft(goal: &str) -> PlanDraft {
        PlanDraft {
            workflow_id: "build-verify".into(),
            params: json!({}),
            brief: None,
            plan: Plan::new(
                goal,
                vec![
                    PlanStep::new(Phase::Specification, "Définir"),
                    PlanStep::new(Phase::Tests, "Tester"),
                ],
            )
            .unwrap(),
        }
    }

    fn run(id: &str, state: RunState, step: Option<&str>, error: Option<&str>) -> Run {
        Run {
            id: id.into(),
            workflow_id: "plan-x".into(),
            session_id: "s-run".into(),
            params: json!({}),
            state,
            current_step: step.map(String::from),
            phase: None,
            iterations: 1,
            max_iterations: 40,
            step_outputs: json!({}),
            workdir: None,
            spent_usd: 0.0,
            spent_tokens: 0,
            started_at: "2026-10-03T04:14:05Z".into(),
            finished_at: None,
            result: None,
            error: error.map(String::from),
            parent_run: None,
            depth: 0,
        }
    }

    /// Le texte du refus dit l'état réel et nomme le run, pas « propose d'abord un
    /// plan » quand le plan est approuvé et lancé (le constat du 03/10).
    #[test]
    fn the_refusal_names_the_run_and_its_state() {
        assert!(Gate::NoPlan.refusal().contains("`workflow_plan`"));
        let mut approved = draft("Corriger /stop");
        approved.approve(1).unwrap();
        let paused = Gate::Launched(Box::new(PlanRun {
            draft: approved.clone(),
            run: run("r_plan_1", RunState::Paused, Some("e1-spec"), None),
        }))
        .refusal();
        assert!(paused.contains("déjà lancé"), "{paused}");
        assert!(paused.contains("run `r_plan_1`"), "{paused}");
        assert!(paused.contains("en pause à l'étape `e1-spec`"), "{paused}");
        assert!(paused.contains("op `resume`"), "{paused}");
        assert!(!paused.contains("propose d'abord"), "{paused}");
        let running = Gate::Launched(Box::new(PlanRun {
            draft: approved.clone(),
            run: run("r_plan_1", RunState::Running, Some("e2-tests"), None),
        }))
        .refusal();
        assert!(
            running.contains("en cours à l'étape `e2-tests`"),
            "{running}"
        );
        assert!(!running.contains("resume"), "{running}");
        let waiting = Gate::Approved {
            draft: Box::new(approved),
        }
        .refusal();
        assert!(
            waiting.contains("approuvé et attend son run") && waiting.contains("vas-y"),
            "{waiting}"
        );
        let review = Gate::InReview {
            draft: Box::new(draft("Autre chose")),
            previous: Some(Box::new(PlanRun {
                draft: draft("Ancien"),
                run: run("r_plan_0", RunState::Running, Some("e1"), None),
            })),
        }
        .refusal();
        assert!(
            review.contains("en revue") && review.contains("`r_plan_0`"),
            "{review}"
        );
    }

    #[test]
    fn the_state_line_follows_the_run() {
        assert_eq!(
            state_line(&run(
                "r",
                RunState::Blocked,
                Some("e3"),
                Some("limite atteinte")
            )),
            "bloqué à l'étape `e3` (limite atteinte)"
        );
        assert_eq!(
            state_line(&run("r", RunState::Done, Some("e9"), None)),
            "terminé"
        );
        assert_eq!(
            state_line(&run("r", RunState::Failed, None, Some("budget"))),
            "en échec (budget)"
        );
        assert_eq!(
            state_line(&run("r", RunState::Running, None, None)),
            "en cours"
        );
    }

    /// « vas-y » sous toutes ses formes courantes, et rien d'autre : « go » ou « ok »
    /// répondent au modèle.
    #[test]
    fn typed_go_is_recognised_strictly() {
        for yes in [
            "vas-y",
            "Vas-y !",
            "vasy",
            "VAS Y",
            "ok vas-y",
            "Oui, vas-y.",
            "vas-y 🚀",
        ] {
            assert!(is_go_text(yes), "{yes}");
        }
        for no in [
            "go",
            "ok",
            "vas-y sur les tests",
            "non",
            "",
            "vas-y ? ou pas",
        ] {
            assert!(!is_go_text(no), "{no}");
        }
    }

    #[tokio::test]
    async fn the_gate_reads_the_plan_and_its_run() {
        use crate::plan::execution::{self, Limits};
        let dir = tempfile::tempdir().unwrap();
        let store = penelope_store::Store::open(dir.path().join("state.db")).unwrap();
        let plans = PlanStore::new(store.clone());
        let clock: penelope_kernel::clock::SharedClock =
            std::sync::Arc::new(penelope_kernel::clock::TestClock::default());
        let runs = RunStore::new(store.clone(), clock);
        assert!(matches!(
            gate(&plans, &runs, "s").await.unwrap(),
            Gate::NoPlan
        ));
        let v1 = draft("Corriger /stop");
        plans.create("s", &v1).await.unwrap();
        assert!(matches!(
            gate(&plans, &runs, "s").await.unwrap(),
            Gate::InReview { previous: None, .. }
        ));
        let mut approved = v1.clone();
        approved.approve(1).unwrap();
        plans.replace("s", &v1, &approved).await.unwrap();
        assert!(matches!(
            gate(&plans, &runs, "s").await.unwrap(),
            Gate::Approved { .. }
        ));
        let wf = execution::compile(&approved, &Limits::default()).unwrap();
        let id = plan_run_id("s", &approved.fingerprint());
        runs.create_as(&id, &wf, "s-run", json!({})).await.unwrap();
        match gate(&plans, &runs, "s").await.unwrap() {
            Gate::Launched(p) => {
                assert_eq!(p.run.id, id);
                assert!(p.is_live());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            live_run(&plans, &runs, "s").await.unwrap().unwrap().run.id,
            id
        );
        runs.control(&id, &crate::Control::Cancel).await.unwrap();
        assert!(live_run(&plans, &runs, "s").await.unwrap().is_none());
        assert!(matches!(
            gate(&plans, &runs, "s").await.unwrap(),
            Gate::Launched(_)
        ));
    }
}
