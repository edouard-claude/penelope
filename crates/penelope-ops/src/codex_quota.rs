//! Jauges du plan ChatGPT (décision 3 de l'issue #142).
//!
//! Un appel par abonnement coûte 0 $ : les plafonds en dollars (`budget.*`) ne comptent
//! rien pour ce fournisseur, et ne le freinent donc pas. La vraie limite est le quota du
//! plan, que le backend annonce à chaque réponse : une fenêtre de cinq heures, une
//! fenêtre hebdomadaire, en pourcents consommés et heure de remise à zéro.
//!
//! Pénélope les range en `kv` à chaque réponse, alerte une fois par fenêtre à
//! `providers.codex.quota_alert_ratio`, et se met en retrait à `quota_stop_ratio` — le
//! fournisseur répond alors `RateLimited` **avant** l'appel, le routeur se replie, et le
//! message distingue un quota atteint d'une panne (issue #139).

use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_llm::Quota;
use std::sync::Arc;

/// Clé de l'instantané le plus récent.
pub const QUOTA_KEY: &str = "codex.quota";

/// Écrit chaque jauge lue par le fournisseur. L'alerte, elle, part de la boucle de
/// rafraîchissement : elle a le canal du propriétaire, et une minute de retard sur une
/// fenêtre de cinq heures ne change rien.
pub struct QuotaWriter {
    services: Arc<Services>,
}

impl QuotaWriter {
    pub fn new(services: Arc<Services>) -> Self {
        QuotaWriter { services }
    }
}

impl penelope_llm::QuotaSink for QuotaWriter {
    fn record(&self, quota: Quota) {
        let s = self.services.clone();
        // Appelé depuis le flux : on ne le fait pas attendre une écriture.
        tokio::spawn(async move {
            if let Err(e) = store(&s, &quota).await {
                tracing::warn!(error = %e, "jauge de quota Codex non rangée");
            }
        });
    }
}

/// Range l'instantané.
pub async fn store(s: &Services, q: &Quota) -> anyhow::Result<()> {
    s.kv_set(QUOTA_KEY, &serde_json::to_string(q)?).await
}

/// Dernier instantané connu, s'il y en a un.
pub async fn snapshot(s: &Services) -> Option<Quota> {
    let raw = s.kv_get(QUOTA_KEY).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Jauge en une ligne : `primary 42 % · retour 18:05 · secondary 12 %`.
pub fn gauge_line(q: &Quota, now_ms: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (name, window) in [("primary", q.primary), ("secondary", q.secondary)] {
        let Some(w) = window else { continue };
        let mut part = format!("{name} {:.0} %", w.used_percent);
        if w.reset_at > 0 && w.reset_at * 1000 > now_ms {
            part.push_str(&format!(" · retour {}", reset_clock(w.reset_at)));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return "quota inconnu".into();
    }
    parts.join(" · ")
}

/// Heure locale de remise à zéro, `HH:MM`.
fn reset_clock(reset_at_s: i64) -> String {
    chrono::DateTime::from_timestamp(reset_at_s, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

/// Alerte le propriétaire quand la fenêtre se remplit : **une seule fois par fenêtre**,
/// comme les alertes de budget (`budget.alert.*`, issue #79).
pub async fn check_alert(
    s: &Services,
    messenger: Option<Arc<dyn Messenger>>,
) -> anyhow::Result<Option<String>> {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    if !c.enabled {
        return Ok(None);
    }
    let Some(q) = snapshot(s).await else {
        return Ok(None);
    };
    let ratio = q.worst_ratio();
    if ratio < c.quota_alert_ratio {
        return Ok(None);
    }
    // La fenêtre fait partie de la clé : la suivante mérite sa propre alerte.
    let window = q
        .primary
        .map(|w| w.reset_at)
        .or_else(|| q.secondary.map(|w| w.reset_at))
        .unwrap_or_default();
    let key = format!(
        "codex.quota.alert.{window}.{:.0}",
        c.quota_alert_ratio * 100.0
    );
    if s.kv_get(&key).await?.is_some() {
        return Ok(None);
    }
    s.kv_set(&key, &s.clock.now_rfc3339()).await?;
    let text = alert_text(&q, ratio, c.quota_stop_ratio, s.clock.now_ms());
    match messenger {
        Some(m) => {
            let origin = crate::bus::Origin::Internal {
                source: "codex".into(),
            };
            if let Err(e) = m.send_text(&origin, &text).await {
                tracing::warn!(error = %e, "alerte de quota Codex non envoyée");
            }
        }
        None => tracing::warn!(alerte = %text, "alerte de quota Codex sans canal"),
    }
    Ok(Some(text))
}

/// Texte de l'alerte : où on en est, ce qui se passera, et quand ça repart.
pub fn alert_text(q: &Quota, ratio: f64, stop: f64, now_ms: i64) -> String {
    format!(
        "📊 **Quota ChatGPT à {:.0} %** ({}).\n\nAu-delà de {:.0} %, Pénélope se met en \
         retrait et repasse par OpenRouter, en le disant. Ce n'est pas une panne : la \
         fenêtre se vide toute seule.",
        ratio * 100.0,
        gauge_line(q, now_ms),
        stop * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_llm::codex::QuotaWindow;

    fn quota(primary: f64, secondary: f64) -> Quota {
        Quota {
            primary: Some(QuotaWindow {
                used_percent: primary,
                window_minutes: 300,
                reset_at: 0,
            }),
            secondary: Some(QuotaWindow {
                used_percent: secondary,
                window_minutes: 10_080,
                reset_at: 0,
            }),
            plan_type: "pro".into(),
            ..Default::default()
        }
    }

    /// #142 : la jauge se lit d'un coup d'œil, et l'heure de retour n'apparaît que
    /// lorsqu'elle est devant nous.
    #[test]
    fn the_gauge_reads_at_a_glance() {
        let mut q = quota(42.4, 12.0);
        assert_eq!(gauge_line(&q, 0), "primary 42 % · secondary 12 %");
        q.primary = Some(QuotaWindow {
            reset_at: 1_790_000_000,
            ..q.primary.unwrap()
        });
        let line = gauge_line(&q, 1_789_000_000_000);
        assert!(line.contains("retour "), "{line}");
        assert_eq!(gauge_line(&Quota::default(), 0), "quota inconnu");
    }

    /// #142 : l'alerte dit où on en est, ce qui se passera, et que ce n'est pas une panne
    /// (issue #139).
    #[test]
    fn the_alert_tells_a_quota_from_a_failure() {
        let t = alert_text(&quota(81.0, 10.0), 0.81, 0.95, 0);
        assert!(t.contains("81 %"), "{t}");
        assert!(t.contains("95 %"), "{t}");
        assert!(
            t.contains("OpenRouter") && t.contains("pas une panne"),
            "{t}"
        );
    }

    /// #142 : la pire des deux fenêtres décide.
    #[test]
    fn the_worst_window_decides() {
        assert!((quota(10.0, 96.0).worst_ratio() - 0.96).abs() < 1e-9);
        assert!(Quota::default().is_empty());
    }
}
