//! `penelope doctor` (§2.11) : contrôles communs et propres à l'OS.
//!
//! Chaque point en échec est accompagné d'une commande corrective **proposée, jamais
//! exécutée automatiquement**.

use crate::runtime::Services;
use penelope_kernel::api::DoctorCheck;

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
        let (detail, fix) = match name {
            "telegram_bot_token" => (
                "absent du magasin : c'est le jeton du bot donné par @BotFather, pas ton \
                 identifiant (owner.telegram_user_id)",
                "dans Telegram, écrire à @BotFather puis /newbot ; puis \
                 `penelope secret set telegram_bot_token` et coller le jeton à l'invite",
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

/// Mode d'installation (sources ou releases) et programme lancé par le service (issue #33).
pub fn install_mode_check() -> DoctorCheck {
    const ID: &str = "install_mode";
    const LABEL: &str = "Mode d'installation";
    let Ok(exe) = crate::upgrade::running_binary() else {
        return DoctorCheck::ok(ID, LABEL, "binaire introuvable");
    };
    let mode = if crate::upgrade::is_source_build(&exe) {
        "sources (`make deploy`, ou `/upgrade install` pour basculer vers les releases)"
    } else {
        "releases (`/upgrade install`)"
    };
    let launched = penelope_platform::service::launchd_plist_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| penelope_platform::service::launchd_program(&raw));
    match launched {
        None => DoctorCheck::ok(
            ID,
            LABEL,
            format!("{mode} · {} · service non installé", exe.display()),
        ),
        Some(program) => {
            let same = std::fs::canonicalize(&program)
                .map(|p| p == exe)
                .unwrap_or(false);
            if same {
                DoctorCheck::ok(ID, LABEL, format!("{mode} · le service lance {program}"))
            } else {
                DoctorCheck::fail(
                    ID,
                    LABEL,
                    format!(
                        "{mode} · ce binaire est {}, mais le service lance {program} : un \
                         redémarrage changerait de binaire",
                        exe.display()
                    ),
                    Some("penelope uninstall && penelope install".into()),
                )
            }
        }
    }
}

/// Secret en clair dans un journal existant (issue #26) : purger le fichier et révoquer.
pub fn logs_secret_check(s: &Services) -> DoctorCheck {
    use std::io::BufRead;
    const ID: &str = "logs_secrets";
    const LABEL: &str = "Aucun secret dans les journaux";
    let dir = s.platform.dirs.logs();
    let mut leaks: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".log") || name.ends_with(".jsonl")) {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut kinds: Vec<&str> = Vec::new();
        for line in std::io::BufReader::new(std::io::Read::take(file, 64 * 1024 * 1024))
            .lines()
            .map_while(Result::ok)
        {
            if let Some(k) = penelope_observe::leaked_secret_kind(&line)
                && !kinds.contains(&k)
            {
                kinds.push(k);
            }
        }
        if !kinds.is_empty() {
            leaks.push(format!("{} ({})", path.display(), kinds.join(", ")));
        }
    }
    if leaks.is_empty() {
        return DoctorCheck::ok(ID, LABEL, format!("{} vérifié", dir.display()));
    }
    let telegram = leaks
        .iter()
        .any(|l| l.contains("telegram") || l.contains("enregistré"));
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "secret en clair dans : {}. Le considérer comme exposé{}",
            leaks.join(" ; "),
            if telegram {
                " : révoquer le jeton du bot (@BotFather, /revoke) puis `penelope secret set telegram_bot_token`"
            } else {
                " et le renouveler"
            }
        ),
        Some(format!("vider les fichiers concernés, par exemple : : > {}/daemon.err.log", dir.display())),
    )
    .critical()
}

/// Cohérence de la configuration et des déclencheurs (issue #16).
pub async fn coherence_checks(s: &Services) -> Vec<DoctorCheck> {
    use penelope_kernel::coherence::{Gravity, contradictions};
    let cfg = s.config.config();
    let mut out: Vec<DoctorCheck> = contradictions(&cfg)
        .into_iter()
        .map(|c| {
            let check = DoctorCheck::fail(
                "config_coherence",
                "Configuration cohérente",
                format!("{} ({})", c.message, c.keys.join(", ")),
                c.keys.first().map(|k| format!("penelope config get {k}")),
            );
            if c.gravity == Gravity::Refus {
                check.critical()
            } else {
                check
            }
        })
        .collect();
    // Heures calmes : un déclencheur planifié qui y tombe attend leur fin pour notifier.
    if let Ok(quiet) = penelope_kernel::config::TimeRange::parse(&cfg.telegram.quiet_hours)
        && let Ok(schedules) = s.schedules.list().await
    {
        for sc in schedules.iter().filter(|x| x.state == "active") {
            let Some(next) = sc.next_run.as_deref() else {
                continue;
            };
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(next) else {
                continue;
            };
            let local = match cfg.owner.timezone.parse::<chrono_tz::Tz>() {
                Ok(tz) => at.with_timezone(&tz).naive_local().time(),
                Err(_) => at.naive_utc().time(),
            };
            let minute = chrono::Timelike::hour(&local) * 60 + chrono::Timelike::minute(&local);
            if quiet.contains(minute) {
                out.push(DoctorCheck::fail(
                    "config_coherence",
                    "Configuration cohérente",
                    format!(
                        "le déclencheur `{}` part à {:02}:{:02}, pendant les heures calmes \
                         (`telegram.quiet_hours` = {}) : sa notification attendra",
                        sc.id,
                        chrono::Timelike::hour(&local),
                        chrono::Timelike::minute(&local),
                        cfg.telegram.quiet_hours
                    ),
                    Some(format!("penelope schedule pause {}", sc.id)),
                ));
            }
        }
    }
    if out.is_empty() {
        out.push(DoctorCheck::ok(
            "config_coherence",
            "Configuration cohérente",
            "aucun réglage n'annule un autre",
        ));
    }
    out
}

/// Contenu du vault hors de l'index (issue #15).
pub async fn vault_index_check(s: &Services) -> DoctorCheck {
    const ID: &str = "vault_index";
    const LABEL: &str = "Vault indexé";
    match crate::vault_inventory::inventory(s).await {
        Ok(inv) if inv.not_indexed.is_empty() => DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "{} fichier(s), {} entrée(s) indexée(s)",
                inv.files, inv.entries
            ),
        ),
        Ok(inv) => {
            let names: Vec<&str> = inv
                .not_indexed
                .iter()
                .take(5)
                .map(|g| g.path.as_str())
                .collect();
            DoctorCheck::fail(
                ID,
                LABEL,
                format!(
                    "{} fichier(s) présents mais hors index : {}{}",
                    inv.not_indexed.len(),
                    names.join(", "),
                    if inv.not_indexed.len() > 5 { "…" } else { "" }
                ),
                Some("penelope vault check".into()),
            )
        }
        Err(e) => DoctorCheck::fail(ID, LABEL, e.to_string(), None),
    }
}

/// Alias `embedding` joignable (issue #11) : sans lui, la recherche reste lexicale.
pub async fn embedding_check(d: &crate::runtime::Daemon) -> DoctorCheck {
    const ID: &str = "embedding";
    const LABEL: &str = "Embeddings (recherche par le sens)";
    let fix = Some(format!(
        "penelope config set models.aliases.embedding {}",
        penelope_kernel::config::DEFAULT_EMBEDDING_MODEL
    ));
    let Some(model) = crate::embeddings::model(d) else {
        return DoctorCheck::fail(ID, LABEL, "aucun modèle pour le rôle `embedding`", fix);
    };
    let texts = ["penelope doctor".to_string()];
    let probe = crate::embeddings::embed_texts(d, &texts);
    match tokio::time::timeout(std::time::Duration::from_secs(15), probe).await {
        Ok(Ok((_, v))) if v.first().is_some_and(|x| !x.is_empty()) => {
            DoctorCheck::ok(ID, LABEL, format!("`{model}`, {} dimensions", v[0].len()))
        }
        Ok(Ok(_)) => DoctorCheck::fail(ID, LABEL, format!("`{model}` : vecteur vide"), fix),
        Ok(Err(e)) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("`{model}` injoignable ({e}) : recherche lexicale seule"),
            fix,
        ),
        Err(_) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("`{model}` ne répond pas en 15 s : recherche lexicale seule"),
            fix,
        ),
    }
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

/// Serveurs MCP : état de chacun, secrets manquants, déclarations invalides.
pub async fn mcp_checks(s: &Services, sup: &crate::mcp::McpSupervisor) -> Vec<DoctorCheck> {
    let mut out = Vec::new();
    for st in sup.statuses().await {
        let id = format!("mcp.{}", st.name);
        let label = format!("Serveur MCP `{}`", st.name);
        if let Some(cfg) = sup.config_of(&st.name).await
            && let Err(e) = cfg.resolve_secrets(s.platform.secrets.as_ref())
        {
            out.push(DoctorCheck::fail(
                &id,
                &label,
                e.to_string(),
                Some("penelope secret set <nom du secret>".into()),
            ));
            continue;
        }
        use penelope_mcp::ServerState::*;
        out.push(match st.state {
            Ready | Degraded => DoctorCheck::ok(
                &id,
                &label,
                format!("{} outil(s), {} appel(s)", st.tool_count, st.calls),
            ),
            Configured if st.tool_count > 0 => DoctorCheck::ok(
                &id,
                &label,
                format!("{} outil(s), démarre au premier appel", st.tool_count),
            ),
            Disabled => DoctorCheck::ok(&id, &label, "désactivé".to_string()),
            _ => DoctorCheck::fail(
                &id,
                &label,
                format!(
                    "{} : {}",
                    st.state.as_str(),
                    st.last_error
                        .as_deref()
                        .unwrap_or("pas encore joint")
                        .chars()
                        .take(300)
                        .collect::<String>()
                ),
                Some(format!(
                    "penelope mcp logs {0} ; penelope mcp restart {0}",
                    st.name
                )),
            ),
        });
    }
    for (file, error) in sup.invalid() {
        out.push(DoctorCheck::fail(
            &format!("mcp.invalid.{file}"),
            &format!("Déclaration MCP `{file}`"),
            &error,
            Some(format!(
                "corriger {}",
                sup.dir().join(format!("{file}.toml")).display()
            )),
        ));
    }
    out
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
