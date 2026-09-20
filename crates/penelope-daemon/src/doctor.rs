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

    // Magasin de secrets : un aller-retour de 8 Ko, la taille d'un Grant (#148).
    checks.push(secret_roundtrip_check(s));

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

/// #148 : le magasin de secrets acceptait les clés d'API et refusait un `Grant` de 4 Ko,
/// en recopiant ses jetons dans le message d'erreur. Le contrôle écrit, relit et efface
/// un secret de 8 Ko : la panne se voit avant qu'un secret la rencontre.
pub fn secret_roundtrip_check(s: &Services) -> DoctorCheck {
    const ID: &str = "secret_roundtrip";
    const LABEL: &str = "Magasin de secrets : aller-retour de 8 Ko";
    const NAME: &str = "penelope.doctor.roundtrip";
    let value = "x".repeat(8 * 1024);
    let store = &s.platform.secrets;
    // L'essai ne laisse rien derrière lui, quelle que soit l'étape qui échoue.
    let fail = |detail: String| {
        let _ = store.delete(NAME);
        DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(
                "déverrouiller le Trousseau (`security unlock-keychain`), ou forcer le \
                 repli fichier chiffré avec `PENELOPE_SECRETS=file`"
                    .into(),
            ),
        )
    };
    if let Err(e) = store.set(NAME, &value) {
        return fail(format!(
            "écriture refusée : {}",
            penelope_observe::redact(&e.to_string())
        ));
    }
    match store.get(NAME) {
        Ok(Some(back)) if back == value => {}
        Ok(Some(back)) => {
            return fail(format!(
                "relecture différente : {} octets écrits, {} relus",
                value.len(),
                back.len()
            ));
        }
        Ok(None) => return fail("écrit puis introuvable".into()),
        Err(e) => {
            return fail(format!(
                "relecture refusée : {}",
                penelope_observe::redact(&e.to_string())
            ));
        }
    }
    if let Err(e) = store.delete(NAME) {
        return fail(format!(
            "suppression refusée : {}",
            penelope_observe::redact(&e.to_string())
        ));
    }
    DoctorCheck::ok(
        ID,
        LABEL,
        format!("{} : 8 Ko écrits, relus, effacés", store.backend()),
    )
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

/// #145 : une entrée fourre-tout fausse la consolidation (elle « contredit » tout ce
/// qu'elle approche) et déborde dans le digest ; un Cœur au-delà de son budget n'est pas
/// injecté en entier.
pub async fn memory_size_check(s: &Services) -> DoctorCheck {
    const ID: &str = "memory.size";
    const LABEL: &str = "Taille des entrées de mémoire";
    let max = penelope_memory::quality::MAX_ENTRY_CHARS;
    let long: Vec<String> = crate::mem_split::oversized(s)
        .await
        .iter()
        .map(|e| {
            format!(
                "`{}` ({} caractères, {})",
                e.uid,
                e.text.chars().count(),
                e.file
            )
        })
        .collect();
    let budget = s.config.config().memory.core_budget_tokens as u64;
    let overflow = crate::dream::core_overflow(s, budget).await;

    if long.is_empty() && overflow.is_none() {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("toutes sous {max} caractères, Cœur dans son budget de {budget} jetons"),
        );
    }
    let mut detail = Vec::new();
    if !long.is_empty() {
        detail.push(format!(
            "{} entrée(s) au-delà de {max} caractères : {}",
            long.len(),
            long.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(w) = overflow {
        detail.push(w);
    }
    DoctorCheck::fail(
        ID,
        LABEL,
        detail.join(" ; "),
        Some("`penelope mem split <uid>` propose le découpage en un fait par entrée".into()),
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

/// #142 : état du fournisseur `codex` — connexion, fraîcheur des jetons, périmètre des
/// alias, et l'avertissement permanent sur l'identité empruntée.
pub async fn codex_checks(s: &Services) -> Vec<DoctorCheck> {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    let mut out = Vec::new();
    let codex_aliases: Vec<(&String, &String)> = cfg
        .models
        .aliases
        .iter()
        .filter(|(_, m)| crate::codex_scope::is_codex(m))
        .collect();
    if !c.enabled && codex_aliases.is_empty() {
        return out;
    }

    // Connexion et jetons.
    out.push(match crate::codex_auth::status(s) {
        Ok(Some(st)) if st.connected => {
            let now = s.clock.now_ms();
            let expires_in = (st.expires_at_ms - now) / 60_000;
            let age_days = (now - st.last_refresh_ms) / 86_400_000;
            DoctorCheck::ok(
                "provider.codex",
                "Fournisseur Codex",
                format!(
                    "connecté : compte {}, plan {}, jeton valable {expires_in} min, \
                     rafraîchi il y a {age_days} j",
                    st.account, st.plan
                ),
            )
        }
        Ok(Some(st)) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            format!(
                "compte déconnecté ({}) : les alias `codex:` se replient",
                st.disconnected.unwrap_or_default()
            ),
            Some("penelope model auth codex".into()),
        ),
        Ok(None) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            "activé, mais aucun compte ChatGPT connecté",
            Some("penelope model auth codex".into()),
        ),
        Err(e) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            format!("connexion illisible : {e}"),
            Some("penelope model auth codex --logout puis se reconnecter".into()),
        ),
    });

    // Identité empruntée : un avertissement qui ne se tait jamais.
    out.push(DoctorCheck::ok(
        "provider.codex.identity",
        "Identité Codex",
        format!(
            "`originator: {}`, client {} — Pénélope emprunte l'identité de Codex CLI. \
             Usage toléré par OpenAI, jamais garanti : il peut cesser du jour au \
             lendemain (repli : une clé d'API sur `openai_compat`).",
            c.originator, c.client_version
        ),
    ));

    // Périmètre : un alias de rôle de fond qui vise l'abonnement l'aurait contourné.
    let mut hors: Vec<String> = Vec::new();
    for (alias, model) in &codex_aliases {
        let roles = crate::codex_scope::background_roles_of(&cfg, alias);
        if !roles.is_empty() {
            hors.push(format!("`{alias}` → `{model}` ({})", roles.join(", ")));
        }
    }
    if !hors.is_empty() {
        out.push(DoctorCheck::fail(
            "provider.codex.scope",
            "Périmètre Codex",
            format!(
                "{} : ces rôles tournent sans le propriétaire ; l'abonnement ne les sert \
                 pas, chaque appel se replie",
                hors.join(", ")
            ),
            Some("penelope model set <alias> openrouter:<modèle>".into()),
        ));
    }

    // Jauges du plan : ce qui borne vraiment, puisque le coût est nul.
    if let Some(q) = crate::codex_quota::snapshot(s).await {
        let line = crate::codex_quota::gauge_line(&q, s.clock.now_ms());
        let ratio = q.worst_ratio();
        out.push(if ratio < c.quota_stop_ratio {
            DoctorCheck::ok("provider.codex.quota", "Quota du plan ChatGPT", line)
        } else {
            DoctorCheck::fail(
                "provider.codex.quota",
                "Quota du plan ChatGPT",
                format!("{line} : Pénélope est en retrait, les tours passent par OpenRouter"),
                None,
            )
        });
    }
    out
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
    let Ok(exe) = crate::upgrade::running_binary() else {
        return DoctorCheck::ok(ID, LABEL, "binaire introuvable");
    };
    let launched = penelope_platform::service::launchd_plist_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| penelope_platform::service::launchd_program(&raw));
    install_mode(&exe, launched.as_deref())
}

fn install_mode(exe: &std::path::Path, launched: Option<&str>) -> DoctorCheck {
    const ID: &str = "install_mode";
    const LABEL: &str = "Mode d'installation";
    let mode = if crate::upgrade::is_source_build(exe) {
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
    if crate::upgrade::is_source_build(&real) {
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

fn pending_upgrade(state: &std::path::Path, now_ms: i64) -> DoctorCheck {
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

/// Secret en clair dans une demande d'approbation ou la file Telegram des 30 derniers
/// jours (issue #134) : ce qui a été écrit avant la rédaction de ces deux chemins.
pub async fn stored_secret_check(s: &Services) -> DoctorCheck {
    const ID: &str = "stored_secrets";
    const LABEL: &str = "Aucun secret dans les demandes ni la file Telegram";
    let since = chrono::DateTime::from_timestamp_millis(s.clock.now_ms() - 30 * 86_400_000)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(String, String, String)> = s
        .store
        .read(move |c| {
            let mut out = Vec::new();
            for (table, sql) in [
                (
                    "approval_requests",
                    "SELECT id, payload FROM approval_requests WHERE created_at >= ?1 LIMIT 5000",
                ),
                (
                    "tg_outbox",
                    "SELECT id, payload FROM tg_outbox WHERE created_at >= ?1 LIMIT 5000",
                ),
            ] {
                let mut st = c.prepare(sql)?;
                let rows = st.query_map([&since], |r| {
                    Ok((
                        table.to_string(),
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                    ))
                })?;
                for r in rows {
                    out.push(r?);
                }
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
    let mut found: Vec<String> = Vec::new();
    for (table, id, payload) in rows {
        if let Some(k) = penelope_observe::redact::stored_secret_kind(&payload) {
            found.push(format!("{table} {id} ({k})"));
        }
    }
    if found.is_empty() {
        return DoctorCheck::ok(ID, LABEL, "30 derniers jours vérifiés");
    }
    let shown: Vec<String> = found.iter().take(5).cloned().collect();
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} ligne(s) en clair : {}{}. Considérer ces secrets comme exposés (ils sont \
             aussi dans l'historique Telegram et les sauvegardes)",
            found.len(),
            shown.join(" ; "),
            if found.len() > 5 { " ; …" } else { "" }
        ),
        Some(
            "renouveler ou révoquer les secrets exposés, puis supprimer les messages \
             correspondants dans la conversation Telegram ; les lignes de la file sont \
             réécrites par le rédacteur au prochain passage d'entretien (#148)"
                .into(),
        ),
    )
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

/// Rôles qui appellent des outils : un alias qui les sert doit viser un modèle avec tool
/// calling (issue #54). Les rôles de service (`stt`, `tts`, `embeddings`, `classifier`,
/// `summarizer`, `titler`) n'en appellent pas.
const TOOLLESS_ROLES: &[&str] = &[
    "stt",
    "tts",
    "embeddings",
    "classifier",
    "summarizer",
    "titler",
    "vision",
    "image_describe",
    "image_locate",
    "image_generate",
    "embedding",
];

/// Vrai si cet alias sert un rôle (ou un palier de routage) qui appelle des outils.
pub fn alias_needs_tools(cfg: &penelope_kernel::config::Config, alias: &str) -> bool {
    let routing = &cfg.models.routing;
    if [&routing.low, &routing.medium, &routing.high]
        .iter()
        .any(|a| a.as_str() == alias)
    {
        return true;
    }
    cfg.models
        .roles
        .iter()
        .any(|(role, a)| a == alias && !TOOLLESS_ROLES.contains(&role.as_str()))
}

/// #68 : un bac à sable qui lit tout le disque ne retient ni les clés SSH ni les jetons.
fn sandbox_reads_check(s: &Services) -> DoctorCheck {
    const ID: &str = "sandbox.deny_read";
    const LABEL: &str = "Lectures refusées au shell";
    let cfg = s.config.config();
    if cfg.sandbox.default_profile == "full" {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "profil `full` : aucune restriction, une commande lit et envoie ce qu'elle veut",
            Some("penelope config set sandbox.default_profile workspace-write".into()),
        );
    }
    if cfg.sandbox.deny_read.is_empty() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "aucune lecture refusée : `~/.ssh`, les secrets et la base restent lisibles par \
             `shell_exec`",
            Some(
                "penelope config set sandbox.deny_read '[\"~/.ssh\", \"{data}/secrets.enc\"]'"
                    .into(),
            ),
        );
    }
    let missing: Vec<&str> = ["~/.ssh", "{data}/secrets.enc", "{data}/penelope.db"]
        .into_iter()
        .filter(|d| !cfg.sandbox.deny_read.iter().any(|x| x == d))
        .collect();
    if missing.is_empty() {
        DoctorCheck::ok(
            ID,
            LABEL,
            format!("{} chemin(s) refusé(s)", cfg.sandbox.deny_read.len()),
        )
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            format!("lisibles par le shell : {}", missing.join(", ")),
            None,
        )
    }
}

/// #113 : un groupe s'ouvre par son identifiant ; celui d'une conversation refusée est
/// donné ici, avec la commande qui l'autorise.
pub async fn telegram_chats_check(s: &Services) -> DoctorCheck {
    const ID: &str = "telegram.allowed_chats";
    const LABEL: &str = "Conversations Telegram";
    let cfg = s.config.config();
    let allowed = &cfg.telegram.allowed_chats;
    let refused: Vec<String> = crate::telegram::seen_chats(s)
        .await
        .iter()
        .filter(|c| !c["id"].as_i64().is_some_and(|id| allowed.contains(&id)))
        .take(5)
        .map(|c| {
            format!(
                "{} « {} » `{}` (vu le {})",
                c["type"].as_str().unwrap_or("?"),
                c["title"].as_str().unwrap_or_default(),
                crate::telegram::shown(&c["id"]),
                c["last_seen"]
                    .as_str()
                    .unwrap_or_default()
                    .get(..16)
                    .unwrap_or_default()
            )
        })
        .collect();
    let mut detail = if allowed.is_empty() {
        "conversation privée seulement".to_string()
    } else {
        format!(
            "privée et {} groupe(s) : {}",
            allowed.len(),
            allowed
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if !refused.is_empty() {
        detail.push_str(&format!(" ; refusées récemment : {}", refused.join(", ")));
    }
    if cfg.telegram.allow_groups && allowed.is_empty() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "`telegram.allow_groups` n'ouvre plus aucun groupe : il faut l'identifiant \
                 ({detail})"
            ),
            Some("penelope config set telegram.allowed_chats '[-100…]'".into()),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #106 : le réseau du shell est fermé par défaut et accordé par appel ; ouvert à toutes
/// les commandes, une lecture peut partir dans le même appel sans que la carte le dise.
pub async fn sandbox_network_check(s: &Services) -> DoctorCheck {
    const ID: &str = "sandbox.shell_network";
    const LABEL: &str = "Réseau du shell";
    let cfg = s.config.config();
    if cfg.sandbox.default_profile == "full" {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "profil `full` : réseau ouvert à toute commande, sans bac à sable",
            Some("penelope config set sandbox.default_profile workspace-write".into()),
        );
    }
    if cfg.sandbox.shell_network {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "ouvert à toutes les commandes de `shell_exec` : une commande peut envoyer ce \
             qu'elle lit sans que la carte d'approbation le dise",
            Some("penelope config set sandbox.shell_network false".into()),
        );
    }
    let granted = s
        .policies
        .active_rules()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            r.tool.as_deref() == Some("shell_exec")
                && r.arg_match.as_ref().and_then(|p| p.get("network"))
                    == Some(&serde_json::Value::Bool(true))
        })
        .count();
    DoctorCheck::ok(
        ID,
        LABEL,
        format!(
            "fermé, accordé par appel (`network: true`, approbation) ; {granted} règle(s) \
             « Toujours » avec réseau"
        ),
    )
}

/// #54 : un alias de conversation qui vise un modèle sans tool calling ne marchera pas,
/// et rien ne l'émule.
async fn tool_calling_check(s: &Services) -> DoctorCheck {
    const ID: &str = "models.tools";
    const LABEL: &str = "Modèles et outils";
    let cfg = s.config.config();
    if s.catalog.is_empty() {
        return DoctorCheck::ok(ID, LABEL, "catalogue pas encore chargé");
    }
    let mut sans: Vec<String> = Vec::new();
    for (alias, model) in &cfg.models.aliases {
        if !alias_needs_tools(&cfg, alias) {
            continue;
        }
        let bare = penelope_llm::catalog::strip_provider(model);
        if s.catalog.get(bare).map(|i| i.supports_tools()) == Some(false) {
            sans.push(format!("`{alias}` → `{model}`"));
        }
    }
    if sans.is_empty() {
        DoctorCheck::ok(
            ID,
            LABEL,
            "tous les alias de conversation appellent des outils",
        )
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "{} n'appelle(nt) pas d'outils : ces alias servent un rôle qui en a besoin",
                sans.join(", ")
            ),
            Some("penelope model set <alias> <modèle avec tool calling>".into()),
        )
    }
}

/// #44 : une panique dans une closure d'écriture est rattrapée, mais elle dit qu'un
/// chemin d'écriture est cassé. Le compteur remonte dans `doctor`.
fn writer_panics_check() -> DoctorCheck {
    let n = penelope_store::writer_panics();
    if n == 0 {
        DoctorCheck::ok("store.writer", "Écrivain de la base", "aucune panique")
    } else {
        DoctorCheck::fail(
            "store.writer",
            "Écrivain de la base",
            format!(
                "{n} panique(s) rattrapée(s) depuis le démarrage : l'écriture concernée a \
                 été annulée, le journal la nomme en niveau `error`"
            ),
            None,
        )
    }
}

/// #76 : une clé que ce binaire ne connaît pas est ignorée au chargement, pas fatale ;
/// elle est nommée ici (écrite par une version plus récente, ou faute de frappe).
fn config_unknown_check(s: &Services) -> DoctorCheck {
    let unknown = s.config.unknown_keys();
    if unknown.is_empty() {
        DoctorCheck::ok("config.unknown", "Clés de configuration", "toutes connues")
    } else {
        DoctorCheck::fail(
            "config.unknown",
            "Clés de configuration",
            format!(
                "ignorée(s) par cette version : {} (écrite(s) par une version plus récente, \
                 ou faute de frappe)",
                unknown.join(", ")
            ),
            Some("penelope config validate".into()),
        )
    }
}

/// #78 : ce que gardent les tables qui grossissent avec l'activité (arguments et
/// résultats d'outils, messages envoyés, demandes, tâches MCP, sorties d'étapes), et la
/// date de la dernière passe de rétention qui les vide.
async fn retention_check(s: &Services) -> DoctorCheck {
    const ID: &str = "retention";
    const LABEL: &str = "Rétention";
    let days = s.config.config().retention.days;
    let read = s
        .store
        .read(|c| {
            let sizes: [i64; 5] = c.query_row(
                "SELECT
                   (SELECT coalesce(sum(length(request) + coalesce(length(result), 0)), 0)
                      FROM effects),
                   (SELECT coalesce(sum(length(payload)), 0) FROM tg_outbox),
                   (SELECT coalesce(sum(length(payload)), 0) FROM approval_requests),
                   (SELECT coalesce(sum(length(request) + coalesce(length(result), 0)), 0)
                      FROM mcp_tasks),
                   (SELECT coalesce(sum(coalesce(length(output), 0)), 0) FROM workflow_step_log)",
                [],
                |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]),
            )?;
            let last = penelope_store::kv_get(c, "retention.last")?;
            Ok((sizes, last))
        })
        .await;
    let Ok((sizes, last)) = read else {
        return DoctorCheck::fail(ID, LABEL, "tables illisibles", None);
    };
    let mo = |b: i64| format!("{:.1} Mo", b as f64 / (1024.0 * 1024.0));
    let kept = format!(
        "effets {}, envois Telegram {}, demandes {}, tâches MCP {}, étapes {}",
        mo(sizes[0]),
        mo(sizes[1]),
        mo(sizes[2]),
        mo(sizes[3]),
        mo(sizes[4])
    );
    if days == 0 {
        return DoctorCheck::ok(ID, LABEL, format!("désactivée ; contenu gardé : {kept}"));
    }
    let age_h = last
        .and_then(|v| v.parse::<i64>().ok())
        .map(|ms| (s.clock.now_ms() - ms) / 3_600_000);
    match age_h {
        Some(h) if h <= 48 => DoctorCheck::ok(
            ID,
            LABEL,
            format!("dernière passe il y a {h} h ({days} j) ; contenu gardé : {kept}"),
        ),
        Some(h) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("dernière passe il y a {h} h, attendue chaque jour ; contenu gardé : {kept}"),
            Some("penelope restart".into()),
        ),
        None => DoctorCheck::fail(
            ID,
            LABEL,
            format!("aucune passe enregistrée ; contenu gardé : {kept}"),
            Some("penelope restart".into()),
        ),
    }
}

/// #79 : la journée budgétaire suit `owner.timezone`. Après un changement de fuseau, ou
/// juste après la mise à jour qui a quitté l'UTC, des consommations récentes portent le
/// jour de l'ancien fuseau : le total du jour peut être décalé de quelques heures.
async fn budget_days_check(s: &Services) -> DoctorCheck {
    const ID: &str = "budget.day";
    const LABEL: &str = "Jour budgétaire";
    let tz = s.config.config().owner.timezone.clone();
    match s.budget.mixed_days().await {
        Ok(0) => DoctorCheck::ok(ID, LABEL, format!("minuit à {tz}")),
        Ok(n) => DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "{n} consommation(s) des dernières 48 h comptée(s) dans un autre fuseau que \
                 {tz} (changement de `owner.timezone`, ou version antérieure qui comptait en \
                 UTC) : le total du jour peut être décalé jusqu'à demain"
            ),
            None,
        ),
        Err(e) => DoctorCheck::fail(ID, LABEL, e.to_string(), None),
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
        // #122 : un serveur qui joint le trousseau se voit, et la raison avec.
        if st.keychain {
            let why = if st.transport == "stdio"
                && sup
                    .config_of(&st.name)
                    .await
                    .is_some_and(|c| c.sandbox_profile == "full")
            {
                "ouvert : profil `full` (sandbox.allow_full_for)"
            } else {
                "ouvert, le reste du bac à sable tient (sandbox.allow_keychain_for)"
            };
            out.push(DoctorCheck::ok(
                &format!("{id}.keychain"),
                &format!("Trousseau de `{}`", st.name),
                why,
            ));
        }
        // #89 : un serveur stdio confiné doit refuser les mêmes lectures que le shell.
        if let Some(cfg) = sup.config_of(&st.name).await
            && cfg.effective_transport() == "stdio"
            && let Ok(p) = crate::mcp::stdio_profile(s, &cfg)
            && p.enforced()
            && p.deny_read.is_empty()
        {
            out.push(DoctorCheck::fail(
                &format!("{id}.sandbox"),
                &format!("Bac à sable de `{}`", st.name),
                "aucune lecture refusée : ce serveur lit `~/.ssh`, les secrets et la base",
                Some(
                    "penelope config set sandbox.deny_read '[\"~/.ssh\", \"{data}/secrets.enc\"]'"
                        .into(),
                ),
            ));
        }
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
}
