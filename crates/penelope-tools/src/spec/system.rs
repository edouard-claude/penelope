//! Catalogue : fichiers, shell, git, réseau et temps.

use super::*;

/// Section « fichiers » du catalogue.
pub(super) fn files() -> Vec<ToolSpec> {
    vec![
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
    ]
}

/// Section « shell » du catalogue.
pub(super) fn shell() -> Vec<ToolSpec> {
    vec![spec(
        "shell_exec",
        RiskClass::Write,
        "Exécute une commande sous bac à sable, avec délai. Une suite de tests (cargo, go, \
             npm, pytest, make test) ou une commande en échec à longue sortie rend un résumé et \
             les échecs seulement, la sortie complète en artefact (`artifact_read`) ; \
             `output: \"full\"` rend la sortie brute. Le réseau est coupé sauf `network: true` \
             (git push/pull/clone, gh, installation de paquets, curl) : l'approbation le dit. \
             Répertoire de travail : `cwd` (dans un workspace), pas de préfixe `cd … &&`. \
             **Une commande par appel** ; plusieurs commandes = plusieurs appels, en \
             parallèle si indépendants. `&&` seulement entre commandes de même nature : \
             `;`, `||`, `$(…)` et redirections empêchent toute règle, donc redemandent à \
             chaque appel. \
             Ne recopie jamais un secret lu (clé, mot de passe) dans la commande : lis-le \
             dans son fichier ou une variable d'environnement. \
             `background: true` (tests, build, installation) : rend la main tout de suite, \
             le résultat revient seul dans la conversation.",
        obj(
            json!({
                "command": {"type":"string"},
                "cwd": {"type":"string"},
                "timeout_ms": {"type":"integer","minimum":1000,"maximum":3600000},
                "output": {"type":"string","enum":["digest","full"]},
                "network": {"type":"boolean"},
                "background": {"type":"boolean"}
            }),
            &["command"],
        ),
        false,
        false,
        false,
    )]
}

/// Section « git » du catalogue.
pub(super) fn git() -> Vec<ToolSpec> {
    vec![
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
            "Clone un dépôt distant dans le workspace, ou retrouve un clone existant de la même origine. `owner/repo` désigne GitHub ; un chemin local est refusé.",
            obj(
                json!({"url": {"type":"string", "description":"URL Git (https://github.com/owner/repo.git, git@github.com:owner/repo.git) ou raccourci owner/repo ; pas de chemin local", "examples":["https://github.com/owner/repo.git", "git@github.com:owner/repo.git", "owner/repo"]}, "dest": {"type":"string"}, "depth": {"type":"integer"}}),
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
    ]
}

/// Section « réseau » du catalogue.
pub(super) fn network() -> Vec<ToolSpec> {
    vec![spec(
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
    )]
}

/// Section « temps » du catalogue.
pub(super) fn time() -> Vec<ToolSpec> {
    vec![
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
             (`path`), `event` (`event`). Le retour arrive dans ce chat, chaque exécution \
             d'un prompt dans sa propre session (`label` dans target pour la nommer) ; une \
             planification identique déjà active est signalée (`doublons`). Un prompt peut \
             déclarer dans target son `livrable` (`message`, `fichier:<chemin>`, `run`) : sans \
             lui, l'exécution compte comme un échec ; et son `etat` (chemin du fichier « déjà \
             vu »), remis tel qu'avant si rien n'est livré.",
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
            "Liste les déclencheurs planifiés, avec la conversation où chacun livre \
             (`destination`).",
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
        spec(
            "schedule_move",
            RiskClass::Write,
            "Change où livre un déclencheur planifié, sans le recréer (historique gardé) : \
             `to: \"here\"` vers cette conversation (ce sujet compris), `\"private\"` vers \
             la conversation privée du propriétaire. `schedule_list` donne la destination \
             actuelle de chacun.",
            obj(
                json!({
                    "id": {"type":"string"},
                    "to": {"type":"string","enum":["here","private"]}
                }),
                &["id", "to"],
            ),
            true,
            false,
            false,
        ),
    ]
}
