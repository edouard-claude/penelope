//! Contrôles de la machine : outils, skills, dossier, binaire, installation, mises à jour.

use super::*;

/// #156 : les binaires de la machine, et pour les forges leur état de connexion.
///
/// Le 20/09, `gh` était installé et connecté pendant que Pénélope faisait des `http_fetch`
/// refusés sur `api.github.com` : elle ne savait pas qu'il existait. Deux contrôles — ce
/// qui est là, ce qui manque — plus une ligne par forge non connectée, puisqu'un `gh`
/// installé mais déconnecté ne sert à rien et n'émet aucun réflexe.
pub async fn machine_checks(s: &Services) -> Vec<DoctorCheck> {
    const ID: &str = "machine.inventory";
    const LABEL: &str = "Binaires de la machine";
    let inv = match crate::machine::refresh(s).await {
        Ok(inv) => inv,
        Err(e) => {
            return vec![DoctorCheck::fail(
                ID,
                LABEL,
                format!("inventaire impossible : {e}"),
                None,
            )];
        }
    };

    let mut out = Vec::new();
    let present: Vec<String> = inv
        .present
        .iter()
        .map(|t| match (&t.account, &t.version) {
            (Some(a), _) => format!("{} ({a})", t.name),
            (None, Some(v)) => format!("{} ({v})", t.name),
            (None, None) => t.name.clone(),
        })
        .collect();
    out.push(DoctorCheck::ok(
        ID,
        LABEL,
        if present.is_empty() {
            "aucun binaire connu trouvé dans le PATH".to_string()
        } else {
            format!("{} présent(s) : {}", present.len(), present.join(", "))
        },
    ));

    if !inv.missing.is_empty() {
        out.push(DoctorCheck::fail(
            "machine.missing",
            "Binaires attendus absents",
            format!(
                "{} absent(s) : {}",
                inv.missing.len(),
                inv.missing.join(", ")
            ),
            Some(brew_install_command(&inv.missing)),
        ));
    }

    // Une forge installée mais déconnectée : le réflexe n'est pas émis, le modèle
    // repartira sur `http_fetch`. C'est exactement l'incident de #156.
    for bin in ["gh", "glab"] {
        if inv.tool(bin).is_some() && !inv.connected(bin) {
            out.push(DoctorCheck::fail(
                &format!("machine.{bin}"),
                &format!("Connexion `{bin}`"),
                format!(
                    "`{bin}` est installé mais non connecté : aucune règle de routage ne \
                     sera donnée au modèle, qui repartira sur `http_fetch`"
                ),
                Some(format!("{bin} auth login")),
            ));
        }
    }
    out
}

/// Le nom d'un exécutable n'est pas toujours celui de sa formule Homebrew.
pub(super) fn brew_install_command(missing: &[String]) -> String {
    let formulas = missing
        .iter()
        .map(|name| {
            if name == "rg" {
                "ripgrep"
            } else {
                name.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("brew install {formulas}")
}

/// #146 : une skill importée déclare ses dépendances (`requires: [pip:…, npm:…, bin:…]`).
/// Elles ne sont jamais installées : `doctor` dit ce qui manque et avec quoi le poser.
pub async fn skill_requirements_check(s: &Services) -> DoctorCheck {
    const ID: &str = "skills.requirements";
    const LABEL: &str = "Dépendances des skills";
    let declared: Vec<String> = s
        .skills
        .all()
        .into_iter()
        .flat_map(|k| k.requires)
        .collect();
    if declared.is_empty() {
        return DoctorCheck::ok(ID, LABEL, "aucune skill n'en déclare");
    }
    let missing = crate::skill_deps::missing_for(&declared).await;
    if missing.is_empty() {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("{} déclarée(s), toutes présentes", declared.len()),
        );
    }
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} manquante(s) : {}",
            missing.len(),
            missing
                .iter()
                .map(|m| m.requirement.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(
            missing
                .iter()
                .map(|m| m.how.clone())
                .collect::<Vec<_>>()
                .join(" ; "),
        ),
    )
}

/// #143 : le chat privé n'est plus lu dès que la conversation vit dans un groupe à
/// sujets. Sans `telegram.home`, tout ce qui n'a pas de session y retombe pourtant.
pub fn home_check(s: &Services) -> DoctorCheck {
    const ID: &str = "telegram.home";
    const LABEL: &str = "Foyer des avis sans session";
    let cfg = s.config.config();
    match cfg.telegram.home.resolved() {
        Some((chat, topic)) => DoctorCheck::ok(
            ID,
            LABEL,
            match topic {
                Some(t) => format!("chat {chat}, sujet {t}"),
                None => format!("chat {chat}"),
            },
        ),
        // Des groupes autorisés mais pas de foyer : les avis partent en privé, que le
        // propriétaire ne lit plus.
        None if !cfg.telegram.allowed_chats.is_empty() => DoctorCheck::fail(
            ID,
            LABEL,
            "aucun foyer réglé alors que des groupes sont autorisés : les alertes de \
             budget, rappels, digest et cartes MCP sans session partent dans le chat privé",
            Some("dans le sujet voulu : /home".into()),
        ),
        None => DoctorCheck::ok(ID, LABEL, "chat privé du propriétaire"),
    }
}

/// Signature du binaire en cours (issue #28) : en ad hoc, macOS redemande l'accès au
/// Trousseau après chaque build.
pub fn binary_signature_check(s: &Services) -> DoctorCheck {
    use penelope_platform::codesign::{Signature, inspect};
    const ID: &str = "binary_signature";
    const LABEL: &str = "Signature du binaire";
    let Ok(exe) = std::env::current_exe() else {
        return DoctorCheck::ok(ID, LABEL, "binaire introuvable");
    };
    let sig = inspect(&exe);
    match &sig {
        Signature::AdHoc { .. } | Signature::Unsigned => {
            let configured = !s
                .config
                .config()
                .upgrade
                .codesign_identity
                .trim()
                .is_empty();
            DoctorCheck::fail(
                ID,
                LABEL,
                format!(
                    "{} : macOS redemande l'accès au Trousseau après chaque build{}. Voir « Signature \
                     locale » dans docs/install-headless.md",
                    sig.describe(),
                    if configured {
                        " (upgrade.codesign_identity est configuré, mais ce binaire vient d'un build non signé)"
                    } else {
                        ""
                    }
                ),
                Some("SIGN_IDENTITY=\"Penelope Dev\" make deploy".into()),
            )
        }
        _ => DoctorCheck::ok(ID, LABEL, sig.describe()),
    }
}

/// Mode d'installation et programme lancé par le service (issues #33 et #36) : le service
/// doit lancer un chemin stable, que les mises à jour remplacent sans le recharger.
pub fn install_mode_check() -> DoctorCheck {
    const ID: &str = "install_mode";
    const LABEL: &str = "Mode d'installation";
    let Ok(exe) = crate::helpers::running_binary() else {
        return DoctorCheck::ok(ID, LABEL, "binaire introuvable");
    };
    let launched = penelope_platform::service::launchd_plist_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| penelope_platform::service::launchd_program(&raw));
    install_mode(&exe, launched.as_deref())
}

pub(super) fn install_mode(exe: &std::path::Path, launched: Option<&str>) -> DoctorCheck {
    const ID: &str = "install_mode";
    const LABEL: &str = "Mode d'installation";
    let mode = if crate::helpers::is_source_build(exe) {
        "binaire de compilation"
    } else {
        "chemin stable (`/upgrade install` ou `make deploy` le remplacent)"
    };
    let Some(program) = launched else {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("{mode} · {} · service non installé", exe.display()),
        );
    };
    let real = std::fs::canonicalize(program).unwrap_or_else(|_| program.into());
    if crate::helpers::is_source_build(&real) {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "le service lance un binaire de compilation ({program}) : chaque mise à jour \
                 devrait recharger le service. `make deploy` sur la machine, ou `/upgrade \
                 install`, le passe au chemin stable (`upgrade.install_dir`)"
            ),
            Some("make deploy".into()),
        );
    }
    if real == exe {
        DoctorCheck::ok(ID, LABEL, format!("{mode} · le service lance {program}"))
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "{mode} · ce binaire est {}, mais le service lance {program} : un redémarrage \
                 changerait de binaire",
                exe.display()
            ),
            Some("penelope uninstall && penelope install".into()),
        )
    }
}

/// Planifications actives dont la dernière exécution a échoué (issue #39).
pub async fn schedules_check(s: &Services) -> DoctorCheck {
    const ID: &str = "schedules";
    const LABEL: &str = "Planifications";
    let list = s.schedules.list().await.unwrap_or_default();
    let active = list.iter().filter(|x| x.state == "active").count();
    let failing: Vec<String> = list
        .iter()
        .filter(|x| x.state == "active")
        .filter_map(|x| {
            x.last_error
                .as_ref()
                .map(|e| format!("{} ({})", x.id, e.chars().take(120).collect::<String>()))
        })
        .collect();
    if failing.is_empty() {
        DoctorCheck::ok(ID, LABEL, format!("{active} active(s), aucune en échec"))
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            format!("{} en échec : {}", failing.len(), failing.join(" ; ")),
            Some("/schedules".into()),
        )
    }
}

/// Délai au-delà duquel une mise à jour jamais démarrée est signalée.
const STALE_UPGRADE_MS: i64 = 5 * 60_000;

/// Mise à jour installée mais jamais démarrée (issue #36) : le service n'a pas été relancé.
pub fn pending_upgrade_check(s: &Services) -> DoctorCheck {
    let state = s.platform.dirs.state();
    pending_upgrade(&state, s.clock.now_ms())
}

pub(super) fn pending_upgrade(state: &std::path::Path, now_ms: i64) -> DoctorCheck {
    const ID: &str = "upgrade_pending";
    const LABEL: &str = "Mise à jour en attente";
    let Some(p) = crate::upgrade::pending(state) else {
        return DoctorCheck::ok(ID, LABEL, "aucune");
    };
    let installed_ms = chrono::DateTime::parse_from_rfc3339(&p.installed_at)
        .map(|t| t.timestamp_millis())
        .ok();
    let stale =
        p.first_boot_ms.is_none() && installed_ms.is_some_and(|t| now_ms - t > STALE_UPGRADE_MS);
    if !stale {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "{} → {} à l'essai ({} démarrage(s))",
                p.from_version, p.to_version, p.attempts
            ),
        );
    }
    let relay_log = state.join("upgrade").join("relay").join("reloader.log");
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} installée le {} n'a jamais démarré : le service n'a pas été relancé. Journal \
             du relais : {}",
            p.to_version,
            p.installed_at,
            relay_log.display()
        ),
        Some("penelope start".into()),
    )
    .critical()
}
