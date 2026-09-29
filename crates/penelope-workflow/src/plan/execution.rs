//! Exécution d'un plan approuvé (T3 de #185, issue #191).
//!
//! Le plan approuvé devient un workflow du moteur de runs : chaque pas du plan est une
//! étape `agent` à contexte neuf (`context: fresh`), avec le modèle que sa phase appelle ;
//! la fin d'une phase de spécification, de tests ou de code pose une carte d'OK ; une
//! revue ou une vérification qui échoue renvoie au code, au plus [`Limits::max_reworks`]
//! fois. Tout le chemin est déplié à la compilation : l'étape courante du run dit à elle
//! seule où il en est (numéro de reprise, « laisse filer » ou non), et une reprise après
//! redémarrage n'a rien d'autre à relire que la ligne du run.
//!
//! ```text
//!  e1-spec ── ok-e1 ──Continuer──► e2-tests ── ok-e2 ─► e3-code ── ok-e3 ─► e4-revue ── passed ─► $done
//!               │                                                            │
//!               └─Laisse filer──► e2-tests-libre ─► e3-code-libre ─► …       └─ failed ─► e3-code-r1 ─► …
//! ```
//!
//! « Laisse filer » saute les cartes d'OK ordinaires qui suivent, jamais un point dur : le
//! gate « vas-y » est avant le run, l'arrêt sur limite de reprises est un `$blocked`, et
//! la livraison (#192) n'a pas de variante libre, pas plus que le gate de production
//! (#193).
//!
//! Un plan qui écrit du code et finit par un juge est livré : le `passed` de ce dernier
//! juge mène à la PR dev, à la CI et à l'E2E ([`crate::delivery`]) au lieu de `$done`.

use super::{Phase, PlanDraft, PlanError, PlanStep};
use crate::delivery;
use crate::model::{
    BLOCKED, Budget, Concurrency, DONE, Metadata, Phase as RunPhase, Settings, Step, Transition,
    Workflow,
};
use serde_json::json;
use std::collections::{BTreeSet, VecDeque};

/// Choix d'une carte d'OK entre deux phases.
pub const CONTINUE: &str = "Continuer";
pub const LET_RUN: &str = "Laisse filer";
pub const STOP: &str = "Arrêter";

/// Résultats qu'une étape de jugement rend par `return_value`.
pub const PASSED: &str = "passed";
pub const FAILED: &str = "failed";

/// Au plus deux retours en arrière par run : au-delà, la revue arrête le run et le
/// propriétaire décide (une boucle sans condition d'arrêt brûle du budget sans converger).
pub const MAX_REWORKS: u32 = 2;

/// Un plan de deux pas au plus, sans revue ni vérification, reste à un seul agent : un
/// système multi-agents ne vaut pas son coût pour une petite demande.
const SINGLE_AGENT_STEPS: usize = 2;

/// Le plan est-il livré en dev (#192) ? Il écrit du code et son dernier pas est un juge :
/// c'est le `passed` de ce juge qui dit que le travail satisfait les tests et la revue.
/// Un plan sans juge n'a personne pour le dire, et n'ouvre donc aucune PR.
pub fn delivers(steps: &[PlanStep]) -> bool {
    steps.iter().any(|s| s.phase == Phase::Implementation)
        && steps.last().is_some_and(|s| s.phase.is_judge())
}

/// Comment le plan est exécuté.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Un seul agent à contexte neuf réalise tout le plan.
    Single,
    /// Un agent à contexte neuf par pas, cartes d'OK et boucle de revue.
    Phased,
}

impl Mode {
    pub fn of(steps: &[PlanStep]) -> Mode {
        if steps.len() <= SINGLE_AGENT_STEPS && !steps.iter().any(|s| s.phase.is_judge()) {
            Mode::Single
        } else {
            Mode::Phased
        }
    }
}

impl Phase {
    fn slug(self) -> &'static str {
        match self {
            Phase::Specification => "spec",
            Phase::Tests => "tests",
            Phase::Implementation => "code",
            Phase::Review => "revue",
            Phase::Verification => "verif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Specification => "Spécification",
            Phase::Tests => "Tests",
            Phase::Implementation => "Code",
            Phase::Review => "Revue",
            Phase::Verification => "Vérification",
        }
    }

    /// Le modèle que l'orchestrateur donne à la phase, alias ou rôle (résolu au lancement
    /// sur la configuration) : raisonner pour spécifier et juger, le rôle `code` pour les
    /// tests et le code, le modèle principal pour vérifier.
    pub fn model_alias(self) -> &'static str {
        match self {
            Phase::Specification | Phase::Review => "reasoning",
            Phase::Tests | Phase::Implementation => "code",
            Phase::Verification => "main",
        }
    }

    /// Une phase qui juge le travail des autres : son échec renvoie au code.
    pub fn is_judge(self) -> bool {
        matches!(self, Phase::Review | Phase::Verification)
    }

    /// Une phase dont la fin attend l'OK du propriétaire (checkpoint ordinaire).
    pub fn has_checkpoint(self) -> bool {
        matches!(
            self,
            Phase::Specification | Phase::Tests | Phase::Implementation
        )
    }

    fn run_phase(self) -> RunPhase {
        match self {
            Phase::Specification => RunPhase::Plan,
            Phase::Tests | Phase::Implementation => RunPhase::Build,
            Phase::Review | Phase::Verification => RunPhase::Verification,
        }
    }

    fn instruction(self) -> &'static str {
        match self {
            Phase::Specification => {
                "Écris la spécification : comportement attendu, critères d'acceptation \
                 vérifiables, cas limites. Rends-la dans `content` de `return_value`."
            }
            Phase::Tests => {
                "Écris les tests qui prouvent les critères, avant le code : ils échouent tant \
                 que le code manque. Résume ce que tu as écrit dans `content` de `return_value`."
            }
            Phase::Implementation => {
                "Écris le code qui fait passer les tests, sans élargir le périmètre. Résume les \
                 changements dans `content` de `return_value`."
            }
            Phase::Review => {
                "Revue contradictoire : tu n'es pas l'auteur de ce travail et tu ne le défends \
                 pas. Cherche ce qui manque ou casse : critère non couvert, test absent, \
                 régression, sécurité. Appelle `return_value` avec `result` = `passed` si le \
                 travail est satisfaisant, sinon `failed` et, dans `content`, ce qu'il faut \
                 reprendre."
            }
            Phase::Verification => {
                "Vérifie par des preuves (commandes lancées, sorties lues) que les critères \
                 sont remplis ; ce qui n'est pas prouvé échoue. Appelle `return_value` avec \
                 `result` = `passed` ou `failed`, et la raison dans `content`."
            }
        }
    }
}

/// Bornes d'un run de plan, reprises du workflow pour lequel le plan a été préparé.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub budget: Budget,
    pub max_iterations: u32,
    pub workspace: String,
    pub max_reworks: u32,
}

impl Limits {
    pub fn of(settings: &Settings) -> Limits {
        Limits {
            budget: settings.budget,
            max_iterations: settings.max_iterations,
            workspace: settings.workspace.clone(),
            max_reworks: MAX_REWORKS,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits::of(&Settings::default())
    }
}

/// Une position du chemin déplié : le pas du plan, le nombre de reprises déjà faites, et
/// si le propriétaire a dit « laisse filer ».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Node {
    index: usize,
    reworks: u32,
    free: bool,
}

fn suffix(reworks: u32, free: bool) -> String {
    let mut s = String::new();
    if reworks > 0 {
        s.push_str(&format!("-r{reworks}"));
    }
    if free {
        s.push_str("-libre");
    }
    s
}

/// Identifiant du workflow compilé : le plan exact, par son empreinte.
pub fn workflow_id(fingerprint: &str) -> String {
    format!("plan-{}", &fingerprint[..fingerprint.len().min(12)])
}

/// Compile un plan approuvé en workflow. Un plan encore en revue est refusé : aucun pas ne
/// démarre avant « vas-y ».
pub fn compile(draft: &PlanDraft, limits: &Limits) -> Result<Workflow, PlanError> {
    if !draft.plan.can_execute() {
        return Err(PlanError::NotApproved);
    }
    let plan = &draft.plan;
    let steps = plan.steps();
    let header = header(draft);
    let mut out: Vec<Step> = Vec::new();
    match Mode::of(steps) {
        Mode::Single => out.push(single_step(&header, steps)),
        Mode::Phased => {
            let delivered = delivers(steps);
            let compiler = Compiler {
                steps,
                header: &header,
                max_reworks: limits.max_reworks,
                end: if delivered { delivery::ENTRY } else { DONE },
            };
            out = compiler.unfold();
            if delivered {
                out.extend(delivery::tail(DONE));
            }
        }
    }
    let goal: String = plan.goal().chars().take(60).collect();
    let fingerprint = draft.fingerprint();
    // Chaque visite d'étape compte une itération : le chemin déplié tient dans la borne.
    let max_iterations = limits.max_iterations.max(out.len() as u32 + 1);
    Ok(Workflow {
        metadata: Metadata {
            id: workflow_id(&fingerprint),
            name: format!("Plan v{} · {goal}", plan.version()),
            description: format!(
                "Plan approuvé v{} de `{}` (empreinte {})",
                plan.version(),
                draft.workflow_id,
                &fingerprint[..12]
            ),
            ..Default::default()
        },
        entry_step: out[0].id.clone(),
        settings: Settings {
            max_iterations,
            budget: limits.budget,
            concurrency: Concurrency {
                max_concurrent: 1,
                admission: "parallel".into(),
            },
            workspace: limits.workspace.clone(),
            ..Default::default()
        },
        start_condition: json!({"type": "always"}),
        steps: out,
    })
}

fn header(draft: &PlanDraft) -> String {
    let plan = &draft.plan;
    let mut lines = vec![
        format!("Plan approuvé v{} : {}", plan.version(), plan.goal()),
        "Étapes du plan :".to_string(),
    ];
    for (i, s) in plan.steps().iter().enumerate() {
        lines.push(format!("{}. [{}] {}", i + 1, s.phase.label(), s.title));
    }
    lines.push(String::new());
    lines.push("Brief de la conversation : {{brief}}".into());
    lines.join("\n")
}

fn single_step(header: &str, steps: &[PlanStep]) -> Step {
    Step {
        id: "e1-tout".into(),
        name: "Plan entier (un seul agent)".into(),
        kind: "agent".into(),
        phase: RunPhase::Build,
        context: "fresh".into(),
        model: "code".into(),
        prompt: format!(
            "{header}\n\nTa tâche : réalise tout le plan toi-même, dans l'ordre ({} étapes). \
             Résume ce que tu as fait dans `content` de `return_value`.",
            steps.len()
        ),
        transitions: advance_unless_error(DONE),
        ..Default::default()
    }
}

/// Toute issue sauf `error` fait avancer ; une erreur bloque le run, qui attend le
/// propriétaire.
fn advance_unless_error(goto: &str) -> Vec<Transition> {
    vec![
        on_result(BLOCKED, "error", "erreur de l'étape"),
        Transition::always(goto),
    ]
}

fn on_result(goto: &str, result: &str, tag: &str) -> Transition {
    Transition {
        tag: tag.into(),
        ..Transition::on_result(goto, result)
    }
}

struct Compiler<'a> {
    steps: &'a [PlanStep],
    header: &'a str,
    max_reworks: u32,
    /// Où va le run quand le dernier pas est franchi : `$done`, ou la livraison.
    end: &'a str,
}

impl Compiler<'_> {
    fn id(&self, n: Node) -> String {
        match self.steps.get(n.index) {
            Some(s) => format!(
                "e{}-{}{}",
                n.index + 1,
                s.phase.slug(),
                suffix(n.reworks, n.free)
            ),
            None => self.end.into(),
        }
    }

    fn checkpoint_id(&self, n: Node) -> String {
        format!("ok-e{}{}", n.index + 1, suffix(n.reworks, false))
    }

    /// La fin d'une phase à checkpoint : le pas suivant change de phase, ou il n'y en a plus.
    fn ends_checkpointed_phase(&self, index: usize) -> bool {
        let phase = self.steps[index].phase;
        phase.has_checkpoint()
            && self
                .steps
                .get(index + 1)
                .is_none_or(|next| next.phase != phase)
    }

    /// Où repart le travail quand un juge le refuse : le début du dernier bloc de code
    /// avant lui, sinon le dernier pas qui ne juge pas.
    fn rework_target(&self, judge: usize) -> Option<usize> {
        let before = &self.steps[..judge];
        match before
            .iter()
            .rposition(|s| s.phase == Phase::Implementation)
        {
            Some(last) => {
                let mut first = last;
                while first > 0 && before[first - 1].phase == Phase::Implementation {
                    first -= 1;
                }
                Some(first)
            }
            None => before.iter().rposition(|s| !s.phase.is_judge()),
        }
    }

    /// Déplie le chemin depuis le premier pas, en largeur ; seules les positions
    /// atteignables deviennent des étapes.
    fn unfold(&self) -> Vec<Step> {
        let start = Node {
            index: 0,
            reworks: 0,
            free: false,
        };
        let mut seen: BTreeSet<Node> = BTreeSet::new();
        let mut queue: VecDeque<Node> = VecDeque::from([start]);
        let mut out = Vec::new();
        while let Some(n) = queue.pop_front() {
            if n.index >= self.steps.len() || !seen.insert(n) {
                continue;
            }
            let (steps, next) = self.expand(n);
            out.extend(steps);
            queue.extend(next);
        }
        out
    }

    /// Les étapes d'une position (le pas, et sa carte d'OK s'il en porte une) et les
    /// positions qu'elles atteignent.
    fn expand(&self, n: Node) -> (Vec<Step>, Vec<Node>) {
        let step = &self.steps[n.index];
        let following = Node {
            index: n.index + 1,
            ..n
        };
        let mut work = Step {
            id: self.id(n),
            name: format!("{} · {}", step.phase.label(), step.title),
            kind: "agent".into(),
            phase: step.phase.run_phase(),
            context: "fresh".into(),
            model: step.phase.model_alias().into(),
            prompt: format!(
                "{}\n\nTa tâche : étape {} ({}) — {}.\n{}",
                self.header,
                n.index + 1,
                step.phase.label(),
                step.title,
                step.phase.instruction()
            ),
            ..Default::default()
        };
        if step.phase.is_judge() {
            let mut reached = vec![following];
            let failed = match self.rework_target(n.index) {
                Some(target) if n.reworks < self.max_reworks => {
                    let back = Node {
                        index: target,
                        reworks: n.reworks + 1,
                        free: n.free,
                    };
                    reached.push(back);
                    Transition::on_result(&self.id(back), FAILED)
                }
                Some(_) => on_result(
                    BLOCKED,
                    FAILED,
                    &format!("limite de {} reprises atteinte", self.max_reworks),
                ),
                None => on_result(BLOCKED, FAILED, "aucun travail à reprendre avant ce juge"),
            };
            work.transitions = vec![
                Transition::on_result(&self.id(following), PASSED),
                failed,
                Transition {
                    tag: "verdict absent : `passed` ou `failed` attendu".into(),
                    ..Transition::always(BLOCKED)
                },
            ];
            return (vec![work], reached);
        }
        if n.free || !self.ends_checkpointed_phase(n.index) {
            work.transitions = advance_unless_error(&self.id(following));
            return (vec![work], vec![following]);
        }
        let free = Node {
            free: true,
            ..following
        };
        let checkpoint = Step {
            id: self.checkpoint_id(n),
            name: format!("OK après {} (étape {})", step.phase.label(), n.index + 1),
            kind: "user".into(),
            phase: RunPhase::Waiting,
            template: "question".into(),
            choices: vec![CONTINUE.into(), LET_RUN.into(), STOP.into()],
            transitions: vec![
                Transition::on_result(&self.id(following), CONTINUE),
                Transition::on_result(&self.id(free), LET_RUN),
                on_result(BLOCKED, STOP, "arrêt demandé par le propriétaire"),
            ],
            ..Default::default()
        };
        work.transitions = advance_unless_error(&checkpoint.id);
        (vec![work, checkpoint], vec![following, free])
    }
}

#[cfg(test)]
mod tests;
