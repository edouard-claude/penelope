//! Les serveurs MCP hors d'usage, tels que le propriétaire les apprend (#466).
//!
//! Un serveur passé en « autorisation requise » ou en panne durable est dit **une fois**,
//! à son entrée dans l'état, avec la commande qui le rebranche ; ensuite le digest du
//! matin le rappelle tant que ça dure, et `doctor` l'écrit dans la carte de
//! l'environnement. Avant, le daemon poussait chaque jour un lien d'autorisation valable
//! dix minutes : un lien mort quand il était lu, et rien pour une panne.
//!
//! ```text
//!  maintenance (60 s) ──► tick(états) ──► kv `mcp.blocked` ──► une alerte groupée
//!                                               │               (retenue aux heures calmes)
//!  digest du matin ◄──── digest_line ───────────┤
//!  doctor, carte de l'environnement ◄─ standing ┘
//! ```
//!
//! Ce qui ne ferme **pas** une entrée : `configured` et `connecting`. Un daemon qui
//! redémarre remet tous ses serveurs au repos sans rien savoir de leur jeton ; fermer
//! l'entrée là redirait la même alerte à chaque redémarrage.

use crate::ports::Messenger;
use crate::services::Services;
use penelope_mcp::{ServerState, ServerStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Clé `kv` de la carte `nom du serveur → entrée`.
pub const KEY: &str = "mcp.blocked";
/// Un serveur qui retombe dans le **même** état moins d'un jour après son alerte n'en
/// reçoit pas une seconde : le digest le dit. Un serveur qui clignote ne sonne pas.
const REPEAT_AFTER_MS: i64 = 24 * 3_600_000;
/// Longueur gardée de la cause.
const CAUSE_CHARS: usize = 200;

/// Un serveur hors d'usage, depuis quand, et si le propriétaire le sait.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Blocked {
    /// `auth_required` ou `failed`.
    pub state: String,
    pub since_ms: i64,
    /// Outils que le serveur n'offre plus.
    pub tools: usize,
    /// Dernière erreur, sans le remède qu'elle porte parfois.
    pub reason: String,
    /// Instant de l'alerte, envoyée ou retenue par les heures calmes.
    #[serde(default)]
    pub told_ms: Option<i64>,
    /// Le serveur est revenu : l'entrée ne reste que pour la règle du jour.
    #[serde(default)]
    pub over_ms: Option<i64>,
}

impl Blocked {
    fn needs_auth(&self) -> bool {
        self.state == ServerState::AuthRequired.as_str()
    }

    /// « autorisation requise » ou « en panne » : ce que lit le propriétaire.
    pub fn label(&self) -> &'static str {
        if self.needs_auth() {
            "autorisation requise"
        } else {
            "en panne"
        }
    }

    /// La commande qui rebranche le serveur.
    pub fn command(&self, server: &str) -> String {
        if self.needs_auth() {
            format!("/mcp auth {server}")
        } else {
            format!("penelope mcp restart {server}")
        }
    }

    fn alert_line(&self, server: &str) -> String {
        let tools = match self.tools {
            0 => String::new(),
            n => format!(" ({n} outil(s))"),
        };
        let cause = if self.reason.is_empty() {
            String::new()
        } else {
            format!(" Cause : {}.", self.reason.trim_end_matches('.'))
        };
        let remedy = if self.needs_auth() {
            format!(
                "Pour le rebrancher : `/mcp auth {server}` dans la conversation, ou \
                 `penelope mcp auth {server}`."
            )
        } else {
            format!("À voir : `penelope mcp logs {server}`, puis `penelope mcp restart {server}`.")
        };
        format!("• `{server}`{tools} : {}.{cause} {remedy}", self.label())
    }
}

/// La cause seule : les erreurs d'autorisation portent déjà un remède, que l'alerte écrit
/// à sa façon.
fn cause(last_error: Option<&str>) -> String {
    let e = last_error.unwrap_or_default().trim();
    let e = e.split(" : autorisation requise, ").next().unwrap_or(e);
    let e = e.split(" ; `penelope mcp auth").next().unwrap_or(e);
    let e = e.strip_prefix("autorisation requise : ").unwrap_or(e);
    let mut out: String = e.chars().take(CAUSE_CHARS).collect();
    if e.chars().count() > CAUSE_CHARS {
        out.push('…');
    }
    out
}

async fn load(s: &Services) -> BTreeMap<String, Blocked> {
    s.kv_get(KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

async fn save(s: &Services, map: &BTreeMap<String, Blocked>) {
    let done = if map.is_empty() {
        s.kv_delete(KEY).await
    } else {
        match serde_json::to_string(map) {
            Ok(raw) => s.kv_set(KEY, &raw).await,
            Err(e) => Err(e.into()),
        }
    };
    if let Err(e) = done {
        tracing::warn!(error = %e, "serveurs MCP hors d'usage non enregistrés");
    }
}

/// Les serveurs hors d'usage en ce moment, par nom.
pub async fn standing(s: &Services) -> BTreeMap<String, Blocked> {
    let mut map = load(s).await;
    map.retain(|_, b| b.over_ms.is_none());
    map
}

/// Le serveur vient d'être autorisé : son entrée est fermée sans attendre la passe.
pub async fn forget(s: &Services, server: &str) {
    let mut map = load(s).await;
    let Some(b) = map.get_mut(server).filter(|b| b.over_ms.is_none()) else {
        return;
    };
    b.over_ms = Some(s.clock.now_ms());
    save(s, &map).await;
}

/// Range l'état d'un serveur dans la carte.
fn observe(map: &mut BTreeMap<String, Blocked>, st: &ServerStatus, now: i64) {
    let out = matches!(st.state, ServerState::AuthRequired | ServerState::Failed);
    if !out {
        let Some(b) = map.get_mut(&st.name).filter(|b| b.over_ms.is_none()) else {
            return;
        };
        // Un serveur paresseux revenu au repos a pu répondre entre deux passes : son
        // dernier succès le dit.
        let worked = st
            .last_ok
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| t.timestamp_millis() > b.since_ms);
        if st.state.is_usable() || st.state == ServerState::Disabled || worked {
            b.over_ms = Some(now);
        }
        return;
    }
    let state = st.state.as_str();
    let reason = cause(st.last_error.as_deref());
    match map.get_mut(&st.name) {
        // Toujours le même état : rien de neuf à dire, la cause est tenue à jour.
        Some(b) if b.over_ms.is_none() && b.state == state => {
            b.reason = reason;
            b.tools = b.tools.max(st.tool_count);
        }
        // Un autre état, ou une rechute : l'entrée repart. La rechute dans le même état
        // moins d'un jour après l'alerte garde son « déjà dit ».
        Some(b) => {
            let told_ms = b
                .told_ms
                .filter(|t| b.state == state && now - t < REPEAT_AFTER_MS);
            *b = Blocked {
                state: state.into(),
                since_ms: now,
                tools: st.tool_count,
                reason,
                told_ms,
                over_ms: None,
            };
        }
        None => {
            map.insert(
                st.name.clone(),
                Blocked {
                    state: state.into(),
                    since_ms: now,
                    tools: st.tool_count,
                    reason,
                    ..Default::default()
                },
            );
        }
    }
}

fn alert_text(map: &BTreeMap<String, Blocked>, fresh: &[String], link: Option<String>) -> String {
    let mut lines = vec![match fresh.len() {
        1 => "🔌 **Serveur MCP hors d'usage**".to_string(),
        n => format!("🔌 **{n} serveurs MCP hors d'usage**"),
    }];
    lines.push(String::new());
    lines.extend(
        fresh
            .iter()
            .filter_map(|n| map.get(n).map(|b| b.alert_line(n))),
    );
    lines.push(String::new());
    lines.push(
        "Les tours qui s'en servent échouent en attendant ; le digest du matin le rappelle \
         tant que ça dure."
            .to_string(),
    );
    if let Some(link) = link {
        lines.push(format!("[Voir les serveurs MCP]({link})"));
    }
    lines.join("\n")
}

/// Une passe : la carte suit les états, et ce qui n'a pas encore été dit part en **un**
/// message. `statuses` est la liste complète des serveurs chargés : un serveur absent
/// n'est plus déclaré, son entrée s'en va. L'appelant n'appelle donc pas avant le premier
/// chargement de `mcp.d` (`McpAdmin::loaded`).
pub async fn tick(s: &Services, statuses: &[ServerStatus], messenger: Option<&dyn Messenger>) {
    let before = load(s).await;
    if before.is_empty()
        && !statuses
            .iter()
            .any(|st| matches!(st.state, ServerState::AuthRequired | ServerState::Failed))
    {
        return;
    }
    let now = s.clock.now_ms();
    let mut map = before.clone();
    map.retain(|name, _| statuses.iter().any(|st| &st.name == name));
    for st in statuses {
        observe(&mut map, st, now);
    }
    // Une entrée fermée ne sert plus qu'à la règle du jour.
    map.retain(|_, b| b.over_ms.is_none() || b.told_ms.is_some_and(|t| now - t < REPEAT_AFTER_MS));
    let fresh: Vec<String> = map
        .iter()
        .filter(|(_, b)| b.over_ms.is_none() && b.told_ms.is_none())
        .map(|(n, _)| n.clone())
        .collect();
    if let Some(m) = messenger.filter(|_| !fresh.is_empty()) {
        let text = alert_text(&map, &fresh, crate::helpers::deep_link(s, "mcp").await);
        let origin = crate::helpers::owner_origin_of(s);
        match crate::quiet::deliver_or_hold(s, m, "mcp_alert", &origin, &text).await {
            Ok(_) => {
                for b in map.values_mut().filter(|b| b.over_ms.is_none()) {
                    b.told_ms.get_or_insert(now);
                }
            }
            Err(e) => tracing::warn!(error = %e, "alerte MCP non remise"),
        }
    }
    if map != before {
        save(s, &map).await;
    }
}

fn age(ms: i64) -> String {
    match ms / 3_600_000 {
        h if h < 1 => "moins d'une heure".into(),
        h if h < 48 => format!("{h} h"),
        h => format!("{} j", h / 24),
    }
}

/// La ligne du digest tant qu'un serveur est hors d'usage : « Serveurs MCP : ❌ `agenda`
/// (autorisation requise depuis 6 j, `/mcp auth agenda`) ».
pub async fn digest_line(s: &Services) -> Option<String> {
    let map = standing(s).await;
    if map.is_empty() {
        return None;
    }
    let now = s.clock.now_ms();
    let list = map
        .iter()
        .map(|(n, b)| {
            format!(
                "`{n}` ({} depuis {}, `{}`)",
                b.label(),
                age(now - b.since_ms),
                b.command(n)
            )
        })
        .collect::<Vec<_>>()
        .join(" ; ");
    Some(format!("Serveurs MCP : ❌ {list}"))
}

#[cfg(test)]
mod tests;
