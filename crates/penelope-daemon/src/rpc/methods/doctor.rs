//! Contrôles de `doctor` qui restent au daemon quand le diagnostic sort dans
//! `penelope-ops` (épopée #208, T28) : ils lisent des modules du daemon (`tool_jobs`,
//! `prompt_snapshot`, `history`) ou l'hôte MCP (`stdio_profile`), dont ops ne dépend pas.

use crate::runtime::Services;
use penelope_kernel::api::DoctorCheck;

/// Contrôles du daemon ajoutés à `doctor::run` : stabilité du prompt, journal, jobs
/// d'outils.
pub(super) async fn daemon_checks(s: &Services) -> Vec<DoctorCheck> {
    vec![
        prompt_stability_check(s).await,
        crate::history::doctor_check(s).await,
        tool_jobs_check(s).await,
    ]
}

/// Serveurs MCP : état de chacun, secrets manquants, déclarations invalides.
pub async fn mcp_checks(s: &Services, sup: &dyn crate::ports::McpAdmin) -> Vec<DoctorCheck> {
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

#[cfg(test)]
mod tests;
