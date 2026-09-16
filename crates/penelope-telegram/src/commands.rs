//! Catalogue de commandes Telegram (§14.6), déclaré via `setMyCommands`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub example: &'static str,
    /// Méthode RPC équivalente (§15, CA 15 : toute action Telegram a un équivalent CLI).
    pub rpc: &'static str,
}

const fn c(
    name: &'static str,
    category: &'static str,
    description: &'static str,
    example: &'static str,
    rpc: &'static str,
) -> Command {
    Command {
        name,
        category,
        description,
        example,
        rpc,
    }
}

use penelope_kernel::api::method as m;

/// Toutes les commandes du tableau §14.6.
pub fn all() -> Vec<Command> {
    vec![
        // Session
        c(
            "new",
            "Session",
            "Nouvelle session, éventuellement nommée",
            "/new refonte facturation",
            m::SESSION_NEW,
        ),
        c(
            "sessions",
            "Session",
            "Liste les sessions",
            "/sessions",
            m::SESSION_LIST,
        ),
        c(
            "switch",
            "Session",
            "Bascule vers une session",
            "/switch s_01J8",
            m::SESSION_SWITCH,
        ),
        c(
            "fork",
            "Session",
            "Duplique la session courante",
            "/fork",
            m::SESSION_FORK,
        ),
        c(
            "rewind",
            "Session",
            "Revient en arrière de n tours",
            "/rewind 2",
            m::SESSION_REWIND,
        ),
        c(
            "compact",
            "Session",
            "Force une compaction",
            "/compact",
            m::SESSION_COMPACT,
        ),
        c(
            "export",
            "Session",
            "Exporte la session",
            "/export",
            m::SESSION_EXPORT,
        ),
        c(
            "stop",
            "Session",
            "Arrête la génération en cours",
            "/stop",
            m::CHAT_STOP,
        ),
        // Modèles
        c(
            "model",
            "Modèles",
            "Modèle de la session, à choisir en un bouton",
            "/model",
            m::SESSION_MODEL,
        ),
        c(
            "models",
            "Modèles",
            "Catalogue filtrable",
            "/models deepseek",
            m::MODEL_LIST,
        ),
        c(
            "budget",
            "Modèles",
            "Coûts : jour, session, requêtes les plus chères",
            "/budget sessions",
            m::USAGE,
        ),
        // Mémoire
        c(
            "note",
            "Mémoire",
            "Note dans le journal du jour",
            "/note le client préfère les PR courtes",
            m::MEM_SEARCH,
        ),
        c(
            "retiens",
            "Mémoire",
            "Écriture directe en mémoire",
            "/retiens toujours répondre en français",
            m::MEM_SEARCH,
        ),
        c(
            "oublie",
            "Mémoire",
            "Recherche puis approbation de retrait",
            "/oublie caprover",
            m::MEM_FORGET,
        ),
        c(
            "recall",
            "Mémoire",
            "Recherche explicite en mémoire",
            "/recall déploiement",
            m::MEM_SEARCH,
        ),
        c(
            "appris",
            "Mémoire",
            "Apprentissages récents",
            "/appris 7",
            m::MEM_LEARNED,
        ),
        c(
            "pratique",
            "Mémoire",
            "Affiche une pratique et ses exceptions",
            "/pratique langage-backend",
            m::MEM_SHOW,
        ),
        c(
            "dream",
            "Mémoire",
            "Lance la consolidation",
            "/dream",
            m::MEM_DREAM,
        ),
        c(
            "intentions",
            "Mémoire",
            "Liste les intentions armées",
            "/intentions",
            m::INTENT_LIST,
        ),
        c(
            "mien",
            "Mémoire",
            "Déclare un document comme rédigé par toi",
            "/mien",
            m::MEM_SEARCH,
        ),
        c(
            "forget",
            "Mémoire",
            "Oublie tout ce qui vient d'une session",
            "/forget s_01J8",
            m::MEM_FORGET,
        ),
        // MCP
        c(
            "mcp",
            "MCP",
            "État et administration des serveurs MCP",
            "/mcp",
            m::MCP_LIST,
        ),
        c(
            "p",
            "MCP",
            "Exécute un prompt MCP",
            "/p redmine resume_ticket",
            m::MCP_SHOW,
        ),
        // Skills
        c(
            "skills",
            "Skills",
            "Liste les skills",
            "/skills",
            m::SKILL_LIST,
        ),
        c(
            "skill",
            "Skills",
            "Administration d'une skill",
            "/skill rollback revue-de-code",
            m::SKILL_ROLLBACK,
        ),
        // Workflows
        c("wf", "Workflows", "Liste les workflows", "/wf", m::WF_LIST),
        c(
            "run",
            "Workflows",
            "Démarre un workflow",
            "/run ticket-to-deploy ticket_url=…",
            m::WF_RUN,
        ),
        c(
            "runs",
            "Workflows",
            "Runs en cours et récents",
            "/runs",
            m::WF_RUNS,
        ),
        c(
            "resume",
            "Workflows",
            "Relance un run bloqué",
            "/resume r_01J8",
            m::WF_CONTROL,
        ),
        // Planification
        c(
            "schedules",
            "Planification",
            "Déclencheurs planifiés",
            "/schedules",
            m::SCHEDULE_LIST,
        ),
        // HITL
        c(
            "approvals",
            "HITL",
            "Demandes en attente",
            "/approvals",
            m::APPROVALS,
        ),
        c(
            "policies",
            "HITL",
            "Règles d'autorisation",
            "/policies",
            m::POLICIES,
        ),
        c(
            "quiet",
            "HITL",
            "Heures silencieuses",
            "/quiet 22:00-07:00",
            m::QUIET,
        ),
        // Système
        c("status", "Système", "État du daemon", "/status", m::STATUS),
        c(
            "doctor",
            "Système",
            "Diagnostic complet",
            "/doctor",
            m::DOCTOR,
        ),
        c(
            "config",
            "Système",
            "Configuration et générations",
            "/config",
            m::CONFIG_STATUS,
        ),
        c(
            "logs",
            "Système",
            "Derniers logs",
            "/logs mcp redmine",
            m::TAIL,
        ),
        c(
            "restart",
            "Système",
            "Redémarre le daemon",
            "/restart",
            m::RESTART,
        ),
        c(
            "upgrade",
            "Système",
            "Met à jour le binaire",
            "/upgrade",
            m::UPGRADE,
        ),
        c(
            "secret",
            "Système",
            "Liste ou supprime un secret (jamais de saisie)",
            "/secret list",
            m::SECRET_LIST,
        ),
        // Aide
        c(
            "help",
            "Aide",
            "Catalogue des commandes avec exemples",
            "/help",
            m::STATUS,
        ),
    ]
}

/// Charge utile de `setMyCommands`.
pub fn to_bot_commands() -> Value {
    Value::Array(
        all()
            .iter()
            .map(|c| json!({"command": c.name, "description": c.description}))
            .collect(),
    )
}

/// Rendu de `/help`, groupé par catégorie.
pub fn help_text() -> String {
    let mut by_cat: std::collections::BTreeMap<&str, Vec<&Command>> = Default::default();
    let cmds = all();
    for c in &cmds {
        by_cat.entry(c.category).or_default().push(c);
    }
    let mut s = String::from("**Commandes**\n");
    for (cat, list) in by_cat {
        s.push_str(&format!("\n### {cat}\n"));
        for c in list {
            s.push_str(&format!(
                "- `/{}` — {}\n  `{}`\n",
                c.name, c.description, c.example
            ));
        }
    }
    s
}

pub fn find(name: &str) -> Option<Command> {
    all().into_iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_covers_the_prd_table() {
        let names: Vec<&str> = all().iter().map(|c| c.name).collect();
        for expected in [
            "new",
            "sessions",
            "switch",
            "fork",
            "rewind",
            "compact",
            "export",
            "stop",
            "model",
            "models",
            "budget",
            "note",
            "retiens",
            "oublie",
            "recall",
            "appris",
            "pratique",
            "dream",
            "intentions",
            "mien",
            "mcp",
            "p",
            "skills",
            "skill",
            "wf",
            "run",
            "runs",
            "resume",
            "schedules",
            "approvals",
            "policies",
            "quiet",
            "status",
            "doctor",
            "config",
            "logs",
            "restart",
            "upgrade",
            "secret",
            "help",
        ] {
            assert!(
                names.contains(&expected),
                "commande manquante : /{expected}"
            );
        }
    }

    #[test]
    fn no_duplicate_commands() {
        let mut v: Vec<&str> = all().iter().map(|c| c.name).collect();
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n);
    }

    /// CA 15 : toute action Telegram a un équivalent CLI.
    #[test]
    fn ca_15_1_every_command_maps_to_an_rpc_method() {
        for c in all() {
            assert!(
                penelope_kernel::api::method::ALL.contains(&c.rpc),
                "/{} pointe vers une méthode RPC inconnue : {}",
                c.name,
                c.rpc
            );
        }
    }

    #[test]
    fn bot_commands_payload_is_valid() {
        let v = to_bot_commands();
        let a = v.as_array().unwrap();
        assert_eq!(a.len(), all().len());
        for c in a {
            let name = c["command"].as_str().unwrap();
            assert!(name.len() <= 32, "nom trop long pour Telegram : {name}");
            assert!(
                name.chars()
                    .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_'),
                "nom invalide : {name}"
            );
            let d = c["description"].as_str().unwrap();
            assert!((1..=256).contains(&d.chars().count()), "description : {d}");
        }
    }

    #[test]
    fn help_groups_by_category_with_examples() {
        let h = help_text();
        assert!(h.contains("### Session"));
        assert!(h.contains("### Mémoire"));
        assert!(h.contains("/doctor"));
        for c in all() {
            assert!(h.contains(c.example), "exemple manquant pour /{}", c.name);
        }
    }

    #[test]
    fn lookup_by_name() {
        assert_eq!(find("doctor").unwrap().category, "Système");
        assert!(find("inexistante").is_none());
    }
}
