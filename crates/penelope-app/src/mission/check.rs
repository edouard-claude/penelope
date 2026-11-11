//! La vérification d'une mission (`--check`) : lancée quand le tour répond « terminé »,
//! sur cette machine ou sur l'hôte de la mission.
//!
//! Elle peut durer vingt minutes, après la fin du tour : plus rien, alors, ne portait un
//! jeton d'annulation. Un `/mission stop` pendant ce temps écrivait « arrêtée », la
//! vérification continuait, et son verdict relançait la mission (#455). Elle s'inscrit
//! donc au registre des jobs de ce processus, sous le nom de sa session : `/mission stop`,
//! un blocage, `/stop` et l'arrêt du daemon tuent son groupe de processus.

use super::Mission;
use crate::services::Services;
use penelope_llm::CancelToken;
use std::time::Duration;

/// Délai de la commande de vérification.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Sortie de la vérification renvoyée au modèle, en caractères (la fin, là où est l'erreur).
const CHECK_TAIL: usize = 6_000;

/// Ce que rend une vérification.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Green,
    /// Rouge, avec la fin de sa sortie.
    Red(String),
    /// Interrompue avant son verdict : arrêt du propriétaire, blocage, arrêt du daemon.
    /// Un verdict arrivé en même temps ne compte pas : l'arrêt gagne.
    Interrupted,
}

/// Le nom de la vérification d'une session au registre des jobs en cours.
fn job(session_id: &str) -> String {
    format!("mission-check:{session_id}")
}

/// Interrompt la vérification en cours de la session, s'il y en a une.
pub(super) fn interrupt(s: &Services, session_id: &str) {
    s.jobs.cancel(&job(session_id));
}

/// Lance la vérification de `m`, interruptible tant qu'elle tourne.
pub(super) async fn verify(s: &Services, m: &Mission, check: &str) -> Verdict {
    let cancel = CancelToken::new();
    let id = job(&m.session_id);
    s.jobs.insert(&id, &m.session_id, cancel.clone());
    // Arrêtée entre la fin du tour et cette inscription : l'arrêt n'a trouvé personne à
    // interrompre, la vérification ne part pas.
    if !super::is_running(s, &m.session_id).await {
        s.jobs.forget(&id);
        return Verdict::Interrupted;
    }
    let checked = match &m.host {
        Some(host) => remote(s, host, check, m.cwd.as_deref(), &cancel).await,
        None => local(s, check, m.cwd.as_deref(), &cancel).await,
    };
    s.jobs.forget(&id);
    match checked {
        _ if cancel.is_cancelled() => Verdict::Interrupted,
        Ok(()) => Verdict::Green,
        Err(output) => Verdict::Red(output),
    }
}

/// La vérification d'une mission `--host` (#395), sur l'hôte, dans `cwd`.
pub async fn run_check_remote(
    s: &Services,
    host: &str,
    check: &str,
    cwd: Option<&str>,
) -> Result<(), String> {
    remote(s, host, check, cwd, &CancelToken::new()).await
}

async fn remote(
    s: &Services,
    host: &str,
    check: &str,
    cwd: Option<&str>,
    cancel: &CancelToken,
) -> Result<(), String> {
    let root = cwd.ok_or("une mission `--host` a toujours son `--cwd`")?;
    let req = remote_request(
        s,
        host,
        root,
        "shell_exec",
        // Réseau ouvert, comme la vérification locale : les dépendances d'un `cargo test`.
        serde_json::json!({"command": check, "network": true}),
        CHECK_TIMEOUT,
    );
    let out = crate::child::remote::call(s, host, &req, cancel)
        .await
        .map_err(|e| format!("vérification impossible : {e}"))?;
    let code = out["exitCode"].as_i64().unwrap_or(-1);
    if code == 0 {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        out["stdout"].as_str().unwrap_or_default(),
        out["stderr"].as_str().unwrap_or_default()
    );
    let chars: Vec<char> = text.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(CHECK_TAIL)..]
        .iter()
        .collect();
    Err(format!("code {code}\n{tail}"))
}

/// Un appel d'outil pour l'hôte d'une mission : sa racine, ses motifs masqués, et le
/// profil du shell (`full` seulement pour un hôte déclaré dans `runners.remote_full_for`).
pub fn remote_request(
    s: &Services,
    host: &str,
    root: &str,
    tool: &str,
    args: serde_json::Value,
    timeout: std::time::Duration,
) -> penelope_tools::remote::ToolRequest {
    let cfg = s.config.config();
    let full = cfg.runners.remote_full_for.iter().any(|h| h == host);
    penelope_tools::remote::ToolRequest {
        tool: tool.into(),
        network: args
            .get("network")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || cfg.sandbox.shell_network,
        args,
        root: root.into(),
        hide: cfg.sandbox.hide.clone(),
        // Sur cette machine (`@local`), les chemins développés ; sur un hôte, les entrées
        // en `~/…` et absolues, développées là-bas.
        deny_read: if host == crate::child::remote::LOCAL {
            crate::helpers::denied_reads(s)
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        } else {
            cfg.sandbox.deny_read.clone()
        },
        profile: if full {
            "full".into()
        } else {
            cfg.sandbox.default_profile.clone()
        },
        timeout_ms: timeout.as_millis() as u64,
        max_output_bytes: cfg.tools.max_output_bytes,
        env: Vec::new(),
    }
}

/// Lance la vérification, sans bac à sable (la commande est celle du propriétaire) ;
/// `Err` porte la fin de sa sortie.
pub async fn run_check(s: &Services, check: &str, cwd: Option<&str>) -> Result<(), String> {
    local(s, check, cwd, &CancelToken::new()).await
}

async fn local(
    s: &Services,
    check: &str,
    cwd: Option<&str>,
    cancel: &CancelToken,
) -> Result<(), String> {
    // Revérifié à chaque lancement : un lien posé depuis le départ ne le sort pas des
    // workspaces (TOCTOU, revue de sécurité du 14/10).
    let cwd = match cwd {
        None => None,
        Some(c) => Some(super::dir_of(s, c).ok_or_else(|| {
            format!("`{c}` n'est plus dans un workspace autorisé : vérification refusée")
        })?),
    };
    // Le bac à sable du shell (#366, revue de sécurité) : la commande est celle du
    // propriétaire, mais elle tourne dans un dépôt que l'agent a modifié. Le réseau est
    // ouvert (dépendances d'un `cargo test`), les lectures refusées restent refusées.
    let cfg = s.config.config();
    let dir = cwd.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
    let profile = penelope_tools::shell::profile_with_denied_reads(
        &cfg.sandbox.default_profile,
        &dir,
        true,
        &crate::helpers::denied_reads(s),
    );
    let out = penelope_tools::shell::exec(
        &s.platform.processes,
        check,
        penelope_tools::shell::ExecOptions {
            profile: Some(&profile),
            cwd: cwd.as_deref(),
            timeout: CHECK_TIMEOUT,
            max_output_bytes: 1024 * 1024,
            cancel: Some(cancel),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("vérification impossible : {e}"))?;
    if out.exit_code == 0 {
        return Ok(());
    }
    let text = format!("{}{}", out.stdout, out.stderr);
    let chars: Vec<char> = text.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(CHECK_TAIL)..]
        .iter()
        .collect();
    Err(format!("code {}\n{tail}", out.exit_code))
}
