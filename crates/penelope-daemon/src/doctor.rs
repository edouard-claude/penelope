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
    checks.push(prompt_stability_check(s).await);

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

/// Un alias qui sert un rôle d'extraction structurée : il rend du JSON, pas de la prose,
/// et son budget de sortie ne doit pas partir en raisonnement (issue #152).
pub fn alias_serves_extraction(cfg: &penelope_kernel::config::Config, alias: &str) -> bool {
    ["compaction", "memory_review"]
        .iter()
        .any(|role| cfg.role_alias(role) == alias)
}

/// #152 : ce que le modèle du rôle `compaction` fera de son budget de sortie.
///
/// La nuit du 20/09, il a dépensé 8 000 tokens à réfléchir sur un seul candidat et n'a
/// rendu aucune opération : `lightest_effort` renvoyait `high` parce qu'OpenRouter ne
/// déclare que `["xhigh","high"]` pour ce modèle. Une nuit sur deux échouait.
pub async fn reasoning_effort_check(s: &Services) -> DoctorCheck {
    const ID: &str = "reasoning_effort";
    const LABEL: &str = "Raisonnement de la consolidation";
    let cfg = s.config.config();
    let alias = cfg.role_alias("compaction");
    let Some(model) = cfg.alias_model(&alias).map(str::to_string) else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!("aucun modèle pour l'alias `{alias}` du rôle `compaction`"),
            Some(format!("penelope model set {alias} <fournisseur:modèle>")),
        );
    };
    let info = s
        .catalog
        .get(model.split_once(':').map_or(model.as_str(), |(_, m)| m));
    // Ce qui partira vraiment : le budget quand le raisonnement est gardé (le défaut),
    // l'effort quand la configuration l'éteint (issue #152).
    // Inconnu du catalogue : on suppose qu'il réfléchit, comme la passe (issue #152).
    let sent = if info.as_ref().is_some_and(|i| !i.reasons()) {
        "modèle sans raisonnement".to_string()
    } else if cfg.memory.consolidation_reasoning == "off" {
        match info.as_ref().and_then(|i| i.lightest_effort()).as_deref() {
            Some("none") => "raisonnement éteint".to_string(),
            Some(e) => format!("éteint demandé, mais le modèle l'impose : effort `{e}`"),
            None => "raisonnement éteint".to_string(),
        }
    } else {
        format!(
            "raisonnement gardé, budget jusqu'à {} jetons",
            cfg.memory.consolidation_reasoning_tokens
        )
    };
    // Part de raisonnement réellement observée sur sept jours.
    let since = (s.clock.now_utc() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let (reasoning, completion): (i64, i64) = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(reasoning), 0), COALESCE(SUM(completion), 0)
                 FROM usage WHERE ts >= ?1 AND role = 'consolidation'",
                [since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap_or((0, 0));
    if completion == 0 {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("{alias} : {sent} ; aucune passe en 7 jours"),
        );
    }
    let share = (reasoning as f64 * 100.0 / completion as f64).round() as i64;
    let detail = format!("{alias} : {sent} ; {share}% de la sortie en raisonnement sur 7 jours");
    // Gardé et budgété, une part haute est normale (la nuit réussie du 19/09 était à
    // 73 %) ; c'est éteint qu'elle trahit un modèle qui n'écoute pas.
    if cfg.memory.consolidation_reasoning == "off" && share > 10 {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(format!(
                "le raisonnement est éteint mais le modèle réfléchit quand même : lui donner \
                 un modèle qui l'accepte (`penelope model set {alias} …`), ou repasser à \
                 `memory.consolidation_reasoning = \"auto\"`"
            )),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #152 : la consolidation tombe à l'heure où le Mac dort. La nuit du 21/09, un trou de
/// journal de douze minutes (« Now drawing from 'Battery Power' ») a coupé un lot en
/// vol ; la passe attend maintenant le retour du réseau, mais une machine sur batterie à
/// l'heure du rêve reste un avertissement.
pub async fn dream_power_check(s: &Services) -> DoctorCheck {
    const ID: &str = "dream_power";
    const LABEL: &str = "Alimentation à l'heure du rêve";
    let cfg = s.config.config();
    let cron = cfg.memory.dreaming_cron.clone();
    // Sonde bloquante (`pmset`) : hors du fil asynchrone.
    let platform = s.platform.clone();
    let now = s.clock.now_ms() / 1_000;
    let on_ac = tokio::task::spawn_blocking(move || platform.host_status(now).on_ac_power())
        .await
        .ok()
        .flatten();
    let detail = match on_ac {
        Some(false) => format!("sur batterie ; consolidation prévue à `{cron}`"),
        Some(true) => format!("sur secteur ; consolidation prévue à `{cron}`"),
        None => format!("alimentation inconnue ; consolidation prévue à `{cron}`"),
    };
    if on_ac == Some(false) {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(
                "une machine sur batterie s'endort et coupe un lot en vol : la brancher avant \
                 la nuit, ou décaler `memory.dreaming_cron`"
                    .into(),
            ),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #153 : le rédacteur rend, sur un corpus fixe, dans un délai.
///
/// Une récursion infinie sur une référence `${SECRET:` orpheline a fait déborder la pile
/// du thread écrivain au démarrage, et revenir en arrière deux versions de suite. Le
/// processus était abattu (`abort`) avant d'écrire une ligne de journal : rien ne pouvait
/// le dire. Ce contrôle passe le corpus dans un thread à **petite** pile, à part : une
/// boucle y meurt sans emporter le daemon, et l'absence de réponse est l'alerte.
pub async fn redactor_check() -> DoctorCheck {
    const ID: &str = "redactor";
    const LABEL: &str = "Rédacteur de secrets";
    const CORPUS: &[&str] = &[
        "${SECRET:telegram_bot_token}",
        "secrets via ${SECRET:…}",
        "${SECRET:timeperformance</pre>",
        "${SECRET:",
        "${SECRET:a} puis ${SECRET:",
        "sk-proj-0123456789abcdef0123456789abcdef0123456789abcdef",
        "deadbeef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "{\"token\": \"ghp_0123456789abcdef0123456789abcdef0123\"}",
        "ligne ordinaire, sans rien à masquer",
    ];
    // Le travail part dans un processus léger à lui : un débordement de pile abat le
    // processus entier, on ne peut donc pas l'exécuter ici et espérer le rattraper. Le
    // délai attrape aussi bien une boucle infinie qu'une lenteur pathologique.
    let work = tokio::task::spawn_blocking(|| {
        for c in CORPUS {
            let _ = penelope_observe::redact(c);
        }
    });
    match tokio::time::timeout(std::time::Duration::from_secs(5), work).await {
        Ok(Ok(())) => DoctorCheck::ok(
            ID,
            LABEL,
            format!("{} formes rendues, références orphelines comprises", CORPUS.len()),
        ),
        Ok(Err(e)) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("le rédacteur a échoué : {e}"),
            Some("`penelope doctor --json` et ouvrir une issue : rien ne doit faire échouer le rédacteur".into()),
        ),
        Err(_) => DoctorCheck::fail(
            ID,
            LABEL,
            "le rédacteur n'a pas rendu en 5 s sur un corpus de neuf textes".to_string(),
            Some(
                "boucle probable : le daemon mourra au prochain texte de cette forme. \
                 Ne pas mettre à jour avant correction"
                    .into(),
            ),
        ),
    }
}

/// #150 : la part des appels `shell_exec` qui collent plusieurs commandes. Sept jours
/// d'appels, pour voir la consigne « une commande par appel » agir — le 20/09, 383 des
/// 549 appels portaient un `&&`, et 129 des 166 cartes portaient sur une ligne collée.
///
/// Une `liste` (`a && b`) est autorisable une fois pour toutes ; une `composee` (`;`,
/// `$(…)`, redirection) ne l'est pas, et redemande à chaque appel : c'est elle qu'on
/// compte.
pub async fn glued_lines_check(s: &Services) -> DoctorCheck {
    const ID: &str = "shell_lines";
    const LABEL: &str = "Lignes de commande collées";
    let since = (s.clock.now_utc() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(String, i64)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT COALESCE(json_extract(payload, '$.shape'), 'inconnue'), COUNT(*)
                 FROM events
                 WHERE kind = 'tool.result' AND ts >= ?1
                   AND json_extract(payload, '$.tool') = 'shell_exec'
                 GROUP BY 1",
            )?;
            let r = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(r.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    let count = |k: &str| {
        rows.iter()
            .find(|(s, _)| s == k)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    let (simple, list, composed) = (count("simple"), count("liste"), count("composee"));
    let total = simple + list + composed;
    if total == 0 {
        return DoctorCheck::ok(ID, LABEL, "aucun appel `shell_exec` en sept jours");
    }
    let share = |n: i64| (n as f64 * 100.0 / total as f64).round() as i64;
    let detail = format!(
        "{total} appels en 7 jours : {}% une commande, {}% listes `&&`, {}% composées",
        share(simple),
        share(list),
        share(composed)
    );
    // Une ligne composée sur cinq : la consigne n'agit pas, et chaque appel redemande.
    if share(composed) > 20 {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(
                "une commande composée ne peut porter aucune règle : elle redemande à \
                 chaque fois. Demander une commande par appel"
                    .into(),
            ),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #149 : un formulaire en cours retient le texte tapé dans son sujet. Oublié ouvert, il
/// avale les messages du propriétaire sans que rien ne le dise. Au-delà d'une heure, il
/// est signalé avec son sujet.
pub async fn open_forms_check(s: &Services) -> DoctorCheck {
    const ID: &str = "telegram_forms";
    const LABEL: &str = "Formulaires Telegram en cours";
    const STALE_MS: i64 = 3_600_000;
    let rows: Vec<(String, String)> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT k, v FROM kv WHERE k LIKE 'tg.form.%' AND v != ''")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(r.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    let now = s.clock.now_ms();
    let mut stale: Vec<String> = Vec::new();
    for (key, raw) in &rows {
        let pending: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        let since = pending["since"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis());
        // Sans horodatage (formulaire ouvert avant #149), on ne présume rien.
        if since.is_some_and(|t| now - t > STALE_MS) {
            let place = key.rsplit_once('.').map(|(_, t)| t).unwrap_or_default();
            stale.push(format!(
                "{} (sujet {place}, depuis {})",
                pending["choice"].as_str().unwrap_or("formulaire"),
                pending["since"].as_str().unwrap_or("?")
            ));
        }
    }
    if stale.is_empty() {
        return DoctorCheck::ok(ID, LABEL, format!("{} en cours, aucun oublié", rows.len()));
    }
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} ouvert(s) depuis plus d'une heure : {}. Ils retiennent le texte tapé dans \
             leur sujet",
            stale.len(),
            stale.join(" ; ")
        ),
        Some("les abandonner depuis leur carte (« ✖️ Abandonner »)".into()),
    )
}

/// #158 : l'intégrité de la base, **confirmée avant d'accuser**.
///
/// Le 21/09, trois `doctor` d'affilée ont rendu « malformed inverted index for FTS5 », sur
/// une table différente à chaque fois, avec pour seule correction proposée
/// `penelope restore --latest` : onze heures de conversations perdues si le propriétaire
/// la suivait — et l'option n'existe même pas (`penelope restore <fichier>`). Le fichier
/// était intègre : sept connexions neuves le relisaient sans rien trouver. Seul un lecteur
/// du pool, ouvert depuis des heures, mentait.
///
/// D'où trois verdicts au lieu d'un, et jamais de restauration proposée pour un index.
pub fn integrity_check_of(s: &Services) -> DoctorCheck {
    const ID: &str = "db";
    const LABEL: &str = "Base SQLite";
    let report = match s.store.integrity_report() {
        Ok(r) => r,
        Err(e) => return DoctorCheck::fail(ID, LABEL, e.to_string(), None).critical(),
    };

    if report.sound() && report.pool == "ok" {
        return DoctorCheck::ok(ID, LABEL, "intègre");
    }

    // Le fichier dément le lecteur : ce n'est pas la base qui est malade, c'est une
    // connexion. Elle vient d'être fermée ; les suivantes repartiront d'une neuve.
    if report.reader_lied() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "le fichier est intègre — un lecteur du pool rendait un verdict faux \
                 (connexion ouverte depuis {} s, {} requêtes servies) : « {} ». Cette \
                 connexion a été fermée et sera remplacée ; un redémarrage les renouvelle \
                 toutes.",
                report.reader.age_s,
                report.reader.served,
                first_line(&report.pool)
            ),
            Some("penelope restart".into()),
        );
    }

    // Les deux sont d'accord, mais sur des index dérivés seulement : rien de perdu.
    if let Some(tables) = report.fts_only() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "index de recherche à reconstruire ({}) — aucune donnée première n'y vit, \
                 la recherche se rebâtit des messages et du vault",
                tables.join(", ")
            ),
            Some("penelope store rebuild".into()),
        );
    }

    // Là seulement, la base est vraiment atteinte. La consigne dit ce qu'on perd, et
    // nomme une commande qui existe : `restore --latest` n'a jamais existé.
    let archive = latest_archive(&s.platform.dirs.data().join("backups"));
    let lost = match &archive {
        Some(a) => format!(" ; dernière sauvegarde : {a}, tout ce qui suit serait perdu"),
        None => " ; aucune sauvegarde n'a été trouvée".to_string(),
    };
    DoctorCheck::fail(
        ID,
        LABEL,
        format!("{}{lost}", report.verdict()),
        Some(match &archive {
            Some(a) => format!("penelope stop, puis penelope restore {a}"),
            None => "penelope stop, puis penelope restore <fichier de sauvegarde>".into(),
        }),
    )
    .critical()
}

/// La sauvegarde la plus récente du dépôt local, par son nom (horodaté).
fn latest_archive(dir: &std::path::Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(".tar.gz.enc").then_some(n)
        })
        .collect();
    names.sort();
    names.pop()
}

/// La première ligne d'un verdict, pour un message qui tient sur une ligne.
fn first_line(verdict: &str) -> &str {
    verdict.lines().next().unwrap_or(verdict).trim()
}

/// #148 : le magasin de secrets acceptait les clés d'API et refusait un `Grant` de 4 Ko,
/// en recopiant ses jetons dans le message d'erreur. Le contrôle écrit, relit et efface
/// un secret de 8 Ko : la panne se voit avant qu'un secret la rencontre.
pub fn secret_roundtrip_check(s: &Services) -> DoctorCheck {
    secret_roundtrip_of(s.platform.secrets.as_ref())
}

/// Le contrôle, sur un magasin quelconque : c'est ce qui le rend vérifiable sans toucher
/// au Trousseau de la machine.
pub fn secret_roundtrip_of(store: &dyn penelope_platform::secrets::SecretStore) -> DoctorCheck {
    const ID: &str = "secret_roundtrip";
    const LABEL: &str = "Magasin de secrets : aller-retour de 8 Ko";
    const NAME: &str = "penelope.doctor.roundtrip";
    let value = "x".repeat(8 * 1024);
    const UNLOCK: &str = "déverrouiller le Trousseau (`security unlock-keychain`), ou forcer \
                          le repli fichier chiffré avec `PENELOPE_SECRETS=file`";
    // L'essai ne laisse rien derrière lui, quelle que soit l'étape qui échoue.
    let fail = |detail: String, fix: &str| {
        let _ = store.delete(NAME);
        DoctorCheck::fail(ID, LABEL, detail, Some(fix.to_string()))
    };
    if let Err(e) = store.set(NAME, &value) {
        return fail(
            format!(
                "écriture refusée : {}",
                penelope_observe::redact(&e.to_string())
            ),
            UNLOCK,
        );
    }
    match store.get(NAME) {
        Ok(Some(back)) if back == value => {}
        // Le magasin a répondu, et il a répondu autre chose : ce n'est pas un Trousseau
        // verrouillé — proposer de le déverrouiller envoie chercher là où il n'y a rien
        // (issue #157). La forme de ce qui revient nomme le vrai coupable.
        Ok(Some(back)) => {
            let hexa = back.len() >= 2
                && back.len().is_multiple_of(2)
                && back.bytes().all(|b| b.is_ascii_hexdigit());
            return fail(
                format!(
                    "relecture altérée : {} octets écrits, {} relus{}. Le magasin répond, \
                     il rend une autre valeur : le Trousseau n'est pas en cause.",
                    value.len(),
                    back.len(),
                    if hexa { ", en hexadécimal" } else { "" }
                ),
                "signaler l'anomalie : un secret long est écrit puis relu faux (issue \
                 #157) ; en attendant, `PENELOPE_SECRETS=file` écrit hors du Trousseau",
            );
        }
        Ok(None) => return fail("écrit puis introuvable".into(), UNLOCK),
        // `security` n'a pas répondu : verrouillé, absent, ou refusé.
        Err(e) => {
            return fail(
                format!(
                    "relecture refusée : {}",
                    penelope_observe::redact(&e.to_string())
                ),
                UNLOCK,
            );
        }
    }
    if let Err(e) = store.delete(NAME) {
        return fail(
            format!(
                "suppression refusée : {}",
                penelope_observe::redact(&e.to_string())
            ),
            UNLOCK,
        );
    }
    DoctorCheck::ok(
        ID,
        LABEL,
        format!("{} : 8 Ko écrits, relus, effacés", store.backend()),
    )
}

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
fn brew_install_command(missing: &[String]) -> String {
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
            let sizes: [i64; 6] = c.query_row(
                "SELECT
                   (SELECT coalesce(sum(length(request) + coalesce(length(result), 0)), 0)
                      FROM effects),
                   (SELECT coalesce(sum(length(payload)), 0) FROM tg_outbox),
                   (SELECT coalesce(sum(length(payload)), 0) FROM approval_requests),
                   (SELECT coalesce(sum(length(request) + coalesce(length(result), 0)), 0)
                      FROM mcp_tasks),
                   (SELECT coalesce(sum(coalesce(length(output), 0)), 0) FROM workflow_step_log),
                   (SELECT coalesce(sum(length(rendered)), 0) FROM prompt_snapshots)",
                [],
                |r| {
                    Ok([
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ])
                },
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
        "effets {}, envois Telegram {}, demandes {}, tâches MCP {}, étapes {}, prompts {}",
        mo(sizes[0]),
        mo(sizes[1]),
        mo(sizes[2]),
        mo(sizes[3]),
        mo(sizes[4]),
        mo(sizes[5])
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

/// #205 : un préfixe stable est la condition du coût (#17). Quand il bouge plusieurs fois
/// par jour **hors** pause et hors compaction, chaque tour repaie son prompt entier : c'est
/// la cause n° 1 des ratés de cache, et l'instantané dit désormais quelle tuile bouge.
async fn prompt_stability_check(s: &Services) -> DoctorCheck {
    const ID: &str = "prompt.stability";
    const LABEL: &str = "Stabilité du prompt système";
    /// Au-delà, ce n'est plus un rechargement isolé mais un préfixe qui ne tient pas.
    const MAX_PER_DAY: i64 = 5;
    let since = (s.clock.now_utc() - chrono::Duration::days(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(String, i64)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT miss_cause, COUNT(DISTINCT system_hash)
                 FROM usage
                 WHERE ts >= ?1 AND miss_cause LIKE 'prefixe%' AND system_hash IS NOT NULL
                 GROUP BY miss_cause",
            )?;
            let r = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(r.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    let total: i64 = rows.iter().map(|(_, n)| n).sum();
    let (rows_count, bytes) = crate::prompt_snapshot::weight_bytes(s)
        .await
        .unwrap_or((0, 0));
    let kept = format!(
        "{rows_count} prompts gardés, {:.1} Mo",
        bytes as f64 / (1024.0 * 1024.0)
    );
    if total <= MAX_PER_DAY {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("{total} changement(s) de préfixe en 24 h ; {kept}"),
        );
    }
    // La cause la plus fréquente porte déjà le nom de la tuile (`prefixe:T1`).
    let worst = rows
        .iter()
        .max_by_key(|(_, n)| *n)
        .map(|(cause, _)| penelope_kernel::budget::miss_label(cause))
        .unwrap_or_default();
    DoctorCheck::fail(
        ID,
        LABEL,
        format!("{total} changements de préfixe en 24 h, surtout : {worst} ; {kept}"),
        Some("penelope usage --by miss --since <jour> pour voir quelle tuile bouge".into()),
    )
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
    // L'URL enregistrée chez le fournisseur doit être exactement celle-ci.
    let mcfg = &s.config.config().mcp;
    let redirect = if mcfg.oauth_redirect_mode == "public_callback" {
        mcfg.public_callback_url.clone()
    } else {
        penelope_mcp::oauth::loopback_redirect(&mcfg.callback_host, mcfg.callback_port)
    };
    out.push(DoctorCheck::ok(
        "mcp.oauth.redirect",
        "Retour OAuth MCP",
        format!("{redirect} (à enregistrer tel quel chez le fournisseur)"),
    ));
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
            "reasoning_effort",
            "dream_power",
            "prompt.stability",
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

    /// #205 : un préfixe qui bouge plusieurs fois par jour hors pause et hors compaction
    /// est la cause n° 1 des ratés de cache (#17). `doctor` le dit, et nomme la tuile.
    #[tokio::test]
    async fn an_unstable_system_prompt_is_reported_with_its_tile() {
        let (_d, s) = services().await;
        let now = s.clock.now_rfc3339();
        let day = now[..10].to_string();
        s.store
            .write(move |tx| {
                for i in 0..6 {
                    tx.execute(
                        "INSERT INTO usage(ts, day, session_id, model, provider, prompt,
                            completion, cost_usd, miss_cause, system_hash)
                         VALUES(?1,?2,'s1','m','p',10000,10,0.1,'prefixe:T1',?3)",
                        penelope_store::rusqlite::params![now, day, format!("h{i}")],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let checks = run(&s).await;
        let c = checks.iter().find(|c| c.id == "prompt.stability").unwrap();
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("6"), "{}", c.detail);
        assert!(c.detail.contains("index des capacités"), "{}", c.detail);
    }

    /// Sur une instance calme, le contrôle est vert et dit le poids gardé.
    #[tokio::test]
    async fn a_quiet_instance_keeps_a_green_prompt_check() {
        let (_d, s) = services().await;
        let checks = run(&s).await;
        let c = checks.iter().find(|c| c.id == "prompt.stability").unwrap();
        assert!(c.ok, "{}", c.detail);
        let r = checks.iter().find(|c| c.id == "retention").unwrap();
        assert!(r.detail.contains("prompts"), "{}", r.detail);
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
