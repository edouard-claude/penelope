//! Expressions cron à 5 champs, avec fuseau (§12.9, défaut `Indian/Reunion`).
//!
//! Implémentation locale : le calcul de la prochaine occurrence doit être
//! **déterministe et testable** en avançant une horloge fictive, et les schedules sont
//! calculés en UTC pour survivre à une dérive d'horloge (§17).
//!
//! Syntaxe : `minute heure jour-du-mois mois jour-de-semaine`, avec `*`, `a-b`, `a,b,c`,
//! `*/n`, `a-b/n`. Jour de semaine : 0 ou 7 = dimanche. Noms courts acceptés
//! (`mon`..`sun`, `jan`..`dec`).

use crate::error::{KernelError, Result};
use chrono::{Datelike, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    doms: Vec<u32>,
    months: Vec<u32>,
    dows: Vec<u32>,
    /// Vrai si le champ jour-du-mois vaut `*` (sémantique OU entre dom et dow sinon).
    dom_star: bool,
    dow_star: bool,
    pub expr: String,
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(KernelError::config(format!(
                "expression cron à 5 champs attendue, reçu {} : `{expr}`",
                parts.len()
            )));
        }
        Ok(Cron {
            minutes: field(parts[0], 0, 59, &[])?,
            hours: field(parts[1], 0, 23, &[])?,
            doms: field(parts[2], 1, 31, &[])?,
            months: field(parts[3], 1, 12, MONTHS)?,
            dows: normalise_dow(field(parts[4], 0, 7, DAYS)?),
            dom_star: parts[2] == "*",
            dow_star: parts[4] == "*",
            expr: expr.to_string(),
        })
    }

    /// Prochaine occurrence **strictement après** `after`, dans le fuseau donné.
    ///
    /// Renvoie `None` si aucune occurrence n'existe dans les 5 prochaines années
    /// (par exemple `0 0 30 2 *`, le 30 février).
    pub fn next_after(
        &self,
        after: chrono::DateTime<chrono::Utc>,
        tz: chrono_tz::Tz,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let local = after.with_timezone(&tz);
        let mut t = local
            .with_second(0)?
            .with_nanosecond(0)?
            .checked_add_signed(chrono::Duration::minutes(1))?;

        // 5 ans de minutes, borne de sécurité franche.
        for _ in 0..(5 * 366 * 24 * 60) {
            if self.matches_local(&t) {
                return Some(t.with_timezone(&chrono::Utc));
            }
            // Optimisation : saute à l'heure suivante si l'heure ne peut pas convenir.
            if !self.hours.contains(&t.hour()) {
                t = t
                    .with_minute(0)?
                    .checked_add_signed(chrono::Duration::hours(1))?;
                continue;
            }
            t = t.checked_add_signed(chrono::Duration::minutes(1))?;
        }
        None
    }

    fn matches_local(&self, t: &chrono::DateTime<chrono_tz::Tz>) -> bool {
        if !self.minutes.contains(&t.minute()) || !self.hours.contains(&t.hour()) {
            return false;
        }
        if !self.months.contains(&t.month()) {
            return false;
        }
        let dom_ok = self.doms.contains(&t.day());
        let dow = t.weekday().num_days_from_sunday();
        let dow_ok = self.dows.contains(&dow);

        // Sémantique POSIX : si les deux champs sont restreints, c'est un OU.
        match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (false, true) => dom_ok,
            (true, false) => dow_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }

    /// Prochaine occurrence à partir d'un instant en millisecondes epoch.
    pub fn next_after_ms(&self, after_ms: i64, tz: &str) -> Option<i64> {
        let tz: chrono_tz::Tz = tz.parse().ok()?;
        let after = chrono::DateTime::from_timestamp_millis(after_ms)?;
        self.next_after(after, tz).map(|d| d.timestamp_millis())
    }

    /// Vrai si l'instant donné correspond exactement à l'expression (à la minute).
    pub fn matches_ms(&self, ms: i64, tz: &str) -> bool {
        let Ok(tz) = tz.parse::<chrono_tz::Tz>() else {
            return false;
        };
        let Some(dt) = chrono::DateTime::from_timestamp_millis(ms) else {
            return false;
        };
        let local = tz.from_utc_datetime(&dt.naive_utc());
        self.matches_local(&local)
    }
}

const MONTHS: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

const DAYS: &[(&str, u32)] = &[
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

fn normalise_dow(mut v: Vec<u32>) -> Vec<u32> {
    for x in v.iter_mut() {
        if *x == 7 {
            *x = 0;
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

fn field(spec: &str, min: u32, max: u32, names: &[(&str, u32)]) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(KernelError::config(format!(
                "champ cron vide dans `{spec}`"
            )));
        }
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .map_err(|_| KernelError::config(format!("pas invalide dans `{part}`")))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            return Err(KernelError::config(format!("pas nul dans `{part}`")));
        }
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                parse_one(a, names, min, max)?,
                parse_one(b, names, min, max)?,
            )
        } else {
            let v = parse_one(range, names, min, max)?;
            (v, v)
        };
        if lo > hi {
            return Err(KernelError::config(format!(
                "intervalle cron inversé dans `{part}`"
            )));
        }
        let mut x = lo;
        while x <= hi {
            out.push(x);
            x += step;
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(KernelError::config(format!("champ cron vide : `{spec}`")));
    }
    Ok(out)
}

fn parse_one(s: &str, names: &[(&str, u32)], min: u32, max: u32) -> Result<u32> {
    let s = s.trim();
    if let Some((_, v)) = names
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&s[..s.len().min(3)]))
        && s.len() == 3
    {
        return Ok(*v);
    }
    let v: u32 = s
        .parse()
        .map_err(|_| KernelError::config(format!("valeur cron invalide : `{s}`")))?;
    if v < min || v > max {
        return Err(KernelError::config(format!(
            "valeur cron hors bornes [{min}, {max}] : `{s}`"
        )));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn parses_prd_defaults() {
        Cron::parse("30 3 * * *").unwrap();
        Cron::parse("0 8 * * *").unwrap();
    }

    #[test]
    fn rejects_bad_expressions() {
        assert!(Cron::parse("30 3 * *").is_err());
        assert!(Cron::parse("60 3 * * *").is_err());
        assert!(Cron::parse("*/0 3 * * *").is_err());
        assert!(Cron::parse("5-1 3 * * *").is_err());
    }

    #[test]
    fn dreaming_cron_fires_at_local_0330() {
        let c = Cron::parse("30 3 * * *").unwrap();
        let tz: chrono_tz::Tz = "Indian/Reunion".parse().unwrap();
        // La Réunion = UTC+4 toute l'année : 03:30 local = 23:30 UTC la veille.
        let next = c.next_after(utc("2026-09-16T10:00:00Z"), tz).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-09-16T23:30:00+00:00");
    }

    #[test]
    fn steps_and_lists() {
        let c = Cron::parse("*/15 * * * *").unwrap();
        let tz: chrono_tz::Tz = "UTC".parse().unwrap();
        let n = c.next_after(utc("2026-01-01T10:02:00Z"), tz).unwrap();
        assert_eq!(n.minute(), 15);
        let c = Cron::parse("0,30 9-17 * * mon-fri").unwrap();
        // Samedi 10:05 → lundi 09:00.
        let n = c.next_after(utc("2026-01-03T10:05:00Z"), tz).unwrap();
        assert_eq!(n.to_rfc3339(), "2026-01-05T09:00:00+00:00");
    }

    #[test]
    fn dom_and_dow_are_or_when_both_restricted() {
        let c = Cron::parse("0 0 1 * mon").unwrap();
        let tz: chrono_tz::Tz = "UTC".parse().unwrap();
        // 2026-01-01 est un jeudi : doit quand même tirer (jour du mois = 1).
        let n = c.next_after(utc("2025-12-31T12:00:00Z"), tz).unwrap();
        assert_eq!(n.to_rfc3339(), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn impossible_date_returns_none() {
        let c = Cron::parse("0 0 30 2 *").unwrap();
        let tz: chrono_tz::Tz = "UTC".parse().unwrap();
        assert!(c.next_after(utc("2026-01-01T00:00:00Z"), tz).is_none());
    }

    #[test]
    fn next_is_strictly_after() {
        let c = Cron::parse("0 * * * *").unwrap();
        let tz: chrono_tz::Tz = "UTC".parse().unwrap();
        let t = utc("2026-01-01T10:00:00Z");
        let n = c.next_after(t, tz).unwrap();
        assert!(n > t);
        assert_eq!(n.to_rfc3339(), "2026-01-01T11:00:00+00:00");
    }

    #[test]
    fn matches_ms_agrees_with_next() {
        let c = Cron::parse("30 3 * * *").unwrap();
        let next = c
            .next_after_ms(
                utc("2026-09-16T10:00:00Z").timestamp_millis(),
                "Indian/Reunion",
            )
            .unwrap();
        assert!(c.matches_ms(next, "Indian/Reunion"));
        assert!(!c.matches_ms(next + 60_000, "Indian/Reunion"));
    }
}
