//! Motifs des règles « Toujours » dérivés d'un appel.

use super::*;

/// Vrai quand un « Toujours » sur cet appel n'écrira aucune règle : une commande composée
/// n'a pas de famille, et une règle sur `shell_exec` entier n'existe pas (issue #111). La
/// carte et le CLI le disent **avant** le clic, plutôt que de laisser croire au contraire
/// (issue #141).
pub fn always_creates_no_rule(subject: &str, args: Option<&Value>) -> bool {
    args.is_some() && subject == "shell_exec" && arg_patterns(subject, args).is_empty()
}

/// Nombre de familles qu'un seul clic peut autoriser. Au-delà, la carte ne peut plus les
/// nommer toutes d'un coup de lecture : le propriétaire autoriserait sans savoir quoi.
pub const MAX_FAMILIES_PER_CLICK: usize = 3;

/// Motifs de règles d'un appel : un par famille (issue #150). Une ligne simple en rend un,
/// une liste `a && b && c` en rend un par famille distincte, et une ligne composée aucun.
///
/// Les étapes de lecture pure (`ls -la tmp/x*`) n'en demandent pas : elles passent déjà
/// sans carte (#111), et une règle sur `ls` ne borne rien.
pub fn arg_patterns(tool: &str, args: Option<&Value>) -> Vec<Value> {
    use penelope_hitl::policy::CMD_PREFIX_OP;
    if tool != "shell_exec" {
        return arg_pattern(tool, args).into_iter().collect();
    }
    let Some(args) = args else {
        return Vec::new();
    };
    let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    // `&&` seulement : `;`, `||`, une substitution ou une redirection laissent la ligne
    // composée, donc sans famille — c'est le choix de #67, inchangé.
    let Some(list) = penelope_hitl::cmdline::list(command) else {
        return Vec::new();
    };
    let network = crate::executor::wants_network(tool, args);
    let mut out: Vec<Value> = Vec::new();
    for step in &list.steps {
        // Une lecture, ou un `cd` qui prépare la suite, n'a besoin d'aucune règle.
        if penelope_hitl::cmdline::needs_no_rule(step) {
            continue;
        }
        // Une étape sans famille lisible rend la ligne entière incouvrable : mieux vaut
        // aucune règle qu'une règle qui n'en couvre qu'une partie.
        let Some(head) = family_of(step) else {
            return Vec::new();
        };
        let pattern = if network {
            json!({"command": {CMD_PREFIX_OP: head}, "network": true})
        } else {
            json!({"command": {CMD_PREFIX_OP: head}})
        };
        if !out.contains(&pattern) {
            out.push(pattern);
        }
    }
    // Trois familles d'un clic au plus : la carte doit pouvoir les nommer toutes.
    if out.len() > MAX_FAMILIES_PER_CLICK {
        return Vec::new();
    }
    out
}

/// Famille d'une étape : son programme, ou ses deux premiers mots pour les commandes qui
/// portent une sous-commande. `None` quand elle ne se relit pas telle qu'écrite.
fn family_of(step: &penelope_hitl::cmdline::Pipeline) -> Option<String> {
    const TWO_WORDS: &[&str] = &[
        "cargo", "git", "gh", "npm", "pnpm", "yarn", "make", "docker", "kubectl", "brew",
        "python3", "uv", "go",
    ];
    let head_words: Vec<String> = match step.head.words.as_slice() {
        [first, second, ..] if TWO_WORDS.contains(&first.as_str()) => {
            vec![first.clone(), second.clone()]
        }
        [first, ..] => vec![first.clone()],
        [] => return None,
    };
    let head = head_words.join(" ");
    // La famille doit se relire comme elle a été écrite, sinon la règle créée ne
    // couvrirait jamais la commande dont elle vient (régression de #111) : un mot qui
    // porte une espace ou un guillemet ne fait pas une famille.
    if penelope_hitl::cmdline::family(&head).is_none_or(|w| w != head_words) {
        return None;
    }
    Some(head)
}

/// Motif d'arguments d'une règle « toujours », dérivé de l'appel : ce qui borne
/// l'autorisation à ce que le propriétaire a vraiment vu (issue #67). `None` : la règle
/// couvre l'outil (outils MCP, outils sans argument significatif).
pub fn arg_pattern(tool: &str, args: Option<&Value>) -> Option<Value> {
    use penelope_hitl::policy::{CMD_PREFIX_OP, ORIGIN_OP, PATH_PREFIX_OP};
    let args = args?;
    let str_of = |k: &str| args.get(k).and_then(|v| v.as_str()).map(String::from);
    let prefix = |k: &str, op: &str, v: String| Some(json!({k: {op: v}}));
    match tool {
        // Famille de commandes : `cargo test …`, `git log …`, `ls …`.
        "shell_exec" => {
            let command = str_of("command")?;
            // Une commande composée (`cd /x && ls`) n'a pas de famille : une règle sur
            // `cd` ne s'appliquerait jamais (issue #111). Pas de motif, donc pas de règle.
            // Le découpage est celui de `cmdline` : un `&` entre guillemets (URL de
            // requête) n'enchaîne rien, `VAR=x cmd` a pour famille `cmd`, et un tube vers
            // une lecture pure (`… | jq`) celle de sa première étape (issue #141).
            let line = penelope_hitl::cmdline::pipeline(&command)?;
            let network = crate::executor::wants_network(tool, args);
            let head = family_of(&line)?;
            // Le réseau accordé l'est à la famille de commandes, jamais au shell (#106) :
            // « Toujours » sur `git push` avec réseau ne donne rien à `curl`.
            if network {
                return Some(json!({"command": {CMD_PREFIX_OP: head}, "network": true}));
            }
            prefix("command", CMD_PREFIX_OP, head)
        }
        // Répertoire du fichier : un « toujours » sur `src/a.rs` vaut pour `src/`.
        "fs_write" | "fs_edit" => {
            let path = str_of("path")?;
            let dir = match path.rfind('/') {
                Some(i) => path[..=i].to_string(),
                None => String::new(),
            };
            prefix("path", PATH_PREFIX_OP, dir)
        }
        "git_push" => {
            let mut m = serde_json::Map::new();
            for k in ["remote", "branch"] {
                if let Some(v) = str_of(k) {
                    m.insert(k.into(), json!(v));
                }
            }
            (!m.is_empty()).then(|| Value::Object(m))
        }
        "git_clone" => {
            let source = str_of("url")?;
            let url = penelope_tools::git::normalize_clone_url(&source).ok()?;
            if url.starts_with("file://") {
                return None;
            }
            let origin = if let Some((scheme, rest)) = url.split_once("://") {
                let host = rest.split('/').next()?.to_ascii_lowercase();
                format!("{}://{host}", scheme.to_ascii_lowercase())
            } else {
                let (user_host, _) = url.split_once(':')?;
                let (_, host) = user_host.split_once('@')?;
                format!("ssh://{}", host.to_ascii_lowercase())
            };
            prefix("url", ORIGIN_OP, origin)
        }
        // Hôte visé, schéma compris.
        "http_fetch" => {
            let url = str_of("url")?;
            let host = url
                .split_once("://")
                .map(|(scheme, rest)| {
                    format!("{scheme}://{}", rest.split('/').next().unwrap_or_default())
                })
                .unwrap_or(url);
            prefix("url", ORIGIN_OP, host)
        }
        "config_set" => str_of("path").map(|p| json!({"path": p})),
        _ => None,
    }
}
