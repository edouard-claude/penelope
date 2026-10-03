//! `RRULE` (RFC 5545 §3.3.10), les règles qu'un agenda personnel emploie : `FREQ`
//! quotidienne, hebdomadaire (`BYDAY`), mensuelle (`BYMONTHDAY`, `BYDAY` avec rang :
//! `-1FR`) et annuelle (`BYMONTH`, `BYMONTHDAY`), `INTERVAL`, `COUNT`, `UNTIL`, `WKST`.
//! Le reste (`BYHOUR`, `BYSETPOS`, fréquences infra-journalières) est ignoré ; une
//! fréquence inconnue vaut une occurrence unique.

use super::When;
use chrono::{Datelike, Days, Months, NaiveDate, NaiveDateTime, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<When>,
    /// `(rang, jour)` : `MO` donne `(None, Mon)`, `-1SU` donne `(Some(-1), Sun)`.
    pub by_day: Vec<(Option<i32>, Weekday)>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u32>,
    pub wkst: Weekday,
}

/// Garde-fous : au-delà, la série est tenue pour infinie et coupée.
const MAX_INSTANCES: usize = 5_000;
const MAX_PERIODS: u32 = 20_000;

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// `-1SU`, `2TU`, `MO`.
fn parse_by_day(s: &str) -> Option<(Option<i32>, Weekday)> {
    let s = s.trim();
    let split = s.len().checked_sub(2)?;
    let (rank, day) = s.split_at(split);
    let day = weekday(day)?;
    if rank.is_empty() {
        return Some((None, day));
    }
    rank.parse::<i32>()
        .ok()
        .filter(|n| *n != 0)
        .map(|n| (Some(n), day))
}

/// Jours entre `d` et le début de sa semaine (`wkst`).
fn days_since_week_start(d: Weekday, wkst: Weekday) -> u64 {
    u64::from((d.num_days_from_monday() + 7 - wkst.num_days_from_monday()) % 7)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let first = NaiveDate::from_ymd_opt(y, m, 1);
    first
        .and_then(|f| {
            f.checked_add_months(Months::new(1))
                .map(|n| (n - f).num_days())
        })
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(30)
}

/// Le jour `d` du mois, négatif depuis la fin (`-1` : le dernier).
fn day_of_month(y: i32, m: u32, d: i32) -> Option<NaiveDate> {
    let last = i32::try_from(days_in_month(y, m)).ok()?;
    let day = match d {
        0 => return None,
        d if d > 0 => d,
        d => last + 1 + d,
    };
    NaiveDate::from_ymd_opt(y, m, u32::try_from(day).ok()?)
}

/// Les `wd` du mois : tous, ou le `rank`-ième (négatif depuis la fin).
fn weekdays_in_month(y: i32, m: u32, wd: Weekday, rank: Option<i32>) -> Vec<NaiveDate> {
    let all: Vec<NaiveDate> = (1..=days_in_month(y, m))
        .filter_map(|d| NaiveDate::from_ymd_opt(y, m, d))
        .filter(|d| d.weekday() == wd)
        .collect();
    match rank {
        None => all,
        Some(n) if n > 0 => all
            .get(n.unsigned_abs() as usize - 1)
            .copied()
            .into_iter()
            .collect(),
        Some(n) => all
            .len()
            .checked_sub(n.unsigned_abs() as usize)
            .and_then(|i| all.get(i).copied())
            .into_iter()
            .collect(),
    }
}

impl RRule {
    /// Lit `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=8`. `None` pour une fréquence hors de celles
    /// servies, ou une valeur illisible.
    pub fn parse(s: &str) -> Option<RRule> {
        let mut r = RRule {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            wkst: Weekday::Mon,
        };
        let mut freq = None;
        for part in s.split(';') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match v.to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => r.interval = v.parse().ok().filter(|n| *n > 0)?,
                "COUNT" => r.count = Some(v.parse().ok()?),
                "UNTIL" => r.until = When::parse(v, None, None),
                "BYDAY" => r.by_day = v.split(',').filter_map(parse_by_day).collect(),
                "BYMONTHDAY" => {
                    r.by_month_day = v.split(',').filter_map(|d| d.trim().parse().ok()).collect()
                }
                "BYMONTH" => {
                    r.by_month = v
                        .split(',')
                        .filter_map(|m| m.trim().parse().ok())
                        .filter(|m| (1..=12).contains(m))
                        .collect()
                }
                "WKST" => r.wkst = weekday(v)?,
                _ => {}
            }
        }
        r.freq = freq?;
        Some(r)
    }

    /// Les débuts d'occurrence, en heure locale, à partir de `first` (le `DTSTART`) et pas
    /// au-delà de `limit`, de `COUNT` ni d'`until` (déjà en heure locale).
    pub fn starts(
        &self,
        first: NaiveDateTime,
        limit: NaiveDateTime,
        until: Option<NaiveDateTime>,
    ) -> Vec<NaiveDateTime> {
        let stop = match until {
            Some(u) if u < limit => u,
            _ => limit,
        };
        let time = first.time();
        let mut out = Vec::new();
        for period in 0..MAX_PERIODS {
            let Some(period_start) = self.period_start(first.date(), period) else {
                break;
            };
            if period_start.and_time(time) > stop {
                break;
            }
            for d in self.dates_in(period_start, first.date()) {
                let cand = d.and_time(time);
                if cand < first {
                    continue;
                }
                if cand > stop {
                    return out;
                }
                out.push(cand);
                if self.count.is_some_and(|c| out.len() >= c as usize) || out.len() >= MAX_INSTANCES
                {
                    return out;
                }
            }
        }
        out
    }

    /// Le premier jour de la période `k` : jour, semaine (selon `WKST`), mois ou année.
    fn period_start(&self, origin: NaiveDate, k: u32) -> Option<NaiveDate> {
        let step = k.checked_mul(self.interval)?;
        match self.freq {
            Freq::Daily => origin.checked_add_days(Days::new(u64::from(step))),
            Freq::Weekly => {
                let week = origin - Days::new(days_since_week_start(origin.weekday(), self.wkst));
                week.checked_add_days(Days::new(7 * u64::from(step)))
            }
            Freq::Monthly => origin.with_day(1)?.checked_add_months(Months::new(step)),
            Freq::Yearly => NaiveDate::from_ymd_opt(origin.year(), 1, 1)?
                .checked_add_months(Months::new(12u32.checked_mul(step)?)),
        }
    }

    /// Les jours candidats d'une période, triés.
    fn dates_in(&self, period_start: NaiveDate, origin: NaiveDate) -> Vec<NaiveDate> {
        let mut v = match self.freq {
            Freq::Daily => {
                let keep = self.by_day.is_empty()
                    || self
                        .by_day
                        .iter()
                        .any(|(_, wd)| *wd == period_start.weekday());
                if keep { vec![period_start] } else { Vec::new() }
            }
            Freq::Weekly => {
                let days: Vec<Weekday> = if self.by_day.is_empty() {
                    vec![origin.weekday()]
                } else {
                    self.by_day.iter().map(|(_, wd)| *wd).collect()
                };
                days.into_iter()
                    .filter_map(|wd| {
                        period_start
                            .checked_add_days(Days::new(days_since_week_start(wd, self.wkst)))
                    })
                    .collect()
            }
            Freq::Monthly => {
                self.month_dates(period_start.year(), period_start.month(), origin.day())
            }
            Freq::Yearly => {
                let months: Vec<u32> = if self.by_month.is_empty() {
                    vec![origin.month()]
                } else {
                    self.by_month.clone()
                };
                months
                    .into_iter()
                    .flat_map(|m| self.month_dates(period_start.year(), m, origin.day()))
                    .collect()
            }
        };
        v.sort();
        v.dedup();
        v
    }

    /// Les jours d'un mois : `BYMONTHDAY`, sinon `BYDAY` (avec ou sans rang), sinon le jour
    /// d'origine s'il existe dans ce mois (le 31 saute février).
    fn month_dates(&self, y: i32, m: u32, origin_day: u32) -> Vec<NaiveDate> {
        if !self.by_month_day.is_empty() {
            return self
                .by_month_day
                .iter()
                .filter_map(|d| day_of_month(y, m, *d))
                .collect();
        }
        if !self.by_day.is_empty() {
            return self
                .by_day
                .iter()
                .flat_map(|(rank, wd)| weekdays_in_month(y, m, *wd, *rank))
                .collect();
        }
        NaiveDate::from_ymd_opt(y, m, origin_day)
            .into_iter()
            .collect()
    }
}
