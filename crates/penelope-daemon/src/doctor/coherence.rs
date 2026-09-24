//! Contrôles de cohérence : configuration, bac à sable, canaux, rétention, prompt.

use super::*;

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

/// #68 : un bac à sable qui lit tout le disque ne retient ni les clés SSH ni les jetons.
pub(super) fn sandbox_reads_check(s: &Services) -> DoctorCheck {
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
    let refused: Vec<String> = crate::helpers::seen_chats(s)
        .await
        .iter()
        .filter(|c| !c["id"].as_i64().is_some_and(|id| allowed.contains(&id)))
        .take(5)
        .map(|c| {
            format!(
                "{} « {} » `{}` (vu le {})",
                c["type"].as_str().unwrap_or("?"),
                c["title"].as_str().unwrap_or_default(),
                crate::helpers::shown(&c["id"]),
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

/// #44 : une panique dans une closure d'écriture est rattrapée, mais elle dit qu'un
/// chemin d'écriture est cassé. Le compteur remonte dans `doctor`.
pub(super) fn writer_panics_check() -> DoctorCheck {
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
pub(super) fn config_unknown_check(s: &Services) -> DoctorCheck {
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
pub(super) async fn retention_check(s: &Services) -> DoctorCheck {
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

/// #204 : un job d'outil tourne hors d'un tour. Deux façons de mal finir : un job qui
/// traîne parce que personne ne l'a relu, et un job de la base que plus aucun jeton de ce
/// processus ne couvre — ce que laisse un redémarrage. Les deux sont nommés, avec leur âge
/// et leur session.
pub(super) async fn tool_jobs_check(s: &Services) -> DoctorCheck {
    const ID: &str = "tool_jobs";
    const LABEL: &str = "Jobs d'outils";
    /// Au-delà, un job n'est plus une commande longue mais un oubli.
    const OLD_S: i64 = 3_600;
    let cfg = s.config.config();
    let now = s.clock.now_ms();
    let live = crate::tool_jobs::store(s).live().await.unwrap_or_default();
    let here = s.jobs.len();
    if live.is_empty() {
        return DoctorCheck::ok(ID, LABEL, "aucun job en cours".to_string());
    }
    let detail = |extra: &str| {
        format!(
            "{} job(s) en cours ({} dans ce processus), plafonds {}/session et {} au              total{extra} : {}",
            live.len(),
            here,
            cfg.tools.jobs_per_session,
            cfg.tools.jobs_total,
            live.iter()
                .map(|j| format!(
                    "`{}` {} ({} s, session {})",
                    j.id,
                    j.tool,
                    j.age_s(now),
                    j.session_id
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    // Un job de la base sans jeton ici n'est plus interruptible : c'est le reste d'un
    // processus mort, que la reprise au démarrage aurait dû trancher.
    if live.len() > here {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail(", dont certains sans processus"),
            Some("penelope restart".into()),
        );
    }
    let vieux = live.iter().filter(|j| j.age_s(now) > OLD_S).count();
    if vieux > 0 {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail(&format!(", dont {vieux} de plus d'une heure")),
            Some("penelope jobs".into()),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail(""))
}

/// #205 : un préfixe stable est la condition du coût (#17). Quand il bouge plusieurs fois
/// par jour **hors** pause et hors compaction, chaque tour repaie son prompt entier : c'est
/// la cause n° 1 des ratés de cache, et l'instantané dit désormais quelle tuile bouge.
pub(super) async fn prompt_stability_check(s: &Services) -> DoctorCheck {
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
pub(super) async fn budget_days_check(s: &Services) -> DoctorCheck {
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

pub(super) fn clock_check(s: &Services) -> DoctorCheck {
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

pub(super) async fn reachable_check(host: &str) -> DoctorCheck {
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
