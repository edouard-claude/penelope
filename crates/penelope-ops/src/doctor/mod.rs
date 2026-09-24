//! `penelope doctor` (§2.11) : contrôles communs et propres à l'OS.
//!
//! Chaque point en échec est accompagné d'une commande corrective **proposée, jamais
//! exécutée automatiquement**.

use penelope_app::services::Services;
use penelope_kernel::api::DoctorCheck;

mod coherence;
mod machine;
mod memory;
mod models;
mod secrets;

pub use coherence::*;
pub use machine::*;
pub use memory::*;
pub use models::*;
pub use secrets::*;

/// Exécute tous les contrôles.
pub async fn run(s: &Services) -> Vec<DoctorCheck> {
    // Les contrôles de l'OS lancent des sous-processus (`pmset`, `docker --version`…) :
    // hors des fils asynchrones, pour ne pas geler les tours en cours.
    let platform = s.platform.clone();
    let os_checks = tokio::task::spawn_blocking(move || platform.doctor())
        .await
        .unwrap_or_default();
    let mut checks: Vec<DoctorCheck> = os_checks
        .into_iter()
        .map(|i| DoctorCheck {
            id: i.id,
            label: i.label,
            ok: i.ok,
            detail: i.detail,
            fix: i.fix,
            severity: "warn".into(),
        })
        .collect();

    let cfg = s.config.config();

    // Propriétaire configuré : sans lui, le bot serait ouvert à tous.
    checks.push(if cfg.owner.telegram_user_id == 0 {
        DoctorCheck::fail(
            "owner",
            "Propriétaire Telegram",
            "aucun `owner.telegram_user_id` : le canal Telegram restera fermé",
            Some("penelope config set owner.telegram_user_id <ton id>".into()),
        )
        .critical()
    } else {
        DoctorCheck::ok(
            "owner",
            "Propriétaire Telegram",
            format!("id {}", cfg.owner.telegram_user_id),
        )
    });

    // Conversations de groupe : autorisées par identifiant, et celles refusées récemment
    // avec le leur (issue #113).
    checks.push(telegram_chats_check(s).await);

    // Secrets attendus.
    for (name, placeholder) in [
        ("telegram_bot_token", cfg.telegram.token.as_str()),
        (
            "openrouter_api_key",
            cfg.providers.openrouter.api_key.as_str(),
        ),
        // La connexion Codex vit dans le magasin comme un secret (#142) : sans elle, le
        // fournisseur activé ne sert rien.
        (
            crate::codex_auth::SECRET,
            if cfg.providers.codex.enabled {
                "${SECRET:codex.oauth}"
            } else {
                ""
            },
        ),
    ] {
        let needed = placeholder.contains("${SECRET:");
        if !needed {
            continue;
        }
        let present = s.platform.secrets.get(name).ok().flatten().is_some();
        let (detail, fix) = match name {
            "telegram_bot_token" => (
                "absent du magasin : c'est le jeton du bot donné par @BotFather, pas ton \
                 identifiant (owner.telegram_user_id)",
                "dans Telegram, écrire à @BotFather puis /newbot ; puis \
                 `penelope secret set telegram_bot_token` et coller le jeton à l'invite",
            ),
            "codex.oauth" => (
                "absent du magasin : le fournisseur `codex` est activé sans compte connecté",
                "penelope model auth codex",
            ),
            _ => (
                "absent du magasin",
                "créer une clé sur https://openrouter.ai/keys, puis \
                 `penelope secret set openrouter_api_key` et coller la clé à l'invite",
            ),
        };
        checks.push(if present {
            DoctorCheck::ok(
                &format!("secret.{name}"),
                &format!("Secret `{name}`"),
                "présent",
            )
        } else {
            DoctorCheck::fail(
                &format!("secret.{name}"),
                &format!("Secret `{name}`"),
                detail,
                Some(fix.to_string()),
            )
        });
    }

    // Intégrité de la base et chaîne d'audit.
    checks.push(integrity_check_of(s));
    match s.events.verify().await {
        Ok(r) if r.ok => checks.push(DoctorCheck::ok(
            "audit",
            "Chaîne d'audit",
            format!("{} événements vérifiés", r.checked),
        )),
        Ok(r) => checks.push(
            DoctorCheck::fail(
                "audit",
                "Chaîne d'audit",
                r.detail.unwrap_or_else(|| "chaîne rompue".into()),
                None,
            )
            .critical(),
        ),
        Err(e) => checks.push(DoctorCheck::fail(
            "audit",
            "Chaîne d'audit",
            e.to_string(),
            None,
        )),
    }

    // Horloge : une dérive fausse tous les schedules.
    checks.push(clock_check(s));

    // Paniques de l'écrivain : la base a survécu, mais une écriture a été perdue (#44).
    checks.push(writer_panics_check());

    // Clés du fichier ignorées : version plus récente ou faute de frappe (#76).
    checks.push(config_unknown_check(s));

    // Rétention : dernière passe et contenu que gardent les tables d'effets (#78).
    checks.push(retention_check(s).await);

    // Jour budgétaire : des lignes récentes comptées dans un autre fuseau (#79).
    checks.push(budget_days_check(s).await);

    // Un alias de conversation vers un modèle sans tool calling ne marchera pas (#54).
    checks.push(tool_calling_check(s).await);

    // Bac à sable : ce qu'une commande peut lire malgré tout (#68), et où elle peut
    // l'envoyer (#106).
    checks.push(sandbox_reads_check(s));
    checks.push(sandbox_network_check(s).await);

    // Effets en attente de décision.
    let unknown = s
        .effects
        .count_by_state(penelope_kernel::effects::EffectState::Unknown)
        .await
        .unwrap_or(0);
    checks.push(if unknown == 0 {
        DoctorCheck::ok("effects", "Effets incertains", "aucun")
    } else {
        DoctorCheck::fail(
            "effects",
            "Effets incertains",
            format!("{unknown} effet(s) en attente de décision"),
            Some("penelope approvals".into()),
        )
    });

    // Workflows et templates invalides.
    let wf_errors = s.workflows.errors();
    checks.push(if wf_errors.is_empty() {
        DoctorCheck::ok(
            "workflows",
            "Workflows",
            format!("{} chargés", s.workflows.ids().len()),
        )
    } else {
        DoctorCheck::fail(
            "workflows",
            "Workflows",
            wf_errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(" ; "),
            Some("penelope wf validate <fichier>".into()),
        )
    });

    let skill_errors = s.skills.errors();
    checks.push(if skill_errors.is_empty() {
        DoctorCheck::ok(
            "skills",
            "Skills",
            format!("{} chargées", s.skills.all().len()),
        )
    } else {
        DoctorCheck::fail(
            "skills",
            "Skills",
            skill_errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(" ; "),
            None,
        )
    });

    // Formulaires Telegram restés ouverts (#149).
    checks.push(open_forms_check(s).await);

    // Part des lignes `shell_exec` collées, sur sept jours (#150).
    checks.push(glued_lines_check(s).await);

    // Le rédacteur rend, sur toutes les formes connues (#153).
    checks.push(redactor_check().await);

    // Effort de raisonnement du rôle de consolidation, et ce qu'il coûte (#152).
    checks.push(reasoning_effort_check(s).await);
    checks.push(dream_power_check(s).await);

    // Magasin de secrets : un aller-retour de 8 Ko, la taille d'un Grant (#148).
    checks.push(secret_roundtrip_check(s));

    // Ce que Pénélope sait de sa machine (#156) : l'inventaire est refait ici, puisque
    // `doctor` est ce qu'on lance après avoir installé ou connecté quelque chose.
    checks.extend(machine_checks(s).await);

    // Dépendances des skills tierces : listées, jamais installées (#146).
    checks.push(skill_requirements_check(s).await);

    // Mémoire : entrées au-delà de la borne, Cœur au-delà de son budget (#145).
    checks.push(memory_size_check(s).await);

    // Foyer du propriétaire (#143) : là où arrivent les avis sans session.
    checks.push(home_check(s));

    // Fournisseur Codex : connexion, jetons, périmètre, identité empruntée (#142).
    checks.extend(codex_checks(s).await);

    // Réseau : hôtes indispensables.
    let mut hosts = vec!["api.telegram.org", "openrouter.ai"];
    if cfg.providers.codex.enabled {
        hosts.extend(["chatgpt.com", "auth.openai.com"]);
    }
    for host in hosts {
        checks.push(reachable_check(host).await);
    }

    checks
}

/// Rendu texte, pour la CLI et pour `/doctor`.
pub fn render(checks: &[DoctorCheck]) -> String {
    let mut s = String::new();
    for c in checks {
        let mark = if c.ok {
            "✅"
        } else if c.severity == "error" {
            "❌"
        } else {
            "⚠️"
        };
        s.push_str(&format!("{mark} {} — {}\n", c.label, c.detail));
        if let Some(fix) = &c.fix {
            s.push_str(&format!("   correction proposée : {fix}\n"));
        }
    }
    let failed = checks.iter().filter(|c| !c.ok).count();
    s.push_str(&format!(
        "\n{} contrôle(s), {failed} en échec.\n",
        checks.len()
    ));
    s
}

#[cfg(test)]
mod tests;
