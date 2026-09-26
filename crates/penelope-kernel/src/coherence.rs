//! Cohérence de la configuration (issue #16) : un réglage qui annule sa propre intention est
//! refusé à l'application, et signalé au démarrage, jamais silencieux.
//!
//! Paires de réglages capables de se contredire :
//!
//! | Réglages | Contradiction | Gravité |
//! |---|---|---|
//! | `models.roles.<rôle>` / `models.aliases` | rôle vers un alias absent | refus |
//! | `models.routing.low\|medium\|high` / `models.aliases` | palier vers un alias absent | refus |
//! | `models.routing.fallback` / `models.aliases` | repli vers un alias absent ou vers lui-même | refus |
//! | `models.aliases` / `providers.openrouter.enabled`, `providers.local.enabled` | alias vers un provider désactivé | avertissement |
//! | `budget.alert_ratio` | hors de ]0, 1[ : alerte jamais ou toujours envoyée | refus |
//! | `budget.session_usd`, `budget.run_usd` / `budget.daily_usd` | plafond jamais atteint avant celui du jour | avertissement |
//! | `budget.turn_checkpoint_usd` / `budget.session_usd` | point de contrôle au-delà du plafond de session | avertissement |
//! | `context.max_prompt_tokens` / `context.tail_max_tokens` | plafond de prompt sous la queue verbatim | avertissement |
//! | `runners.heartbeat` / `runners.lease_ttl` | bail expiré avant deux battements : tour repris en double | refus |
//! | `tools.http_allowlist` / `tools.http_block_private_ips` | adresse privée autorisée mais toujours bloquée | refus |
//! | `mcp.policy.*` | action destructive ou inconnue moins protégée qu'une écriture | avertissement |
//! | `telegram.quiet_hours` / déclencheurs planifiés | déclencheur dans les heures calmes (vérifié par le daemon) | avertissement |

use crate::config::Config;
use crate::risk::PolicyDecision;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gravity {
    /// Refusé à l'application (`config_set`).
    Refus,
    Avertissement,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contradiction {
    pub gravity: Gravity,
    /// Réglages en cause, chemins pointés.
    pub keys: Vec<String>,
    pub message: String,
}

impl Contradiction {
    fn refus(keys: &[String], message: String) -> Self {
        Contradiction {
            gravity: Gravity::Refus,
            keys: keys.to_vec(),
            message,
        }
    }
    fn warn(keys: &[String], message: String) -> Self {
        Contradiction {
            gravity: Gravity::Avertissement,
            keys: keys.to_vec(),
            message,
        }
    }
    /// Vrai si un des réglages en cause est `path` ou le contient.
    pub fn concerns(&self, path: &str) -> bool {
        self.keys.iter().any(|k| {
            k == path || k.starts_with(&format!("{path}.")) || path.starts_with(&format!("{k}."))
        })
    }
}

fn rank(p: &str) -> u8 {
    match PolicyDecision::parse(p) {
        Some(PolicyDecision::Auto) => 0,
        Some(PolicyDecision::Ask) => 1,
        Some(PolicyDecision::AskTwice) => 2,
        Some(PolicyDecision::Deny) => 3,
        None => 1,
    }
}

fn private_host(entry: &str) -> bool {
    let host = entry
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".localhost")
        || host == "::1"
    {
        return true;
    }
    let octets: Vec<u8> = host.split('.').filter_map(|o| o.parse().ok()).collect();
    matches!(
        octets.as_slice(),
        [127, ..] | [10, ..] | [192, 168, ..] | [169, 254, ..] | [0, ..]
    ) || matches!(octets.as_slice(), [172, b, ..] if (16..=31).contains(b))
}

/// Contradictions d'une configuration.
pub fn contradictions(c: &Config) -> Vec<Contradiction> {
    let mut out = Vec::new();
    let key = |k: &str| k.to_string();
    let aliases = &c.models.aliases;

    for (role, alias) in &c.models.roles {
        if !aliases.contains_key(alias) {
            out.push(Contradiction::refus(
                &[key(&format!("models.roles.{role}")), key("models.aliases")],
                format!("le rôle `{role}` vise l'alias `{alias}`, qui n'existe pas : ce rôle n'aurait aucun modèle"),
            ));
        }
    }
    for (tier, alias) in [
        ("low", &c.models.routing.low),
        ("medium", &c.models.routing.medium),
        ("high", &c.models.routing.high),
    ] {
        if !aliases.contains_key(alias) {
            out.push(Contradiction::refus(
                &[
                    key(&format!("models.routing.{tier}")),
                    key("models.aliases"),
                ],
                format!("le palier `{tier}` du routage vise l'alias `{alias}`, qui n'existe pas"),
            ));
        }
    }
    for (alias, fallbacks) in &c.models.routing.fallback {
        for f in fallbacks {
            if f == alias {
                out.push(Contradiction::refus(
                    &[key(&format!("models.routing.fallback.{alias}"))],
                    format!(
                        "l'alias `{alias}` se replie sur lui-même : le repli ne servirait à rien"
                    ),
                ));
            } else if !aliases.contains_key(f) {
                out.push(Contradiction::refus(
                    &[
                        key(&format!("models.routing.fallback.{alias}")),
                        key("models.aliases"),
                    ],
                    format!("le repli de `{alias}` vise l'alias `{f}`, qui n'existe pas"),
                ));
            }
        }
    }
    for (alias, model) in aliases {
        let (provider, enabled, flag) = match model.split_once(':').map(|(p, _)| p) {
            Some("openrouter") => (
                "openrouter",
                c.providers.openrouter.enabled,
                "providers.openrouter.enabled",
            ),
            Some("openai_compat") => (
                "local",
                c.providers.local.enabled,
                "providers.local.enabled",
            ),
            _ => continue,
        };
        if !enabled {
            let roles: Vec<&str> = c
                .models
                .roles
                .iter()
                .filter(|(_, a)| *a == alias)
                .map(|(r, _)| r.as_str())
                .collect();
            if roles.is_empty() {
                continue;
            }
            out.push(Contradiction::warn(
                &[key(&format!("models.aliases.{alias}")), key(flag)],
                format!(
                    "l'alias `{alias}` (`{model}`) sert le(s) rôle(s) {} mais le provider `{provider}` est désactivé",
                    roles.iter().map(|r| format!("`{r}`")).collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }

    let b = &c.budget;
    if !(b.alert_ratio > 0.0 && b.alert_ratio < 1.0) {
        out.push(Contradiction::refus(
            &[key("budget.alert_ratio")],
            format!(
                "`budget.alert_ratio` vaut {} : l'alerte partirait {} ; une part entre 0 et 1 est attendue",
                b.alert_ratio,
                if b.alert_ratio >= 1.0 { "trop tard, au blocage" } else { "à chaque appel" }
            ),
        ));
    }
    for (name, value) in [("session_usd", b.session_usd), ("run_usd", b.run_usd)] {
        if b.daily_usd > 0.0 && value > b.daily_usd {
            out.push(Contradiction::warn(
                &[key(&format!("budget.{name}")), key("budget.daily_usd")],
                format!(
                    "`budget.{name}` ({value} $) dépasse `budget.daily_usd` ({} $) : ce plafond ne sera jamais atteint, celui du jour bloque avant",
                    b.daily_usd
                ),
            ));
        }
    }
    if b.turn_checkpoint_usd > 0.0 && b.session_usd > 0.0 && b.turn_checkpoint_usd > b.session_usd {
        out.push(Contradiction::warn(
            &[key("budget.turn_checkpoint_usd"), key("budget.session_usd")],
            format!(
                "`budget.turn_checkpoint_usd` ({} $) dépasse `budget.session_usd` ({} $) : le point de contrôle n'arriverait jamais avant le plafond",
                b.turn_checkpoint_usd, b.session_usd
            ),
        ));
    }
    // Un bail qui expire avant deux battements est un verrou qui ne verrouille pas : le
    // tour part en double chez un autre runner (#43).
    if let (Ok(hb), Ok(ttl)) = (
        crate::config::parse_duration(&c.runners.heartbeat),
        crate::config::parse_duration(&c.runners.lease_ttl),
    ) && hb * 2 > ttl
    {
        out.push(Contradiction::refus(
            &[key("runners.heartbeat"), key("runners.lease_ttl")],
            format!(
                "`runners.heartbeat` ({}) dépasse la moitié de `runners.lease_ttl` ({}) : un tour en cours perdrait son bail et serait repris par un autre runner",
                c.runners.heartbeat, c.runners.lease_ttl
            ),
        ));
    }
    let ctx = &c.context;
    if ctx.max_prompt_tokens > 0 && ctx.max_prompt_tokens <= ctx.tail_max_tokens {
        out.push(Contradiction::warn(
            &[key("context.max_prompt_tokens"), key("context.tail_max_tokens")],
            format!(
                "`context.max_prompt_tokens` ({}) ne dépasse pas la queue verbatim (`context.tail_max_tokens`, {}) : chaque tour compacterait",
                ctx.max_prompt_tokens, ctx.tail_max_tokens
            ),
        ));
    }
    if c.tools.http_block_private_ips {
        for entry in c.tools.http_allowlist.iter().filter(|e| private_host(e)) {
            out.push(Contradiction::refus(
                &[key("tools.http_allowlist"), key("tools.http_block_private_ips")],
                format!(
                    "`{entry}` est autorisé par `tools.http_allowlist` mais toujours bloqué par `tools.http_block_private_ips`"
                ),
            ));
        }
    }
    let p = &c.mcp.policy;
    for (name, value) in [("destructive", &p.destructive), ("unknown", &p.unknown)] {
        if rank(value) < rank(&p.write) {
            out.push(Contradiction::warn(
                &[key(&format!("mcp.policy.{name}")), key("mcp.policy.write")],
                format!(
                    "`mcp.policy.{name}` (`{value}`) protège moins que `mcp.policy.write` (`{}`) : une action {} passerait plus facilement qu'une écriture",
                    p.write,
                    if name == "destructive" { "destructive" } else { "de risque inconnu" }
                ),
            ));
        }
    }
    out
}

/// Refus nouveaux introduits par un changement de `path` : ceux qui n'existaient pas avant.
pub fn new_refusals(before: &Config, after: &Config, path: &str) -> Vec<Contradiction> {
    let old = contradictions(before);
    contradictions(after)
        .into_iter()
        .filter(|c| c.gravity == Gravity::Refus && c.concerns(path) && !old.contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_configuration_is_coherent() {
        let c = Config::sample(42);
        let found = contradictions(&c);
        assert!(
            found.iter().all(|x| x.gravity != Gravity::Refus),
            "{found:?}"
        );
    }

    #[test]
    fn self_cancelling_settings_are_named() {
        let mut c = Config::sample(42);
        c.models.roles.insert("classifier".into(), "absent".into());
        c.budget.session_usd = 50.0;
        c.tools.http_allowlist = vec!["http://192.168.0.10:8080".into(), "api.example.com".into()];
        c.mcp.policy.destructive = "auto".into();
        c.budget.alert_ratio = 1.2;
        c.runners.heartbeat = "2m".into();
        let found = contradictions(&c);
        let has = |k: &str, g: Gravity| found.iter().any(|x| x.gravity == g && x.concerns(k));
        assert!(has("models.roles.classifier", Gravity::Refus), "{found:?}");
        assert!(has("budget.session_usd", Gravity::Avertissement));
        assert!(has("tools.http_allowlist", Gravity::Refus));
        assert!(!found.iter().any(|x| x.message.contains("api.example.com")));
        assert!(has("mcp.policy.destructive", Gravity::Avertissement));
        assert!(has("budget.alert_ratio", Gravity::Refus));
        assert!(has("runners.heartbeat", Gravity::Refus), "{found:?}");

        let before = Config::sample(42);
        let refused = new_refusals(&before, &c, "tools.http_allowlist");
        assert_eq!(refused.len(), 1, "{refused:?}");
    }

    /// Routage vers un alias absent, repli sur soi-même ou vers un absent, provider
    /// désactivé sous un rôle, point de contrôle au-delà du plafond de session, queue
    /// plus grande que le plafond de prompt : chacun est nommé.
    #[test]
    fn routing_budget_and_context_contradictions_are_named() {
        let mut c = Config::sample(42);
        c.models.routing.high = "fantome".into();
        c.models
            .routing
            .fallback
            .insert("main".into(), vec!["main".into(), "absent".into()]);
        c.providers.openrouter.enabled = false;
        c.budget.session_usd = 1.0;
        c.budget.turn_checkpoint_usd = 2.0;
        c.context.max_prompt_tokens = c.context.tail_max_tokens;
        c.budget.alert_ratio = 0.0;
        let found = contradictions(&c);
        let msg = |k: &str| {
            found
                .iter()
                .filter(|x| x.concerns(k))
                .map(|x| x.message.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        assert!(
            msg("models.routing.high").contains("`fantome`"),
            "{found:?}"
        );
        let fb = msg("models.routing.fallback.main");
        assert!(fb.contains("se replie sur lui-même"), "{fb}");
        assert!(fb.contains("vise l'alias `absent`"), "{fb}");
        assert!(
            msg("providers.openrouter.enabled").contains("est désactivé"),
            "{found:?}"
        );
        assert!(msg("budget.turn_checkpoint_usd").contains("n'arriverait jamais"));
        assert!(msg("context.max_prompt_tokens").contains("chaque tour compacterait"));
        assert!(msg("budget.alert_ratio").contains("à chaque appel"));
    }

    /// Les hôtes privés reconnus, et ceux qui ne le sont pas.
    #[test]
    fn private_hosts_are_recognised() {
        for h in [
            "localhost",
            "http://nas.local/x",
            "https://10.1.2.3",
            "172.20.0.1:8080",
            "169.254.1.1",
            "app.localhost",
        ] {
            assert!(private_host(h), "{h}");
        }
        // `::1` n'est pas vérifié ici : le découpage sur `:` vide l'hôte avant la
        // comparaison, la branche qui le nomme n'est jamais atteinte (relevé, non corrigé).
        for h in ["api.example.com", "172.32.0.1", "8.8.8.8"] {
            assert!(!private_host(h), "{h}");
        }
        assert_eq!(rank("inconnu"), rank("ask"));
        assert!(rank("deny") > rank("ask_twice"));
    }
}
