//! `penelope approvals stats` : la mesure qui décide si le juge d'approbation vaut son coût
//! (issue #203, arbitrage 6, épopée #208 lot K tâche T21).
//!
//! Compte, sur N jours, les cartes `shell_exec` pour lesquelles « Toujours » ne peut écrire
//! aucune règle : ce sont les seules que le juge aurait à voir. La base est ouverte en
//! **lecture seule** (`SQLITE_OPEN_READ_ONLY` et `PRAGMA query_only`) : la commande tourne
//! daemon lancé ou arrêté, sans socket, et n'écrit jamais rien.
//!
//! Le caractère « sans motif » n'est pas enregistré dans `approval_requests` : la colonne
//! `rule_created` dit seulement si un clic a écrit une règle (`always`) ou non (`NULL`), et
//! un « Autoriser » simple la laisse vide aussi. On le recalcule donc avec le même
//! prédicat que la carte, `always_creates_no_rule`, sur les arguments de la demande :
//! c'est le lexer d'aujourd'hui qui juge, donc ce que le juge verrait s'il était branché.

use super::*;
use chrono::{DateTime, Duration, Utc};
use clap::Subcommand;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Subcommand, Debug)]
pub enum ApprovalsCmd {
    /// Cartes `shell_exec` sans motif possible sur N jours : la mesure préalable au juge
    /// d'approbation (#203). Lecture seule, sans daemon.
    Stats {
        /// Fenêtre de mesure, en jours.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
}

/// Seuils proposés pour passer au juge (T22). Motivés dans `docs/install-headless.md`,
/// « Mesurer avant d'activer le juge ».
const GO_PER_WEEK: f64 = 10.0;
const GO_DISTINCT: usize = 5;
const GO_APPROVED_SHARE: f64 = 0.8;
/// Longueur de commande affichée, après masquage des secrets.
const SHOWN_CHARS: usize = 40;
const TOP: usize = 10;

#[derive(Debug, Serialize, PartialEq)]
pub(super) struct Stats {
    pub jours: u32,
    pub depuis: String,
    /// Toutes les cartes `shell_exec` de la fenêtre.
    pub cartes_shell: usize,
    /// Celles où « Toujours » n'écrit aucune règle.
    pub sans_motif: usize,
    /// Parmi elles, celles que le juge de #203 verrait : ni double confirmation, ni
    /// risque destructif.
    pub eligibles_juge: usize,
    pub par_semaine: f64,
    pub commandes_distinctes: usize,
    /// Par état final : approved, denied, expired, cancelled, pending.
    pub par_etat: BTreeMap<String, usize>,
    /// Part approuvée parmi les cartes sans motif tranchées (approuvées ou refusées).
    pub part_oui: Option<f64>,
    pub plus_frequentes: Vec<Frequent>,
    pub verdict: Verdict,
}

#[derive(Debug, Serialize, PartialEq)]
pub(super) struct Frequent {
    /// Début de la commande normalisée, secrets masqués.
    pub commande: String,
    /// Douze premiers caractères du SHA-256 de la commande normalisée entière.
    pub empreinte: String,
    pub cartes: usize,
    pub oui: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub(super) struct Verdict {
    pub go: bool,
    pub raisons: Vec<String>,
}

pub(super) fn run(cli: &Cli, days: u32) -> CliResult<()> {
    if days == 0 {
        return Err(CliError::Usage("`--days` vaut au moins 1".into()));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let db = dirs.db_path();
    if !db.is_file() {
        return Err(CliError::Io(format!("aucune base à {}", db.display())));
    }
    let conn = open_read_only(&db).map_err(|e| CliError::Io(e.to_string()))?;
    let stats = measure(&conn, Utc::now(), days).map_err(|e| CliError::Io(e.to_string()))?;
    if cli.json {
        let v = serde_json::to_value(&stats).map_err(|e| CliError::Io(e.to_string()))?;
        output::print(&v, true);
    } else {
        println!("{}", render(&stats));
    }
    Ok(())
}

/// Ouverture qui ne peut rien écrire : ni la base, ni un journal, ni une migration.
pub(super) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA query_only=ON;")?;
    Ok(conn)
}

/// Une commande normalisée : blancs de tête et de fin retirés, suites de blancs réduites
/// à une espace. Deux appels qui ne diffèrent que par la mise en page comptent pour un.
fn normalise(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Ce qui s'affiche d'une commande : secrets masqués d'abord, puis tronquée.
fn shown(command: &str) -> String {
    let masked = penelope_observe::redact::redact(command);
    if masked.chars().count() <= SHOWN_CHARS {
        return masked;
    }
    let head: String = masked.chars().take(SHOWN_CHARS).collect();
    format!("{head}…")
}

pub(super) fn measure(conn: &Connection, now: DateTime<Utc>, days: u32) -> rusqlite::Result<Stats> {
    let since = now - Duration::days(i64::from(days));
    let mut st = conn.prepare(
        "SELECT created_at, state, risk, payload FROM approval_requests
         WHERE kind = 'tool_call' AND subject = 'shell_exec'",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;

    let (mut cartes_shell, mut sans_motif, mut eligibles_juge) = (0, 0, 0);
    let mut par_etat: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_command: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for row in rows {
        let (created_at, state, risk, payload) = row?;
        // Une date illisible est hors fenêtre plutôt que comptée au hasard.
        let Ok(created) = DateTime::parse_from_rfc3339(&created_at) else {
            continue;
        };
        let created = created.with_timezone(&Utc);
        if created < since || created > now {
            continue;
        }
        cartes_shell += 1;
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let arguments = payload.get("arguments");
        if !penelope_daemon::agent::always_creates_no_rule("shell_exec", arguments) {
            continue;
        }
        sans_motif += 1;
        if risk != "destructive" && payload["double"] != Value::Bool(true) {
            eligibles_juge += 1;
        }
        *par_etat.entry(state.clone()).or_default() += 1;
        let command = normalise(
            arguments
                .and_then(|a| a.get("command"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        let entry = by_command.entry(command).or_default();
        entry.0 += 1;
        entry.1 += usize::from(state == "approved");
    }

    let approved = par_etat.get("approved").copied().unwrap_or(0);
    let decided = approved + par_etat.get("denied").copied().unwrap_or(0);
    let part_oui = (decided > 0).then(|| approved as f64 / decided as f64);
    let par_semaine = eligibles_juge as f64 * 7.0 / f64::from(days);
    let commandes_distinctes = by_command.len();

    let mut ranked: Vec<(String, (usize, usize))> = by_command.into_iter().collect();
    // Du plus fréquent au moins fréquent ; à égalité, l'ordre de la commande, pour une
    // sortie stable.
    ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    let plus_frequentes = ranked
        .into_iter()
        .take(TOP)
        .map(|(command, (cartes, oui))| Frequent {
            commande: shown(&command),
            empreinte: penelope_kernel::canonical::sha256_hex(command.as_bytes())[..12].to_string(),
            cartes,
            oui,
        })
        .collect();

    Ok(Stats {
        jours: days,
        depuis: since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        cartes_shell,
        sans_motif,
        eligibles_juge,
        par_semaine: (par_semaine * 10.0).round() / 10.0,
        commandes_distinctes,
        par_etat,
        part_oui: part_oui.map(|p| (p * 100.0).round() / 100.0),
        plus_frequentes,
        verdict: verdict(par_semaine, commandes_distinctes, part_oui),
    })
}

/// Go pour T22 quand les trois seuils tiennent ; sinon chaque seuil manqué dit pourquoi.
fn verdict(per_week: f64, distinct: usize, approved_share: Option<f64>) -> Verdict {
    let mut raisons = Vec::new();
    if per_week < GO_PER_WEEK {
        raisons.push(format!(
            "{per_week:.1} cartes éligibles par semaine, moins de {GO_PER_WEEK} : le coût \
             reste faible pour le propriétaire"
        ));
    }
    if distinct < GO_DISTINCT {
        raisons.push(format!(
            "{distinct} commandes distinctes, moins de {GO_DISTINCT} : une règle ou un \
             correctif du lexer pour ces formes-là coûte moins qu'un juge"
        ));
    }
    match approved_share {
        Some(share) if share >= GO_APPROVED_SHARE => {}
        Some(share) => raisons.push(format!(
            "{:.0} % de oui, moins de {:.0} % : les cartes arrêtent des commandes, un juge \
             qui les adoucit n'est pas souhaitable",
            share * 100.0,
            GO_APPROVED_SHARE * 100.0
        )),
        None => raisons.push("aucune carte tranchée : rien à mesurer".into()),
    }
    Verdict {
        go: raisons.is_empty(),
        raisons,
    }
}

pub(super) fn render(s: &Stats) -> String {
    let mut out = format!(
        "Cartes shell_exec sur {} jours (depuis {}) : {}\n\
         Sans motif possible (« Toujours » n'écrit aucune règle) : {}\n\
         Dont éligibles au juge (ni double confirmation, ni destructif) : {} ({} par semaine)\n\
         Commandes distinctes : {}\n",
        s.jours,
        s.depuis,
        s.cartes_shell,
        s.sans_motif,
        s.eligibles_juge,
        s.par_semaine,
        s.commandes_distinctes,
    );
    let states: Vec<String> = s.par_etat.iter().map(|(k, v)| format!("{k} {v}")).collect();
    out.push_str(&format!(
        "États : {}\n",
        if states.is_empty() {
            "(aucun)".to_string()
        } else {
            states.join(", ")
        }
    ));
    out.push_str(&match s.part_oui {
        Some(p) => format!("Part décidée « oui » : {:.0} %\n", p * 100.0),
        None => "Part décidée « oui » : (aucune carte tranchée)\n".to_string(),
    });
    if !s.plus_frequentes.is_empty() {
        let rows: Vec<Value> = s
            .plus_frequentes
            .iter()
            .map(|f| {
                json!({
                    "cartes": f.cartes,
                    "oui": f.oui,
                    "empreinte": f.empreinte,
                    "commande": f.commande,
                })
            })
            .collect();
        out.push_str("\nLes plus fréquentes :\n");
        out.push_str(&output::table(&rows));
        out.push('\n');
    }
    out.push_str(&format!(
        "\nVerdict proposé pour le juge (T22) : {}",
        if s.verdict.go { "go" } else { "no-go" }
    ));
    for r in &s.verdict.raisons {
        out.push_str(&format!("\n  - {r}"));
    }
    out
}

#[cfg(test)]
mod tests;
