//! Ce que Pénélope sait de sa machine (issue #156).
//!
//! Le 20/09, le propriétaire colle un lien GitHub : deux `http_fetch` refusés sur
//! `api.github.com`, puis un `curl … | python3` qui casse sur un `===` interprété par
//! zsh, avant qu'il ne demande « pourquoi tu n'utilises pas `gh` ? ». `gh` était installé
//! et connecté. Rien, dans le message système, ne le disait au modèle : ni la machine, ni
//! les binaires, ni leur état de connexion. Chaque nouveau sujet repartait de zéro.
//!
//! Une passe détecte les binaires par `which`, leur version par une commande courte, et
//! pour les forges leur **état de connexion**. Le résultat vit dans `kv`
//! ([`KV_KEY`]), sort par `self_status` et `doctor`, et tient en **une ligne** dans le
//! message système.
//!
//! Cette ligne est en T1 : elle doit être identique octet pour octet d'un tour à l'autre,
//! sinon le cache de préfixe tombe à chaque tour (#104, décision 0008). D'où la règle la
//! plus importante de ce module : **ni version, ni date, ni chemin dans la ligne** — un
//! `brew upgrade gh` ne doit rien changer au prompt. Les versions restent dans
//! `self_status`, qui n'est lu que sur demande.

use crate::runtime::Services;
use penelope_kernel::config::Config;
use penelope_platform::host::HostStatus;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Clé de l'inventaire dans le magasin clé/valeur.
pub const KV_KEY: &str = "machine.inventory";

/// Délai d'une sonde. `gh auth status` part sur le réseau et `docker version` réveille un
/// démon : trois secondes, puis on considère l'outil injoignable. Ces sondes tournent au
/// démarrage, toutes les heures et dans `doctor`, jamais dans un tour de conversation.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Binaires cherchés sur toutes les machines, dans l'ordre où ils sortiront dans la ligne
/// système. L'ordre est figé : c'est lui qui rend la ligne stable d'un tour à l'autre.
pub const KNOWN: &[&str] = &[
    "gh", "glab", "git", "docker", "yt-dlp", "ffmpeg", "brew", "node", "npx", "python3", "uvx",
    "go", "cargo", "jq", "rg", "make", "ssh",
];

/// Réflexes de routage : ce qu'il faut lancer au lieu de `curl` ou `http_fetch`. La règle
/// n'est émise que si son binaire est **présent**, et pour une forge, **connecté** : dire
/// « utilise `glab` » sur une machine sans `glab` envoie le modèle dans le mur.
const REFLEX: &[(&str, &str)] = &[
    ("gh", "GitHub (github.com, api.github.com) → `gh api …`"),
    ("glab", "GitLab → `glab`"),
    ("yt-dlp", "YouTube et vidéos → `yt-dlp`"),
    ("docker", "images et conteneurs → `docker`"),
];

/// Les binaires dont l'absence de connexion se voit : sans elle, la règle de réflexe
/// n'est pas émise, même si le binaire est là.
const NEEDS_LOGIN: &[&str] = &["gh", "glab"];

/// Un binaire trouvé sur la machine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    /// Version rendue par le binaire. Pour `self_status` et `doctor` seulement : elle
    /// change à chaque mise à jour et n'entre jamais dans le message système.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// À quoi ce client est connecté : le compte pour `gh`, l'hôte pour `glab`,
    /// `joignable` pour `docker`. `None` : installé, pas connecté.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// L'état de la machine tel que le modèle peut l'apprendre.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    pub os: String,
    pub arch: String,
    /// Profil de bac à sable effectif (`sandbox.default_profile`).
    pub sandbox_profile: String,
    /// Le shell part-il avec le réseau par défaut (`sandbox.shell_network`) ?
    pub shell_network: bool,
    pub present: Vec<Tool>,
    /// Binaires connus cherchés et non trouvés.
    pub missing: Vec<String>,
    /// Horodatage de la passe. Hors de la ligne système, par construction.
    pub checked_at: String,
}

impl Inventory {
    /// Le binaire nommé, s'il est là.
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.present.iter().find(|t| t.name == name)
    }

    /// Présent **et** connecté : le seul état qui autorise une règle de réflexe vers une
    /// forge.
    pub fn connected(&self, name: &str) -> bool {
        self.tool(name).is_some_and(|t| t.account.is_some())
    }

    /// La ligne T1, et les réflexes qu'elle porte.
    ///
    /// Stable par construction : que des noms et des états de connexion. Deux inventaires
    /// égaux rendent la même chaîne ; une version qui monte ne la touche pas.
    pub fn prompt_line(&self) -> String {
        let net = if self.shell_network {
            "réseau ouvert"
        } else {
            "réseau accordé par appel"
        };
        let mut line = format!(
            "Machine : {} {}, bac à sable `{}`, {net}",
            self.os, self.arch, self.sandbox_profile
        );

        let (linked, plain): (Vec<&Tool>, Vec<&Tool>) =
            self.present.iter().partition(|t| t.account.is_some());
        if !linked.is_empty() {
            let list = linked
                .iter()
                .map(|t| format!("{} ({})", t.name, t.account.as_deref().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ");
            line.push_str(&format!(" · installés et connectés : {list}"));
        }
        if !plain.is_empty() {
            let list = plain
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            line.push_str(&format!(" · installés : {list}"));
        }
        line.push_str(&format!(
            " · absents : {}",
            if self.missing.is_empty() {
                "—".to_string()
            } else {
                self.missing.join(", ")
            }
        ));

        let reflexes: Vec<&str> = REFLEX
            .iter()
            .filter(|(bin, _)| {
                if NEEDS_LOGIN.contains(bin) {
                    self.connected(bin)
                } else {
                    self.tool(bin).is_some()
                }
            })
            .map(|(_, rule)| *rule)
            .collect();
        if !reflexes.is_empty() {
            line.push_str(&format!(
                "\nLance la commande installée plutôt que le réseau brut : {}. Jamais \
                 `curl` ni `http_fetch` vers une forge dont le client est connecté : il \
                 porte déjà l'authentification, la pagination et le format.",
                reflexes.join(" ; ")
            ));
        }
        line
    }
}

/// Détecte l'état de la machine. Bloquant (un `which` par binaire, quelques sondes) : à
/// appeler sous `spawn_blocking`.
pub fn detect(cfg: &Config, host: &HostStatus) -> Inventory {
    let mut wanted: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
    // Les ajouts du propriétaire viennent après les connus, triés : deux configurations
    // identiques donnent le même ordre, donc la même ligne.
    let mut extra: Vec<String> = cfg
        .tools
        .inventory_extra
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !wanted.contains(s))
        .collect();
    extra.sort();
    extra.dedup();
    wanted.extend(extra);

    let mut present = Vec::new();
    let mut missing = Vec::new();
    for name in &wanted {
        let Some(path) = penelope_platform::process::which(name) else {
            missing.push(name.clone());
            continue;
        };
        present.push(Tool {
            name: name.clone(),
            version: penelope_platform::process::probe_version(&path, PROBE_TIMEOUT)
                .map(|v| penelope_observe::redact::redact(&v)),
            account: account_of(name, &path),
        });
    }

    Inventory {
        os: host.os.clone().unwrap_or_else(|| "OS inconnu".into()),
        arch: host.arch.clone().unwrap_or_else(|| "arch inconnue".into()),
        sandbox_profile: cfg.sandbox.default_profile.clone(),
        shell_network: cfg.sandbox.shell_network,
        present,
        missing,
        checked_at: String::new(),
    }
}

/// À quoi ce binaire est connecté, quand la question a un sens.
///
/// Jamais `--show-token` : ces commandes savent afficher un jeton, et la sortie d'une
/// sonde finit dans `kv`, dans `self_status` et dans le message système. Ce qui est gardé
/// passe quand même par le rédacteur.
fn account_of(name: &str, path: &std::path::Path) -> Option<String> {
    use penelope_platform::process::probe_command;
    let raw = match name {
        "gh" => {
            let p = probe_command(path, &["auth", "status"], PROBE_TIMEOUT)?;
            forge_login(p.ok, p.text(), Field::Account)?
        }
        "glab" => {
            let p = probe_command(path, &["auth", "status"], PROBE_TIMEOUT)?;
            forge_login(p.ok, p.text(), Field::Host)?
        }
        // Installé ne veut pas dire que le démon tourne : sans lui, `docker run` échoue.
        "docker" => {
            let p = probe_command(
                path,
                &["version", "--format", "{{.Server.Version}}"],
                PROBE_TIMEOUT,
            )?;
            p.ok.then(|| "joignable".to_string())?
        }
        _ => return None,
    };
    let clean = penelope_observe::redact::redact(raw.trim());
    (!clean.is_empty()).then_some(clean)
}

/// Une forge peut renvoyer un code d'échec global parce qu'un autre hôte est
/// déconnecté ; une ligne de connexion positive reste valable pour cet hôte.
fn forge_login(status_ok: bool, text: &str, want: Field) -> Option<String> {
    if !status_ok && !text.contains("✓ Logged in to") {
        return None;
    }
    let login = login_in(text, want);
    (!login.is_empty()).then_some(login)
}

/// Ce qu'on retient d'une ligne « Logged in to … » : le compte, ou l'hôte.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Account,
    Host,
}

/// Lit l'hôte ou le compte dans la sortie de `gh auth status` / `glab auth status`.
///
/// Les deux formats vus en production :
/// `✓ Logged in to github.com as edouard-claude (keyring)` (ancien) et
/// `✓ Logged in to github.com account edouard-claude (keyring)` (récent).
fn login_in(text: &str, want: Field) -> String {
    for line in text.lines() {
        if !line.contains("Logged in to") {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(to) = words.iter().position(|w| *w == "to") else {
            continue;
        };
        if to == 0
            || words
                .get(to - 1)
                .is_none_or(|w| !w.eq_ignore_ascii_case("in"))
        {
            continue;
        }
        let Some(host) = words.get(to + 1) else {
            continue;
        };
        if want == Field::Host {
            return host.trim_end_matches(':').to_string();
        }
        // Le compte suit `as` ou `account`, selon la version du client.
        if let Some(k) = words
            .iter()
            .position(|w| *w == "as" || *w == "account")
            .filter(|k| *k > to)
            && let Some(login) = words.get(k + 1)
        {
            return login
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                .to_string();
        }
    }
    String::new()
}

/// Relance la détection et range le résultat dans `kv`.
pub async fn refresh(s: &Services) -> anyhow::Result<Inventory> {
    let cfg = s.config.config();
    let platform = s.platform.clone();
    let now = s.clock.now_ms() / 1000;
    let mut inv = tokio::task::spawn_blocking(move || {
        let host = platform.host_status(now);
        detect(&cfg, &host)
    })
    .await?;
    inv.checked_at = s.clock.now_rfc3339();
    s.kv_set(KV_KEY, &serde_json::to_string(&inv)?).await?;
    Ok(inv)
}

/// Le dernier inventaire connu, sans rien sonder. C'est celui que lit le message système :
/// un tour de conversation ne lance pas de processus.
pub async fn cached(s: &Services) -> Option<Inventory> {
    let raw = s.kv_get(KV_KEY).await.ok()??;
    serde_json::from_str(&raw).ok()
}

/// Hôtes desservis par `gh`, quel que soit le compte.
const GITHUB_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "gist.github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "codeload.github.com",
];

/// Remarque à joindre à un `http_fetch` qui vise une forge dont le client est installé
/// **et connecté** (issue #156).
///
/// Elle ne bloque rien : `http_fetch` marche toujours, et il reste le bon outil pour une
/// page publique. Mais sur `api.github.com` sans jeton, il rend un 403 sans User-Agent,
/// ou une page tronquée ; `gh api` porte l'authentification, la pagination et le format.
/// C'est la boucle du 20/09, dite une fois au lieu d'être réapprise à chaque sujet.
pub fn forge_hint(inv: &Inventory, url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_lowercase();
    let host = host.trim_start_matches("www.").to_string();

    if GITHUB_HOSTS.contains(&host.as_str()) && inv.connected("gh") {
        let who = inv.tool("gh")?.account.clone().unwrap_or_default();
        return Some(format!(
            "`gh` est installé et connecté ({who}) : préfère `gh api …` (ou `gh repo view`, \
             `gh pr view`) pour {host}. Il porte l'authentification, la pagination et le \
             format JSON ; `http_fetch` sur l'API rend un refus ou une page tronquée."
        ));
    }

    // GitLab : l'hôte connu est celui auquel `glab` est connecté, pas une liste en dur —
    // l'instance du propriétaire n'est pas `gitlab.com`.
    if inv.connected("glab") {
        let known = inv.tool("glab")?.account.clone().unwrap_or_default();
        if host == known.to_lowercase() || host == "gitlab.com" {
            return Some(format!(
                "`glab` est installé et connecté ({known}) : préfère `glab` (par exemple \
                 `glab api`, `glab mr view`) pour {host}, plutôt que `http_fetch`."
            ));
        }
    }
    None
}

/// Les binaires manquants parmi les dépendances déclarées par une skill (issue #156).
///
/// Seulement les `bin:` : ils se vérifient par une recherche dans le PATH, sans lancer de
/// processus, donc sans coût dans un tour de conversation. Les `pip:` et `npm:` demandent
/// une sonde par interpréteur — c'est le travail de `doctor` (#146), pas celui d'une
/// recherche de skill.
pub fn missing_binaries(requires: &[String]) -> Vec<String> {
    let mut out: Vec<String> = requires
        .iter()
        .filter_map(|r| r.strip_prefix("bin:"))
        .map(str::trim)
        .filter(|n| !n.is_empty() && penelope_platform::process::which(n).is_none())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tool(name: &str, version: Option<&str>, account: Option<&str>) -> Tool {
        Tool {
            name: name.into(),
            version: version.map(str::to_string),
            account: account.map(str::to_string),
        }
    }

    /// Une machine du propriétaire : `gh` connecté, `glab` absent.
    fn inventory() -> Inventory {
        Inventory {
            os: "macOS 27".into(),
            arch: "arm64".into(),
            sandbox_profile: "workspace-write".into(),
            shell_network: false,
            present: vec![
                tool("gh", Some("gh version 2.62.0"), Some("edouard-claude")),
                tool("git", Some("git version 2.54.0"), None),
                tool("docker", Some("Docker version 27.3.1"), Some("joignable")),
            ],
            missing: vec!["glab".into(), "yt-dlp".into()],
            checked_at: "2026-09-21T09:00:00+04:00".into(),
        }
    }

    /// #156 : la ligne dit ce qui est connecté, ce qui est installé, ce qui manque ; et
    /// ne propose un réflexe que pour ce qui est utilisable.
    #[test]
    fn the_line_names_what_is_connected_and_routes_only_to_that() {
        let line = inventory().prompt_line();
        assert!(line.contains("gh (edouard-claude)"), "{line}");
        assert!(line.contains("installés : git"), "{line}");
        assert!(line.contains("absents : glab, yt-dlp"), "{line}");
        // `gh` est connecté : la règle part. `glab` est absent, `yt-dlp` aussi : aucune
        // règle vers eux, sans quoi le modèle lance une commande qui n'existe pas.
        assert!(line.contains("gh api"), "{line}");
        assert!(!line.contains("glab`"), "{line}");
        assert!(!line.contains("yt-dlp`"), "{line}");
        // Docker est là et joignable : sa règle part aussi.
        assert!(line.contains("docker"), "{line}");
    }

    /// #104 et décision 0008 : la ligne est en T1, donc dans le préfixe mis en cache. Une
    /// version qui monte ne doit pas la toucher ; une déconnexion, si.
    #[test]
    fn the_line_survives_an_upgrade_but_not_a_logout() {
        let base = inventory().prompt_line();
        assert_eq!(base, inventory().prompt_line(), "deux passes identiques");

        let mut upgraded = inventory();
        upgraded.present[0].version = Some("gh version 2.63.0".into());
        upgraded.checked_at = "2026-09-21T10:00:00+04:00".into();
        assert_eq!(
            base,
            upgraded.prompt_line(),
            "une mise à jour de `gh` ne casse pas le cache de préfixe"
        );

        let mut logged_out = inventory();
        logged_out.present[0].account = None;
        let after = logged_out.prompt_line();
        assert_ne!(base, after, "une déconnexion change la ligne");
        assert!(
            !after.contains("gh api"),
            "plus de réflexe vers `gh` : {after}"
        );
        assert!(after.contains("installés : gh, git"), "{after}");
    }

    /// Le profil de bac à sable et le réseau du shell sont dits : ils décident de ce qui
    /// passera sans demande (#106).
    #[test]
    fn the_line_says_the_sandbox_and_the_network() {
        let line = inventory().prompt_line();
        assert!(line.contains("bac à sable `workspace-write`"), "{line}");
        assert!(line.contains("réseau accordé par appel"), "{line}");
        let mut open = inventory();
        open.shell_network = true;
        assert!(open.prompt_line().contains("réseau ouvert"));
    }

    /// Les deux formats de `gh auth status` vus en production, et celui de `glab`.
    #[test]
    fn a_login_line_is_read_in_both_formats() {
        let recent = "github.com\n  ✓ Logged in to github.com account edouard-claude (keyring)";
        assert_eq!(login_in(recent, Field::Account), "edouard-claude");
        let older = "github.com\n  ✓ Logged in to github.com as edouard-claude (oauth_token)";
        assert_eq!(login_in(older, Field::Account), "edouard-claude");
        // Pour GitLab, c'est l'instance qui compte : celle du propriétaire n'est pas
        // `gitlab.com`.
        let glab = "gitlab.apnl.tech\n  ✓ Logged in to gitlab.apnl.tech as edouard (keyring)";
        assert_eq!(login_in(glab, Field::Host), "gitlab.apnl.tech");
        // Déconnecté : rien à retenir, et surtout pas une ligne au hasard.
        assert_eq!(
            login_in("You are not logged into any hosts", Field::Account),
            ""
        );
    }

    /// #182 : `glab auth status` sort en échec si gitlab.com est déconnecté, même quand
    /// l'instance privée du propriétaire est authentifiée.
    #[test]
    fn a_connected_gitlab_host_survives_another_hosts_failure() {
        let mixed = "gitlab.com\n  x gitlab.com: API call failed: 401\n\
                     gitlab.apnl.tech\n  ✓ Logged in to gitlab.apnl.tech as edouard (keyring)\n\
                     X could not authenticate to one or more configured instances";
        assert_eq!(
            forge_login(false, mixed, Field::Host).as_deref(),
            Some("gitlab.apnl.tech")
        );
        assert_eq!(
            forge_login(false, "not logged in to gitlab.com", Field::Host),
            None
        );
    }

    /// #156 : `http_fetch` sur une forge dont le client est connecté porte une remarque ;
    /// ailleurs, et sans client connecté, aucune.
    #[test]
    fn a_fetch_to_a_connected_forge_is_remarked() {
        let inv = inventory();
        let hint =
            forge_hint(&inv, "https://api.github.com/repos/edouard-claude/penelope").unwrap();
        assert!(hint.contains("gh api"), "{hint}");
        assert!(hint.contains("edouard-claude"), "{hint}");
        assert!(forge_hint(&inv, "https://example.com/page").is_none());

        // Sans `gh` connecté, pas de remarque : elle enverrait vers une commande qui
        // rendra « not logged in ».
        let mut logged_out = inventory();
        logged_out.present[0].account = None;
        assert!(forge_hint(&logged_out, "https://api.github.com/x").is_none());

        // GitLab : l'hôte reconnu est celui de la connexion, pas une liste en dur.
        let mut with_glab = inventory();
        with_glab
            .present
            .push(tool("glab", None, Some("gitlab.apnl.tech")));
        let h = forge_hint(&with_glab, "https://gitlab.apnl.tech/groupe/projet").unwrap();
        assert!(h.contains("glab"), "{h}");
        assert!(forge_hint(&with_glab, "https://gitlab.autre.tech/x").is_none());
    }

    /// Les dépendances `bin:` d'une skill sont vérifiées sans lancer de processus ; les
    /// `pip:` et `npm:` restent à `doctor` (#146).
    #[test]
    fn only_binary_requirements_are_checked_here() {
        let reqs = vec![
            "bin:yt-dlp-qui-n-existe-pas".to_string(),
            "pip:openpyxl".to_string(),
            "npm:docx".to_string(),
            "bin:sh".to_string(),
        ];
        let missing = missing_binaries(&reqs);
        assert_eq!(missing, vec!["yt-dlp-qui-n-existe-pas".to_string()]);
    }

    /// L'inventaire fait un aller-retour par `kv` : c'est lui que lit le message système,
    /// sans rien sonder dans le tour.
    #[tokio::test]
    async fn the_inventory_is_stored_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::TestClock::default());
        let s = crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        assert!(cached(&s).await.is_none(), "rien avant la première passe");

        let inv = refresh(&s).await.unwrap();
        assert!(!inv.checked_at.is_empty(), "la passe est horodatée");
        let back = cached(&s).await.unwrap();
        assert_eq!(back, inv);
        // `sh` existe sur toute machine qui fait tourner la suite : la détection marche.
        assert!(
            back.tool("git").is_some() || !back.missing.is_empty(),
            "la détection a bien tourné : {back:?}"
        );
        // La ligne ne porte jamais l'horodatage, sinon le préfixe change chaque heure.
        assert!(!back.prompt_line().contains(&back.checked_at));
    }
}
