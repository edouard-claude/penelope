//! Proposer, pas imposer (issue #260) : une capacité trouvée et non branchée est
//! proposée au propriétaire, une fois.
//!
//! « Safari expose un MCP : je le branche ? » Rien n'est écrit dans `mcp.d` ni dans
//! `[providers]` : le propriétaire branche, ou préfère un autre outil. Une capacité
//! proposée est retenue dans `kv` ([`KV_KEY`]) avec sa date et ne revient pas : un refus
//! n'est pas redemandé toutes les heures. `env_explore` montre cette date.

use super::{Capability, Environment};
use crate::ports::Messenger;
use crate::services::Services;
use std::collections::BTreeMap;

/// Capacités déjà proposées : identifiant → horodatage.
pub const KV_KEY: &str = "machine.proposed";

/// Ce qui mérite une proposition : un MCP exposé par une application et pas déclaré, un
/// serveur d'inférence qui répond avec des modèles et qu'aucun fournisseur ne vise. Déjà
/// proposé : jamais deux fois.
pub fn pending<'a>(
    env: &'a Environment,
    proposed: &BTreeMap<String, String>,
) -> Vec<&'a Capability> {
    env.capabilities
        .iter()
        .filter(|c| !c.declared && !proposed.contains_key(&c.id))
        .filter(|c| match c.kind.as_str() {
            "mcp" => c.origin == "application",
            "inference" => c.reachable == Some(true) && !c.models.is_empty(),
            _ => false,
        })
        .collect()
}

/// Le message au propriétaire.
pub fn message(caps: &[&Capability]) -> String {
    let mut lines =
        vec!["Sur cette machine, j'ai trouvé ce que je n'utilise pas encore :".to_string()];
    for c in caps {
        lines.push(match c.kind.as_str() {
            "mcp" => format!(
                "- {} expose un serveur MCP (`{}`) : je le branche ? Il suffit de le \
                 déclarer dans `mcp.d` (`command`, `args`), puis `penelope mcp add`.",
                c.name, c.via
            ),
            _ => format!(
                "- {} sert {} modèle(s) sur {} ({}) : je peux lui confier des rôles en local, \
                 par un fournisseur `[providers.…]`.",
                c.name,
                c.models.len(),
                c.via,
                c.models
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    lines.push(
        "Rien n'est branché sans toi. Si tu préfères un autre outil, dis-le : je ne \
         reproposerai pas celui-ci."
            .into(),
    );
    lines.join("\n")
}

/// Les capacités déjà proposées.
pub async fn proposed(s: &Services) -> BTreeMap<String, String> {
    s.kv_get(KV_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Propose au propriétaire ce que la dernière carte a trouvé de neuf. Retenu seulement si
/// le message est parti : sans canal, la proposition attend la passe suivante.
pub async fn propose(s: &Services, messenger: &dyn Messenger) -> anyhow::Result<usize> {
    let Some(env) = super::cached(s).await else {
        return Ok(0);
    };
    let mut done = proposed(s).await;
    let caps = pending(&env, &done);
    if caps.is_empty() {
        return Ok(0);
    }
    let origin = crate::helpers::owner_origin_of(s);
    messenger
        .send_text(&origin, &message(&caps))
        .await
        .map_err(|e| anyhow::anyhow!("proposition des capacités : {e}"))?;
    let at = s.clock.now_rfc3339();
    let n = caps.len();
    for c in caps {
        done.insert(c.id.clone(), at.clone());
    }
    s.kv_set(KV_KEY, &serde_json::to_string(&done)?).await?;
    Ok(n)
}
