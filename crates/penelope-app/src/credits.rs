//! Crédits épuisés (#339) : quota du plan Codex, 402 d'OpenRouter, budget journalier.
//!
//! La boucle d'agent qui abandonne un appel sur une telle erreur range un [`CreditStop`]
//! sous la session du tour, et commence son message d'échec par [`CREDITS_EXHAUSTED`] :
//! le canal y reconnaît un arrêt à reprendre, pas une panne, et le moteur de workflows
//! met le run en pause au lieu de le faire échouer. Rangé par session : deux tours qui
//! échouent en même temps ne se lisent pas l'un l'autre.

use penelope_store::Store;
use serde::{Deserialize, Serialize};

/// Début du message d'un tour arrêté faute de crédits : une pause, pas une erreur.
pub const CREDITS_EXHAUSTED: &str = "⏸ crédits épuisés";

/// Fournisseur réservé au budget journalier (`budget.daily_usd`).
pub const DAILY_BUDGET: &str = "budget";

/// Un arrêt faute de crédits : chez qui, pourquoi, depuis quand, et jusqu'à quand quand le
/// fournisseur le dit (`resets_at` du quota Codex, `Retry-After`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditStop {
    pub provider: String,
    #[serde(default)]
    pub model: String,
    pub reason: String,
    pub at_ms: i64,
    #[serde(default)]
    pub until_ms: Option<i64>,
}

impl CreditStop {
    /// `crédits Codex épuisés`, `crédits OpenRouter épuisés`, `budget journalier atteint`.
    pub fn what(&self) -> String {
        match self.provider.as_str() {
            DAILY_BUDGET => "budget journalier atteint".into(),
            "codex" => "crédits Codex épuisés".into(),
            "openrouter" => "crédits OpenRouter épuisés".into(),
            other => format!("crédits {other} épuisés"),
        }
    }

    /// Ce qui permet la reprise : `au retour du quota`, `à la recharge des crédits`.
    pub fn back_when(&self) -> &'static str {
        match self.provider.as_str() {
            DAILY_BUDGET => "au changement de jour",
            "openrouter" => "à la recharge des crédits",
            _ => "au retour du quota",
        }
    }

    /// Le message d'échec d'un tour : préfixe, motif, et retour prévu s'il est connu.
    pub fn failure_text(&self, timezone: &str) -> String {
        let mut text = format!("{CREDITS_EXHAUSTED} : {} ({}).", self.what(), self.reason);
        match self.until_ms {
            Some(at) => text.push_str(&format!(
                " Retour prévu à {}, {}.",
                clock_at(at, timezone),
                self.back_when()
            )),
            None => text.push_str(&format!(" Reprise {}.", self.back_when())),
        }
        text
    }
}

fn key(session_id: &str) -> String {
    format!("credits.stop.{session_id}")
}

/// Range l'arrêt d'un tour de la session.
pub async fn record(store: &Store, session_id: &str, stop: &CreditStop) -> anyhow::Result<()> {
    let (k, v) = (key(session_id), serde_json::to_string(stop)?);
    store
        .write(move |tx| penelope_store::kv_set(tx, &k, &v))
        .await?;
    Ok(())
}

/// Reprend l'arrêt rangé pour la session, et l'efface.
pub async fn take(store: &Store, session_id: &str) -> Option<CreditStop> {
    let k = key(session_id);
    let raw = store
        .write(move |tx| {
            let v = penelope_store::kv_get(tx, &k)?;
            tx.execute("DELETE FROM kv WHERE k = ?1", [&k])?;
            Ok(v)
        })
        .await
        .ok()
        .flatten()?;
    serde_json::from_str(&raw).ok()
}

/// `17 h 40` à l'heure du propriétaire ; `demain 08 h 00`, ou la date, au-delà du jour.
pub fn clock_at(at_ms: i64, timezone: &str) -> String {
    let now = chrono::Utc::now();
    let Some(at) = chrono::DateTime::from_timestamp_millis(at_ms) else {
        return "?".into();
    };
    match timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => clock_in(at.with_timezone(&tz), now.with_timezone(&tz)),
        Err(_) => clock_in(at, now),
    }
}

fn clock_in<Z: chrono::TimeZone>(at: chrono::DateTime<Z>, now: chrono::DateTime<Z>) -> String
where
    Z::Offset: std::fmt::Display,
{
    let hour = at.format("%H h %M").to_string();
    let (day, today) = (at.date_naive(), now.date_naive());
    if day == today {
        hour
    } else if Some(day) == today.succ_opt() {
        format!("demain {hour}")
    } else {
        format!("{} {hour}", at.format("%d/%m"))
    }
}

/// Minuit suivant à l'heure du propriétaire, en millisecondes : le retour d'un budget
/// journalier atteint.
pub fn next_midnight_ms(now_ms: i64, timezone: &str) -> i64 {
    let at = chrono::DateTime::from_timestamp_millis(now_ms).unwrap_or_default();
    let next = |day: chrono::NaiveDate| day.succ_opt().and_then(|d| d.and_hms_opt(0, 0, 0));
    match timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => next(at.with_timezone(&tz).date_naive())
            .and_then(|n| n.and_local_timezone(tz).earliest())
            .map(|d| d.timestamp_millis()),
        Err(_) => next(at.date_naive()).map(|n| n.and_utc().timestamp_millis()),
    }
    .unwrap_or(now_ms + 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_failure_text_says_what_and_when() {
        let stop = CreditStop {
            provider: "codex".into(),
            model: "codex:gpt".into(),
            reason: "quota ChatGPT atteint".into(),
            at_ms: 0,
            until_ms: None,
        };
        let text = stop.failure_text("Europe/Paris");
        assert!(text.starts_with(CREDITS_EXHAUSTED), "{text}");
        assert!(text.contains("crédits Codex épuisés"), "{text}");
        assert!(text.ends_with("Reprise au retour du quota."), "{text}");
        let later = CreditStop {
            until_ms: Some(chrono::Utc::now().timestamp_millis() + 3_600_000),
            ..stop
        };
        assert!(later.failure_text("UTC").contains(" h "), "{later:?}");
        let card = CreditStop {
            provider: "openrouter".into(),
            ..later
        };
        assert!(card.what().contains("OpenRouter"));
        assert_eq!(card.back_when(), "à la recharge des crédits");
    }

    #[test]
    fn midnight_is_the_owner_s() {
        // 2026-10-08 21:30 UTC, 23:30 à Paris : minuit de Paris est dans 30 minutes.
        let now = 1_791_495_000_000;
        assert_eq!(next_midnight_ms(now, "Europe/Paris") - now, 30 * 60_000);
        assert_eq!(next_midnight_ms(now, "UTC") - now, 150 * 60_000);
        assert_eq!(
            clock_in(
                chrono::DateTime::from_timestamp_millis(now + 3_600_000).unwrap(),
                chrono::DateTime::from_timestamp_millis(now).unwrap()
            ),
            "22 h 30"
        );
    }

    #[tokio::test]
    async fn a_stop_is_taken_once() {
        let store = Store::open_memory().unwrap();
        let stop = CreditStop {
            provider: "openrouter".into(),
            model: String::new(),
            reason: "402".into(),
            at_ms: 1,
            until_ms: None,
        };
        record(&store, "s1", &stop).await.unwrap();
        assert_eq!(take(&store, "s2").await, None);
        assert_eq!(take(&store, "s1").await, Some(stop));
        assert_eq!(take(&store, "s1").await, None);
    }
}
