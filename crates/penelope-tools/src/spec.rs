//! Catalogue des outils natifs (§11) : nom, risque, schéma, annotations.
//!
//! La liste est **fixe et triée** : elle est identique d'un tour à l'autre, ce qui est la
//! condition du préfixe stable (§5.2).

use penelope_kernel::risk::RiskClass;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub risk: RiskClass,
    pub description: &'static str,
    pub schema: Value,
    /// Déclaré idempotent : un effet `unknown` peut être relancé sans HITL (§4.2).
    pub idempotent: bool,
    /// Atteint le réseau : contamine la provenance du tour (§6.5).
    pub network: bool,
    /// Disponible uniquement dans un run de workflow.
    pub workflow_only: bool,
}

impl ToolSpec {
    pub const fn new(
        name: &'static str,
        risk: RiskClass,
        description: &'static str,
        schema: Value,
    ) -> Self {
        ToolSpec {
            name,
            risk,
            description,
            schema,
            idempotent: false,
            network: false,
            workflow_only: false,
        }
    }
}

fn spec(
    name: &'static str,
    risk: RiskClass,
    description: &'static str,
    schema: Value,
    idempotent: bool,
    network: bool,
    workflow_only: bool,
) -> ToolSpec {
    ToolSpec {
        name,
        risk,
        description,
        schema,
        idempotent,
        network,
        workflow_only,
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

/// Tous les outils natifs du §11, triés par nom.
pub fn all() -> Vec<ToolSpec> {
    let mut v = vec![
        // ------------------------------------------------------------ fichiers
        spec(
            "fs_read",
            RiskClass::Read,
            "Lit un fichier du workspace autorisé, avec pagination par lignes.",
            obj(
                json!({
                    "path": {"type":"string"},
                    "offset": {"type":"integer","minimum":0},
                    "limit": {"type":"integer","minimum":1,"maximum":5000}
                }),
                &["path"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "fs_list",
            RiskClass::Read,
            "Liste le contenu d'un répertoire du workspace.",
            obj(
                json!({
                    "path": {"type":"string"},
                    "recursive": {"type":"boolean"},
                    "max_entries": {"type":"integer","minimum":1,"maximum":5000}
                }),
                &["path"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "fs_search",
            RiskClass::Read,
            "Recherche une expression régulière dans les fichiers du workspace.",
            obj(
                json!({
                    "pattern": {"type":"string"},
                    "path": {"type":"string"},
                    "glob": {"type":"string"},
                    "max_results": {"type":"integer","minimum":1,"maximum":500}
                }),
                &["pattern"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "fs_write",
            RiskClass::Write,
            "Écrit un fichier dans le workspace. Un point de reprise git est posé si le \
             workspace est un dépôt.",
            obj(
                json!({
                    "path": {"type":"string"},
                    "content": {"type":"string"}
                }),
                &["path", "content"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "fs_edit",
            RiskClass::Write,
            "Remplace une portion exacte d'un fichier. Échoue si la portion n'est pas \
             unique : c'est ce qui empêche une édition au mauvais endroit.",
            obj(
                json!({
                    "path": {"type":"string"},
                    "old": {"type":"string"},
                    "new": {"type":"string"},
                    "replace_all": {"type":"boolean"}
                }),
                &["path", "old", "new"],
            ),
            false,
            false,
            false,
        ),
        // ------------------------------------------------------------ shell
        spec(
            "shell_exec",
            RiskClass::Write,
            "Exécute une commande sous bac à sable, avec délai et sortie tronquée en \
             artefact au-delà du plafond.",
            obj(
                json!({
                    "command": {"type":"string"},
                    "cwd": {"type":"string"},
                    "timeout_ms": {"type":"integer","minimum":1000,"maximum":3600000}
                }),
                &["command"],
            ),
            false,
            false,
            false,
        ),
        // ------------------------------------------------------------ git
        spec(
            "git_status",
            RiskClass::Read,
            "État du dépôt : branche, fichiers modifiés.",
            obj(json!({"cwd": {"type":"string"}}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "git_diff",
            RiskClass::Read,
            "Diff du dépôt, éventuellement contre une référence.",
            obj(
                json!({"cwd": {"type":"string"}, "against": {"type":"string"}, "staged": {"type":"boolean"}}),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "git_branch",
            RiskClass::Write,
            "Crée ou change de branche.",
            obj(
                json!({"cwd": {"type":"string"}, "name": {"type":"string"}, "create": {"type":"boolean"}}),
                &["name"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "git_commit",
            RiskClass::Write,
            "Valide les changements indexés.",
            obj(
                json!({"cwd": {"type":"string"}, "message": {"type":"string"}, "all": {"type":"boolean"}}),
                &["message"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "git_clone",
            RiskClass::External,
            "Clone un dépôt distant dans le workspace.",
            obj(
                json!({"url": {"type":"string"}, "dest": {"type":"string"}, "depth": {"type":"integer"}}),
                &["url", "dest"],
            ),
            false,
            true,
            false,
        ),
        spec(
            "git_push",
            RiskClass::External,
            "Pousse une branche vers le dépôt distant. Toujours soumis à approbation.",
            obj(
                json!({"cwd": {"type":"string"}, "remote": {"type":"string"}, "branch": {"type":"string"}}),
                &["branch"],
            ),
            false,
            true,
            false,
        ),
        // ------------------------------------------------------------ réseau
        spec(
            "http_fetch",
            RiskClass::External,
            "Récupère une URL. Les adresses privées et les points de métadonnées cloud \
             sont refusés.",
            obj(
                json!({
                    "url": {"type":"string"},
                    "method": {"type":"string","enum":["GET","POST","HEAD"]},
                    "headers": {"type":"object"},
                    "body": {"type":"string"},
                    "max_bytes": {"type":"integer","minimum":1024,"maximum":10485760}
                }),
                &["url"],
            ),
            false,
            true,
            false,
        ),
        // ------------------------------------------------------------ temps
        spec(
            "time_now",
            RiskClass::Read,
            "Date et heure courantes dans le fuseau du propriétaire.",
            obj(json!({"timezone": {"type":"string"}}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "schedule_create",
            RiskClass::Write,
            "Crée un déclencheur planifié, soumis à approbation. Rappel daté : kind \
             `cron`, spec `{\"expr\": \"0 9 20 9 *\", \"once\": true}` (fuseau du \
             propriétaire par défaut, `tz` pour un autre), target `{\"type\": \"notify\", \
             \"template\": \"⏰ Appeler Paul\"}`. Tâche récurrente : sans `once`, ou target \
             `{\"type\": \"prompt\", \"prompt\": \"…\"}` pour travailler à l'heure dite. \
             Autres kinds : `interval` (`every_ms`), `mcp_poll` (`server`, `tool` en lecture, \
             `args`, `every_ms` ≥ 60000, `item_path`, `id_path`, `filter`), `watch_file` \
             (`path`), `event` (`event`). Le retour arrive dans cette conversation.",
            obj(
                json!({
                    "kind": {"type":"string","enum":["cron","interval","mcp_poll","watch_file","event"]},
                    "spec": {"type":"object"},
                    "target": {"type":"object"},
                    "dedup": {"type":"object"}
                }),
                &["kind", "spec", "target"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "schedule_list",
            RiskClass::Read,
            "Liste les déclencheurs planifiés.",
            obj(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "schedule_delete",
            RiskClass::Write,
            "Supprime un déclencheur planifié.",
            obj(json!({"id": {"type":"string"}}), &["id"]),
            true,
            false,
            false,
        ),
        // ------------------------------------------------------------ canal
        spec(
            "send_message",
            RiskClass::Write,
            "Envoie un message au propriétaire.",
            obj(
                json!({"text": {"type":"string"}, "template": {"type":"string"}}),
                &["text"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "send_file",
            RiskClass::Write,
            "Envoie un fichier au propriétaire.",
            obj(
                json!({"path": {"type":"string"}, "caption": {"type":"string"}}),
                &["path"],
            ),
            false,
            false,
            false,
        ),
        // ------------------------------------------------------------ mémoire
        spec(
            "mem_search",
            RiskClass::Read,
            "Recherche dans la mémoire curée, dans les documents ingérés (`vault/sources`, \
             passages encadrés comme non fiables, `slug` pour un seul document) et, sur \
             demande explicite, épisodique.",
            obj(
                json!({
                    "query": {"type":"string"},
                    "level": {"type":"string"},
                    "projet": {"type":"string"},
                    "slug": {"type":"string"},
                    "include_episodic": {"type":"boolean"},
                    "limit": {"type":"integer","minimum":1,"maximum":50}
                }),
                &["query"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "mem_get",
            RiskClass::Read,
            "Lit une entrée de mémoire par uid ou par slug.",
            obj(
                json!({"uid": {"type":"string"}, "slug": {"type":"string"}}),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "mem_note",
            RiskClass::Write,
            "Note une observation dans le journal du jour. N'écrit jamais dans le niveau \
             curé.",
            obj(
                json!({
                    "type": {"type":"string","enum":["fait","preference","correction","ecart","decision","procedure_candidate"]},
                    "texte": {"type":"string"},
                    "quand": {"type":"string"},
                    "importance": {"type":"integer","minimum":1,"maximum":10}
                }),
                &["type", "texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "mem_remember",
            RiskClass::Write,
            "Écrit directement en mémoire. Refusé si le message courant ne le demande pas \
             explicitement.",
            obj(
                json!({
                    "niveau": {"type":"string","enum":["profil","coeur","projet","cure"]},
                    "texte": {"type":"string"}
                }),
                &["niveau", "texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "mem_forget",
            RiskClass::Destructive,
            "Retire une entrée de mémoire. Toujours soumis à approbation.",
            obj(json!({"uid": {"type":"string"}}), &["uid"]),
            false,
            false,
            false,
        ),
        spec(
            "intent_create",
            RiskClass::Write,
            "Arme une intention événementielle : « quand on reparle de X, rappelle-moi Y ». \
             Elle revient dans le contexte du message qui en parle. Pour un rappel daté, \
             utiliser `schedule_create`.",
            obj(
                json!({"texte": {"type":"string"}, "declencheurs": {"type":"array","items":{"type":"string"}}}),
                &["texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "intent_list",
            RiskClass::Read,
            "Liste les intentions armées.",
            obj(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "intent_cancel",
            RiskClass::Write,
            "Annule une intention.",
            obj(json!({"id": {"type":"string"}}), &["id"]),
            true,
            false,
            false,
        ),
        // ------------------------------------------------------------ historique
        spec(
            "history_grep",
            RiskClass::Read,
            "Recherche plein texte dans les messages bruts et les résumés. Par défaut dans \
             la session en cours ; `scope: \"all\"` cherche dans toutes les sessions, y \
             compris fermées, pour retrouver une conversation antérieure. `query` attend des \
             mots-clés, pas une phrase : chaque mot doit apparaître. Chaque extrait donne le \
             titre et la date de sa session.",
            obj(
                json!({
                    "query": {
                        "type":"string",
                        "description":"mots-clés, tous exigés (ex. « facturation ACME »)"
                    },
                    "scope": {
                        "type":"string",
                        "enum":["session","all"],
                        "description":"session : la conversation en cours (défaut) ; all : \
                                       toutes les sessions"
                    },
                    "depth": {"type":"integer","minimum":0,"maximum":10}
                }),
                &["query"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "history_describe",
            RiskClass::Read,
            "Manifeste d'un nœud de résumé : tokens, intervalle, enfants.",
            obj(json!({"node_id": {"type":"string"}}), &["node_id"]),
            true,
            false,
            false,
        ),
        spec(
            "history_expand",
            RiskClass::Read,
            "Contenu paginé d'un nœud ou d'un intervalle brut.",
            obj(
                json!({"node_id": {"type":"string"}, "page": {"type":"integer","minimum":0}}),
                &["node_id"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "history_expand_query",
            RiskClass::Read,
            "Retrouve, dans toutes les sessions y compris fermées, les passages liés à une \
             question en langage naturel : recherche mot significatif par mot significatif, \
             extraits classés par nombre de mots trouvés, avec le titre et la date de leur \
             session. `history_expand` lit ensuite un passage en entier.",
            obj(
                json!({"question": {"type":"string"}, "budget": {"type":"integer"}}),
                &["question"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "artifact_read",
            RiskClass::Read,
            "Lecture paginée d'un artefact ; le curseur n'avance que des octets renvoyés.",
            obj(
                json!({"id": {"type":"string"}, "cursor": {"type":"integer","minimum":0}}),
                &["id"],
            ),
            true,
            false,
            false,
        ),
        // ------------------------------------------------------------ skills et MCP
        spec(
            "skill_search",
            RiskClass::Read,
            "Cherche une skill par mots-clés.",
            obj(
                json!({"query": {"type":"string"}, "limit": {"type":"integer"}}),
                &["query"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "skill_load",
            RiskClass::Read,
            "Charge une skill dans le tour courant.",
            obj(json!({"name": {"type":"string"}}), &["name"]),
            true,
            false,
            false,
        ),
        spec(
            "skill_propose",
            RiskClass::Write,
            "Propose une nouvelle skill. Jamais activée sans approbation.",
            obj(
                json!({
                    "name": {"type":"string"},
                    "description": {"type":"string"},
                    "body": {"type":"string"},
                    "allowed_tools": {"type":"array","items":{"type":"string"}}
                }),
                &["name", "description", "body"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "skill_patch",
            RiskClass::Write,
            "Propose une modification de skill. Jamais appliquée sans approbation.",
            obj(
                json!({"name": {"type":"string"}, "body": {"type":"string"}}),
                &["name", "body"],
            ),
            false,
            false,
            false,
        ),
        // ------------------------------------------------------------ workflows
        spec(
            "workflow_list",
            RiskClass::Read,
            "Liste les workflows disponibles.",
            obj(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "workflow_describe",
            RiskClass::Read,
            "Décrit un workflow : étapes, paramètres, budget.",
            obj(json!({"id": {"type":"string"}}), &["id"]),
            true,
            false,
            false,
        ),
        spec(
            "workflow_start",
            RiskClass::Write,
            "Démarre un workflow avec ses paramètres.",
            obj(
                json!({"id": {"type":"string"}, "params": {"type":"object"}}),
                &["id"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "workflow_status",
            RiskClass::Read,
            "État d'un run.",
            obj(json!({"run_id": {"type":"string"}}), &["run_id"]),
            true,
            false,
            false,
        ),
        spec(
            "workflow_control",
            RiskClass::Write,
            "Contrôle un run : pause, reprise, annulation, relance d'étape.",
            obj(
                json!({
                    "run_id": {"type":"string"},
                    "op": {"type":"string","enum":["pause","resume","cancel","retry-step","skip-step"]}
                }),
                &["run_id", "op"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "workflow_author",
            RiskClass::Write,
            "Rédige un workflow. Validation, aperçu, puis approbation avant écriture.",
            obj(json!({"draft": {"type":"object"}}), &["draft"]),
            false,
            false,
            false,
        ),
        // ------------------------------------------------------------ agent
        spec(
            "sub_agent_spawn",
            RiskClass::Write,
            "Lance un sub-agent à contexte neuf, outils restreints, retour structuré.",
            obj(
                json!({
                    "kind": {"type":"string"},
                    "prompt": {"type":"string"},
                    "model": {"type":"string"},
                    "budget": {"type":"integer"},
                    "tools": {"type":"array","items":{"type":"string"}}
                }),
                &["kind", "prompt"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "session_metadata",
            RiskClass::Write,
            "Lit ou modifie les métadonnées de session : critères, findings, todos.",
            obj(
                json!({
                    "op": {"type":"string","enum":["set","append","update","remove"]},
                    "key": {"type":"string"},
                    "entry": {}
                }),
                &["op", "key"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "ask_user",
            RiskClass::Read,
            "Pose une question au propriétaire et attend sa réponse.",
            obj(
                json!({
                    "question": {"type":"string"},
                    "choices": {"type":"array","items":{"type":"string"}},
                    "input": {"type":"string","enum":["none","text"]}
                }),
                &["question"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "step_done",
            RiskClass::Read,
            "Déclare l'étape de workflow terminée.",
            obj(json!({}), &[]),
            true,
            false,
            true,
        ),
        spec(
            "return_value",
            RiskClass::Read,
            "Renvoie le résultat d'une étape de workflow.",
            obj(
                json!({"result": {"type":"string"}, "content": {}}),
                &["result"],
            ),
            true,
            false,
            true,
        ),
        // ------------------------------------------------------------ images
        spec(
            "image_generate",
            RiskClass::External,
            "Génère une image et la stocke en artefact.",
            obj(
                json!({
                    "prompt": {"type":"string"},
                    "ref_images": {"type":"array","items":{"type":"string"}},
                    "size": {"type":"string"}
                }),
                &["prompt"],
            ),
            false,
            true,
            false,
        ),
        // ------------------------------------------------------------ soi-même
        spec(
            "self_status",
            RiskClass::Read,
            "État complet de Pénélope et de sa machine : version, modèle qui répond à ce \
             tour et routage, configuration effective (alias, rôles, bac à sable, budgets, \
             Telegram, providers, transcription), coûts du jour et de la session, file de \
             travail, chemins, et machine (batterie, secteur, disque, mémoire, charge, \
             démarrage, système). À appeler pour toute question sur toi-même ou sur \
             l'ordinateur, plutôt que de supposer. Aucun secret n'y figure.",
            obj(
                json!({
                    "section": {
                        "type": "string",
                        "enum": ["all", "model", "config", "costs", "machine"],
                        "description": "Partie voulue ; `all` par défaut."
                    }
                }),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "config_set",
            RiskClass::Write,
            "Modifie un réglage de ta propre configuration, appliqué à chaud : chemin \
             pointé et valeur, par exemple `models.aliases.main` = \
             `openrouter:z-ai/glm-5.3`, `models.routing.classifier` = `false`, \
             `budget.daily_usd` = `30`. Lire d'abord `self_status` (section config). \
             Jamais de secret : une clé se pose en SSH avec `penelope secret set`. \
             Approbation du propriétaire requise, double pour le bac à sable, les \
             providers et Telegram.",
            obj(
                json!({
                    "path": {"type": "string", "description": "Chemin pointé, ex. `models.aliases.main`."},
                    "value": {
                        "type": "string",
                        "description": "Valeur en texte : `true`, `42`, `openrouter:z-ai/glm-5.3`, ou JSON pour une liste ou un objet."
                    }
                }),
                &["path", "value"],
            ),
            false,
            false,
            false,
        ),
    ];
    v.sort_by_key(|s| s.name);
    v
}

/// Les outils exposés **en permanence** au modèle : liste fixe, triée, identique d'un tour
/// à l'autre (§5.2).
pub fn always_exposed(in_workflow: bool) -> Vec<ToolSpec> {
    all()
        .into_iter()
        .filter(|s| in_workflow || !s.workflow_only)
        .collect()
}

pub fn get(name: &str) -> Option<ToolSpec> {
    all().into_iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prd_tool_is_present() {
        let names: Vec<&str> = all().iter().map(|s| s.name).collect();
        for expected in [
            "fs_read",
            "fs_list",
            "fs_search",
            "fs_write",
            "fs_edit",
            "shell_exec",
            "git_status",
            "git_diff",
            "git_branch",
            "git_commit",
            "git_clone",
            "git_push",
            "http_fetch",
            "time_now",
            "schedule_create",
            "schedule_list",
            "schedule_delete",
            "send_file",
            "send_message",
            "mem_search",
            "mem_get",
            "mem_note",
            "mem_remember",
            "mem_forget",
            "intent_create",
            "intent_list",
            "intent_cancel",
            "history_grep",
            "history_describe",
            "history_expand",
            "history_expand_query",
            "artifact_read",
            "skill_search",
            "skill_load",
            "skill_propose",
            "skill_patch",
            "workflow_list",
            "workflow_describe",
            "workflow_start",
            "workflow_status",
            "workflow_control",
            "workflow_author",
            "sub_agent_spawn",
            "session_metadata",
            "step_done",
            "return_value",
            "ask_user",
            "image_generate",
            "self_status",
            "config_set",
        ] {
            assert!(names.contains(&expected), "outil manquant : {expected}");
        }
    }

    #[test]
    fn the_list_is_sorted_and_stable() {
        let a: Vec<&str> = all().iter().map(|s| s.name).collect();
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(
            a, sorted,
            "la liste doit être triée pour stabiliser le préfixe"
        );
        let b: Vec<&str> = all().iter().map(|s| s.name).collect();
        assert_eq!(a, b, "deux appels donnent exactement la même liste");
    }

    #[test]
    fn no_duplicate_names() {
        let mut names: Vec<&str> = all().iter().map(|s| s.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn risk_classes_follow_the_prd_table() {
        assert_eq!(get("fs_read").unwrap().risk, RiskClass::Read);
        assert_eq!(get("fs_write").unwrap().risk, RiskClass::Write);
        assert_eq!(get("shell_exec").unwrap().risk, RiskClass::Write);
        assert_eq!(get("git_push").unwrap().risk, RiskClass::External);
        assert_eq!(get("http_fetch").unwrap().risk, RiskClass::External);
        assert_eq!(get("mem_forget").unwrap().risk, RiskClass::Destructive);
    }

    #[test]
    fn network_tools_are_marked_for_contamination() {
        for n in ["http_fetch", "git_clone", "git_push", "image_generate"] {
            assert!(get(n).unwrap().network, "{n} doit être marqué réseau");
        }
        for n in ["fs_read", "shell_exec", "mem_search"] {
            assert!(
                !get(n).unwrap().network,
                "{n} ne doit pas être marqué réseau"
            );
        }
    }

    #[test]
    fn read_tools_are_idempotent() {
        for s in all() {
            if s.risk == RiskClass::Read && !matches!(s.name, "ask_user" | "history_expand_query") {
                assert!(s.idempotent, "{} devrait être idempotent", s.name);
            }
        }
    }

    #[test]
    fn workflow_only_tools_are_hidden_outside_runs() {
        let chat: Vec<&str> = always_exposed(false).iter().map(|s| s.name).collect();
        assert!(!chat.contains(&"step_done"));
        assert!(!chat.contains(&"return_value"));
        let run: Vec<&str> = always_exposed(true).iter().map(|s| s.name).collect();
        assert!(run.contains(&"step_done"));
    }

    #[test]
    fn every_schema_is_a_valid_object_schema() {
        for s in all() {
            assert_eq!(s.schema["type"], "object", "{}", s.name);
            // Le schéma doit accepter un objet vide ou refuser proprement, jamais paniquer.
            let _ = penelope_kernel::schema::validate(&s.schema, &json!({}));
            assert!(!s.description.is_empty(), "{} sans description", s.name);
        }
    }

    #[test]
    fn required_fields_are_enforced() {
        let s = get("fs_read").unwrap();
        assert!(penelope_kernel::schema::validate(&s.schema, &json!({"path":"a.rs"})).is_empty());
        assert!(!penelope_kernel::schema::validate(&s.schema, &json!({})).is_empty());
        assert!(
            !penelope_kernel::schema::validate(&s.schema, &json!({"path":"a","inconnu":1}))
                .is_empty(),
            "additionalProperties: false doit rejeter les champs inconnus"
        );
    }
}
