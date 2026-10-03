//! Durées et plages horaires lisibles de la configuration.

use super::*;

/// Analyse une durée lisible : `500ms`, `30s`, `15m`, `6h`, `90d`.
pub fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Err(KernelError::config("durée vide"));
    }
    let (num, unit) = s.split_at(
        s.find(|c: char| c.is_ascii_alphabetic())
            .ok_or_else(|| KernelError::config(format!("durée sans unité : `{s}`")))?,
    );
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| KernelError::config(format!("durée invalide : `{s}`")))?;
    let ms = match unit.trim() {
        "ms" => n,
        "s" => n * 1000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        "d" => n * 86_400_000.0,
        other => {
            return Err(KernelError::config(format!(
                "unité de durée inconnue : `{other}`"
            )));
        }
    };
    if ms < 0.0 {
        return Err(KernelError::config("durée négative"));
    }
    Ok(std::time::Duration::from_millis(ms as u64))
}

/// Plage horaire `HH:MM-HH:MM`, éventuellement à cheval sur minuit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start_min: u32,
    pub end_min: u32,
}

impl TimeRange {
    pub fn parse(s: &str) -> Result<Self> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| KernelError::config(format!("plage horaire invalide : `{s}`")))?;
        Ok(TimeRange {
            start_min: parse_hhmm(a.trim())?,
            end_min: parse_hhmm(b.trim())?,
        })
    }

    /// Vrai si `minute_of_day` est dans la plage (bornes incluses côté début).
    pub fn contains(&self, minute_of_day: u32) -> bool {
        if self.start_min <= self.end_min {
            minute_of_day >= self.start_min && minute_of_day < self.end_min
        } else {
            // à cheval sur minuit : 22:00-07:00
            minute_of_day >= self.start_min || minute_of_day < self.end_min
        }
    }

    /// Vrai si l'instant `now_ms` tombe dans la plage, à l'heure locale du fuseau `tz`
    /// (UTC si le fuseau est inconnu). Heures calmes (#296).
    pub fn contains_at(&self, now_ms: i64, tz: &str) -> bool {
        self.contains(minute_of_day(now_ms, tz))
    }

    /// `22:00-07:00`.
    pub fn text(&self) -> String {
        format!("{}-{}", hhmm(self.start_min), hhmm(self.end_min))
    }

    /// Fin de la plage, `07:00`.
    pub fn end_text(&self) -> String {
        hhmm(self.end_min)
    }
}

/// Minute du jour (0..1440) de `ms` dans le fuseau `tz`, UTC si le fuseau est inconnu.
pub fn minute_of_day(ms: i64, tz: &str) -> u32 {
    use chrono::Timelike;
    let utc = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    let local = match tz.parse::<chrono_tz::Tz>() {
        Ok(zone) => utc.with_timezone(&zone).naive_local().time(),
        Err(_) => utc.naive_utc().time(),
    };
    local.hour() * 60 + local.minute()
}

fn hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

fn parse_hhmm(s: &str) -> Result<u32> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| KernelError::config(format!("heure invalide : `{s}`")))?;
    let h: u32 = h
        .parse()
        .map_err(|_| KernelError::config(format!("heure invalide : `{s}`")))?;
    let m: u32 = m
        .parse()
        .map_err(|_| KernelError::config(format!("heure invalide : `{s}`")))?;
    if h > 23 || m > 59 {
        return Err(KernelError::config(format!("heure hors bornes : `{s}`")));
    }
    Ok(h * 60 + m)
}
