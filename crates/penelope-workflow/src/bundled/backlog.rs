//! `backlog` et `tache` (#338) : dérouler une liste de tâches, dans l'ordre, une tâche
//! après l'autre, chacune de la spécification au statut du tracker.
//!
//! Le tracker est un paramètre (ClickUp, Redmine, issues GitHub ou fichier) : la première
//! étape lit la liste par les outils du tracker (MCP) ou dans le fichier, sans code propre
//! à aucun d'eux, et la rend en JSON ; l'étape `foreach` la fige puis appelle `tache` pour
//! chaque élément.

use super::step;
use crate::model::*;
use serde_json::json;

fn param(id: &str, label: &str, required: bool, default: Option<&str>) -> Parameter {
    Parameter {
        id: id.into(),
        label: label.into(),
        kind: "string".into(),
        required,
        default: default.map(|d| json!(d)),
        ..Default::default()
    }
}

/// `backlog` : lit la liste, la fige, la déroule avec `tache`, point d'arrêt à chaque fin
/// d'épique.
pub fn backlog() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "backlog".into(),
            name: "Dérouler un backlog".into(),
            description: "Lit une liste de tâches (ClickUp, Redmine, GitHub, fichier) et la \
                          déroule dans l'ordre avec `tache`, point d'arrêt à chaque fin \
                          d'épique."
                .into(),
            parameters: vec![
                param("tracker", "Tracker", false, Some("fichier")),
                param("source", "Liste, requête ou fichier", true, None),
                param("filtre", "Filtre", false, Some("tâches à faire")),
                param("depot", "Dépôt", false, Some(".")),
            ],
            ..Default::default()
        },
        entry_step: "lister".into(),
        settings: Settings {
            max_iterations: 10,
            budget: Budget {
                max_usd: 200.0,
                max_tokens: 0,
                max_wall_ms: 0,
                max_cached_tokens: 0,
            },
            workspace: "persistent:backlog".into(),
            // Lancé le matin pour la semaine : il ne s'arrête qu'aux fins d'épique, sur
            // un élément en échec ou faute de crédits, jamais sur une approbation (#346).
            autonomous: true,
            ..Settings::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                prompt: "Lis la liste de tâches à dérouler dans {{tracker}} : {{source}} \
                         ({{filtre}}). Avec `fichier`, c'est un fichier du dépôt {{depot}} ; \
                         sinon, passe par les outils du tracker (`tool_search`). Ne modifie \
                         rien. Garde l'ordre de travail (épiques puis stories, par priorité). \
                         Rends la liste par `return_value(result=\"success\", content=[{\"id\": \
                         …, \"title\": …, \"url\": …, \"epic\": …}, …])`, puis `step_done()`."
                    .into(),
                context: "fresh".into(),
                max_calls: Some(40),
                transitions: vec![Transition::always("derouler")],
                ..step("lister", "agent", Phase::Plan)
            },
            Step {
                items: json!({"step": "lister", "path": "content"}),
                workflow_id: "tache".into(),
                params: json!({"tracker": "{{tracker}}", "source": "{{source}}",
                               "depot": "{{depot}}"}),
                on_error: "stop".into(),
                pause_after: json!({"changes": "epic"}),
                item_noun: "tâche".into(),
                item_label: "{{item.title}}".into(),
                transitions: vec![
                    Transition::on_result(DONE, "success"),
                    Transition::on_result(DONE, "partial"),
                    Transition {
                        goto: BLOCKED.into(),
                        condition: json!({"type":"always"}),
                        tag: "une tâche en échec arrête la liste".into(),
                    },
                ],
                ..step("derouler", "foreach", Phase::Build)
            },
        ],
    }
}

const ITEM: &str = "Tâche {{item_index}}/{{item_total}} : {{item.title}} ({{item.url}}, \
                    identifiant {{item.id}}, tracker {{tracker}}). Dépôt : {{depot}}.";

/// `tache` : une tâche du backlog, de la spécification au statut du tracker.
pub fn task() -> Workflow {
    let agent = |id: &str, phase: Phase, prompt: String, next: Vec<Transition>| Step {
        prompt,
        transitions: next,
        ..step(id, "agent", phase)
    };
    let passed_or =
        |ok: &str, ko: &str| vec![Transition::on_result(ok, "passed"), Transition::always(ko)];
    Workflow {
        metadata: Metadata {
            id: "tache".into(),
            name: "Tâche du backlog".into(),
            description: "Spécification, tests, développement, tests locaux, commit et push, \
                          vérification sur ce qui tourne (pipeline, URLs, /version), revue \
                          adverse, statut du tracker."
                .into(),
            parameters: vec![
                Parameter {
                    id: "item".into(),
                    label: "Tâche".into(),
                    kind: "object".into(),
                    required: true,
                    ..Default::default()
                },
                param("item_index", "Rang", false, Some("1")),
                param("item_total", "Total", false, Some("1")),
                param("tracker", "Tracker", false, Some("fichier")),
                param("source", "Liste, requête ou fichier", false, Some("")),
                param("depot", "Dépôt", false, Some(".")),
            ],
            ..Default::default()
        },
        entry_step: "spec".into(),
        settings: Settings {
            max_iterations: 30,
            budget: Budget {
                max_usd: 10.0,
                max_tokens: 4_000_000,
                max_wall_ms: 0,
                max_cached_tokens: 0,
            },
            ..Settings::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![
            agent(
                "spec",
                Phase::Plan,
                format!(
                    "{ITEM}\nLis la tâche dans son tracker et le code concerné. Écris une \
                     spécification courte : comportement attendu, critères d'acceptation \
                     vérifiables, fichiers touchés. Chaque critère dit comment le prouver avec \
                     les outils de la machine (test, commande, pipeline suivi par `glab` ou \
                     `gh`, URL interrogée par `curl`) : l'étape `verifier` le prouvera après le \
                     push. Pose les critères : `session_metadata` \
                     op=set key=criteria entry=[{{\"id\": \"…\", \"text\": \"…\", \"status\": \
                     \"pending\"}}, …]. Puis `step_done()`."
                ),
                vec![Transition::always("tests")],
            ),
            agent(
                "tests",
                Phase::Build,
                "Écris d'abord les tests qui traduisent les critères, sans toucher au code \
                 produit, et lance-les : ils doivent échouer pour la bonne raison. Puis \
                 `step_done()`."
                    .into(),
                vec![Transition::always("dev")],
            ),
            Step {
                max_calls: Some(120),
                max_turns: Some(3),
                ..agent(
                    "dev",
                    Phase::Build,
                    "Implémente jusqu'à ce que les tests passent. Coche chaque critère rempli : \
                     `session_metadata` op=update key=criteria entry={\"id\": \"<id>\", \
                     \"status\": \"completed\"}. {{pendingCount}} critère(s) restant(s) :\n\
                     {{criteriaList}}\nCe que la dernière vérification a refusé, s'il y en a : \
                     {{reason}}. Puis `step_done()`."
                        .into(),
                    vec![Transition::always("tests-locaux")],
                )
            },
            // Démarrer l'application avant de pousser (#348) : le 12/10, le test
            // d'acceptation de la story 1.1 ne lisait que le texte des fichiers compose, et
            // la revue a trouvé l'isolation des hôtes cassée.
            Step {
                max_calls: Some(120),
                ..agent(
                    "tests-locaux",
                    Phase::Verification,
                    "Lance dans {{depot}} ce que la CI lancera : format, lint, tests complets. \
                     Puis fais tourner ce que tu as changé, en local, avant tout push. Si le \
                     changement touche l'exécution ou le déploiement (Dockerfile, `docker \
                     compose`, CI, routage, configuration, migrations), démarre la pile \
                     (`docker compose -f <fichier> up -d --build`) et attends qu'elle réponde. \
                     Interroge chaque service (`curl -fsS`, avec `-H 'Host: …'` pour un routage \
                     par hôte) : `/version`, ce que chaque hôte doit servir et ce qu'il doit \
                     refuser. Arrête-la ensuite (`docker compose down`, sans `-v`). Si c'est du \
                     code applicatif, lance l'application et exerce le parcours touché (requête, \
                     page, commande). Tout vert : `return_value(result=\"passed\", content=<les \
                     commandes et ce qu'elles ont rendu>)`. Sinon `return_value(result=\
                     \"failed\", content=<l'échec, la commande et sa sortie utile>)`. Puis \
                     `step_done()`."
                        .into(),
                    passed_or("commit", "dev"),
                )
            },
            Step {
                context: "fresh".into(),
                ..agent(
                    "revue",
                    Phase::Verification,
                    format!(
                        "{ITEM}\nTu fais la revue adverse de ce changement : tu ne l'as pas \
                         écrit et tu cherches ce qui casse. Lis le diff (`git diff` depuis la \
                         branche de départ), les tests ajoutés et les critères. Bugs, \
                         régressions, failles, cas limites non testés : rends \
                         `return_value(result=\"failed\", content=<les corrections, chacune \
                         avec fichier et raison>)`. Rien de bloquant : \
                         `return_value(result=\"passed\")`. La vérification a déjà prouvé les \
                         critères sur ce qui tourne : {{{{steps.verifier.content}}}}. Puis \
                         `step_done()`."
                    ),
                    passed_or("statut", "dev"),
                )
            },
            agent(
                "commit",
                Phase::Deploy,
                "Commite le changement (message : ce qui change et pourquoi, avec \
                 l'identifiant {{item.id}}) et pousse-le dans {{depot}} sur la branche que le \
                 flux du projet prévoit pour ce travail (README, CLAUDE.md, CI : la branche de \
                 développement, ou celle de l'environnement que la tâche déploie), sans \
                 `--force`. Rends `return_value(result=\"success\", content=<SHA court et \
                 branche>)`, puis `step_done()`."
                    .into(),
                vec![Transition::always("verifier")],
            ),
            // La preuve sur ce qui tourne vient après le push, avant la revue (#348) : le
            // 12/10, la revue exigeait des déploiements que le run ne poussait qu'après elle,
            // et il a conclu, à tort, qu'il ne pouvait rien vérifier.
            Step {
                max_calls: Some(120),
                ..agent(
                    "verifier",
                    Phase::Verification,
                    "Le changement est poussé : {{steps.commit.content}}. Vérifie chaque \
                     critère d'acceptation sur ce qui tourne vraiment, avec les outils de la \
                     machine. Suis le pipeline de ce commit jusqu'à sa fin (`glab ci status`, \
                     `glab ci view`, `glab ci trace` sur GitLab ; `gh run watch` sur GitHub) et \
                     lis ses journaux s'il échoue. Interroge ce qui est déployé : `curl -fsS \
                     <url>/version` doit rendre le SHA poussé, chaque hôte doit répondre ce \
                     qu'il doit et refuser le reste. Compare la configuration (`docker compose \
                     config`). Chaque critère prouvé : `session_metadata` op=update key=criteria \
                     entry={\"id\": \"<id>\", \"status\": \"passed\"}, avec la commande et \
                     sa sortie utile. Ne déclare jamais un critère invérifiable sans l'avoir \
                     essayé. Ce qui manque à un environnement de développement ou de recette \
                     (`dev`, `qa`, `staging`) est à toi de le créer : une variable CI absente, \
                     tu la poses (`glab variable set <CLÉ> --scope <env> --masked`, la valeur \
                     générée sur la machine, `openssl rand -hex 32` pour un secret, lue sur \
                     l'entrée standard, jamais affichée, écrite ni rendue), puis tu relances le \
                     pipeline et tu reprends la vérification. Tout est prouvé : \
                     `return_value(result=\"passed\", content=<les preuves, une par critère, \
                     et les variables créées, sans leur valeur>)`. Un critère échoue : \
                     `return_value(result=\"failed\", content=<critère, commande, sortie, \
                     correction attendue>)`. Seul ce qui touche la production, un compte ou un \
                     service externe que tu ne peux pas créer, ou un droit refusé, revient au \
                     propriétaire : `return_value(result=\"blocked\", content=<ce qui manque, \
                     exactement, et où le poser>)`. Puis `step_done()`."
                        .into(),
                    vec![
                        Transition::on_result("revue", "passed"),
                        Transition::on_result(BLOCKED, "blocked"),
                        Transition::always("dev"),
                    ],
                )
            },
            agent(
                "statut",
                Phase::Done,
                "Passe la tâche {{item.id}} à l'état « fait » dans {{tracker}}, avec un \
                 commentaire court : ce qui a été fait et le commit {{steps.commit.content}}. \
                 Avec `fichier`, coche sa case dans {{source}}. Puis `step_done()`."
                    .into(),
                vec![Transition::always(DONE)],
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_of(w: &Workflow, id: &str) -> Vec<(String, String)> {
        w.steps
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("étape {id}"))
            .transitions
            .iter()
            .map(|t| {
                let on = t
                    .condition
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("*")
                    .to_string();
                (on, t.goto.clone())
            })
            .collect()
    }

    /// #348 : le push précède la preuve, la preuve précède la revue ; un échec renvoie au
    /// développement, un accès que seul le propriétaire peut donner arrête la liste.
    #[test]
    fn a_task_is_pushed_then_proved_then_reviewed() {
        let w = task();
        let pair = |on: &str, to: &str| (on.to_string(), to.to_string());
        assert_eq!(
            next_of(&w, "tests-locaux"),
            [pair("passed", "commit"), pair("*", "dev")]
        );
        assert_eq!(next_of(&w, "commit"), [pair("*", "verifier")]);
        assert_eq!(
            next_of(&w, "verifier"),
            [
                pair("passed", "revue"),
                pair("blocked", BLOCKED),
                pair("*", "dev")
            ]
        );
        assert_eq!(
            next_of(&w, "revue"),
            [pair("passed", "statut"), pair("*", "dev")]
        );
        let verify = &w.steps.iter().find(|s| s.id == "verifier").unwrap().prompt;
        for tool in [
            "glab ci",
            "gh run watch",
            "curl -fsS",
            "/version",
            "docker compose",
        ] {
            assert!(verify.contains(tool), "{tool}");
        }
    }
}
