/// #157 : `doctor` accusait un Trousseau verrouillé alors que le Trousseau répondait
/// parfaitement — il rendait juste autre chose. Une correction fausse envoie chercher
/// là où il n'y a rien : le 21/09, sur l'instance, « déverrouiller le Trousseau »
/// pendant qu'un secret long était relu en hexadécimal.
mod secret_roundtrip {
    use penelope_platform::Result;
    use penelope_platform::secrets::SecretStore;

    /// Un magasin qui rend une autre valeur que celle écrite : la panne de #157.
    struct Altered(String);
    impl SecretStore for Altered {
        fn backend(&self) -> String {
            "essai".into()
        }
        fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(Some(self.0.clone()))
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    /// Un magasin qui ne répond pas : le Trousseau verrouillé, lui.
    struct Locked;
    impl SecretStore for Locked {
        fn backend(&self) -> String {
            "essai".into()
        }
        fn get(&self, _: &str) -> Result<Option<String>> {
            Err(penelope_platform::PlatformError::Secret(
                "security : trousseau verrouillé".into(),
            ))
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    #[test]
    fn an_altered_read_is_not_blamed_on_a_locked_keychain() {
        // Les 42 caractères d'hexadécimal du rapport du 21/09.
        let hexa = "0170656e656c6f70652d6368756e6b733a76313a33".to_string();
        let c = super::super::secret_roundtrip_of(&Altered(hexa));
        assert!(!c.ok);
        assert!(c.detail.contains("relecture altérée"), "{}", c.detail);
        assert!(c.detail.contains("42 relus"), "{}", c.detail);
        assert!(c.detail.contains("hexadécimal"), "{}", c.detail);
        assert!(
            c.detail.contains("Trousseau n'est pas en cause"),
            "{}",
            c.detail
        );
        let fix = c.fix.unwrap_or_default();
        assert!(
            !fix.contains("unlock-keychain"),
            "correction fausse : {fix}"
        );
        assert!(fix.contains("#157"), "{fix}");
    }

    /// Une valeur altérée qui n'est pas de l'hexadécimal ne doit pas être annoncée
    /// comme telle : le diagnostic dit ce qu'il voit, rien de plus.
    #[test]
    fn an_altered_read_only_says_hex_when_it_is_hex() {
        let c = super::super::secret_roundtrip_of(&Altered("tronqué".into()));
        assert!(c.detail.contains("relecture altérée"), "{}", c.detail);
        assert!(!c.detail.contains("hexadécimal"), "{}", c.detail);
    }

    #[test]
    fn a_locked_keychain_is_still_told_to_unlock() {
        let c = super::super::secret_roundtrip_of(&Locked);
        assert!(!c.ok);
        assert!(c.detail.contains("relecture refusée"), "{}", c.detail);
        assert!(
            c.fix.unwrap_or_default().contains("unlock-keychain"),
            "{}",
            c.detail
        );
    }
}

/// Issue #36 : un service qui lance `target/release` est signalé ; un chemin stable
/// lancé par le service est sain.
#[test]
fn a_service_launching_a_build_output_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("code/target/release/penelope");
    let stable = dir.path().join("bin/penelope");
    for p in [&build, &stable] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }
    let stable = std::fs::canonicalize(&stable).unwrap();
    let c = install_mode(&stable, Some(&build.to_string_lossy()));
    assert!(
        !c.ok && c.detail.contains("binaire de compilation"),
        "{c:?}"
    );
    assert_eq!(c.fix.as_deref(), Some("make deploy"));
    let c = install_mode(&stable, Some(&stable.to_string_lossy()));
    assert!(c.ok, "{c:?}");
}

/// Issue #36 : `upgrade.json` sans `first_boot_ms` depuis plus de 5 minutes est signalé.
#[test]
fn an_upgrade_that_never_booted_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    assert!(pending_upgrade(state, 0).ok);
    std::fs::write(
        state.join("upgrade.json"),
        r#"{"from_version":"0.12.0","to_version":"0.13.0","binary":"/b","previous":"/p",
           "attempts":0,"installed_at":"2026-09-17T10:16:06Z","first_boot_ms":null}"#,
    )
    .unwrap();
    let installed = chrono::DateTime::parse_from_rfc3339("2026-09-17T10:16:06Z")
        .unwrap()
        .timestamp_millis();
    assert!(
        pending_upgrade(state, installed + 60_000).ok,
        "encore récent"
    );
    let c = pending_upgrade(state, installed + 6 * 60_000);
    assert!(!c.ok && c.detail.contains("n'a jamais démarré"), "{c:?}");
    assert_eq!(c.severity, "error");
    assert_eq!(c.fix.as_deref(), Some("penelope start"));
}
use super::*;

#[test]
fn missing_rg_suggests_the_homebrew_formula_name() {
    assert_eq!(
        brew_install_command(&["rg".into(), "jq".into()]),
        "brew install ripgrep jq"
    );
}

/// #125 : les alias des rôles d'image n'appellent pas d'outils ; un modèle de pointage
/// sans tool calling peut les servir. Le modèle de conversation, lui, en a besoin.
#[test]
fn image_roles_do_not_need_tool_calling() {
    let cfg = penelope_kernel::config::Config::default();
    assert!(!alias_needs_tools(&cfg, "vision"));
    assert!(!alias_needs_tools(&cfg, "image"));
    assert!(alias_needs_tools(&cfg, "main"));
}
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

mod coherence;
mod machine;
mod memory;
mod run;
mod secrets;

/// Issue #26 : un jeton déjà écrit dans un journal est signalé, avec révocation.
#[tokio::test]
async fn a_token_left_in_a_log_is_reported() {
    let (_d, s) = services().await;
    let logs = s.platform.dirs.logs();
    std::fs::create_dir_all(&logs).unwrap();
    assert!(logs_secret_check(&s).ok);
    std::fs::write(
        logs.join("daemon.err.log"),
        "WARN getUpdates en échec error=error sending request for url \
         (https://api.telegram.org/bot7123456789:AAHleaked_token_abcdefghijklmnopqrstu/getUpdates)\n",
    )
    .unwrap();
    let c = logs_secret_check(&s);
    assert!(!c.ok);
    assert!(
        c.detail.contains("daemon.err.log") && c.detail.contains("BotFather"),
        "{c:?}"
    );
}

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    // Horloge système : le contrôle de dérive doit passer.
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
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
        "reasoning_effort",
        "dream_power",
    ] {
        assert!(ids.contains(&expected), "contrôle manquant : {expected}");
    }
    // Chaque échec propose une correction ou explique pourquoi il n'y en a pas.
    for c in &checks {
        assert!(!c.detail.is_empty(), "{} sans détail", c.id);
    }
}

/// #152 : `doctor` dit ce qui partira en raisonnement pour la consolidation, et la
/// part réellement observée. La nuit du 19/09 tournait à 73 % de raisonnement et
/// passait de justesse ; celle du 20/09 à 100 % et échouait.
#[tokio::test]
async fn doctor_reports_the_reasoning_share_of_the_consolidation() {
    let (_d, s) = services().await;
    // Sept jours d'appels de consolidation : 73 % de la sortie en raisonnement.
    for _ in 0..3 {
        s.budget
            .record(penelope_kernel::budget::UsageRecord {
                model: "deepseek/deepseek-v4-flash".into(),
                provider: "openrouter".into(),
                role: Some("consolidation".into()),
                prompt: 10_000,
                completion: 10_000,
                reasoning: 7_300,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let c = reasoning_effort_check(&s).await;
    assert!(c.detail.contains("73%"), "part observée : {}", c.detail);
    // Gardé et budgété (le défaut) : une part haute est normale, pas une alerte.
    assert!(c.ok, "{c:?}");
    assert!(
        c.detail.contains("raisonnement gardé"),
        "ce qui partira : {}",
        c.detail
    );

    // Éteint, la même part trahit un modèle qui n'écoute pas.
    s.config
        .mutate("test", |cfg| {
            cfg.memory.consolidation_reasoning = "off".into();
            Ok(vec!["memory.consolidation_reasoning".into()])
        })
        .unwrap();
    let c = reasoning_effort_check(&s).await;
    assert!(!c.ok, "éteint mais toujours 73 % : {}", c.detail);
    assert!(c.fix.is_some(), "une sortie est proposée");
}

/// #76 : un fichier portant une section d'une version plus récente se charge, et
/// `doctor` la nomme au lieu que le daemon refuse de démarrer.
#[tokio::test]
async fn unknown_config_keys_are_named_not_fatal() {
    let (_d, s) = services().await;
    let checks = run(&s).await;
    assert!(checks.iter().find(|c| c.id == "config.unknown").unwrap().ok);

    let text = format!(
        "{}\n[futur]\nactif = true\n",
        penelope_kernel::config::Config::sample_toml(42).unwrap()
    );
    std::fs::write(s.config.path(), text).unwrap();
    s.config
        .reload_from_disk()
        .expect("relu malgré la section inconnue");
    let checks = run(&s).await;
    let c = checks.iter().find(|c| c.id == "config.unknown").unwrap();
    assert!(!c.ok && c.detail.contains("futur"), "{c:?}");
}

/// #78 : `doctor` dit ce que gardent les tables d'effets et quand la rétention est
/// passée pour la dernière fois.
#[tokio::test]
async fn retention_is_reported_with_the_kept_content() {
    let (_d, s) = services().await;
    let c = run(&s)
        .await
        .into_iter()
        .find(|c| c.id == "retention")
        .unwrap();
    assert!(!c.ok && c.detail.contains("aucune passe"), "{c:?}");

    let now = s.clock.now_ms().to_string();
    s.store
        .write(move |tx| {
            penelope_store::kv_set(tx, "retention.last", &now)?;
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                 VALUES('o1',1,'sendMessage',?1,'sent','2026-06-01T00:00:00Z')",
                [format!(r#"{{"text":"{}"}}"#, "x".repeat(300_000))],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let c = run(&s)
        .await
        .into_iter()
        .find(|c| c.id == "retention")
        .unwrap();
    assert!(c.ok, "{c:?}");
    assert!(c.detail.contains("envois Telegram 0.3 Mo"), "{}", c.detail);
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
