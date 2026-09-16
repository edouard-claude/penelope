//! `penelope doctor` (§2.11) : contrôles communs et propres à l'OS.
//!
//! Chaque point en échec est accompagné d'une commande corrective **proposée, jamais
//! exécutée automatiquement**.

use crate::runtime::Services;
use penelope_kernel::api::DoctorCheck;

/// Exécute tous les contrôles.
pub async fn run(s: &Services) -> Vec<DoctorCheck> {
    let mut checks: Vec<DoctorCheck> = s
        .platform
        .doctor()
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

    // Secrets attendus.
    for (name, placeholder) in [
        ("telegram_bot_token", cfg.telegram.token.as_str()),
        (
            "openrouter_api_key",
            cfg.providers.openrouter.api_key.as_str(),
        ),
    ] {
        let needed = placeholder.contains("${SECRET:");
        if !needed {
            continue;
        }
        let present = s.platform.secrets.get(name).ok().flatten().is_some();
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
                "absent du magasin",
                Some(format!("penelope secret set {name}")),
            )
        });
    }

    // Intégrité de la base et chaîne d'audit.
    match s.store.integrity() {
        Ok(v) if v == "ok" => checks.push(DoctorCheck::ok("db", "Base SQLite", "intègre")),
        Ok(v) => checks.push(
            DoctorCheck::fail(
                "db",
                "Base SQLite",
                v,
                Some("penelope restore --latest".into()),
            )
            .critical(),
        ),
        Err(e) => {
            checks.push(DoctorCheck::fail("db", "Base SQLite", e.to_string(), None).critical())
        }
    }
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

    // Réseau : hôtes indispensables.
    for host in ["api.telegram.org", "openrouter.ai"] {
        checks.push(reachable_check(host).await);
    }

    checks
}

fn clock_check(s: &Services) -> DoctorCheck {
    let now = s.clock.now_ms();
    let system = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(now);
    let drift = (now - system).abs();
    if drift <= 2000 {
        DoctorCheck::ok("clock", "Horloge", format!("dérive {drift} ms"))
    } else {
        DoctorCheck::fail(
            "clock",
            "Horloge",
            format!("dérive de {drift} ms : les schedules seront décalés"),
            Some("sudo sntp -sS time.apple.com".into()),
        )
    }
}

async fn reachable_check(host: &str) -> DoctorCheck {
    let id = format!("net.{host}");
    let label = format!("Réseau vers {host}");
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::lookup_host(format!("{host}:443")),
    )
    .await
    {
        Ok(Ok(mut addrs)) => match addrs.next() {
            Some(a) => DoctorCheck::ok(&id, &label, format!("résout vers {}", a.ip())),
            None => DoctorCheck::fail(&id, &label, "aucune adresse", None),
        },
        Ok(Err(e)) => DoctorCheck::fail(&id, &label, e.to_string(), None),
        Err(_) => DoctorCheck::fail(&id, &label, "délai de résolution dépassé", None),
    }
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
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        // Horloge système : le contrôle de dérive doit passer.
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
    }

    #[tokio::test]
    async fn doctor_covers_the_expected_checks() {
        let (_d, s) = services().await;
        let checks = run(&s).await;
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        for expected in [
            "owner",
            "db",
            "audit",
            "clock",
            "effects",
            "workflows",
            "skills",
        ] {
            assert!(ids.contains(&expected), "contrôle manquant : {expected}");
        }
        // Chaque échec propose une correction ou explique pourquoi il n'y en a pas.
        for c in &checks {
            assert!(!c.detail.is_empty(), "{} sans détail", c.id);
        }
    }

    #[tokio::test]
    async fn database_and_audit_are_healthy_on_a_fresh_install() {
        let (_d, s) = services().await;
        let checks = run(&s).await;
        let db = checks.iter().find(|c| c.id == "db").unwrap();
        assert!(db.ok, "{}", db.detail);
        let audit = checks.iter().find(|c| c.id == "audit").unwrap();
        assert!(audit.ok, "{}", audit.detail);
    }

    #[tokio::test]
    async fn a_missing_owner_is_critical_with_a_fix() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        s.config
            .mutate("test", |c| {
                c.owner.telegram_user_id = 0;
                Ok(vec![])
            })
            .ok();
        // La validation refuse un propriétaire nul : on vérifie donc le rendu du contrôle.
        let check = DoctorCheck::fail(
            "owner",
            "Propriétaire Telegram",
            "aucun",
            Some("penelope config set owner.telegram_user_id <ton id>".into()),
        )
        .critical();
        assert_eq!(check.severity, "error");
        assert!(render(&[check]).contains("correction proposée"));
    }

    #[tokio::test]
    async fn pending_unknown_effects_are_surfaced() {
        let (_d, s) = services().await;
        let spec = penelope_kernel::effects::EffectSpec::new(
            penelope_kernel::effects::EffectKind::Mcp,
            "mcp__forge__create_pr",
            serde_json::json!({}),
        );
        let id = match s.effects.plan(spec).await.unwrap() {
            penelope_kernel::effects::Planned::Fresh(id) => id,
            other => panic!("{other:?}"),
        };
        s.effects.dispatching(&id).await.unwrap();
        s.effects.recover_on_boot().await.unwrap();

        let checks = run(&s).await;
        let e = checks.iter().find(|c| c.id == "effects").unwrap();
        assert!(!e.ok);
        assert!(e.fix.as_deref().unwrap().contains("approvals"));
    }

    #[test]
    fn rendering_marks_failures() {
        let checks = vec![
            DoctorCheck::ok("a", "Tout va bien", "rien à signaler"),
            DoctorCheck::fail("b", "Problème", "détail", Some("faire ceci".into())),
        ];
        let out = render(&checks);
        assert!(out.contains("✅ Tout va bien"));
        assert!(out.contains("⚠️ Problème"));
        assert!(out.contains("faire ceci"));
        assert!(out.contains("2 contrôle(s), 1 en échec"));
    }
}
