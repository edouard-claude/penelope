//! Workflows livrés (§12.10) : `build-verify`, `review`, `ticket-to-deploy`,
//! `deploy-generic`.

use crate::model::*;
use serde_json::json;

fn step(id: &str, kind: &str, phase: Phase) -> Step {
    Step {
        id: id.into(),
        name: id.into(),
        kind: kind.into(),
        phase,
        ..Default::default()
    }
}

/// `build-verify` : équivalent OpenFox (build → verify → boucle).
pub fn build_verify() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "build-verify".into(),
            name: "Construire puis vérifier".into(),
            description: "Implémente, vérifie, et boucle tant que les critères ne sont pas \
                          remplis."
                .into(),
            parameters: vec![Parameter {
                id: "objectif".into(),
                label: "Objectif".into(),
                kind: "string".into(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        entry_step: "plan".into(),
        settings: Settings::default(),
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                prompt: "Objectif : {{objectif}}.\nAnalyse le code, écris les critères de \
                         succès dans `session_metadata.criteria`, puis appelle `step_done()`."
                    .into(),
                model: "reasoning".into(),
                transitions: vec![Transition::always("build")],
                ..step("plan", "agent", Phase::Plan)
            },
            Step {
                prompt: "Implémente. Coche chaque critère au fur et à mesure.\n\
                         {{pendingCount}} critère(s) restant(s) :\n{{criteriaList}}"
                    .into(),
                nudge_prompt: "Continue : {{pendingCount}} critère(s) restant(s).".into(),
                model: "reasoning".into(),
                transitions: vec![
                    Transition {
                        goto: "verify".into(),
                        condition: json!({
                            "type":"metadata_all_in","key":"criteria","field":"status",
                            "values":["completed","passed"]
                        }),
                        tag: String::new(),
                    },
                    Transition::always("build"),
                ],
                ..step("build", "agent", Phase::Build)
            },
            Step {
                verifier: "verifier".into(),
                criteria_key: "criteria".into(),
                checks: vec![json!({"type":"shell","command":"cargo test"})],
                transitions: vec![
                    Transition::on_result(DONE, "passed"),
                    Transition::always("build"),
                ],
                ..step("verify", "verify", Phase::Verification)
            },
        ],
    }
}

/// `review` : revue de PR/MR, lint et tests en parallèle.
pub fn review() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "review".into(),
            name: "Revue de PR".into(),
            description: "Lint, tests et relecture en parallèle ; findings dans \
                          `review_findings`."
                .into(),
            parameters: vec![Parameter {
                id: "pr_url".into(),
                label: "PR/MR".into(),
                kind: "string".into(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        entry_step: "analyse".into(),
        settings: Settings::default(),
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                children: vec![
                    Step {
                        command: json!({"unix":"cargo clippy -- -D warnings","windows":"cargo clippy -- -D warnings"}),
                        ..step("lint", "shell", Phase::Verification)
                    },
                    Step {
                        command: json!({"unix":"cargo test","windows":"cargo test"}),
                        ..step("tests", "shell", Phase::Verification)
                    },
                    Step {
                        sub_agent_type: "code_reviewer".into(),
                        prompt: "Relis la PR {{pr_url}}. Renvoie les problèmes bloquants, \
                                 triés par gravité."
                            .into(),
                        model: "reasoning".into(),
                        output_schema: Some(json!({
                            "type":"object",
                            "properties":{
                                "findings":{"type":"array","items":{"type":"object"}}
                            },
                            "required":["findings"]
                        })),
                        ..step("reviewer", "sub_agent", Phase::Verification)
                    },
                ],
                transitions: vec![Transition::always("rapport")],
                ..step("analyse", "parallel", Phase::Verification)
            },
            Step {
                prompt: "Rédige le compte rendu de revue à partir de {{stepOutput.reviewer}}, \
                         du lint et des tests. Écris les findings dans \
                         `session_metadata.review_findings`, puis `step_done()`."
                    .into(),
                transitions: vec![Transition::always(DONE)],
                ..step("rapport", "agent", Phase::Done)
            },
        ],
    }
}

/// `deploy-generic` : déploiement piloté par `.penelope/deploy.toml`.
pub fn deploy_generic() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "deploy-generic".into(),
            name: "Déploiement générique".into(),
            description: "Lit `.penelope/deploy.toml` du dépôt : commandes, environnements, \
                          vérifications post-déploiement, rollback."
                .into(),
            parameters: vec![
                Parameter {
                    id: "environnement".into(),
                    label: "Environnement".into(),
                    kind: "string".into(),
                    required: true,
                    ..Default::default()
                },
                Parameter {
                    id: "repo".into(),
                    label: "Répertoire du dépôt".into(),
                    kind: "string".into(),
                    required: false,
                    default: Some(json!(".")),
                    description: "Chemin du dépôt cloné ; relatif à l'espace de travail du \
                                  run, qu'un sous-workflow partage avec son parent."
                        .into(),
                },
            ],
            ..Default::default()
        },
        entry_step: "lire_config".into(),
        settings: Settings::default(),
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                tool: "fs_read".into(),
                args: json!({"path":"{{repo}}/.penelope/deploy.toml"}),
                transitions: vec![
                    Transition::on_result("deployer", "success"),
                    Transition::always(BLOCKED),
                ],
                ..step("lire_config", "tool", Phase::Plan)
            },
            Step {
                command: json!({
                    "unix":"make deploy ENV={{environnement}}",
                    "windows":"make deploy ENV={{environnement}}"
                }),
                cwd: "{{repo}}".into(),
                transitions: vec![
                    Transition::on_result("verifier", "success"),
                    Transition::always("rollback"),
                ],
                network: true,
                ..step("deployer", "shell", Phase::Deploy)
            },
            Step {
                command: json!({
                    "unix":"make smoke ENV={{environnement}}",
                    "windows":"make smoke ENV={{environnement}}"
                }),
                cwd: "{{repo}}".into(),
                transitions: vec![
                    Transition::on_result(DONE, "success"),
                    Transition::always("rollback"),
                ],
                network: true,
                ..step("verifier", "shell", Phase::Verification)
            },
            Step {
                command: json!({
                    "unix":"make rollback ENV={{environnement}}",
                    "windows":"make rollback ENV={{environnement}}"
                }),
                cwd: "{{repo}}".into(),
                transitions: vec![Transition::always(BLOCKED)],
                network: true,
                ..step("rollback", "shell", Phase::Deploy)
            },
        ],
    }
}

/// Tests du dépôt : cible `test` du Makefile, sinon l'outil de l'écosystème détecté.
pub const TEST_COMMAND: &str = "if [ -f Makefile ] && grep -q '^test:' Makefile; then make test; \
elif [ -f Cargo.toml ]; then cargo test; \
elif [ -f package.json ]; then npm test; \
elif [ -f go.mod ]; then go test ./...; \
else echo 'aucune commande de test reconnue (Makefile, Cargo.toml, package.json, go.mod)' >&2; exit 1; fi";

/// Lint du dépôt : cible `lint` du Makefile, sinon l'outil de l'écosystème ; rien à lancer
/// n'est pas une erreur.
pub const LINT_COMMAND: &str = "if [ -f Makefile ] && grep -q '^lint:' Makefile; then make lint; \
elif [ -f Cargo.toml ]; then cargo clippy -- -D warnings; \
elif [ -f package.json ]; then npm run lint --if-present; \
elif [ -f go.mod ]; then go vet ./...; \
else echo 'aucun lint reconnu'; fi";

/// Outils d'une étape qui parle au tracker ou à la forge par leurs serveurs MCP.
const MCP_TOOLS: [&str; 3] = ["tool_search", "tool_describe", "tool_call"];

/// Lecture du ticket, quel que soit le tracker (issue #35).
const TRACKER_READ: &str = "Lis le ticket {{ticket_id}} ({{ticket_url}}) dans son tracker. \
Tracker indiqué : « {{tracker}} » ; sinon déduis-le de l'adresse (Redmine, ClickUp ou autre). \
Trouve l'outil de lecture avec `tool_search`, appelle-le avec `tool_call`, puis rends le nom \
du serveur MCP du tracker (`tracker`), le titre et la description du ticket.";

/// `ticket-to-deploy` : scénario de référence du §12.10. Tracker et forge sont ceux du
/// ticket et du dépôt : leurs outils MCP sont trouvés à l'exécution (issue #35).
pub fn ticket_to_deploy() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "ticket-to-deploy".into(),
            name: "Ticket → correctif → déploiement".into(),
            description: "Du ticket au déploiement, avec approbations aux points sensibles.".into(),
            parameters: vec![
                Parameter {
                    id: "ticket_url".into(),
                    label: "Ticket".into(),
                    kind: "string".into(),
                    required: true,
                    ..Default::default()
                },
                Parameter {
                    id: "ticket_id".into(),
                    label: "Identifiant du ticket".into(),
                    kind: "string".into(),
                    required: true,
                    ..Default::default()
                },
                Parameter {
                    id: "repo".into(),
                    label: "Dépôt".into(),
                    kind: "string".into(),
                    required: false,
                    default: Some(json!("")),
                    description: "Adresse du dépôt, si elle est connue.".into(),
                },
                Parameter {
                    id: "tracker".into(),
                    label: "Tracker".into(),
                    kind: "string".into(),
                    required: false,
                    default: Some(json!("")),
                    description: "Serveur MCP du tracker (redmine, clickup…) ; déduit de \
                                  l'adresse du ticket sinon."
                        .into(),
                },
            ],
            ..Default::default()
        },
        entry_step: "fetch_ticket".into(),
        settings: Settings {
            max_iterations: 60,
            ..Default::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![
            // 1 : le tracker est celui du ticket (Redmine, ClickUp…), trouvé par ses outils MCP.
            Step {
                sub_agent_type: "tracker".into(),
                prompt: TRACKER_READ.into(),
                tools: MCP_TOOLS.iter().map(|t| t.to_string()).collect(),
                output_schema: Some(json!({
                    "type":"object",
                    "properties":{
                        "tracker":{"type":"string"},
                        "title":{"type":"string"},
                        "description":{"type":"string"}
                    },
                    "required":["tracker","title"]
                })),
                transitions: vec![
                    Transition::on_result("resolve_repo", "success"),
                    Transition::always(BLOCKED),
                ],
                ..step("fetch_ticket", "sub_agent", Phase::Plan)
            },
            // 2
            Step {
                sub_agent_type: "repo_resolver".into(),
                prompt: "À partir du ticket « {{steps.fetch_ticket.data.title}} » \
                         ({{ticket_url}}), détermine le dépôt et sa forge (GitHub ou GitLab) : \
                         dépôt indiqué « {{repo}} », réponse du propriétaire « {{reason}} », \
                         champ du ticket, table `projects.toml` ; sinon échoue pour qu'on \
                         demande.\n\nTicket :\n{{steps.fetch_ticket.data.description}}"
                    .into(),
                output_schema: Some(json!({
                    "type":"object",
                    "properties":{
                        "forge":{"type":"string","enum":["github","gitlab"]},
                        "repo":{"type":"string"},
                        "base_branch":{"type":"string"}
                    },
                    "required":["forge","repo","base_branch"]
                })),
                transitions: vec![
                    Transition::on_result("checkout", "success"),
                    Transition::always("ask_repo"),
                ],
                ..step("resolve_repo", "sub_agent", Phase::Plan)
            },
            // 2b
            Step {
                template: "question".into(),
                choices: vec!["Répondu".into()],
                input: "text".into(),
                // La réponse repasse par la résolution : `checkout` lit toujours sa sortie.
                transitions: vec![Transition::always("resolve_repo")],
                ..step("ask_repo", "user", Phase::Plan)
            },
            // 3
            Step {
                command: json!({
                    "unix":"git clone --depth 50 {{steps.resolve_repo.data.repo}} {{workdir}}/repo && \
                            cd {{workdir}}/repo && git checkout -b penelope/{{ticket_id}}",
                    "windows":"git clone --depth 50 {{steps.resolve_repo.data.repo}} {{workdir}}/repo"
                }),
                transitions: vec![
                    Transition::on_result("analyze", "success"),
                    Transition::always(BLOCKED),
                ],
                network: true,
                ..step("checkout", "shell", Phase::Plan)
            },
            // 4
            Step {
                model: "reasoning".into(),
                prompt: "Ticket {{ticket_id}} ({{ticket_url}}) : « {{steps.fetch_ticket.data.title}} »\n\
                         {{steps.fetch_ticket.data.description}}\n\n\
                         Brief de la conversation qui a lancé le run :\n{{brief}}\n\n\
                         Lis le code, localise le problème, écris les critères dans \
                         `session_metadata.criteria`, puis propose un plan de correctif via \
                         `return_value`."
                    .into(),
                transitions: vec![Transition::always("propose")],
                ..step("analyze", "agent", Phase::Plan)
            },
            // 5
            Step {
                template: "plan_proposal".into(),
                choices: vec!["Appliquer".into(), "Réviser".into(), "Rejeter".into()],
                input: "text".into(),
                transitions: vec![
                    Transition::on_result("implement", "Appliquer"),
                    Transition::on_result("analyze", "Réviser"),
                    Transition::on_result("report", "Rejeter"),
                ],
                ..step("propose", "user", Phase::Plan)
            },
            // 6
            Step {
                model: "reasoning".into(),
                prompt: "Implémente le correctif. Coche les critères.\n{{criteriaList}}".into(),
                nudge_prompt: "Continue : {{pendingCount}} critère(s) restant(s).".into(),
                transitions: vec![
                    Transition {
                        goto: "verify".into(),
                        condition: json!({
                            "type":"metadata_all_in","key":"criteria","field":"status",
                            "values":["completed","passed"]
                        }),
                        tag: String::new(),
                    },
                    Transition::always("implement"),
                ],
                ..step("implement", "agent", Phase::Build)
            },
            // 7
            Step {
                children: vec![
                    Step {
                        command: json!({"unix": TEST_COMMAND, "windows": "cargo test"}),
                        cwd: "{{workdir}}/repo".into(),
                        ..step("tests", "shell", Phase::Verification)
                    },
                    Step {
                        command: json!({"unix": LINT_COMMAND, "windows": "cargo clippy -- -D warnings"}),
                        cwd: "{{workdir}}/repo".into(),
                        ..step("lint", "shell", Phase::Verification)
                    },
                    Step {
                        sub_agent_type: "code_reviewer".into(),
                        prompt: "Relis le diff produit pour {{ticket_url}}.".into(),
                        model: "reasoning".into(),
                        ..step("review", "sub_agent", Phase::Verification)
                    },
                    Step {
                        sub_agent_type: "verifier".into(),
                        prompt: "Vérifie chaque critère de `session_metadata.criteria`.".into(),
                        ..step("verifier", "sub_agent", Phase::Verification)
                    },
                ],
                transitions: vec![
                    Transition::on_result("open_pr", "success"),
                    Transition::always("implement"),
                ],
                ..step("verify", "parallel", Phase::Verification)
            },
            // 8
            Step {
                tool: "git_push".into(),
                args: json!({
                    "cwd":"{{workdir}}/repo",
                    "remote":"origin",
                    "branch":"penelope/{{ticket_id}}"
                }),
                transitions: vec![
                    Transition::on_result("create_pr", "success"),
                    Transition::always(BLOCKED),
                ],
                ..step("open_pr", "tool", Phase::Verification)
            },
            // 8b : la PR (GitHub) ou MR (GitLab) via le MCP de la forge, liée au ticket.
            Step {
                prompt: "Ouvre la demande de fusion de la branche `penelope/{{ticket_id}}` vers \
                         `{{steps.resolve_repo.data.base_branch}}` du dépôt \
                         {{steps.resolve_repo.data.repo}}, sur sa forge \
                         ({{steps.resolve_repo.data.forge}} : pull request GitHub ou merge \
                         request GitLab). Trouve l'outil de la forge avec `tool_search`, \
                         appelle-le avec `tool_call`. Titre : « Correctif du ticket \
                         {{ticket_id}} » ; corps : {{ticket_url}} puis le plan retenu.\n\n\
                         {{steps.analyze.content}}\n\n\
                         Rends l'adresse de la demande avec `return_value`, puis `step_done()`."
                    .into(),
                tools: MCP_TOOLS.iter().map(|t| t.to_string()).collect(),
                transitions: vec![
                    Transition::on_result("approve_deploy", "completed"),
                    Transition::on_result("approve_deploy", "success"),
                    Transition::always(BLOCKED),
                ],
                ..step("create_pr", "agent", Phase::Verification)
            },
            // 9
            Step {
                template: "deploy_gate".into(),
                choices: vec![
                    "Déployer".into(),
                    "Attendre la review".into(),
                    "Annuler".into(),
                ],
                transitions: vec![
                    Transition::on_result("deploy", "Déployer"),
                    Transition::on_result("wait_review", "Attendre la review"),
                    Transition::on_result("report", "Annuler"),
                ],
                ..step("approve_deploy", "user", Phase::Waiting)
            },
            // 9w
            Step {
                on: json!({"duration_ms": 3600000}),
                transitions: vec![Transition::always("approve_deploy")],
                ..step("wait_review", "wait", Phase::Waiting)
            },
            // 10
            Step {
                workflow_id: "deploy-generic".into(),
                params: json!({"environnement":"prod","repo":"{{workdir}}/repo"}),
                transitions: vec![
                    Transition::on_result("update_ticket", "success"),
                    Transition::always("deploy_failed"),
                ],
                ..step("deploy", "workflow", Phase::Deploy)
            },
            // 10f
            Step {
                template: "incident".into(),
                choices: vec!["Rollback".into(), "Réessayer".into(), "Laisser".into()],
                transitions: vec![
                    Transition::on_result("report", "Rollback"),
                    Transition::on_result("deploy", "Réessayer"),
                    Transition::on_result("report", "Laisser"),
                ],
                ..step("deploy_failed", "user", Phase::Deploy)
            },
            // 11
            Step {
                prompt: "Dans le tracker `{{steps.fetch_ticket.data.tracker}}`, commente le \
                         ticket {{ticket_id}} ({{ticket_url}}) : « Correctif déployé par \
                         Pénélope. » et passe-le à l'état résolu (ou son équivalent). Un seul \
                         appel d'écriture si l'outil le permet : `tool_search` puis \
                         `tool_call`. Puis `step_done()`."
                    .into(),
                tools: MCP_TOOLS.iter().map(|t| t.to_string()).collect(),
                transitions: vec![Transition::always(DONE)],
                ..step("update_ticket", "agent", Phase::Done)
            },
            // 12
            Step {
                prompt: "Dans le tracker du ticket {{ticket_id}} ({{ticket_url}}), ajoute ce \
                         commentaire : « Workflow interrompu : {{reason}} ». Outils : \
                         `tool_search` puis `tool_call`. Puis `step_done()`."
                    .into(),
                tools: MCP_TOOLS.iter().map(|t| t.to_string()).collect(),
                transitions: vec![Transition::always(DONE)],
                ..step("report", "agent", Phase::Done)
            },
        ],
    }
}

/// Les quatre workflows livrés.
pub fn all() -> Vec<Workflow> {
    vec![
        build_verify(),
        review(),
        ticket_to_deploy(),
        deploy_generic(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{Known, validate};

    fn known() -> Known {
        Known {
            workflow_ids: [
                "build-verify",
                "review",
                "ticket-to-deploy",
                "deploy-generic",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            model_aliases: ["main", "fast", "reasoning", "code", "summarizer"]
                .into_iter()
                .map(String::from)
                .collect(),
            native_tools: ["fs_read", "git_push", "shell_exec"]
                .into_iter()
                .map(String::from)
                .collect(),
            mcp_tools: Default::default(),
            templates: ["question", "plan_proposal", "deploy_gate", "incident"]
                .into_iter()
                .map(String::from)
                .collect(),
            max_depth: 3,
        }
    }

    #[test]
    fn all_bundled_workflows_validate() {
        for w in all() {
            let r = validate(&w, Some(&w.metadata.id), &known());
            assert!(r.is_valid(), "{} :\n{}", w.metadata.id, r.render());
        }
    }

    #[test]
    fn ticket_to_deploy_follows_the_reference_table() {
        let w = ticket_to_deploy();
        for id in [
            "fetch_ticket",
            "resolve_repo",
            "ask_repo",
            "checkout",
            "analyze",
            "propose",
            "implement",
            "verify",
            "open_pr",
            "approve_deploy",
            "wait_review",
            "deploy",
            "deploy_failed",
            "update_ticket",
            "report",
        ] {
            assert!(w.step(id).is_some(), "étape manquante : {id}");
        }
        assert_eq!(w.entry_step, "fetch_ticket");

        // Les points sensibles passent par une étape `user`.
        for id in ["propose", "approve_deploy", "deploy_failed"] {
            assert_eq!(w.step(id).unwrap().kind, "user", "{id}");
        }
        // Le push est un outil `external` : il sera soumis à approbation par la politique.
        assert_eq!(w.step("open_pr").unwrap().tool, "git_push");
        // Tracker et forge ne sont pas figés : aucune étape ne nomme un outil MCP précis.
        for id in ["fetch_ticket", "create_pr", "update_ticket", "report"] {
            let s = w.step(id).unwrap();
            assert!(s.tool.is_empty(), "{id}");
            assert!(s.tools.iter().any(|t| t == "tool_call"), "{id}");
        }
        assert!(w.step("analyze").unwrap().prompt.contains("{{brief}}"));
        // Après la question, le dépôt est résolu à nouveau : `checkout` a toujours sa sortie.
        assert_eq!(
            w.step("ask_repo").unwrap().transitions[0].goto,
            "resolve_repo"
        );
        // Le déploiement délègue au sous-workflow.
        assert_eq!(w.step("deploy").unwrap().workflow_id, "deploy-generic");
    }

    #[test]
    fn the_verify_step_runs_four_children_in_parallel() {
        let w = ticket_to_deploy();
        let v = w.step("verify").unwrap();
        assert_eq!(v.kind, "parallel");
        let names: Vec<&str> = v.children.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(names, vec!["tests", "lint", "review", "verifier"]);
        for c in &v.children {
            assert!(
                crate::model::PARALLEL_CHILD_KINDS.contains(&c.kind.as_str()),
                "{} : type interdit en parallèle",
                c.id
            );
        }
    }

    #[test]
    fn build_verify_loops_until_criteria_are_met() {
        let w = build_verify();
        let build = w.step("build").unwrap();
        assert_eq!(build.transitions[0].goto, "verify");
        assert_eq!(
            build.transitions[0].condition["type"], "metadata_all_in",
            "la boucle sort sur les critères, pas sur un compteur"
        );
        assert_eq!(build.transitions[1].goto, "build");
        assert!(build.transitions[1].is_always());
    }

    #[test]
    fn deploy_generic_has_a_rollback_path() {
        let w = deploy_generic();
        assert!(w.step("rollback").is_some());
        let deploy = w.step("deployer").unwrap();
        assert_eq!(deploy.transitions.last().unwrap().goto, "rollback");
    }

    /// #106 : le réseau n'est déclaré que là où la commande en vit (clone, déploiement,
    /// vérification et retour arrière d'un environnement), et l'aperçu le montre ; tests
    /// et lint exécutent le code du dépôt sans réseau.
    #[test]
    fn network_is_declared_only_where_the_command_lives_on_it() {
        let deploy = deploy_generic();
        for id in ["deployer", "verifier", "rollback"] {
            assert!(deploy.step(id).unwrap().network, "{id}");
        }
        assert!(
            deploy
                .render_graph()
                .contains("deployer [shell] (deploy) · réseau")
        );
        let ticket = ticket_to_deploy();
        assert!(ticket.step("checkout").unwrap().network);
        let mut offline = Vec::new();
        for w in all() {
            for s in w
                .steps
                .iter()
                .chain(w.steps.iter().flat_map(|s| s.children.iter()))
            {
                if s.kind == "shell" && !s.network {
                    offline.push(s.id.as_str().to_string());
                }
            }
        }
        assert!(
            offline.iter().all(|id| id == "tests" || id == "lint"),
            "{offline:?}"
        );
    }

    #[test]
    fn shell_steps_declare_commands_for_both_families() {
        for w in all() {
            for s in &w.steps {
                if s.kind != "shell" {
                    continue;
                }
                assert!(
                    s.command_for_os("macos").is_some(),
                    "{}/{} : pas de commande unix",
                    w.metadata.id,
                    s.id
                );
                assert!(
                    s.command_for_os("windows").is_some(),
                    "{}/{} : pas de commande windows",
                    w.metadata.id,
                    s.id
                );
            }
        }
    }

    #[test]
    fn every_bundled_workflow_can_reach_done() {
        for w in all() {
            let r = validate(&w, Some(&w.metadata.id), &known());
            assert!(
                !r.render().contains("$done"),
                "{} : $done inatteignable",
                w.metadata.id
            );
        }
    }
}
