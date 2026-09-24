//! Contrôles des secrets : caviardage, formulaires, intégrité, trousseau, journaux.

use super::*;

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
