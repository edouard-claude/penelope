//! Sortie de veille (#228) : l'ordonnanceur sait qu'il a dormi.
//!
//! ```text
//! chaque passage
//!   écart = temps mural écoulé − temps monotone écoulé
//!   écart > 60 s ─► host.woke {slept_ms} ─► passe de santé (canal, MCP) ─► host.health
//!                                           └─► créneaux en retard : un seul run par
//!                                               planification, annoncé avec l'heure prévue
//! ```
//!
//! L'horloge monotone ne compte pas le temps de veille (macOS comme Linux) : c'est l'écart
//! entre les deux horloges qui mesure la veille. Un passage lent sur une machine chargée
//! fait avancer les deux ensemble, l'écart reste nul ; on ne compare jamais à la période
//! attendue.

use super::*;
use penelope_kernel::event::EventDraft;
use penelope_mcp::supervisor::ServerState;
use std::time::Instant;

/// Au-delà de cet écart entre les deux horloges, la machine a dormi.
pub const WAKE_GAP_MS: i64 = 60_000;

/// Un créneau parti plus de cinq minutes après son heure est annoncé en retard.
pub const LATE_AFTER_MS: i64 = 5 * 60_000;

/// Essais de la sonde du canal au réveil : le réseau revient rarement à la première
/// seconde. Pauses entre essais, en millisecondes.
const PROBE_PAUSES_MS: [u64; 3] = [500, 1_000, 2_000];

/// Variable d'un tir en retard : la phrase qui le dit, reprise par la cible.
pub(super) const LATE: &str = "retard";

/// Une sortie de veille : quand, et combien de temps la machine a dormi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wake {
    pub at_ms: i64,
    pub slept_ms: i64,
}

/// Les deux horloges au passage précédent, et la dernière veille constatée.
#[derive(Debug, Clone)]
pub struct WakeWatch {
    wall_ms: i64,
    mono: Instant,
    last: Option<Wake>,
}

impl WakeWatch {
    pub fn new(wall_ms: i64) -> WakeWatch {
        WakeWatch {
            wall_ms,
            mono: Instant::now(),
            last: None,
        }
    }

    /// Compare ce passage au précédent ; `Some` si la machine a dormi entre les deux.
    pub fn observe(&mut self, wall_ms: i64, mono: Instant) -> Option<Wake> {
        let wall = wall_ms - self.wall_ms;
        let steady = mono.saturating_duration_since(self.mono).as_millis() as i64;
        self.wall_ms = wall_ms;
        self.mono = mono;
        let gap = wall - steady;
        if gap <= WAKE_GAP_MS {
            return None;
        }
        let wake = Wake {
            at_ms: wall_ms,
            slept_ms: gap,
        };
        self.last = Some(wake);
        Some(wake)
    }

    /// La dernière veille constatée : elle dit pourquoi un créneau est parti en retard.
    pub fn last(&self) -> Option<Wake> {
        self.last
    }
}

/// Un passage de l'ordonnanceur : s'il suit une veille, la journalise et vérifie les
/// connexions avant que les créneaux en retard ne partent.
pub async fn wake_check(d: &Context, ports: &Ports, watch: &mut WakeWatch) -> Option<Wake> {
    let wake = watch.observe(d.services.clock.now_ms(), Instant::now())?;
    tracing::info!(slept_ms = wake.slept_ms, "sortie de veille");
    let events = &d.services.events;
    let woke = EventDraft::new(
        "host.woke",
        json!({"slept_ms": wake.slept_ms, "slept": duration_text(wake.slept_ms)}),
    );
    if let Err(e) = events.append(woke).await {
        tracing::warn!(error = %e, "sortie de veille non journalisée");
    }
    let health = health(ports).await;
    if let Err(e) = events.append(EventDraft::new("host.health", health)).await {
        tracing::warn!(error = %e, "passe de santé non journalisée");
    }
    Some(wake)
}

/// Passe de santé au réveil : le canal répond-il, les serveurs MCP tiennent-ils ? Un serveur
/// dégradé, en échec ou en attente de reprise est relancé tout de suite, plutôt qu'au
/// prochain appel raté.
pub async fn health(ports: &Ports) -> Value {
    let channel = match ports.delivery.get() {
        None => json!("absent"),
        Some(c) => {
            let mut result = c.probe().await;
            for pause in PROBE_PAUSES_MS {
                if result.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(pause)).await;
                result = c.probe().await;
            }
            match result {
                Ok(()) => json!("ok"),
                Err(e) => {
                    tracing::warn!(error = %e, "canal injoignable au réveil");
                    json!({"error": e})
                }
            }
        }
    };
    let mut restarted = Vec::new();
    if let Some(mcp) = ports.mcp.get() {
        for status in mcp.statuses().await {
            // Un serveur paresseux au repos n'a rien à rattraper ; un serveur qui a échoué
            // attendrait son délai de reprise, jusqu'à cinq minutes.
            let stuck = match status.state {
                ServerState::Degraded | ServerState::Failed => true,
                ServerState::Configured | ServerState::Connecting => status.failures > 0,
                ServerState::Ready | ServerState::Disabled | ServerState::AuthRequired => false,
            };
            if !stuck {
                continue;
            }
            let outcome = match mcp.restart(&status.name).await {
                Ok(now) => json!(now.state.as_str()),
                Err(e) => json!({"error": e}),
            };
            restarted.push(json!({"server": status.name, "state": outcome}));
        }
    }
    json!({"channel": channel, "mcp_restarted": restarted})
}

/// Un créneau parti après son heure : l'heure prévue, les créneaux qu'il rattrape, et si
/// une veille l'explique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Late {
    pub planned_ms: i64,
    pub missed: u32,
    pub slept: Option<i64>,
}

/// Retard du créneau `sched.next_run` à `now_ms`, au-delà de [`LATE_AFTER_MS`]. Les
/// créneaux manqués entre-temps sont comptés : ils partent en un seul run.
pub fn late_of(sched: &Schedule, now_ms: i64, tz: &str, wake: Option<Wake>) -> Option<Late> {
    let planned_ms = chrono::DateTime::parse_from_rfc3339(sched.next_run.as_deref()?)
        .ok()?
        .timestamp_millis();
    if now_ms - planned_ms < LATE_AFTER_MS {
        return None;
    }
    let mut missed = 1u32;
    let mut slot = planned_ms;
    while missed < 1_000 {
        match sched.next_after(slot, tz) {
            Some(next) if next > slot && next <= now_ms => {
                missed += 1;
                slot = next;
            }
            _ => break,
        }
    }
    let slept = wake
        .filter(|w| planned_ms >= w.at_ms - w.slept_ms && planned_ms <= w.at_ms)
        .map(|w| w.slept_ms);
    Some(Late {
        planned_ms,
        missed,
        slept,
    })
}

/// « ⏰ Exécution en retard : prévue à 8h30, lancée à 10h02 après une veille de 3 h 32. »
pub fn late_text(late: &Late, now_ms: i64, tz: &str) -> String {
    let zone = tz.parse::<chrono_tz::Tz>().unwrap_or(chrono_tz::Tz::UTC);
    let local = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap_or_default()
            .with_timezone(&zone)
    };
    let (planned, now) = (local(late.planned_ms), local(now_ms));
    let hour = |t: &chrono::DateTime<chrono_tz::Tz>| t.format("%-Hh%M").to_string();
    let when = if planned.date_naive() == now.date_naive() {
        format!("à {}", hour(&planned))
    } else {
        format!("le {} à {}", planned.format("%d/%m"), hour(&planned))
    };
    let mut text = format!(
        "⏰ Exécution en retard : prévue {when}, lancée à {}",
        hour(&now)
    );
    if let Some(slept) = late.slept {
        text.push_str(&format!(" après une veille de {}", duration_text(slept)));
    }
    text.push('.');
    if late.missed > 1 {
        text.push_str(&format!(
            " Les {} créneaux manqués partent en une seule exécution.",
            late.missed
        ));
    }
    text
}

/// « 3 h 32 », « 12 min ».
fn duration_text(ms: i64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_step_is_not_a_sleep() {
        let start = Instant::now();
        let mut watch = WakeWatch {
            wall_ms: 0,
            mono: start,
            last: None,
        };
        // Machine chargée : cinq minutes pour un passage, sur les deux horloges.
        let five = Duration::from_secs(300);
        assert_eq!(watch.observe(300_000, start + five), None);
        // Veille de trois heures : l'horloge monotone n'a pas bougé.
        let wake = watch
            .observe(300_000 + 3 * 3_600_000, start + five)
            .unwrap();
        assert_eq!(wake.slept_ms, 3 * 3_600_000);
        assert_eq!(watch.last(), Some(wake));
        // Horloge murale reculée (NTP) : pas une veille.
        assert_eq!(watch.observe(0, start + five), None);
    }

    #[test]
    fn durations_read_in_hours_and_minutes() {
        assert_eq!(duration_text(12 * 60_000), "12 min");
        assert_eq!(duration_text(3 * 3_600_000 + 2 * 60_000), "3 h 02");
    }
}
