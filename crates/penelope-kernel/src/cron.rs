//! Expressions cron à 5 champs, avec fuseau (§12.9, défaut `Indian/Reunion`).
//!
//! Implémentation locale : le calcul de la prochaine occurrence doit être
//! **déterministe et testable** en avançant une horloge fictive, et les schedules sont
//! calculés en UTC pour survivre à une dérive d'horloge (§17).
//!
//! Heure d'été : voir [`Cron::next_after`] (une heure ambiguë tire une fois, une heure
//! absente tire à l'instant du saut).
//!
//! Syntaxe : `minute heure jour-du-mois mois jour-de-semaine`, avec `*`, `a-b`, `a,b,c`,
//! `*/n`, `a-b/n`. Jour de semaine : 0 ou 7 = dimanche. Noms courts acceptés
//! (`mon`..`sun`, `jan`..`dec`).

use crate::error::{KernelError, Result};
use chrono::{Datelike, Offset, TimeZone, Timelike};

type Local = chrono::DateTime<chrono_tz::Tz>;

/// Décalage du fuseau à cet instant, en secondes à l'est d'UTC.
fn offset_s(t: &Local) -> i64 {
    i64::from(t.offset().fix().local_minus_utc())
}

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
    ///
    /// Le calcul avance en UTC et convertit pour comparer (#458) : un instant UTC a
    /// toujours une heure locale, alors qu'une heure locale construite peut être ambiguë
    /// ou ne pas exister, et faisait sortir le calcul sans résultat. Aux changements
    /// d'heure :
    ///
    /// - une heure locale **ambiguë** (elle revient à l'automne) tire une fois, la
    ///   première ;
    /// - une heure locale **absente** (sautée au printemps) tire une fois, à l'instant du
    ///   saut ;
    /// - un cron à toutes les heures (`*/15 * * * *`, `0 * * * *`) suit le temps réel : il
    ///   tire dans les deux passages de l'heure répétée et ne rattrape rien au saut.
    pub fn next_after(
        &self,
        after: chrono::DateTime<chrono::Utc>,
        tz: chrono_tz::Tz,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let minute = chrono::Duration::minutes(1);
        // 5 ans, borne de sécurité franche.
        let limit = after.checked_add_signed(chrono::Duration::days(5 * 366))?;
        let start = chrono::DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60, 0)?;
        let mut prev = start.with_timezone(&tz);
        let mut t = start.checked_add_signed(minute)?;
        while t <= limit {
            let local = t.with_timezone(&tz);
            if self.fires(&prev, &local) {
                return Some(t);
            }
            t = self.step(t, &local)?;
            prev = local;
        }
        None
    }

    /// Vrai si l'expression tire à `local`. `prev` est l'instant examiné juste avant : un
    /// décalage qui a grandi entre les deux dit quelles heures locales ont été sautées.
    fn fires(&self, prev: &Local, local: &Local) -> bool {
        let naive = local.naive_local();
        if self.matches_naive(&naive) {
            if self.every_hour() {
                return true;
            }
            // Second passage d'une heure ambiguë : le premier a déjà tiré.
            match local.timezone().from_local_datetime(&naive) {
                chrono::LocalResult::Ambiguous(first, _) if first != *local => {}
                _ => return true,
            }
        }
        let jump = offset_s(local) - offset_s(prev);
        if jump <= 0 || self.every_hour() {
            return false;
        }
        // Les heures locales de `naive - jump` à `naive` n'ont pas existé.
        (1..=jump / 60).any(|back| {
            naive
                .checked_sub_signed(chrono::Duration::minutes(back))
                .is_some_and(|missing| self.matches_naive(&missing))
        })
    }

    /// L'instant suivant à examiner : le lendemain si la date ne convient pas, l'heure
    /// suivante si l'heure ne convient pas, sinon la minute suivante. Un saut ne traverse
    /// jamais un changement d'heure : si le décalage diffère à l'arrivée, on raccourcit,
    /// jusqu'à la minute, et [`Cron::fires`] voit le changement entre deux minutes.
    fn step(
        &self,
        t: chrono::DateTime<chrono::Utc>,
        local: &Local,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let to_hour = 60 - i64::from(local.minute());
        let to_day = (23 - i64::from(local.hour())) * 60 + to_hour;
        let wanted = if !self.matches_date(&local.naive_local()) {
            to_day
        } else if !self.hours.contains(&local.hour()) {
            to_hour
        } else {
            1
        };
        for minutes in [wanted, to_hour.min(wanted)] {
            let next = t.checked_add_signed(chrono::Duration::minutes(minutes))?;
            if minutes == 1 || offset_s(&next.with_timezone(&local.timezone())) == offset_s(local) {
                return Some(next);
            }
        }
        t.checked_add_signed(chrono::Duration::minutes(1))
    }

    /// Toutes les heures du jour : le cron suit le temps réel aux changements d'heure.
    fn every_hour(&self) -> bool {
        self.hours.len() == 24
    }

    fn matches_local(&self, t: &Local) -> bool {
        self.matches_naive(&t.naive_local())
    }

    fn matches_naive(&self, t: &chrono::NaiveDateTime) -> bool {
        self.minutes.contains(&t.minute()) && self.hours.contains(&t.hour()) && self.matches_date(t)
    }

    fn matches_date(&self, t: &chrono::NaiveDateTime) -> bool {
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

    /// Les tirs successifs après `from`, à l'heure UTC.
    fn runs(expr: &str, zone: &str, from: &str, count: usize) -> Vec<String> {
        let (c, tz) = (Cron::parse(expr).unwrap(), zone.parse().unwrap());
        let mut t = utc(from);
        (0..count)
            .map(|_| {
                t = c.next_after(t, tz).expect("un prochain tir");
                t.format("%d %H:%M").to_string()
            })
            .collect()
    }

    /// #458 : le calcul avançait en heure locale et sortait sans résultat sur l'heure
    /// ambiguë du dernier dimanche d'octobre (le 25 en 2026, 03:00 redevient 02:00 à
    /// 01:00 UTC). Un cron quotidien perdait son prochain tir le samedi.
    #[test]
    fn a_daily_cron_crosses_both_clock_changes() {
        assert_eq!(
            runs("0 8 * * *", "Europe/Paris", "2026-10-23T12:00:00Z", 4),
            ["24 06:00", "25 07:00", "26 07:00", "27 07:00"]
        );
        // Dernier dimanche de mars (le 29 en 2026, 02:00 devient 03:00 à 01:00 UTC).
        assert_eq!(
            runs("0 8 * * *", "Europe/Paris", "2026-03-27T12:00:00Z", 3),
            ["28 07:00", "29 06:00", "30 06:00"]
        );
    }

    /// Une heure locale ambiguë tire une fois, la première ; une heure locale absente
    /// tire à l'instant du saut. Jamais zéro, jamais deux.
    #[test]
    fn an_ambiguous_hour_fires_once_and_a_missing_one_at_the_jump() {
        // 02:30 existe deux fois le 25 : 00:30 UTC (heure d'été) puis 01:30 UTC.
        assert_eq!(
            runs("30 2 * * *", "Europe/Paris", "2026-10-23T12:00:00Z", 3),
            ["24 00:30", "25 00:30", "26 01:30"]
        );
        // 02:30 n'existe pas le 29 : le tir part à 03:00 locales, 01:00 UTC.
        assert_eq!(
            runs("30 2 * * *", "Europe/Paris", "2026-03-27T12:00:00Z", 3),
            ["28 01:30", "29 01:00", "30 00:30"]
        );
        // Plusieurs minutes de l'heure absente : un seul tir au saut.
        assert_eq!(
            runs("*/20 2 * * *", "Europe/Paris", "2026-03-28T12:00:00Z", 2),
            ["29 01:00", "30 00:00"]
        );
    }

    /// Un recalcul fait pendant l'heure ambiguë, première ou seconde fois, aboutit.
    #[test]
    fn a_recomputation_inside_the_ambiguous_hour_succeeds() {
        for inside in ["2026-10-25T00:45:10Z", "2026-10-25T01:45:10Z"] {
            assert_eq!(
                runs("0 8 * * *", "Europe/Paris", inside, 1),
                ["25 07:00"],
                "{inside}"
            );
        }
        // 02:50 la seconde fois : la première a déjà tiré, le suivant est demain.
        assert_eq!(
            runs("50 2 * * *", "Europe/Paris", "2026-10-25T01:45:00Z", 1),
            ["26 01:50"]
        );
    }

    /// Un cron à toutes les heures suit le temps réel : il ne saute pas l'heure répétée
    /// et ne rattrape rien au printemps.
    #[test]
    fn an_every_hour_cron_follows_real_time() {
        assert_eq!(
            runs("*/30 * * * *", "Europe/Paris", "2026-10-25T00:20:00Z", 4),
            ["25 00:30", "25 01:00", "25 01:30", "25 02:00"]
        );
        assert_eq!(
            runs("30 * * * *", "Europe/Paris", "2026-03-29T00:00:00Z", 3),
            ["29 00:30", "29 01:30", "29 02:30"]
        );
    }

    /// Fuseaux à décalage non entier et saut d'une demi-heure.
    #[test]
    fn odd_offsets_keep_their_local_hour() {
        assert_eq!(
            runs("0 9 * * *", "Asia/Kolkata", "2026-01-01T00:00:00Z", 2),
            ["01 03:30", "02 03:30"]
        );
        // Lord Howe avance de trente minutes le 4 octobre 2026 : 02:00 devient 02:30
        // (15:30 UTC la veille). 02:15 n'existe pas ce jour-là.
        assert_eq!(
            runs(
                "15 2 * * *",
                "Australia/Lord_Howe",
                "2026-10-02T00:00:00Z",
                3
            ),
            ["02 15:45", "03 15:30", "04 15:15"]
        );
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
