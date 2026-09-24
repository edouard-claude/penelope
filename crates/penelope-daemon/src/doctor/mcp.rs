//! Contrôles des serveurs MCP.

use super::*;

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
