//! Mode d'approbation d'une session (issue #111) : ce qui part sans demande.
//!
//! ```text
//!  ask    demander tout : même une lecture du shell attend le propriétaire
//!  reads  lectures sans demande (défaut), le reste selon la politique et les règles
//!  auto   tout sans demande, sauf le destructif et ce qu'un serveur MCP impose
//! ```

use crate::runtime::Services;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    Ask,
    Reads,
    Auto,
}

impl ApprovalMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "ask" | "demander" | "tout" => ApprovalMode::Ask,
            "reads" | "lectures" | "defaut" | "défaut" => ApprovalMode::Reads,
            "auto" => ApprovalMode::Auto,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalMode::Ask => "ask",
            ApprovalMode::Reads => "reads",
            ApprovalMode::Auto => "auto",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ApprovalMode::Ask => "demander tout",
            ApprovalMode::Reads => "lectures sans demande",
            ApprovalMode::Auto => "tout sauf le destructif",
        }
    }
}

fn key(session_id: &str) -> String {
    format!("session.approval_mode.{session_id}")
}

/// Mode d'une session : le sien s'il a été choisi, sinon `tools.approval_mode`.
pub async fn of_session(s: &Services, session_id: &str) -> ApprovalMode {
    let k = key(session_id);
    let own = s
        .store
        .read(move |c| penelope_store::kv_get(c, &k))
        .await
        .ok()
        .flatten()
        .and_then(|v| ApprovalMode::parse(&v));
    own.or_else(|| ApprovalMode::parse(&s.config.config().tools.approval_mode))
        .unwrap_or(ApprovalMode::Reads)
}

/// Fixe le mode d'une session ; `None` : retour au mode de la configuration.
pub async fn set(
    s: &Services,
    session_id: &str,
    mode: Option<ApprovalMode>,
) -> penelope_store::Result<()> {
    let k = key(session_id);
    s.store
        .write(move |tx| {
            match mode {
                Some(m) => penelope_store::kv_set(tx, &k, m.as_str())?,
                None => {
                    tx.execute("DELETE FROM kv WHERE k = ?1", [&k])?;
                }
            }
            Ok(())
        })
        .await
}

/// Autorisation déclarée d'avance dans la configuration (issue #111) : une commande
/// `shell_exec` d'une famille de `tools.shell_allow`, ou de `tools.shell_allow_network`
/// quand elle demande le réseau. Renvoie la raison, pour la trace.
pub fn declared_allow(
    cfg: &penelope_kernel::config::Config,
    tool: &str,
    args: &serde_json::Value,
) -> Option<String> {
    if tool != "shell_exec" {
        return None;
    }
    let command = args.get("command").and_then(|v| v.as_str())?.trim();
    let (families, key) = if crate::executor::wants_network(tool, args) {
        (&cfg.tools.shell_allow_network, "tools.shell_allow_network")
    } else {
        (&cfg.tools.shell_allow, "tools.shell_allow")
    };
    families
        .iter()
        .find(|f| penelope_hitl::policy::command_matches(f.trim(), command))
        .map(|f| format!("autorisé d'avance : famille « {f} » de `{key}`"))
}

/// Ce qui rend une règle inutile, pour `penelope policies` et `/policies` (issue #111) :
/// une famille qui ne peut rien couvrir, une lecture déjà autorisée d'office, ou aucun
/// usage depuis une semaine.
pub fn rule_note(r: &penelope_hitl::PolicyRule, now_ms: i64) -> Option<String> {
    use penelope_hitl::policy::{CHAINING, CMD_PREFIX_OP};
    if r.tool.as_deref() == Some("shell_exec")
        && let Some(family) = r
            .arg_match
            .as_ref()
            .and_then(|p| p.get("command"))
            .and_then(|c| c.get(CMD_PREFIX_OP))
            .and_then(|f| f.as_str())
    {
        if family.contains(CHAINING) || family.contains(['"', '\'', '=']) {
            return Some(
                "ne peut jamais s'appliquer : famille issue d'une commande composée".into(),
            );
        }
        if matches!(family, "cd" | "export" | "set" | "unset" | "source" | ".") {
            return Some(format!(
                "inutile : « {family} » seul ne fait rien, il n'arrive qu'en commande composée"
            ));
        }
        let network = r.arg_match.as_ref().and_then(|p| p.get("network"))
            == Some(&serde_json::Value::Bool(true));
        if !network && penelope_tools::shell::is_read_command(family) {
            return Some("inutile : les lectures passent sans demande".into());
        }
    }
    let created = chrono::DateTime::parse_from_rfc3339(&r.created_at)
        .map(|t| t.timestamp_millis())
        .unwrap_or(now_ms);
    if r.hits == 0 && now_ms - created > 7 * 86_400_000 {
        return Some("jamais utilisée depuis sa création, il y a plus d'une semaine".into());
    }
    None
}
